use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use base64::Engine;
use prost::Message;
use rusqlite::{types::ValueRef, Connection, OpenFlags, Row};
use sha2::{Digest, Sha256};

use crate::restore::scoped_opaque_id;
use crate::{
    CanonicalConversation, CanonicalMessage, CanonicalParticipant, ConversationKind,
    ConversationMembership, ConversationMembershipRole, EntityDecodeState, EntitySourceRecord,
    LocalProfileState, PreparedCatalog, PreparedDatabase, RawSQLiteValue, RestoreError,
};

#[derive(Debug, Default)]
pub struct EntitySeeds {
    conversations: BTreeMap<String, Vec<u8>>,
    participants: BTreeMap<String, Vec<u8>>,
    observed_memberships: BTreeMap<String, BTreeSet<String>>,
}

impl EntitySeeds {
    pub fn observe_message(&mut self, message: &CanonicalMessage) {
        if let Ok(value) = base64::engine::general_purpose::STANDARD
            .decode(&message.conversation_source_identifier_base64)
        {
            self.conversations
                .entry(message.conversation_id.clone())
                .or_insert(value.clone());
            if conversation_kind(&value) == ConversationKind::Direct {
                let participant_id = scoped_opaque_id(&message.account_id, &value);
                self.participants
                    .entry(participant_id.clone())
                    .or_insert(value);
                self.observed_memberships
                    .entry(message.conversation_id.clone())
                    .or_default()
                    .insert(participant_id);
            }
        }
        if let (Some(participant_id), Some(encoded)) = (
            message.sender_id.as_ref(),
            message.sender_source_identifier_base64.as_ref(),
        ) {
            if let Ok(value) = base64::engine::general_purpose::STANDARD.decode(encoded) {
                self.participants
                    .entry(participant_id.clone())
                    .or_insert(value);
                self.observed_memberships
                    .entry(message.conversation_id.clone())
                    .or_default()
                    .insert(participant_id.clone());
            }
        }
    }
}

#[derive(Debug)]
pub struct EntityRestorationResult {
    pub conversations_path: PathBuf,
    pub participants_path: PathBuf,
    pub conversation_count: u64,
    pub participant_count: u64,
    pub group_member_count: u64,
    pub source_row_count: u64,
    pub decode_gap_count: u64,
    pub missing_local_profile_count: u64,
    pub unresolved_conversation_count: u64,
}

#[derive(Debug)]
struct ParticipantBuilder {
    participant: CanonicalParticipant,
}

#[derive(Debug)]
struct ConversationBuilder {
    conversation: CanonicalConversation,
    membership_keys: BTreeSet<(String, String)>,
}

pub fn restore_entities(
    catalog: &PreparedCatalog,
    account_id: &str,
    seeds: EntitySeeds,
    output_directory: &Path,
) -> Result<EntityRestorationResult, RestoreError> {
    let conversations_path = output_directory.join("conversations.ndjson");
    let participants_path = output_directory.join("participants.ndjson");
    let mut conversations = BTreeMap::<String, ConversationBuilder>::new();
    let mut participants = BTreeMap::<String, ParticipantBuilder>::new();
    let mut source_row_count = 0_u64;

    for (id, source) in seeds.conversations {
        ensure_conversation(&mut conversations, account_id, id, source);
    }
    for (id, source) in seeds.participants {
        ensure_participant(&mut participants, account_id, id, source);
    }
    for (conversation_id, participant_ids) in seeds.observed_memberships {
        for participant_id in participant_ids {
            add_membership(
                &mut conversations,
                &mut participants,
                &conversation_id,
                &participant_id,
                ConversationMembershipRole::ObservedSender,
                None,
            );
        }
    }

    for database in &catalog.databases {
        let connection = match readonly_connection(database) {
            Ok(connection) => connection,
            Err(_) => continue,
        };
        for table in &database.tables {
            if table.eq_ignore_ascii_case("SessionTable") {
                let columns = match table_columns(&connection, table) {
                    Ok(columns) => columns,
                    Err(_) => continue,
                };
                let Some(username_index) = find_column(
                    &columns,
                    &["username", "user_name", "talker", "conversation_id"],
                ) else {
                    continue;
                };
                for_each_row(&connection, table, |row| {
                    let source = get_bytes(row.get_ref(username_index + 1).ok());
                    if source.is_empty() {
                        return Ok(());
                    }
                    let conversation_id = scoped_opaque_id(account_id, &source);
                    let builder = ensure_conversation(
                        &mut conversations,
                        account_id,
                        conversation_id.clone(),
                        source.clone(),
                    );
                    builder
                        .conversation
                        .source_records
                        .push(source_record(database, table, &columns, row)?);
                    source_row_count += 1;
                    if conversation_kind(&source) == ConversationKind::Direct {
                        let participant_id = scoped_opaque_id(account_id, &source);
                        ensure_participant(
                            &mut participants,
                            account_id,
                            participant_id.clone(),
                            source,
                        );
                        add_membership(
                            &mut conversations,
                            &mut participants,
                            &conversation_id,
                            &participant_id,
                            ConversationMembershipRole::DirectPeer,
                            None,
                        );
                    }
                    Ok(())
                })?;
            }
        }
    }

    for database in &catalog.databases {
        let connection = match readonly_connection(database) {
            Ok(connection) => connection,
            Err(_) => continue,
        };
        for table in &database.tables {
            if !table.eq_ignore_ascii_case("chat_room") {
                continue;
            }
            let columns = match table_columns(&connection, table) {
                Ok(columns) => columns,
                Err(_) => continue,
            };
            let Some(username_index) = find_column(&columns, &["username", "user_name"]) else {
                continue;
            };
            let owner_index = find_column(&columns, &["owner", "owner_username"]);
            let ext_index = find_column(&columns, &["ext_buffer", "room_data", "member_data"]);
            for_each_row(&connection, table, |row| {
                let source = get_bytes(row.get_ref(username_index + 1).ok());
                if source.is_empty() {
                    return Ok(());
                }
                let conversation_id = scoped_opaque_id(account_id, &source);
                let builder = ensure_conversation(
                    &mut conversations,
                    account_id,
                    conversation_id.clone(),
                    source,
                );
                builder.conversation.kind = ConversationKind::Group;
                builder
                    .conversation
                    .source_records
                    .push(source_record(database, table, &columns, row)?);
                source_row_count += 1;

                if let Some(owner) = owner_index
                    .map(|index| get_bytes(row.get_ref(index + 1).ok()))
                    .filter(|value| !value.is_empty())
                {
                    let participant_id = scoped_opaque_id(account_id, &owner);
                    ensure_participant(
                        &mut participants,
                        account_id,
                        participant_id.clone(),
                        owner,
                    );
                    add_membership(
                        &mut conversations,
                        &mut participants,
                        &conversation_id,
                        &participant_id,
                        ConversationMembershipRole::Owner,
                        None,
                    );
                    if let Some(conversation) = conversations.get_mut(&conversation_id) {
                        conversation.conversation.owner_participant_id = Some(participant_id);
                    }
                }

                let ext = ext_index
                    .map(|index| get_bytes(row.get_ref(index + 1).ok()))
                    .unwrap_or_default();
                if ext.is_empty() {
                    if let Some(conversation) = conversations.get_mut(&conversation_id) {
                        conversation.conversation.entity_decode_state = EntityDecodeState::RawOnly;
                    }
                    return Ok(());
                }
                match RoomDataProto::decode(ext.as_slice()) {
                    Ok(room) => {
                        for member in room.users {
                            if member.user_name.is_empty() {
                                continue;
                            }
                            let source = member.user_name.into_bytes();
                            let participant_id = scoped_opaque_id(account_id, &source);
                            ensure_participant(
                                &mut participants,
                                account_id,
                                participant_id.clone(),
                                source,
                            );
                            let display = member
                                .display_name
                                .filter(|value| !value.is_empty())
                                .map(|value| {
                                    base64::engine::general_purpose::STANDARD
                                        .encode(value.as_bytes())
                                });
                            add_membership(
                                &mut conversations,
                                &mut participants,
                                &conversation_id,
                                &participant_id,
                                ConversationMembershipRole::Member,
                                display,
                            );
                        }
                    }
                    Err(_) => {
                        if let Some(conversation) = conversations.get_mut(&conversation_id) {
                            conversation.conversation.entity_decode_state =
                                EntityDecodeState::Failed;
                        }
                    }
                }
                Ok(())
            })?;
        }
    }

    for database in &catalog.databases {
        let connection = match readonly_connection(database) {
            Ok(connection) => connection,
            Err(_) => continue,
        };
        for table in &database.tables {
            if !table.eq_ignore_ascii_case("contact") {
                continue;
            }
            let columns = match table_columns(&connection, table) {
                Ok(columns) => columns,
                Err(_) => continue,
            };
            let Some(username_index) = find_column(&columns, &["username", "user_name"]) else {
                continue;
            };
            let alias_index = find_column(&columns, &["alias"]);
            let remark_index = find_column(&columns, &["remark", "remark_name"]);
            let nickname_index = find_column(&columns, &["nick_name", "nickname"]);
            for_each_row(&connection, table, |row| {
                let source = get_bytes(row.get_ref(username_index + 1).ok());
                if source.is_empty() {
                    return Ok(());
                }
                let participant_id = scoped_opaque_id(account_id, &source);
                let referenced_as_direct_conversation = conversations
                    .get(&participant_id)
                    .is_some_and(|value| value.conversation.kind == ConversationKind::Direct);
                if !participants.contains_key(&participant_id) && !referenced_as_direct_conversation
                {
                    return Ok(());
                }
                let participant =
                    ensure_participant(&mut participants, account_id, participant_id, source);
                participant.participant.alias_base64 =
                    encoded_column(row, alias_index).filter(|value| !value.is_empty());
                participant.participant.remark_base64 =
                    encoded_column(row, remark_index).filter(|value| !value.is_empty());
                participant.participant.nickname_base64 =
                    encoded_column(row, nickname_index).filter(|value| !value.is_empty());
                participant
                    .participant
                    .source_records
                    .push(source_record(database, table, &columns, row)?);
                source_row_count += 1;
                Ok(())
            })?;
        }
    }

    for conversation in conversations.values_mut() {
        conversation.conversation.participant_ids = conversation
            .conversation
            .memberships
            .iter()
            .map(|membership| membership.participant_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        conversation
            .conversation
            .memberships
            .sort_by(|left, right| {
                (&left.participant_id, role_rank(left.role))
                    .cmp(&(&right.participant_id, role_rank(right.role)))
            });
        conversation
            .conversation
            .source_records
            .sort_by(source_record_order);
    }
    for participant in participants.values_mut() {
        participant.participant.conversation_ids.sort();
        participant.participant.conversation_ids.dedup();
        participant
            .participant
            .source_records
            .sort_by(source_record_order);
        participant.participant.local_profile_state =
            if participant.participant.source_records.is_empty() {
                LocalProfileState::MissingLocalRecord
            } else {
                LocalProfileState::Hydrated
            };
    }

    let mut conversation_writer = owner_only_writer(&conversations_path)?;
    for conversation in conversations.values() {
        serde_json::to_writer(&mut conversation_writer, &conversation.conversation)?;
        conversation_writer.write_all(b"\n")?;
    }
    conversation_writer.flush()?;
    let mut participant_writer = owner_only_writer(&participants_path)?;
    for participant in participants.values() {
        serde_json::to_writer(&mut participant_writer, &participant.participant)?;
        participant_writer.write_all(b"\n")?;
    }
    participant_writer.flush()?;

    Ok(EntityRestorationResult {
        conversations_path,
        participants_path,
        conversation_count: conversations.len() as u64,
        participant_count: participants.len() as u64,
        group_member_count: conversations
            .values()
            .flat_map(|value| &value.conversation.memberships)
            .filter(|membership| membership.role == ConversationMembershipRole::Member)
            .count() as u64,
        source_row_count,
        decode_gap_count: conversations
            .values()
            .filter(|value| value.conversation.entity_decode_state != EntityDecodeState::Complete)
            .count() as u64,
        missing_local_profile_count: participants
            .values()
            .filter(|value| {
                value.participant.local_profile_state == LocalProfileState::MissingLocalRecord
            })
            .count() as u64,
        unresolved_conversation_count: conversations
            .values()
            .filter(|value| value.conversation.kind == ConversationKind::Unresolved)
            .count() as u64,
    })
}

fn ensure_conversation<'a>(
    conversations: &'a mut BTreeMap<String, ConversationBuilder>,
    account_id: &str,
    conversation_id: String,
    source: Vec<u8>,
) -> &'a mut ConversationBuilder {
    conversations
        .entry(conversation_id.clone())
        .or_insert_with(|| ConversationBuilder {
            conversation: CanonicalConversation {
                conversation_id,
                account_id: account_id.to_string(),
                source_identifier_base64: base64::engine::general_purpose::STANDARD.encode(&source),
                kind: conversation_kind(&source),
                participant_ids: Vec::new(),
                memberships: Vec::new(),
                owner_participant_id: None,
                entity_decode_state: EntityDecodeState::Complete,
                source_records: Vec::new(),
            },
            membership_keys: BTreeSet::new(),
        })
}

fn ensure_participant<'a>(
    participants: &'a mut BTreeMap<String, ParticipantBuilder>,
    account_id: &str,
    participant_id: String,
    source: Vec<u8>,
) -> &'a mut ParticipantBuilder {
    participants
        .entry(participant_id.clone())
        .or_insert_with(|| ParticipantBuilder {
            participant: CanonicalParticipant {
                participant_id,
                account_id: account_id.to_string(),
                source_identifier_base64: base64::engine::general_purpose::STANDARD.encode(source),
                alias_base64: None,
                remark_base64: None,
                nickname_base64: None,
                display_name_base64: None,
                local_profile_state: LocalProfileState::MissingLocalRecord,
                conversation_ids: Vec::new(),
                source_records: Vec::new(),
            },
        })
}

fn add_membership(
    conversations: &mut BTreeMap<String, ConversationBuilder>,
    participants: &mut BTreeMap<String, ParticipantBuilder>,
    conversation_id: &str,
    participant_id: &str,
    role: ConversationMembershipRole,
    display_name_base64: Option<String>,
) {
    let Some(conversation) = conversations.get_mut(conversation_id) else {
        return;
    };
    let key = (
        participant_id.to_string(),
        format!("{:02}", role_rank(role)),
    );
    if conversation.membership_keys.insert(key) {
        conversation
            .conversation
            .memberships
            .push(ConversationMembership {
                participant_id: participant_id.to_string(),
                role,
                display_name_base64: display_name_base64.clone(),
            });
    }
    if let Some(participant) = participants.get_mut(participant_id) {
        participant
            .participant
            .conversation_ids
            .push(conversation_id.to_string());
        if participant.participant.display_name_base64.is_none() {
            participant.participant.display_name_base64 = display_name_base64;
        }
    }
}

fn conversation_kind(source: &[u8]) -> ConversationKind {
    let value = String::from_utf8_lossy(source).to_ascii_lowercase();
    if value.starts_with("unresolved:") {
        ConversationKind::Unresolved
    } else if value.ends_with("@chatroom") {
        ConversationKind::Group
    } else if value.starts_with("gh_") {
        ConversationKind::Business
    } else if value.contains("chatbot") {
        ConversationKind::Chatbot
    } else if value == "weixin" || value.starts_with("medianote") || value.starts_with("fmessage") {
        ConversationKind::System
    } else {
        ConversationKind::Direct
    }
}

#[derive(Clone, PartialEq, Message)]
struct RoomDataProto {
    #[prost(message, repeated, tag = "1")]
    users: Vec<RoomDataUserProto>,
}

#[derive(Clone, PartialEq, Message)]
struct RoomDataUserProto {
    #[prost(string, tag = "1")]
    user_name: String,
    #[prost(string, optional, tag = "2")]
    display_name: Option<String>,
}

fn readonly_connection(database: &PreparedDatabase) -> Result<Connection, RestoreError> {
    let connection = Connection::open_with_flags(&database.path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.execute_batch("PRAGMA query_only = ON")?;
    Ok(connection)
}

fn table_columns(connection: &Connection, table: &str) -> Result<Vec<String>, RestoreError> {
    let sql = format!("PRAGMA table_info({})", quote_identifier(table));
    let mut statement = connection.prepare(&sql)?;
    let result = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(result)
}

fn for_each_row(
    connection: &Connection,
    table: &str,
    mut operation: impl FnMut(&Row<'_>) -> Result<(), RestoreError>,
) -> Result<(), RestoreError> {
    let sql = format!(
        "SELECT rowid, * FROM {} ORDER BY rowid",
        quote_identifier(table)
    );
    let mut statement = match connection.prepare(&sql) {
        Ok(statement) => statement,
        Err(_) => return Ok(()),
    };
    let mut rows = match statement.query([]) {
        Ok(rows) => rows,
        Err(_) => return Ok(()),
    };
    loop {
        let row = match rows.next() {
            Ok(Some(row)) => row,
            Ok(None) | Err(_) => break,
        };
        // Entity tables enrich message-derived seeds. A malformed profile or
        // group row cannot make those canonical message identities unusable.
        let _ = operation(row);
    }
    Ok(())
}

fn source_record(
    database: &PreparedDatabase,
    table: &str,
    columns: &[String],
    row: &Row<'_>,
) -> Result<EntitySourceRecord, RestoreError> {
    let raw_columns = columns
        .iter()
        .enumerate()
        .map(|(index, name)| Ok((name.clone(), raw_sqlite_value(row.get_ref(index + 1)?))))
        .collect::<Result<BTreeMap<_, _>, rusqlite::Error>>()?;
    Ok(EntitySourceRecord {
        source_set_id: database.source_set_id.clone(),
        source_logical_path: database.logical_path.clone(),
        source_table_id: opaque_id(table.as_bytes()),
        source_table_name: table.to_string(),
        source_row_id: get_i64(row.get_ref(0).ok()).unwrap_or_default(),
        raw_columns,
    })
}

fn find_column(columns: &[String], aliases: &[&str]) -> Option<usize> {
    columns.iter().position(|column| {
        aliases
            .iter()
            .any(|alias| column.eq_ignore_ascii_case(alias))
    })
}

fn encoded_column(row: &Row<'_>, index: Option<usize>) -> Option<String> {
    index
        .map(|index| get_bytes(row.get_ref(index + 1).ok()))
        .filter(|value| !value.is_empty())
        .map(|value| base64::engine::general_purpose::STANDARD.encode(value))
}

fn get_i64(value: Option<ValueRef<'_>>) -> Option<i64> {
    match value? {
        ValueRef::Integer(value) => Some(value),
        ValueRef::Real(value) => Some(value as i64),
        ValueRef::Text(value) => std::str::from_utf8(value).ok()?.parse().ok(),
        _ => None,
    }
}

fn get_bytes(value: Option<ValueRef<'_>>) -> Vec<u8> {
    match value {
        Some(ValueRef::Blob(value)) | Some(ValueRef::Text(value)) => value.to_vec(),
        Some(ValueRef::Integer(value)) => value.to_string().into_bytes(),
        Some(ValueRef::Real(value)) => value.to_string().into_bytes(),
        _ => Vec::new(),
    }
}

fn raw_sqlite_value(value: ValueRef<'_>) -> RawSQLiteValue {
    match value {
        ValueRef::Null => RawSQLiteValue::Null,
        ValueRef::Integer(value) => RawSQLiteValue::Integer(value),
        ValueRef::Real(value) => RawSQLiteValue::Real(value),
        ValueRef::Text(value) => {
            RawSQLiteValue::TextBase64(base64::engine::general_purpose::STANDARD.encode(value))
        }
        ValueRef::Blob(value) => {
            RawSQLiteValue::BlobBase64(base64::engine::general_purpose::STANDARD.encode(value))
        }
    }
}

fn source_record_order(
    left: &EntitySourceRecord,
    right: &EntitySourceRecord,
) -> std::cmp::Ordering {
    (
        &left.source_logical_path,
        &left.source_table_name,
        left.source_row_id,
    )
        .cmp(&(
            &right.source_logical_path,
            &right.source_table_name,
            right.source_row_id,
        ))
}

fn role_rank(role: ConversationMembershipRole) -> u8 {
    match role {
        ConversationMembershipRole::Owner => 0,
        ConversationMembershipRole::DirectPeer => 1,
        ConversationMembershipRole::Member => 2,
        ConversationMembershipRole::ObservedSender => 3,
    }
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn opaque_id(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}

fn owner_only_writer(path: &Path) -> Result<BufWriter<File>, RestoreError> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    Ok(BufWriter::new(file))
}
