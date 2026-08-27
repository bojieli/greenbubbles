use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use rusqlite::backup::Backup;
use rusqlite::{params, Connection, OpenFlags, Transaction, TransactionBehavior};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::archive::{ensure_private_directory, ensure_private_regular_file, load_report};
use crate::{
    CanonicalArtifact, CanonicalConversation, CanonicalMessage, CanonicalParticipant, ReplicaKey,
    RestorationCoverage, RestorationReport, RestoreError, TypedPayload,
};

const CURRENT_SCHEMA_VERSION: u32 = 2;
const REPLICA_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplicaBootstrapReport {
    pub format_version: u32,
    pub schema_version: u32,
    pub account_id: String,
    pub source_fingerprint: String,
    pub cipher_version: String,
    pub encrypted_at_rest: bool,
    pub idempotent: bool,
    pub conversation_count: u64,
    pub participant_count: u64,
    pub message_count: u64,
    pub artifact_count: u64,
    pub relationship_count: u64,
    pub message_artifact_count: u64,
    pub pre_migration_backup_file_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplicaStatus {
    pub format_version: u32,
    pub schema_version: u32,
    pub account_id: Option<String>,
    pub current_source_fingerprint: Option<String>,
    pub cipher_version: String,
    pub encrypted_at_rest: bool,
    pub conversation_count: u64,
    pub participant_count: u64,
    pub message_count: u64,
    pub artifact_count: u64,
    pub last_checkpoint_unix_nanoseconds: Option<u128>,
    pub restoration_complete: Option<bool>,
}

struct OpenedReplica {
    connection: Connection,
    cipher_version: String,
    pre_migration_backup_file_name: Option<String>,
}

#[derive(Default)]
struct ImportCounts {
    conversations: u64,
    participants: u64,
    messages: u64,
    artifacts: u64,
    relationships: u64,
    message_artifacts: u64,
}

pub fn bootstrap_replica(
    archive_directory: &Path,
    replica_path: &Path,
    key: &ReplicaKey,
) -> Result<ReplicaBootstrapReport, RestoreError> {
    ensure_private_directory(archive_directory)?;
    let report = load_report(archive_directory)?;
    let mut opened = open_replica(replica_path, key)?;
    let existing_account: Option<String> = opened
        .connection
        .query_row(
            "SELECT account_id FROM replica_identity WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if existing_account
        .as_deref()
        .is_some_and(|account| account != report.account_id)
    {
        return Err(RestoreError::Integrity(
            "replica belongs to a different account".to_string(),
        ));
    }
    let existing_checkpoint: Option<String> = opened
        .connection
        .query_row(
            "SELECT source_fingerprint FROM source_checkpoint WHERE account_id = ?1",
            [&report.account_id],
            |row| row.get(0),
        )
        .optional()?;
    if existing_checkpoint.as_deref() == Some(&report.source_fingerprint) {
        return bootstrap_report(&opened, &report, true);
    }
    if existing_checkpoint.is_some() {
        return Err(RestoreError::Integrity(
            "replica is already bootstrapped from another checkpoint; use synchronization"
                .to_string(),
        ));
    }

    let counts =
        import_archive_transactionally(&mut opened.connection, archive_directory, &report)?;
    checkpoint_and_secure(&opened.connection, replica_path)?;
    Ok(ReplicaBootstrapReport {
        format_version: REPLICA_FORMAT_VERSION,
        schema_version: CURRENT_SCHEMA_VERSION,
        account_id: report.account_id,
        source_fingerprint: report.source_fingerprint,
        cipher_version: opened.cipher_version,
        encrypted_at_rest: true,
        idempotent: false,
        conversation_count: counts.conversations,
        participant_count: counts.participants,
        message_count: counts.messages,
        artifact_count: counts.artifacts,
        relationship_count: counts.relationships,
        message_artifact_count: counts.message_artifacts,
        pre_migration_backup_file_name: opened.pre_migration_backup_file_name,
    })
}

pub fn replica_status(
    replica_path: &Path,
    key: &ReplicaKey,
) -> Result<ReplicaStatus, RestoreError> {
    let opened = open_replica(replica_path, key)?;
    let identity = opened
        .connection
        .query_row(
            "SELECT account_id, current_source_fingerprint, restoration_complete
             FROM replica_identity WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<bool>>(2)?,
                ))
            },
        )
        .optional()?;
    let checkpoint = if let Some((account, _, _)) = identity.as_ref() {
        let encoded = opened
            .connection
            .query_row(
                "SELECT committed_at_unix_nanoseconds FROM source_checkpoint
                 WHERE account_id = ?1",
                [account],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        encoded
            .map(|value| {
                value.parse::<u128>().map_err(|_| {
                    RestoreError::Integrity("replica checkpoint timestamp is invalid".to_string())
                })
            })
            .transpose()?
    } else {
        None
    };
    Ok(ReplicaStatus {
        format_version: REPLICA_FORMAT_VERSION,
        schema_version: CURRENT_SCHEMA_VERSION,
        account_id: identity.as_ref().map(|value| value.0.clone()),
        current_source_fingerprint: identity.as_ref().and_then(|value| value.1.clone()),
        cipher_version: opened.cipher_version,
        encrypted_at_rest: true,
        conversation_count: table_count(&opened.connection, "conversation")?,
        participant_count: table_count(&opened.connection, "participant")?,
        message_count: table_count(&opened.connection, "message")?,
        artifact_count: table_count(&opened.connection, "artifact")?,
        last_checkpoint_unix_nanoseconds: checkpoint,
        restoration_complete: identity.and_then(|value| value.2),
    })
}

fn open_replica(path: &Path, key: &ReplicaKey) -> Result<OpenedReplica, RestoreError> {
    let existed = path.try_exists()?;
    if existed {
        ensure_private_regular_file(path)?;
    } else {
        let parent = path
            .parent()
            .ok_or_else(|| RestoreError::UnsafePath("replica has no parent".to_string()))?;
        ensure_private_directory(parent)?;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)?;
    }
    let result = (|| {
        let mut connection = open_keyed_connection(path, key)?;
        let version = schema_version(&connection)?;
        if version > CURRENT_SCHEMA_VERSION {
            return Err(RestoreError::Integrity(format!(
                "replica schema version {version} is newer than supported version {CURRENT_SCHEMA_VERSION}"
            )));
        }
        let pre_migration_backup_file_name = if version > 0 && version < CURRENT_SCHEMA_VERSION {
            Some(create_pre_migration_backup(
                &connection,
                path,
                key,
                version,
            )?)
        } else {
            None
        };
        apply_migrations(&mut connection, version)?;
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;
             PRAGMA wal_autocheckpoint = 1000;",
        )?;
        let cipher_version =
            connection.pragma_query_value(None, "cipher_version", |row| row.get::<_, String>(0))?;
        if cipher_version.is_empty() {
            return Err(RestoreError::Integrity(
                "replica SQLite build does not provide SQLCipher".to_string(),
            ));
        }
        secure_replica_files(path)?;
        Ok(OpenedReplica {
            connection,
            cipher_version,
            pre_migration_backup_file_name,
        })
    })();
    if result.is_err() && !existed {
        remove_failed_replica_files(path);
    }
    result
}

fn open_keyed_connection(path: &Path, key: &ReplicaKey) -> Result<Connection, RestoreError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let mut key_hex = hex::encode(key.expose_for_replica_operation());
    let key_statement = Zeroizing::new(format!("PRAGMA key = \"x'{key_hex}'\";"));
    key_hex.zeroize();
    connection.execute_batch(
        "PRAGMA cipher_compatibility = 4;
         PRAGMA cipher_memory_security = ON;",
    )?;
    connection.execute_batch(&key_statement)?;
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA trusted_schema = OFF;
         PRAGMA temp_store = MEMORY;
         PRAGMA secure_delete = ON;",
    )?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.query_row("SELECT count(*) FROM sqlite_schema", [], |_| Ok(()))?;
    Ok(connection)
}

fn schema_version(connection: &Connection) -> Result<u32, RestoreError> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'replica_schema'
         )",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(0);
    }
    let version: i64 = connection.query_row(
        "SELECT schema_version FROM replica_schema WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    u32::try_from(version).map_err(|_| {
        RestoreError::Integrity("replica schema version is outside the supported range".to_string())
    })
}

fn apply_migrations(connection: &mut Connection, from: u32) -> Result<(), RestoreError> {
    for version in (from + 1)..=CURRENT_SCHEMA_VERSION {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        match version {
            1 => migration_1(&transaction)?,
            2 => migration_2(&transaction)?,
            _ => unreachable!("all replica migrations are enumerated"),
        }
        transaction.commit()?;
    }
    Ok(())
}

fn migration_1(transaction: &Transaction<'_>) -> Result<(), RestoreError> {
    transaction.execute_batch(
        "CREATE TABLE replica_schema(
           singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
           schema_version INTEGER NOT NULL,
           replica_format_version INTEGER NOT NULL
         );
         INSERT INTO replica_schema VALUES (1, 1, 1);
         CREATE TABLE migration_history(
           schema_version INTEGER PRIMARY KEY,
           applied_at_unix_nanoseconds TEXT NOT NULL,
           migration_sha256 TEXT NOT NULL
         );
         CREATE TABLE replica_identity(
           singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
           account_id TEXT NOT NULL UNIQUE,
           current_source_fingerprint TEXT,
           restoration_complete INTEGER,
           created_at_unix_nanoseconds TEXT NOT NULL,
           updated_at_unix_nanoseconds TEXT NOT NULL
         );
         CREATE TABLE conversation(
           account_id TEXT NOT NULL,
           conversation_id TEXT NOT NULL,
           kind TEXT NOT NULL,
           entity_decode_state TEXT NOT NULL,
           participant_count INTEGER NOT NULL,
           record_sha256 TEXT NOT NULL,
           record_json BLOB NOT NULL,
           PRIMARY KEY(account_id, conversation_id),
           FOREIGN KEY(account_id) REFERENCES replica_identity(account_id) ON DELETE CASCADE
         ) WITHOUT ROWID;
         CREATE TABLE participant(
           account_id TEXT NOT NULL,
           participant_id TEXT NOT NULL,
           local_profile_state TEXT NOT NULL,
           record_sha256 TEXT NOT NULL,
           record_json BLOB NOT NULL,
           PRIMARY KEY(account_id, participant_id),
           FOREIGN KEY(account_id) REFERENCES replica_identity(account_id) ON DELETE CASCADE
         ) WITHOUT ROWID;
         CREATE TABLE conversation_participant(
           account_id TEXT NOT NULL,
           conversation_id TEXT NOT NULL,
           participant_id TEXT NOT NULL,
           membership_role TEXT NOT NULL,
           display_name_base64 TEXT,
           PRIMARY KEY(account_id, conversation_id, participant_id, membership_role),
           FOREIGN KEY(account_id, conversation_id)
             REFERENCES conversation(account_id, conversation_id) ON DELETE CASCADE,
           FOREIGN KEY(account_id, participant_id)
             REFERENCES participant(account_id, participant_id) ON DELETE CASCADE
         ) WITHOUT ROWID;
         CREATE TABLE artifact(
           account_id TEXT NOT NULL,
           artifact_id TEXT NOT NULL,
           kind TEXT NOT NULL,
           role TEXT NOT NULL,
           availability TEXT NOT NULL,
           source_sha256 TEXT,
           decoded_sha256 TEXT,
           record_sha256 TEXT NOT NULL,
           record_json BLOB NOT NULL,
           PRIMARY KEY(account_id, artifact_id),
           FOREIGN KEY(account_id) REFERENCES replica_identity(account_id) ON DELETE CASCADE
         ) WITHOUT ROWID;
         CREATE TABLE message(
           account_id TEXT NOT NULL,
           canonical_id TEXT NOT NULL,
           conversation_id TEXT NOT NULL,
           sender_id TEXT,
           conversation_ordinal INTEGER NOT NULL,
           created_at_unix INTEGER,
           direction TEXT NOT NULL,
           logical_type INTEGER,
           sub_type INTEGER,
           semantic_decode_state TEXT NOT NULL,
           search_text TEXT NOT NULL,
           record_sha256 TEXT NOT NULL,
           record_json BLOB NOT NULL,
           PRIMARY KEY(account_id, canonical_id),
           UNIQUE(account_id, conversation_id, conversation_ordinal),
           FOREIGN KEY(account_id, conversation_id)
             REFERENCES conversation(account_id, conversation_id) ON DELETE CASCADE
         ) WITHOUT ROWID;
         CREATE TABLE message_relationship(
           account_id TEXT NOT NULL,
           source_canonical_id TEXT NOT NULL,
           relationship_ordinal INTEGER NOT NULL,
           kind TEXT NOT NULL,
           target_canonical_id TEXT,
           resolved INTEGER NOT NULL,
           record_json BLOB NOT NULL,
           PRIMARY KEY(account_id, source_canonical_id, relationship_ordinal),
           FOREIGN KEY(account_id, source_canonical_id)
             REFERENCES message(account_id, canonical_id) ON DELETE CASCADE
         ) WITHOUT ROWID;
         CREATE TABLE message_artifact(
           account_id TEXT NOT NULL,
           canonical_id TEXT NOT NULL,
           artifact_ordinal INTEGER NOT NULL,
           artifact_id TEXT NOT NULL,
           role TEXT NOT NULL,
           preferred INTEGER NOT NULL,
           PRIMARY KEY(account_id, canonical_id, artifact_ordinal),
           FOREIGN KEY(account_id, canonical_id)
             REFERENCES message(account_id, canonical_id) ON DELETE CASCADE,
           FOREIGN KEY(account_id, artifact_id)
             REFERENCES artifact(account_id, artifact_id) ON DELETE CASCADE
         ) WITHOUT ROWID;
         CREATE TABLE coverage_state(
           account_id TEXT PRIMARY KEY,
           source_fingerprint TEXT NOT NULL,
           coverage_json BLOB NOT NULL,
           report_json BLOB NOT NULL,
           full_restoration_achieved INTEGER NOT NULL,
           FOREIGN KEY(account_id) REFERENCES replica_identity(account_id) ON DELETE CASCADE
         ) WITHOUT ROWID;",
    )?;
    record_migration(transaction, 1, "canonical replica base schema")?;
    Ok(())
}

fn migration_2(transaction: &Transaction<'_>) -> Result<(), RestoreError> {
    transaction.execute_batch(
        "CREATE TABLE source_checkpoint(
           account_id TEXT PRIMARY KEY,
           source_fingerprint TEXT NOT NULL UNIQUE,
           committed_at_unix_nanoseconds TEXT NOT NULL,
           conversation_count INTEGER NOT NULL,
           participant_count INTEGER NOT NULL,
           message_count INTEGER NOT NULL,
           artifact_count INTEGER NOT NULL,
           FOREIGN KEY(account_id) REFERENCES replica_identity(account_id) ON DELETE CASCADE
         ) WITHOUT ROWID;
         CREATE TABLE sync_run(
           run_id TEXT PRIMARY KEY,
           account_id TEXT NOT NULL,
           mode TEXT NOT NULL,
           source_fingerprint TEXT NOT NULL,
           started_at_unix_nanoseconds TEXT NOT NULL,
           committed_at_unix_nanoseconds TEXT NOT NULL,
           changed_record_count INTEGER NOT NULL,
           FOREIGN KEY(account_id) REFERENCES replica_identity(account_id) ON DELETE CASCADE
         ) WITHOUT ROWID;
         CREATE TABLE change_log(
           sequence INTEGER PRIMARY KEY AUTOINCREMENT,
           account_id TEXT NOT NULL,
           source_fingerprint TEXT NOT NULL,
           change_kind TEXT NOT NULL,
           entity_kind TEXT NOT NULL,
           entity_id TEXT NOT NULL,
           conversation_id TEXT,
           record_sha256 TEXT,
           observed_at_unix_nanoseconds TEXT NOT NULL,
           FOREIGN KEY(account_id) REFERENCES replica_identity(account_id) ON DELETE CASCADE
         );
         CREATE INDEX message_by_conversation_time
           ON message(account_id, conversation_id, created_at_unix, conversation_ordinal);
         CREATE INDEX message_by_sender
           ON message(account_id, sender_id, created_at_unix);
         CREATE INDEX message_by_type
           ON message(account_id, logical_type, sub_type, created_at_unix);
         CREATE INDEX relationship_by_target
           ON message_relationship(account_id, target_canonical_id);
         CREATE INDEX change_by_account_sequence
           ON change_log(account_id, sequence);
         CREATE VIRTUAL TABLE message_fts USING fts5(
           account_id UNINDEXED,
           canonical_id UNINDEXED,
           conversation_id UNINDEXED,
           search_text,
           tokenize = 'unicode61'
         );
         UPDATE replica_schema SET schema_version = 2 WHERE singleton = 1;",
    )?;
    record_migration(transaction, 2, "checkpoints change stream and exact FTS")?;
    Ok(())
}

fn record_migration(
    transaction: &Transaction<'_>,
    version: u32,
    identity: &str,
) -> Result<(), RestoreError> {
    transaction.execute(
        "INSERT INTO migration_history VALUES (?1, ?2, ?3)",
        params![
            version,
            unix_nanoseconds()?.to_string(),
            hex::encode(Sha256::digest(identity.as_bytes()))
        ],
    )?;
    Ok(())
}

fn create_pre_migration_backup(
    source: &Connection,
    replica_path: &Path,
    key: &ReplicaKey,
    version: u32,
) -> Result<String, RestoreError> {
    let parent = replica_path
        .parent()
        .ok_or_else(|| RestoreError::UnsafePath("replica has no parent".to_string()))?;
    ensure_private_directory(parent)?;
    let base = replica_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("replica.db");
    let file_name = format!(
        ".{base}.pre-migration-v{version}-{}.db",
        unix_nanoseconds()?
    );
    let path = parent.join(&file_name);
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&path)?;
    let result = (|| {
        let mut destination = open_keyed_connection(&path, key)?;
        let backup = Backup::new(source, &mut destination)?;
        backup.run_to_completion(128, Duration::from_millis(2), None)?;
        drop(backup);
        destination.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        secure_replica_files(&path)?;
        Ok(())
    })();
    if result.is_err() {
        remove_failed_replica_files(&path);
    }
    result.map(|()| file_name)
}

fn import_archive_transactionally(
    connection: &mut Connection,
    archive_directory: &Path,
    report: &RestorationReport,
) -> Result<ImportCounts, RestoreError> {
    let conversations_path = archive_directory.join("conversations.ndjson");
    let participants_path = archive_directory.join("participants.ndjson");
    let messages_path = archive_directory.join("messages.ndjson");
    let artifacts_path = archive_directory.join("artifacts.ndjson");
    let coverage_path = archive_directory.join("coverage.json");
    for path in [
        &conversations_path,
        &participants_path,
        &messages_path,
        &artifacts_path,
        &coverage_path,
    ] {
        ensure_private_regular_file(path)?;
    }
    let coverage: RestorationCoverage = serde_json::from_slice(&fs::read(&coverage_path)?)?;
    let started = unix_nanoseconds()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute(
        "INSERT INTO replica_identity(
           singleton, account_id, current_source_fingerprint, restoration_complete,
           created_at_unix_nanoseconds, updated_at_unix_nanoseconds
         ) VALUES (1, ?1, NULL, NULL, ?2, ?2)",
        params![report.account_id, started.to_string()],
    )?;
    let mut counts = ImportCounts::default();

    for_each_ndjson::<CanonicalConversation>(&conversations_path, |conversation, bytes| {
        require_account(&conversation.account_id, &report.account_id)?;
        transaction.execute(
            "INSERT INTO conversation VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                report.account_id,
                conversation.conversation_id,
                json_enum(&conversation.kind)?,
                json_enum(&conversation.entity_decode_state)?,
                checked_usize_i64(conversation.participant_ids.len())?,
                sha256(&bytes),
                bytes,
            ],
        )?;
        counts.conversations += 1;
        Ok(())
    })?;
    for_each_ndjson::<CanonicalParticipant>(&participants_path, |participant, bytes| {
        require_account(&participant.account_id, &report.account_id)?;
        transaction.execute(
            "INSERT INTO participant VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                report.account_id,
                participant.participant_id,
                json_enum(&participant.local_profile_state)?,
                sha256(&bytes),
                bytes,
            ],
        )?;
        counts.participants += 1;
        Ok(())
    })?;
    for_each_ndjson::<CanonicalConversation>(&conversations_path, |conversation, _| {
        for membership in conversation.memberships {
            transaction.execute(
                "INSERT INTO conversation_participant VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    report.account_id,
                    conversation.conversation_id,
                    membership.participant_id,
                    json_enum(&membership.role)?,
                    membership.display_name_base64,
                ],
            )?;
        }
        Ok(())
    })?;
    for_each_ndjson::<CanonicalArtifact>(&artifacts_path, |artifact, bytes| {
        transaction.execute(
            "INSERT INTO artifact VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                report.account_id,
                artifact.artifact_id,
                json_enum(&artifact.kind)?,
                json_enum(&artifact.role)?,
                json_enum(&artifact.availability)?,
                artifact.source_sha256,
                artifact.decoded_sha256,
                sha256(&bytes),
                bytes,
            ],
        )?;
        counts.artifacts += 1;
        Ok(())
    })?;
    for_each_ndjson::<CanonicalMessage>(&messages_path, |message, bytes| {
        require_account(&message.account_id, &report.account_id)?;
        let search_text = message_search_text(&message);
        let record_sha = sha256(&bytes);
        transaction.execute(
            "INSERT INTO message VALUES (
               ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13
             )",
            params![
                report.account_id,
                message.canonical_id,
                message.conversation_id,
                message.sender_id,
                checked_i64(message.conversation_ordinal)?,
                message.created_at_unix,
                json_enum(&message.direction)?,
                message.logical_type,
                message.sub_type,
                json_enum(&message.semantic_decode_state)?,
                search_text,
                record_sha,
                bytes,
            ],
        )?;
        transaction.execute(
            "INSERT INTO message_fts(account_id, canonical_id, conversation_id, search_text)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                report.account_id,
                message.canonical_id,
                message.conversation_id,
                search_text,
            ],
        )?;
        for (ordinal, relationship) in message.relationships.into_iter().enumerate() {
            transaction.execute(
                "INSERT INTO message_relationship VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    report.account_id,
                    message.canonical_id,
                    checked_usize_i64(ordinal)?,
                    json_enum(&relationship.kind)?,
                    relationship.target_canonical_id,
                    relationship.resolved,
                    serde_json::to_vec(&relationship)?,
                ],
            )?;
            counts.relationships += 1;
        }
        for (ordinal, reference) in message.artifact_references.into_iter().enumerate() {
            transaction.execute(
                "INSERT INTO message_artifact VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    report.account_id,
                    message.canonical_id,
                    checked_usize_i64(ordinal)?,
                    reference.artifact_id,
                    json_enum(&reference.role)?,
                    reference.preferred,
                ],
            )?;
            counts.message_artifacts += 1;
        }
        counts.messages += 1;
        Ok(())
    })?;

    let committed = unix_nanoseconds()?;
    transaction.execute(
        "INSERT INTO coverage_state VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            report.account_id,
            report.source_fingerprint,
            serde_json::to_vec(&coverage)?,
            serde_json::to_vec(report)?,
            report.completion.full_restoration_achieved,
        ],
    )?;
    transaction.execute(
        "INSERT INTO source_checkpoint VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            report.account_id,
            report.source_fingerprint,
            committed.to_string(),
            checked_i64(counts.conversations)?,
            checked_i64(counts.participants)?,
            checked_i64(counts.messages)?,
            checked_i64(counts.artifacts)?,
        ],
    )?;
    let run_id = sha256(
        format!(
            "{}:{}:{started}",
            report.account_id, report.source_fingerprint
        )
        .as_bytes(),
    );
    transaction.execute(
        "INSERT INTO sync_run VALUES (?1, ?2, 'bootstrap', ?3, ?4, ?5, ?6)",
        params![
            run_id,
            report.account_id,
            report.source_fingerprint,
            started.to_string(),
            committed.to_string(),
            checked_i64(
                counts.conversations + counts.participants + counts.messages + counts.artifacts
            )?,
        ],
    )?;
    transaction.execute(
        "INSERT INTO change_log(
           account_id, source_fingerprint, change_kind, entity_kind, entity_id,
           conversation_id, record_sha256, observed_at_unix_nanoseconds
         ) VALUES (?1, ?2, 'bootstrap', 'checkpoint', ?2, NULL, NULL, ?3)",
        params![
            report.account_id,
            report.source_fingerprint,
            committed.to_string()
        ],
    )?;
    transaction.execute(
        "UPDATE replica_identity SET
           current_source_fingerprint = ?2,
           restoration_complete = ?3,
           updated_at_unix_nanoseconds = ?4
         WHERE account_id = ?1",
        params![
            report.account_id,
            report.source_fingerprint,
            report.completion.full_restoration_achieved,
            committed.to_string(),
        ],
    )?;
    transaction.commit()?;
    Ok(counts)
}

fn bootstrap_report(
    opened: &OpenedReplica,
    report: &RestorationReport,
    idempotent: bool,
) -> Result<ReplicaBootstrapReport, RestoreError> {
    Ok(ReplicaBootstrapReport {
        format_version: REPLICA_FORMAT_VERSION,
        schema_version: CURRENT_SCHEMA_VERSION,
        account_id: report.account_id.clone(),
        source_fingerprint: report.source_fingerprint.clone(),
        cipher_version: opened.cipher_version.clone(),
        encrypted_at_rest: true,
        idempotent,
        conversation_count: table_count(&opened.connection, "conversation")?,
        participant_count: table_count(&opened.connection, "participant")?,
        message_count: table_count(&opened.connection, "message")?,
        artifact_count: table_count(&opened.connection, "artifact")?,
        relationship_count: table_count(&opened.connection, "message_relationship")?,
        message_artifact_count: table_count(&opened.connection, "message_artifact")?,
        pre_migration_backup_file_name: opened.pre_migration_backup_file_name.clone(),
    })
}

fn for_each_ndjson<T: DeserializeOwned + Serialize>(
    path: &Path,
    mut body: impl FnMut(T, Vec<u8>) -> Result<(), RestoreError>,
) -> Result<(), RestoreError> {
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        let value: T = serde_json::from_str(&line)?;
        let canonical = serde_json::to_vec(&value)?;
        body(value, canonical)?;
    }
    Ok(())
}

fn message_search_text(message: &CanonicalMessage) -> String {
    let mut values = Vec::new();
    if let TypedPayload::Decoded(value) = &message.typed_payload {
        collect_search_strings(value, None, &mut values);
    }
    if let Some(content) = &message.content_base64 {
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(content) {
            if let Ok(value) = String::from_utf8(bytes) {
                if !values.iter().any(|existing| existing == &value) {
                    values.push(value);
                }
            }
        }
    }
    values.join("\n")
}

fn collect_search_strings(
    value: &serde_json::Value,
    field: Option<&str>,
    output: &mut Vec<String>,
) {
    if matches!(field, Some("raw_xml" | "raw")) {
        return;
    }
    match value {
        serde_json::Value::String(value) if !value.is_empty() => output.push(value.clone()),
        serde_json::Value::Array(values) => {
            for value in values {
                collect_search_strings(value, field, output);
            }
        }
        serde_json::Value::Object(values) => {
            for (name, value) in values {
                collect_search_strings(value, Some(name), output);
            }
        }
        _ => {}
    }
}

fn require_account(actual: &str, expected: &str) -> Result<(), RestoreError> {
    if actual != expected {
        return Err(RestoreError::Integrity(
            "archive record crossed the account isolation boundary".to_string(),
        ));
    }
    Ok(())
}

fn json_enum(value: &impl Serialize) -> Result<String, RestoreError> {
    let encoded = serde_json::to_string(value)?;
    Ok(encoded.trim_matches('"').to_string())
}

fn table_count(connection: &Connection, table: &str) -> Result<u64, RestoreError> {
    let sql = format!("SELECT count(*) FROM {table}");
    let count: i64 = connection.query_row(&sql, [], |row| row.get(0))?;
    Ok(count.max(0) as u64)
}

fn sha256(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}

fn checked_i64(value: u64) -> Result<i64, RestoreError> {
    i64::try_from(value)
        .map_err(|_| RestoreError::Integrity("replica count exceeds SQLite range".to_string()))
}

fn checked_usize_i64(value: usize) -> Result<i64, RestoreError> {
    i64::try_from(value)
        .map_err(|_| RestoreError::Integrity("replica count exceeds SQLite range".to_string()))
}

fn unix_nanoseconds() -> Result<u128, RestoreError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .map_err(|_| RestoreError::Integrity("system clock predates Unix epoch".to_string()))
}

fn checkpoint_and_secure(connection: &Connection, path: &Path) -> Result<(), RestoreError> {
    connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    secure_replica_files(path)
}

fn secure_replica_files(path: &Path) -> Result<(), RestoreError> {
    for candidate in replica_file_set(path) {
        if candidate.try_exists()? {
            let metadata = fs::symlink_metadata(&candidate)?;
            if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.nlink() != 1 {
                return Err(RestoreError::Integrity(
                    "replica storage contains an unsafe file identity".to_string(),
                ));
            }
            fs::set_permissions(candidate, fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}

fn remove_failed_replica_files(path: &Path) {
    for candidate in replica_file_set(path) {
        let _ = fs::remove_file(candidate);
    }
}

fn replica_file_set(path: &Path) -> [PathBuf; 3] {
    let bytes = path.as_os_str().as_encoded_bytes();
    let mut wal = bytes.to_vec();
    wal.extend_from_slice(b"-wal");
    let mut shm = bytes.to_vec();
    shm.extend_from_slice(b"-shm");
    [
        path.to_path_buf(),
        PathBuf::from(std::ffi::OsString::from_vec(wal)),
        PathBuf::from(std::ffi::OsString::from_vec(shm)),
    ]
}

trait OptionalRow<T> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error>;
}

impl<T> OptionalRow<T> for Result<T, rusqlite::Error> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error),
        }
    }
}
