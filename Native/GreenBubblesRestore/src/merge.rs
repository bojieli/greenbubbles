use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use serde::{de::DeserializeOwned, Serialize};
use sha2::{Digest, Sha256};

use crate::archive::{ensure_private_directory, ensure_private_regular_file, load_report};
use crate::schema::{
    schema_profile_fingerprint, validate_cached_coverage_schema,
    validate_restoration_coverage_schema,
};
use crate::{
    ArtifactAvailability, ArtifactDecodeState, CachedSurfaceCompleteness, CachedSurfaceCoverage,
    CanonicalArtifact, CanonicalCachedMoment, CanonicalCachedMomentInteraction,
    CanonicalConversation, CanonicalMessage, CanonicalParticipant, ConversationKind,
    ConversationMembership, ConversationMembershipRole, EntityDecodeState, EntitySourceRecord,
    LocalProfileState, MessageDirection, MessageOrderingBasis, RejectedRow,
    RelationshipResolutionState, RestorationArchiveScope, RestorationCompletion,
    RestorationCoverage, RestorationIntegrity, RestorationMediaPhase, RestorationReport,
    RestoreError, SemanticDecodeState, SnapshotAcquisitionMode, TableCoverageRole, TypedPayload,
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveMergeReport {
    pub format_version: u32,
    pub account_id: String,
    pub previous_source_fingerprint: String,
    pub current_source_fingerprint: String,
    pub replaced_source_set_count: u64,
    pub deleted_source_set_count: u64,
    pub conversation_count: u64,
    pub participant_count: u64,
    pub message_count: u64,
    pub artifact_count: u64,
    pub rejection_count: u64,
    pub full_restoration_achieved: bool,
}

pub fn merge_incremental_archive(
    previous_archive: &Path,
    fragment_archive: &Path,
    output_archive: &Path,
) -> Result<ArchiveMergeReport, RestoreError> {
    ensure_private_directory(previous_archive)?;
    ensure_private_directory(fragment_archive)?;
    let previous_report = load_report(previous_archive)?;
    let fragment_report = load_report(fragment_archive)?;
    validate_merge_inputs(&previous_report, &fragment_report)?;
    let acquisition = fragment_report.acquisition.as_ref().ok_or_else(|| {
        RestoreError::Integrity("incremental fragment has no acquisition evidence".to_string())
    })?;
    let selected = acquisition
        .selected_source_set_ids()
        .into_iter()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    let deleted = acquisition
        .deleted_source_set_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let affected = selected.union(&deleted).cloned().collect::<BTreeSet<_>>();

    let output_parent = output_archive
        .parent()
        .ok_or_else(|| RestoreError::UnsafePath(output_archive.display().to_string()))?;
    ensure_private_directory(output_parent)?;
    if fs::symlink_metadata(output_archive).is_ok() {
        return Err(RestoreError::Integrity(
            "merged archive output already exists".to_string(),
        ));
    }
    let temporary = tempfile::Builder::new()
        .prefix(".greenbubbles-merge-")
        .tempdir_in(output_parent)?;
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))?;

    let mut messages = merge_messages(previous_archive, fragment_archive, &selected, &affected)?;
    order_and_resolve_messages(&mut messages);
    let conversations = merge_conversations(
        previous_archive,
        fragment_archive,
        &messages,
        &selected,
        &affected,
    )?;
    let participants = merge_participants(
        previous_archive,
        fragment_archive,
        &messages,
        &conversations,
        &selected,
        &affected,
    )?;
    let mut artifacts = merge_artifacts(previous_archive, fragment_archive, &messages)?;
    relocate_connector_artifacts(
        &mut artifacts,
        previous_archive,
        fragment_archive,
        temporary.path(),
        output_archive,
    )?;
    let rejections = merge_rejections(previous_archive, fragment_archive, &selected, &affected)?;
    let coverage = merge_coverage(previous_archive, fragment_archive, &affected, &messages)?;
    let cached_surfaces =
        merge_cached_surfaces(previous_archive, fragment_archive, &selected, &affected)?;
    let integrity = calculate_integrity(
        acquisition.source_sets.len(),
        &messages,
        &artifacts,
        &conversations,
        &participants,
        &rejections,
        &coverage,
        cached_surfaces.as_ref(),
    );
    if !integrity.row_equation_holds() {
        return Err(RestoreError::Integrity(
            "merged archive row equation failed".to_string(),
        ));
    }
    let mut completion = RestorationCompletion::evaluate(&integrity);
    if !fragment_report
        .client_build_compatibility
        .production_compatible
    {
        completion.full_restoration_achieved = false;
    }
    let media_phase = if previous_report.media_phase == RestorationMediaPhase::Deferred
        || fragment_report.media_phase == RestorationMediaPhase::Deferred
    {
        completion.full_restoration_achieved = false;
        RestorationMediaPhase::Deferred
    } else {
        RestorationMediaPhase::Resolved
    };

    write_ndjson(&temporary.path().join("messages.ndjson"), &messages)?;
    write_ndjson(
        &temporary.path().join("conversations.ndjson"),
        &conversations,
    )?;
    write_ndjson(&temporary.path().join("participants.ndjson"), &participants)?;
    write_ndjson(&temporary.path().join("artifacts.ndjson"), &artifacts)?;
    write_ndjson(&temporary.path().join("rejections.ndjson"), &rejections)?;
    write_json(&temporary.path().join("coverage.json"), &coverage)?;
    if let Some((moments, interactions, cached_coverage)) = &cached_surfaces {
        write_ndjson(&temporary.path().join("cached-moments.ndjson"), moments)?;
        write_ndjson(
            &temporary.path().join("cached-moment-interactions.ndjson"),
            interactions,
        )?;
        write_json(
            &temporary.path().join("cached-surfaces.json"),
            cached_coverage,
        )?;
    }

    let cached_paths = cached_surfaces.as_ref().map(|_| {
        (
            output_archive
                .join("cached-moments.ndjson")
                .display()
                .to_string(),
            output_archive
                .join("cached-moment-interactions.ndjson")
                .display()
                .to_string(),
            output_archive
                .join("cached-surfaces.json")
                .display()
                .to_string(),
        )
    });

    let final_report = RestorationReport {
        format_version: 4,
        account_id: fragment_report.account_id.clone(),
        source_fingerprint: fragment_report.source_fingerprint.clone(),
        client_build_compatibility: fragment_report.client_build_compatibility.clone(),
        acquisition: fragment_report.acquisition.clone(),
        archive_scope: RestorationArchiveScope::Authoritative,
        media_phase,
        messages_path: output_archive.join("messages.ndjson").display().to_string(),
        rejections_path: output_archive
            .join("rejections.ndjson")
            .display()
            .to_string(),
        artifacts_path: output_archive
            .join("artifacts.ndjson")
            .display()
            .to_string(),
        conversations_path: output_archive
            .join("conversations.ndjson")
            .display()
            .to_string(),
        participants_path: output_archive
            .join("participants.ndjson")
            .display()
            .to_string(),
        cached_moments_path: cached_paths.as_ref().map(|paths| paths.0.clone()),
        cached_moment_interactions_path: cached_paths.as_ref().map(|paths| paths.1.clone()),
        cached_surfaces_path: cached_paths.as_ref().map(|paths| paths.2.clone()),
        coverage_path: output_archive.join("coverage.json").display().to_string(),
        report_path: output_archive.join("report.json").display().to_string(),
        integrity,
        completion,
    };
    write_json(&temporary.path().join("report.json"), &final_report)?;
    sync_directory(temporary.path())?;
    let persisted = temporary.keep();
    fs::rename(&persisted, output_archive)?;
    sync_directory(output_parent)?;

    Ok(ArchiveMergeReport {
        format_version: 1,
        account_id: final_report.account_id,
        previous_source_fingerprint: previous_report.source_fingerprint,
        current_source_fingerprint: final_report.source_fingerprint,
        replaced_source_set_count: selected.len() as u64,
        deleted_source_set_count: deleted.len() as u64,
        conversation_count: conversations.len() as u64,
        participant_count: participants.len() as u64,
        message_count: messages.len() as u64,
        artifact_count: artifacts.len() as u64,
        rejection_count: rejections.len() as u64,
        full_restoration_achieved: final_report.completion.full_restoration_achieved,
    })
}

type MergedCachedSurfaces = (
    Vec<CanonicalCachedMoment>,
    Vec<CanonicalCachedMomentInteraction>,
    CachedSurfaceCoverage,
);

fn merge_cached_surfaces(
    previous_archive: &Path,
    fragment_archive: &Path,
    selected: &BTreeSet<String>,
    affected: &BTreeSet<String>,
) -> Result<Option<MergedCachedSurfaces>, RestoreError> {
    let previous = load_cached_surfaces(previous_archive)?;
    let fragment = load_cached_surfaces(fragment_archive)?;
    if previous.is_none() && fragment.is_none() {
        return Ok(None);
    }

    let mut moments = BTreeMap::new();
    if let Some((previous_moments, _, _)) = previous.as_ref() {
        for moment in previous_moments
            .iter()
            .filter(|moment| !affected.contains(&moment.source_set_id))
        {
            insert_unique(
                &mut moments,
                moment.canonical_id.clone(),
                moment.clone(),
                "cached moment",
            )?;
        }
    }
    if let Some((fragment_moments, _, _)) = fragment.as_ref() {
        for moment in fragment_moments {
            if !selected.contains(&moment.source_set_id) {
                return Err(RestoreError::Integrity(
                    "incremental fragment contains a cached moment from an unselected source set"
                        .to_string(),
                ));
            }
            insert_unique(
                &mut moments,
                moment.canonical_id.clone(),
                moment.clone(),
                "cached moment",
            )?;
        }
    }

    let mut interactions = BTreeMap::new();
    if let Some((_, previous_interactions, _)) = previous.as_ref() {
        for interaction in previous_interactions
            .iter()
            .filter(|interaction| !affected.contains(&interaction.source_set_id))
        {
            insert_unique(
                &mut interactions,
                interaction.canonical_id.clone(),
                interaction.clone(),
                "cached moment interaction",
            )?;
        }
    }
    if let Some((_, fragment_interactions, _)) = fragment.as_ref() {
        for interaction in fragment_interactions {
            if !selected.contains(&interaction.source_set_id) {
                return Err(RestoreError::Integrity(
                    "incremental fragment contains a cached moment interaction from an unselected source set"
                        .to_string(),
                ));
            }
            insert_unique(
                &mut interactions,
                interaction.canonical_id.clone(),
                interaction.clone(),
                "cached moment interaction",
            )?;
        }
    }

    let mut tables = Vec::new();
    if let Some((_, _, previous_coverage)) = previous.as_ref() {
        tables.extend(
            previous_coverage
                .tables
                .iter()
                .filter(|table| !affected.contains(&table.source_set_id))
                .cloned(),
        );
    }
    if let Some((_, _, fragment_coverage)) = fragment.as_ref() {
        if fragment_coverage
            .tables
            .iter()
            .any(|table| !selected.contains(&table.source_set_id))
        {
            return Err(RestoreError::Integrity(
                "incremental fragment contains cached-surface coverage from an unselected source set"
                    .to_string(),
            ));
        }
        tables.extend(fragment_coverage.tables.iter().cloned());
    }
    tables.sort_by(|left, right| {
        (
            &left.source_logical_path,
            &left.source_table_name,
            &left.source_set_id,
        )
            .cmp(&(
                &right.source_logical_path,
                &right.source_table_name,
                &right.source_set_id,
            ))
    });
    let unique_tables = tables
        .iter()
        .map(|table| (&table.source_set_id, &table.source_table_id))
        .collect::<BTreeSet<_>>();
    if unique_tables.len() != tables.len() {
        return Err(RestoreError::Integrity(
            "merged cached-surface coverage contains duplicate table identities".to_string(),
        ));
    }

    let cached_was_reobserved = previous.as_ref().is_some_and(|(_, _, coverage)| {
        coverage
            .tables
            .iter()
            .any(|table| affected.contains(&table.source_set_id))
    }) || fragment.as_ref().is_some_and(|(_, _, coverage)| {
        coverage
            .tables
            .iter()
            .any(|table| selected.contains(&table.source_set_id))
    });
    let observed_at = if cached_was_reobserved {
        fragment
            .as_ref()
            .map(|(_, _, coverage)| coverage.observed_at.clone())
            .or_else(|| {
                previous
                    .as_ref()
                    .map(|(_, _, coverage)| coverage.observed_at.clone())
            })
    } else {
        previous
            .as_ref()
            .map(|(_, _, coverage)| coverage.observed_at.clone())
            .or_else(|| {
                fragment
                    .as_ref()
                    .map(|(_, _, coverage)| coverage.observed_at.clone())
            })
    }
    .ok_or_else(|| {
        RestoreError::Integrity("cached-surface observation time is absent".to_string())
    })?;
    let source_database_present = !tables.is_empty()
        || (!cached_was_reobserved
            && previous
                .as_ref()
                .is_some_and(|(_, _, coverage)| coverage.source_database_present))
        || (cached_was_reobserved
            && fragment
                .as_ref()
                .is_some_and(|(_, _, coverage)| coverage.source_database_present));
    let moments = moments.into_values().collect::<Vec<_>>();
    let interactions = interactions.into_values().collect::<Vec<_>>();
    let schema_profile_fingerprint = schema_profile_fingerprint(tables.iter().map(|table| {
        (
            table.source_logical_path.as_str(),
            table.source_table_name.as_str(),
            table.schema_fingerprint.as_deref(),
        )
    }));
    let coverage = CachedSurfaceCoverage {
        format_version: fragment
            .as_ref()
            .map(|(_, _, coverage)| coverage.format_version)
            .or_else(|| {
                previous
                    .as_ref()
                    .map(|(_, _, coverage)| coverage.format_version)
            })
            .unwrap_or(1),
        schema_profile_fingerprint,
        observed_at,
        cache_completeness: CachedSurfaceCompleteness::PartialLocalCache,
        source_database_present,
        moment_count: moments.len() as u64,
        interaction_count: interactions.len() as u64,
        semantic_gap_count: moments
            .iter()
            .filter(|moment| moment.semantic_decode_state != SemanticDecodeState::Complete)
            .count() as u64,
        tables,
    };
    Ok(Some((moments, interactions, coverage)))
}

fn load_cached_surfaces(archive: &Path) -> Result<Option<MergedCachedSurfaces>, RestoreError> {
    let moments = archive.join("cached-moments.ndjson");
    let interactions = archive.join("cached-moment-interactions.ndjson");
    let coverage = archive.join("cached-surfaces.json");
    let exists = [
        moments.try_exists()?,
        interactions.try_exists()?,
        coverage.try_exists()?,
    ];
    if exists.iter().all(|exists| !exists) {
        return Ok(None);
    }
    if !exists.iter().all(|exists| *exists) {
        return Err(RestoreError::Integrity(
            "cached-surface archive files are incomplete".to_string(),
        ));
    }
    let coverage = read_json(&coverage)?;
    validate_cached_coverage_schema(&coverage)?;
    Ok(Some((
        read_ndjson(&moments)?,
        read_ndjson(&interactions)?,
        coverage,
    )))
}

fn validate_merge_inputs(
    previous: &RestorationReport,
    fragment: &RestorationReport,
) -> Result<(), RestoreError> {
    if previous.archive_scope != RestorationArchiveScope::Authoritative
        || fragment.archive_scope != RestorationArchiveScope::IncrementalFragment
    {
        return Err(RestoreError::Integrity(
            "merge requires one authoritative archive and one incremental fragment".to_string(),
        ));
    }
    if previous.account_id != fragment.account_id {
        return Err(RestoreError::Integrity(
            "merge inputs belong to different accounts".to_string(),
        ));
    }
    let acquisition = fragment.acquisition.as_ref().ok_or_else(|| {
        RestoreError::Integrity("incremental fragment has no acquisition evidence".to_string())
    })?;
    if acquisition.mode != SnapshotAcquisitionMode::Incremental
        || acquisition.previous_source_fingerprint.as_deref()
            != Some(previous.source_fingerprint.as_str())
    {
        return Err(RestoreError::Integrity(
            "incremental fragment is not based on the supplied authoritative archive".to_string(),
        ));
    }
    Ok(())
}

fn merge_messages(
    previous_archive: &Path,
    fragment_archive: &Path,
    selected: &BTreeSet<String>,
    affected: &BTreeSet<String>,
) -> Result<Vec<CanonicalMessage>, RestoreError> {
    let mut merged = BTreeMap::new();
    for message in read_ndjson::<CanonicalMessage>(&previous_archive.join("messages.ndjson"))? {
        if !affected.contains(&message.source_set_id) {
            insert_unique(
                &mut merged,
                message.canonical_id.clone(),
                message,
                "message",
            )?;
        }
    }
    for message in read_ndjson::<CanonicalMessage>(&fragment_archive.join("messages.ndjson"))? {
        if !selected.contains(&message.source_set_id) {
            return Err(RestoreError::Integrity(
                "incremental fragment contains a message from an unselected source set".to_string(),
            ));
        }
        insert_unique(
            &mut merged,
            message.canonical_id.clone(),
            message,
            "message",
        )?;
    }
    Ok(merged.into_values().collect())
}

fn order_and_resolve_messages(messages: &mut Vec<CanonicalMessage>) {
    let mut groups = BTreeMap::<String, Vec<CanonicalMessage>>::new();
    for message in messages.drain(..) {
        groups
            .entry(message.conversation_id.clone())
            .or_default()
            .push(message);
    }
    for group in groups.values_mut() {
        let basis = conversation_ordering_basis(group);
        group.sort_by(|left, right| compare_messages(left, right, basis));
        for (ordinal, message) in group.iter_mut().enumerate() {
            message.conversation_ordinal = ordinal as u64;
            message.ordering_basis = basis;
        }
    }
    *messages = groups.into_values().flatten().collect();

    let mut by_server = HashMap::<(String, i64), Vec<String>>::new();
    let mut by_local = HashMap::<(String, i64), Vec<String>>::new();
    for message in messages.iter() {
        if let Some(identifier) = message.server_id {
            by_server
                .entry((message.conversation_id.clone(), identifier))
                .or_default()
                .push(message.canonical_id.clone());
        }
        if let Some(identifier) = message.local_id {
            by_local
                .entry((message.conversation_id.clone(), identifier))
                .or_default()
                .push(message.canonical_id.clone());
        }
    }
    for message in messages {
        for relationship in &mut message.relationships {
            let targets = relationship
                .target_server_id
                .and_then(|identifier| {
                    by_server.get(&(message.conversation_id.clone(), identifier))
                })
                .or_else(|| {
                    relationship.target_local_id.and_then(|identifier| {
                        by_local.get(&(message.conversation_id.clone(), identifier))
                    })
                });
            relationship.resolution_state = if relationship.target_server_id.is_none()
                && relationship.target_local_id.is_none()
            {
                RelationshipResolutionState::ReferenceIdentifierMissing
            } else {
                match targets.map(Vec::len).unwrap_or_default() {
                    0 => RelationshipResolutionState::TargetNotPresentLocally,
                    1 => RelationshipResolutionState::Resolved,
                    _ => RelationshipResolutionState::Ambiguous,
                }
            };
            relationship.resolved =
                relationship.resolution_state == RelationshipResolutionState::Resolved;
            relationship.target_canonical_id = targets
                .filter(|targets| targets.len() == 1)
                .map(|targets| targets[0].clone());
        }
    }
}

fn conversation_ordering_basis(messages: &[CanonicalMessage]) -> MessageOrderingBasis {
    if messages
        .iter()
        .all(|message| message.sort_sequence.is_some())
    {
        MessageOrderingBasis::SortSequence
    } else if messages.iter().all(|message| message.server_id.is_some()) {
        MessageOrderingBasis::ServerId
    } else if messages
        .iter()
        .all(|message| message.created_at_unix.is_some())
    {
        MessageOrderingBasis::CreatedAt
    } else if messages.iter().all(|message| message.local_id.is_some()) {
        MessageOrderingBasis::LocalId
    } else {
        MessageOrderingBasis::HybridSourceFallback
    }
}

fn compare_messages(
    left: &CanonicalMessage,
    right: &CanonicalMessage,
    basis: MessageOrderingBasis,
) -> Ordering {
    let primary = |message: &CanonicalMessage| match basis {
        MessageOrderingBasis::SortSequence => message.sort_sequence.unwrap_or_default(),
        MessageOrderingBasis::ServerId => message.server_id.unwrap_or_default(),
        MessageOrderingBasis::CreatedAt => message.created_at_unix.unwrap_or_default(),
        MessageOrderingBasis::LocalId => message.local_id.unwrap_or_default(),
        MessageOrderingBasis::HybridSourceFallback => message
            .sort_sequence
            .or(message.server_id)
            .or(message.created_at_unix)
            .or(message.local_id)
            .unwrap_or(message.source_row_id),
    };
    primary(left)
        .cmp(&primary(right))
        .then_with(|| left.sort_sequence.cmp(&right.sort_sequence))
        .then_with(|| left.server_id.cmp(&right.server_id))
        .then_with(|| left.created_at_unix.cmp(&right.created_at_unix))
        .then_with(|| left.local_id.cmp(&right.local_id))
        .then_with(|| left.source_logical_path.cmp(&right.source_logical_path))
        .then_with(|| left.source_table_id.cmp(&right.source_table_id))
        .then_with(|| left.source_row_id.cmp(&right.source_row_id))
        .then_with(|| left.canonical_id.cmp(&right.canonical_id))
}

fn merge_conversations(
    previous_archive: &Path,
    fragment_archive: &Path,
    messages: &[CanonicalMessage],
    selected: &BTreeSet<String>,
    affected: &BTreeSet<String>,
) -> Result<Vec<CanonicalConversation>, RestoreError> {
    let mut previous = keyed(
        read_ndjson(previous_archive.join("conversations.ndjson").as_path())?,
        |value: &CanonicalConversation| value.conversation_id.clone(),
        "conversation",
    )?;
    for value in previous.values_mut() {
        value
            .source_records
            .retain(|record| !affected.contains(&record.source_set_id));
    }
    let current = keyed(
        read_ndjson(fragment_archive.join("conversations.ndjson").as_path())?,
        |value: &CanonicalConversation| value.conversation_id.clone(),
        "conversation",
    )?;
    let message_participants = message_participants(messages);
    for (identifier, mut value) in current {
        validate_source_records(&value.source_records, selected, "conversation")?;
        if let Some(old) = previous.remove(&identifier) {
            value.source_records =
                merge_source_records(old.source_records, value.source_records, affected)?;
            value.memberships = merge_memberships(old.memberships, value.memberships);
            value.participant_ids.extend(old.participant_ids);
        }
        if let Some(observed) = message_participants.get(&identifier) {
            for participant in observed {
                value.participant_ids.push(participant.clone());
                value.memberships.push(ConversationMembership {
                    participant_id: participant.clone(),
                    role: ConversationMembershipRole::ObservedSender,
                    display_name_base64: None,
                });
            }
        }
        normalize_conversation(&mut value);
        previous.insert(identifier, value);
    }
    for value in previous.values_mut() {
        if let Some(observed) = message_participants.get(&value.conversation_id) {
            value.participant_ids.extend(observed.iter().cloned());
        }
        normalize_conversation(value);
    }
    let message_conversations = messages
        .iter()
        .map(|message| message.conversation_id.as_str())
        .collect::<BTreeSet<_>>();
    previous.retain(|identifier, conversation| {
        message_conversations.contains(identifier.as_str())
            || !conversation.source_records.is_empty()
    });
    Ok(previous.into_values().collect())
}

fn merge_participants(
    previous_archive: &Path,
    fragment_archive: &Path,
    messages: &[CanonicalMessage],
    conversations: &[CanonicalConversation],
    selected: &BTreeSet<String>,
    affected: &BTreeSet<String>,
) -> Result<Vec<CanonicalParticipant>, RestoreError> {
    let mut previous = keyed(
        read_ndjson(previous_archive.join("participants.ndjson").as_path())?,
        |value: &CanonicalParticipant| value.participant_id.clone(),
        "participant",
    )?;
    for value in previous.values_mut() {
        value
            .source_records
            .retain(|record| !affected.contains(&record.source_set_id));
    }
    let current = keyed(
        read_ndjson(fragment_archive.join("participants.ndjson").as_path())?,
        |value: &CanonicalParticipant| value.participant_id.clone(),
        "participant",
    )?;
    for (identifier, mut value) in current {
        validate_source_records(&value.source_records, selected, "participant")?;
        if let Some(old) = previous.remove(&identifier) {
            value.source_records =
                merge_source_records(old.source_records, value.source_records, affected)?;
        }
        previous.insert(identifier, value);
    }
    for value in previous.values_mut() {
        value.conversation_ids.clear();
    }
    let mut referenced = BTreeMap::<String, BTreeSet<String>>::new();
    for conversation in conversations {
        for identifier in &conversation.participant_ids {
            referenced
                .entry(identifier.clone())
                .or_default()
                .insert(conversation.conversation_id.clone());
        }
    }
    for message in messages {
        if let Some(identifier) = &message.sender_id {
            referenced
                .entry(identifier.clone())
                .or_default()
                .insert(message.conversation_id.clone());
        }
    }
    for (identifier, conversation_ids) in referenced {
        let Some(participant) = previous.get_mut(&identifier) else {
            return Err(RestoreError::Integrity(
                "merged conversation references an absent participant".to_string(),
            ));
        };
        participant.conversation_ids = conversation_ids.into_iter().collect();
    }
    previous.retain(|_, participant| {
        !participant.source_records.is_empty() || !participant.conversation_ids.is_empty()
    });
    Ok(previous.into_values().collect())
}

fn merge_artifacts(
    previous_archive: &Path,
    fragment_archive: &Path,
    messages: &[CanonicalMessage],
) -> Result<Vec<CanonicalArtifact>, RestoreError> {
    let mut artifacts = keyed(
        read_ndjson(previous_archive.join("artifacts.ndjson").as_path())?,
        |value: &CanonicalArtifact| value.artifact_id.clone(),
        "artifact",
    )?;
    for artifact in read_ndjson::<CanonicalArtifact>(&fragment_archive.join("artifacts.ndjson"))? {
        artifacts.insert(artifact.artifact_id.clone(), artifact);
    }
    let referenced = messages
        .iter()
        .flat_map(|message| &message.artifact_references)
        .map(|reference| reference.artifact_id.as_str())
        .collect::<BTreeSet<_>>();
    if let Some(missing) = referenced
        .iter()
        .find(|identifier| !artifacts.contains_key(**identifier))
    {
        return Err(RestoreError::Integrity(format!(
            "merged message references absent artifact {missing}"
        )));
    }
    artifacts.retain(|identifier, _| referenced.contains(identifier.as_str()));
    Ok(artifacts.into_values().collect())
}

fn relocate_connector_artifacts(
    artifacts: &mut [CanonicalArtifact],
    previous_archive: &Path,
    fragment_archive: &Path,
    temporary_archive: &Path,
    final_archive: &Path,
) -> Result<(), RestoreError> {
    let previous = fs::canonicalize(previous_archive)?;
    let fragment = fs::canonicalize(fragment_archive)?;
    for artifact in artifacts {
        if let Some(path) = artifact.materialized_local_path.clone() {
            artifact.materialized_local_path = relocate_one_artifact(
                &path,
                artifact.source_sha256.as_deref(),
                &artifact.artifact_id,
                "materialized",
                [&previous, &fragment],
                temporary_archive,
                final_archive,
            )?;
        }
        if let Some(path) = artifact.decoded_local_path.clone() {
            artifact.decoded_local_path = relocate_one_artifact(
                &path,
                artifact.decoded_sha256.as_deref(),
                &artifact.artifact_id,
                "decoded",
                [&previous, &fragment],
                temporary_archive,
                final_archive,
            )?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn relocate_one_artifact(
    path: &str,
    expected_sha256: Option<&str>,
    artifact_id: &str,
    variant: &str,
    archive_roots: [&Path; 2],
    temporary_archive: &Path,
    final_archive: &Path,
) -> Result<Option<String>, RestoreError> {
    let source = Path::new(path);
    let canonical = match fs::canonicalize(source) {
        Ok(value) => value,
        Err(error) => {
            if archive_roots.iter().any(|root| source.starts_with(root)) {
                return Err(error.into());
            }
            return Ok(Some(path.to_string()));
        }
    };
    let Some(root) = archive_roots
        .iter()
        .find(|root| canonical.starts_with(*root))
    else {
        return Ok(Some(path.to_string()));
    };
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(RestoreError::Integrity(
            "connector-owned artifact is not a private regular file".to_string(),
        ));
    }
    if !canonical.starts_with(root) {
        return Err(RestoreError::Integrity(
            "connector-owned artifact escapes its restoration archive".to_string(),
        ));
    }
    let expected = expected_sha256
        .filter(|value| valid_sha256(value))
        .ok_or_else(|| {
            RestoreError::Integrity(
                "connector-owned artifact has no verified SHA-256 identity".to_string(),
            )
        })?;
    let media = temporary_archive.join("media");
    if !media.exists() {
        fs::create_dir(&media)?;
        fs::set_permissions(&media, fs::Permissions::from_mode(0o700))?;
    }
    let identity = hex::encode(Sha256::digest(artifact_id.as_bytes()));
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 12
                && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
        .map(|value| format!(".{value}"))
        .unwrap_or_default();
    let file_name = format!("{identity}-{variant}{extension}");
    let temporary_destination = media.join(&file_name);
    let final_destination = final_archive.join("media").join(file_name);
    copy_verified_private_file(&canonical, &temporary_destination, expected)?;
    Ok(Some(final_destination.display().to_string()))
}

fn copy_verified_private_file(
    source: &Path,
    destination: &Path,
    expected_sha256: &str,
) -> Result<(), RestoreError> {
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(destination)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
    }
    output.sync_all()?;
    if !hex::encode(hasher.finalize()).eq_ignore_ascii_case(expected_sha256) {
        return Err(RestoreError::Integrity(
            "connector-owned artifact changed before incremental merge".to_string(),
        ));
    }
    Ok(())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn merge_rejections(
    previous_archive: &Path,
    fragment_archive: &Path,
    selected: &BTreeSet<String>,
    affected: &BTreeSet<String>,
) -> Result<Vec<RejectedRow>, RestoreError> {
    let mut result = read_ndjson::<RejectedRow>(&previous_archive.join("rejections.ndjson"))?
        .into_iter()
        .filter(|row| !affected.contains(&row.source_set_id))
        .collect::<Vec<_>>();
    let current = read_ndjson::<RejectedRow>(&fragment_archive.join("rejections.ndjson"))?;
    if current
        .iter()
        .any(|row| !selected.contains(&row.source_set_id))
    {
        return Err(RestoreError::Integrity(
            "incremental fragment contains a rejection from an unselected source set".to_string(),
        ));
    }
    result.extend(current);
    result.sort_by(|left, right| {
        (
            &left.source_set_id,
            &left.source_table_id,
            left.source_row_id,
            &left.reason,
        )
            .cmp(&(
                &right.source_set_id,
                &right.source_table_id,
                right.source_row_id,
                &right.reason,
            ))
    });
    Ok(result)
}

fn merge_coverage(
    previous_archive: &Path,
    fragment_archive: &Path,
    affected: &BTreeSet<String>,
    messages: &[CanonicalMessage],
) -> Result<RestorationCoverage, RestoreError> {
    let previous: RestorationCoverage = read_json(&previous_archive.join("coverage.json"))?;
    let fragment: RestorationCoverage = read_json(&fragment_archive.join("coverage.json"))?;
    validate_restoration_coverage_schema(&previous)?;
    validate_restoration_coverage_schema(&fragment)?;
    let mut message_tables = previous
        .message_tables
        .into_iter()
        .filter(|table| !affected.contains(&table.source_set_id))
        .collect::<Vec<_>>();
    message_tables.extend(fragment.message_tables);
    message_tables.sort_by(|left, right| {
        (
            &left.source_logical_path,
            &left.source_table_name,
            &left.source_set_id,
        )
            .cmp(&(
                &right.source_logical_path,
                &right.source_table_name,
                &right.source_set_id,
            ))
    });
    let mut all_tables = previous
        .all_tables
        .into_iter()
        .filter(|table| !affected.contains(&table.source_set_id))
        .collect::<Vec<_>>();
    all_tables.extend(fragment.all_tables);
    all_tables.sort_by(|left, right| {
        (
            &left.source_logical_path,
            &left.source_table_name,
            &left.source_set_id,
        )
            .cmp(&(
                &right.source_logical_path,
                &right.source_table_name,
                &right.source_set_id,
            ))
    });
    ensure_unique_table_coverage(&message_tables, &all_tables)?;
    let counts = message_type_counts(messages);
    let schema_profile_fingerprint = schema_profile_fingerprint(all_tables.iter().map(|table| {
        (
            table.source_logical_path.as_str(),
            table.source_table_name.as_str(),
            table.schema_fingerprint.as_deref(),
        )
    }));
    Ok(RestorationCoverage {
        format_version: fragment.format_version,
        decoder_name: fragment.decoder_name,
        decoder_version: fragment.decoder_version,
        snapshot_manifest_format_version: fragment.snapshot_manifest_format_version,
        schema_profile_fingerprint,
        message_tables,
        all_tables,
        logical_type_counts: counts.0,
        logical_sub_type_counts: counts.1,
        unknown_payload_reason_counts: count_unknown_reasons(messages),
        semantic_gap_reason_counts: count_semantic_gaps(messages),
    })
}

#[allow(clippy::too_many_arguments)]
fn calculate_integrity(
    database_count: usize,
    messages: &[CanonicalMessage],
    artifacts: &[CanonicalArtifact],
    conversations: &[CanonicalConversation],
    participants: &[CanonicalParticipant],
    rejections: &[RejectedRow],
    coverage: &RestorationCoverage,
    cached_surfaces: Option<&MergedCachedSurfaces>,
) -> RestorationIntegrity {
    let mut integrity = RestorationIntegrity {
        database_count: database_count as u64,
        message_table_count: coverage.message_tables.len() as u64,
        message_candidate_gap_count: coverage
            .all_tables
            .iter()
            .filter(|table| table.role == TableCoverageRole::UnhandledMessageCandidate)
            .count() as u64,
        source_row_count: coverage
            .message_tables
            .iter()
            .map(|table| table.source_row_count)
            .sum(),
        restored_row_count: messages.len() as u64,
        rejected_row_count: rejections.len() as u64,
        conversation_count: conversations.len() as u64,
        participant_count: participants.len() as u64,
        group_member_count: conversations
            .iter()
            .flat_map(|conversation| &conversation.memberships)
            .filter(|membership| membership.role == ConversationMembershipRole::Member)
            .count() as u64,
        entity_source_row_count: conversations
            .iter()
            .map(|value| value.source_records.len() as u64)
            .chain(
                participants
                    .iter()
                    .map(|value| value.source_records.len() as u64),
            )
            .sum(),
        entity_decode_gap_count: conversations
            .iter()
            .filter(|conversation| conversation.entity_decode_state != EntityDecodeState::Complete)
            .count() as u64,
        missing_local_profile_count: participants
            .iter()
            .filter(|participant| {
                participant.local_profile_state == LocalProfileState::MissingLocalRecord
            })
            .count() as u64,
        unresolved_conversation_count: conversations
            .iter()
            .filter(|conversation| conversation.kind == ConversationKind::Unresolved)
            .count() as u64,
        cached_moment_count: cached_surfaces
            .map(|(moments, _, _)| moments.len() as u64)
            .unwrap_or_default(),
        cached_moment_interaction_count: cached_surfaces
            .map(|(_, interactions, _)| interactions.len() as u64)
            .unwrap_or_default(),
        cached_surface_semantic_gap_count: cached_surfaces
            .map(|(_, _, coverage)| coverage.semantic_gap_count)
            .unwrap_or_default(),
        ..Default::default()
    };
    let counts = message_type_counts(messages);
    integrity.logical_type_counts = counts.0;
    integrity.logical_sub_type_counts = counts.1;
    integrity.unknown_payload_reason_counts = count_unknown_reasons(messages);
    integrity.unknown_payload_count = integrity.unknown_payload_reason_counts.values().sum();
    integrity.semantic_gap_reason_counts = count_semantic_gaps(messages);
    integrity.semantic_gap_count = messages
        .iter()
        .filter(|message| message.semantic_decode_state != SemanticDecodeState::Complete)
        .count() as u64;
    for table in &coverage.message_tables {
        let identity = table.schema_fingerprint.clone().unwrap_or_else(|| {
            let legacy_identity = table
                .columns
                .iter()
                .map(|column| column.to_ascii_lowercase())
                .collect::<Vec<_>>()
                .join("\u{1f}");
            hex::encode(Sha256::digest(legacy_identity.as_bytes()))
        });
        *integrity.message_schema_counts.entry(identity).or_default() += 1;
    }
    for message in messages {
        integrity.artifact_reference_count += message.artifact_references.len() as u64;
        integrity.relationship_reference_count += message.relationships.len() as u64;
        let order = match message.ordering_basis {
            MessageOrderingBasis::SortSequence => "sortSequence",
            MessageOrderingBasis::ServerId => "serverId",
            MessageOrderingBasis::CreatedAt => "createdAt",
            MessageOrderingBasis::LocalId => "localId",
            MessageOrderingBasis::HybridSourceFallback => "hybridSourceFallback",
        };
        *integrity
            .ordering_basis_counts
            .entry(order.to_string())
            .or_default() += 1;
        let direction = match message.direction {
            MessageDirection::Incoming => "incoming",
            MessageDirection::Outgoing => "outgoing",
            MessageDirection::Unknown => "unknown",
        };
        *integrity
            .direction_counts
            .entry(direction.to_string())
            .or_default() += 1;
        for relationship in &message.relationships {
            match relationship.resolution_state {
                RelationshipResolutionState::Resolved => integrity.resolved_relationship_count += 1,
                RelationshipResolutionState::TargetNotPresentLocally => {
                    integrity.unresolved_relationship_count += 1;
                    integrity.absent_relationship_target_count += 1;
                }
                RelationshipResolutionState::ReferenceIdentifierMissing => {
                    integrity.unresolved_relationship_count += 1;
                    integrity.missing_relationship_identifier_count += 1;
                }
                RelationshipResolutionState::Ambiguous => {
                    integrity.unresolved_relationship_count += 1;
                    integrity.ambiguous_relationship_count += 1;
                }
                RelationshipResolutionState::Pending => {
                    integrity.unresolved_relationship_count += 1
                }
            }
        }
    }
    integrity.unique_artifact_count = artifacts.len() as u64;
    for artifact in artifacts {
        match artifact.availability {
            ArtifactAvailability::Downloaded => integrity.downloaded_artifact_count += 1,
            ArtifactAvailability::MaterializedFromDatabase => {
                integrity.materialized_artifact_count += 1
            }
            ArtifactAvailability::NotDownloaded
            | ArtifactAvailability::RemoteOnly
            | ArtifactAvailability::Expired
            | ArtifactAvailability::Deleted
            | ArtifactAvailability::MetadataMissing => integrity.missing_artifact_count += 1,
            ArtifactAvailability::AccountRootUnavailable => {
                integrity.missing_artifact_count += 1;
                integrity.account_root_unavailable_artifact_count += 1;
            }
            ArtifactAvailability::Ambiguous => integrity.ambiguous_artifact_count += 1,
            ArtifactAvailability::Corrupt => integrity.corrupt_artifact_count += 1,
            ArtifactAvailability::UnsafePath => integrity.unsafe_artifact_count += 1,
        }
        if artifact.decode_state == ArtifactDecodeState::Decoded {
            integrity.decoded_artifact_count += 1;
        }
        if matches!(
            artifact.availability,
            ArtifactAvailability::Downloaded
                | ArtifactAvailability::MaterializedFromDatabase
                | ArtifactAvailability::Ambiguous
        ) && matches!(
            artifact.decode_state,
            ArtifactDecodeState::KeyUnavailable
                | ArtifactDecodeState::Unsupported
                | ArtifactDecodeState::Failed
        ) {
            integrity.artifact_decode_gap_count += 1;
        }
    }
    integrity
}

fn message_type_counts(
    messages: &[CanonicalMessage],
) -> (BTreeMap<String, u64>, BTreeMap<String, u64>) {
    let mut logical = BTreeMap::new();
    let mut sub = BTreeMap::new();
    for message in messages {
        let logical_key = message
            .logical_type
            .map(|value| value.to_string())
            .unwrap_or_else(|| "missing".to_string());
        *logical.entry(logical_key).or_default() += 1;
        let sub_key = match (message.logical_type, message.sub_type) {
            (Some(logical), Some(sub)) => format!("{logical}:{sub}"),
            _ => "missing".to_string(),
        };
        *sub.entry(sub_key).or_default() += 1;
    }
    (logical, sub)
}

fn count_unknown_reasons(messages: &[CanonicalMessage]) -> BTreeMap<String, u64> {
    let mut result = BTreeMap::new();
    for message in messages {
        if let TypedPayload::Unknown { reason } = &message.typed_payload {
            *result.entry(reason.clone()).or_default() += 1;
        }
    }
    result
}

fn count_semantic_gaps(messages: &[CanonicalMessage]) -> BTreeMap<String, u64> {
    let mut result = BTreeMap::new();
    for message in messages {
        if message.semantic_decode_state != SemanticDecodeState::Complete {
            *result
                .entry(
                    message
                        .semantic_gap_reason
                        .clone()
                        .unwrap_or_else(|| "unspecified semantic coverage gap".to_string()),
                )
                .or_default() += 1;
        }
    }
    result
}

fn merge_source_records(
    previous: Vec<EntitySourceRecord>,
    current: Vec<EntitySourceRecord>,
    affected: &BTreeSet<String>,
) -> Result<Vec<EntitySourceRecord>, RestoreError> {
    let mut records = BTreeMap::new();
    for record in previous
        .into_iter()
        .filter(|record| !affected.contains(&record.source_set_id))
        .chain(current)
    {
        let key = (
            record.source_set_id.clone(),
            record.source_table_id.clone(),
            record.source_row_id,
        );
        insert_unique(&mut records, key, record, "entity source record")?;
    }
    Ok(records.into_values().collect())
}

fn validate_source_records(
    records: &[EntitySourceRecord],
    selected: &BTreeSet<String>,
    kind: &str,
) -> Result<(), RestoreError> {
    if records
        .iter()
        .any(|record| !selected.contains(&record.source_set_id))
    {
        return Err(RestoreError::Integrity(format!(
            "incremental fragment contains a {kind} source record from an unselected set"
        )));
    }
    Ok(())
}

fn merge_memberships(
    previous: Vec<ConversationMembership>,
    current: Vec<ConversationMembership>,
) -> Vec<ConversationMembership> {
    let mut merged = BTreeMap::new();
    for membership in previous.into_iter().chain(current) {
        merged.insert(
            (membership.participant_id.clone(), membership.role as u8),
            membership,
        );
    }
    merged.into_values().collect()
}

fn normalize_conversation(conversation: &mut CanonicalConversation) {
    conversation.participant_ids.extend(
        conversation
            .memberships
            .iter()
            .map(|membership| membership.participant_id.clone()),
    );
    conversation.participant_ids.sort();
    conversation.participant_ids.dedup();
    conversation.memberships.sort_by(|left, right| {
        (&left.participant_id, left.role as u8).cmp(&(&right.participant_id, right.role as u8))
    });
    conversation.memberships.dedup_by(|left, right| {
        left.participant_id == right.participant_id && left.role == right.role
    });
    conversation.source_records.sort_by(source_record_order);
}

fn message_participants(messages: &[CanonicalMessage]) -> BTreeMap<String, BTreeSet<String>> {
    let mut result = BTreeMap::new();
    for message in messages {
        if let Some(sender) = &message.sender_id {
            result
                .entry(message.conversation_id.clone())
                .or_insert_with(BTreeSet::new)
                .insert(sender.clone());
        }
    }
    result
}

fn source_record_order(left: &EntitySourceRecord, right: &EntitySourceRecord) -> Ordering {
    (
        &left.source_logical_path,
        &left.source_table_name,
        &left.source_set_id,
        left.source_row_id,
    )
        .cmp(&(
            &right.source_logical_path,
            &right.source_table_name,
            &right.source_set_id,
            right.source_row_id,
        ))
}

fn ensure_unique_table_coverage(
    message_tables: &[crate::MessageTableCoverage],
    all_tables: &[crate::TableSchemaCoverage],
) -> Result<(), RestoreError> {
    let message_keys = message_tables
        .iter()
        .map(|table| (&table.source_set_id, &table.source_table_id))
        .collect::<BTreeSet<_>>();
    let all_keys = all_tables
        .iter()
        .map(|table| (&table.source_set_id, &table.source_table_id))
        .collect::<BTreeSet<_>>();
    if message_keys.len() != message_tables.len() || all_keys.len() != all_tables.len() {
        return Err(RestoreError::Integrity(
            "merged schema coverage contains duplicate table identities".to_string(),
        ));
    }
    Ok(())
}

fn keyed<T>(
    values: Vec<T>,
    key: impl Fn(&T) -> String,
    kind: &str,
) -> Result<BTreeMap<String, T>, RestoreError> {
    let mut result = BTreeMap::new();
    for value in values {
        insert_unique(&mut result, key(&value), value, kind)?;
    }
    Ok(result)
}

fn insert_unique<K: Ord, V>(
    values: &mut BTreeMap<K, V>,
    key: K,
    value: V,
    kind: &str,
) -> Result<(), RestoreError> {
    if values.insert(key, value).is_some() {
        return Err(RestoreError::Integrity(format!(
            "merged archive contains duplicate {kind} identity"
        )));
    }
    Ok(())
}

fn read_ndjson<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>, RestoreError> {
    ensure_private_regular_file(path)?;
    let reader = BufReader::new(File::open(path)?);
    let mut result = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if !line.is_empty() {
            result.push(serde_json::from_str(&line)?);
        }
    }
    Ok(result)
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, RestoreError> {
    ensure_private_regular_file(path)?;
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn write_ndjson<T: Serialize>(path: &Path, values: &[T]) -> Result<(), RestoreError> {
    let mut writer = owner_only_writer(path)?;
    for value in values {
        serde_json::to_writer(&mut writer, value)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    writer.get_ref().sync_all()?;
    Ok(())
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), RestoreError> {
    let mut writer = owner_only_writer(path)?;
    serde_json::to_writer_pretty(&mut writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    Ok(())
}

fn owner_only_writer(path: &Path) -> Result<BufWriter<File>, RestoreError> {
    Ok(BufWriter::new(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)?,
    ))
}

fn sync_directory(path: &Path) -> Result<(), RestoreError> {
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)?;
    directory.sync_all()?;
    Ok(())
}
