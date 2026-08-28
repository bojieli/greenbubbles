#![recursion_limit = "256"]

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};
use std::time::{Duration, Instant};

use greenbubbles_restore::{
    acquisition_audit::audit_acquisition_chain,
    ai_context::{
        audit_ai_context_with_progress, export_ai_context, load_ai_query_request, query_ai_context,
    },
    ai_memory::{
        audit_ai_memory_with_progress, export_ai_memory_with_progress, AiMemoryExportOptions,
    },
    archive::{create_conversation_policy, read_conversation_page},
    audit::audit_archive_with_progress,
    benchmark::{run_synthetic_benchmark, SyntheticBenchmarkConfig},
    connector::{audit_connector_log, ConnectorDestination, ConnectorService},
    diagnostic::{profile_archive_payloads_with_progress, profile_archive_schema_with_progress},
    follow::{
        follow_replica_once, publish_replica_handoff, quarantine_retired_replica_archives,
        replica_follower_status, restore_quarantined_replica_archive,
    },
    latency::{compose_latency_evidence_sample, summarize_latency_evidence_samples},
    merge::merge_incremental_archive,
    operator::{restore_snapshot_and_publish_with_progress, OfflineRestorePublishOptions},
    preflight_snapshot_with_progress, prepare_available_catalog_with_progress,
    prepare_catalog_batch_with_progress, prepare_catalog_with_progress,
    reconcile::reconcile_archives,
    replica::{
        audit_replica_backup_with_progress, audit_replica_with_progress,
        bootstrap_replica_with_progress, get_replica_changes, get_replica_message,
        list_replica_conversations, load_replica_message_filter, prepare_replica_recovery,
        replica_coverage, replica_status, search_replica_cached_moments, search_replica_messages,
        synchronize_replica_with_progress, ReplicaCachedMomentFilter,
    },
    restore_catalog_with_progress,
    tools::{
        create_all_conversations_tool_policy_with_cached_moments,
        create_tool_policy_with_cached_moments, CachedMomentField, CachedMomentsToolScope,
        ConversationToolScope, LocalToolService, ToolCapability, ToolDataDestination,
        ToolMessageField,
    },
    transport::{load_connector_request, send_unix_request, serve_unix},
    DatabaseKeySet, DatabasePassphrase, DatabaseUnlockMaterial, ProgressEvent, ProgressObserver,
    ProgressPhase, ProgressState, ProgressUnit, ReplicaKey, RestorationOptions,
};
use zeroize::Zeroizing;

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args().skip(1).peekable();
    let command = arguments.next().unwrap_or_else(|| "help".to_string());
    if matches!(arguments.peek().map(String::as_str), Some("--help" | "-h")) {
        if let Some(help) = ai_command_help(&command) {
            println!("{help}");
            return Ok(());
        }
    }
    if command == "help" {
        if let Some(help) = arguments.next().as_deref().and_then(ai_command_help) {
            println!("{help}");
            return Ok(());
        }
    }
    match command.as_str() {
        "synthetic-benchmark" => {
            let work_directory = required_path(arguments.next(), "private work directory")?;
            let remaining = arguments.collect::<Vec<_>>();
            let defaults = SyntheticBenchmarkConfig::default();
            let config = SyntheticBenchmarkConfig {
                samples: option_usize(&remaining, "--samples")?.unwrap_or(defaults.samples),
                small_message_count: option_usize(&remaining, "--small-messages")?
                    .unwrap_or(defaults.small_message_count),
                large_message_count: option_usize(&remaining, "--large-messages")?
                    .unwrap_or(defaults.large_message_count),
                burst_message_count: option_usize(&remaining, "--burst-messages")?
                    .unwrap_or(defaults.burst_message_count),
            };
            let report = run_synthetic_benchmark(&work_directory, &config)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "probe" => {
            let snapshot = required_path(arguments.next(), "snapshot directory")?;
            let remaining = arguments.collect::<Vec<_>>();
            let unlock = load_database_unlock(&remaining)?;
            let reporter = ProgressReporter::from_arguments(
                &remaining,
                ProgressWorkflow::Probe,
                unlock.validates_exported_keys(),
            )?;
            let catalog = prepare_catalog_with_progress(&snapshot, unlock.material(), &reporter)?;
            let report = serde_json::json!({
                "snapshotId": catalog.manifest.snapshot_id,
                "clientBuildCompatibility": catalog.manifest.client_build_compatibility(),
                "databaseCount": catalog.databases.len(),
                "storageFamilies": catalog.storage_family_counts(),
                "databases": catalog.databases,
            });
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "preflight" => {
            let snapshot = required_path(arguments.next(), "snapshot directory")?;
            let remaining = arguments.collect::<Vec<_>>();
            let reporter =
                ProgressReporter::from_arguments(&remaining, ProgressWorkflow::Preflight, false)?;
            let report = preflight_snapshot_with_progress(&snapshot, &reporter)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "restore" => {
            let snapshot = required_path(arguments.next(), "snapshot directory")?;
            let output = required_path(arguments.next(), "output directory")?;
            let remaining = arguments.collect::<Vec<_>>();
            let account_root = option_path(&remaining, "--account-root")?;
            let defer_media = remaining.iter().any(|value| value == "--defer-media");
            let unlock = load_database_unlock(&remaining)?;
            let reporter = ProgressReporter::from_arguments(
                &remaining,
                ProgressWorkflow::Restore,
                unlock.validates_exported_keys(),
            )?;
            let catalog = prepare_catalog_with_progress(&snapshot, unlock.material(), &reporter)?;
            let report = restore_catalog_with_progress(
                &catalog,
                &RestorationOptions {
                    output_directory: output,
                    account_root,
                    defer_media,
                },
                &reporter,
            )?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "diagnose-batch" => {
            let snapshot = required_path(arguments.next(), "snapshot directory")?;
            let output = required_path(arguments.next(), "diagnostic output directory")?;
            let remaining = arguments.collect::<Vec<_>>();
            let offset = option_usize(&remaining, "--database-offset")?.unwrap_or(0);
            let limit = option_usize(&remaining, "--database-limit")?.unwrap_or(1);
            let unlock = load_database_unlock(&remaining)?;
            let reporter = ProgressReporter::from_arguments(
                &remaining,
                ProgressWorkflow::RestoreAndAudit,
                unlock.validates_exported_keys(),
            )?;
            let catalog = prepare_catalog_batch_with_progress(
                &snapshot,
                unlock.material(),
                offset,
                limit,
                &reporter,
            )?;
            let batch = catalog
                .diagnostic_batch
                .ok_or("diagnostic catalog lost its batch boundary")?;
            let report = restore_catalog_with_progress(
                &catalog,
                &RestorationOptions {
                    output_directory: output.clone(),
                    account_root: option_path(&remaining, "--account-root")?,
                    defer_media: !remaining.iter().any(|value| value == "--resolve-media"),
                },
                &reporter,
            )?;
            let audit_progress = PhaseRangeProgress::new(&reporter, 0, 800_000);
            let audit = audit_archive_with_progress(&output, &audit_progress)?;
            let profile_progress = PhaseRangeProgress::new(&reporter, 800_000, 1_000_000);
            let payload_profiles =
                profile_archive_payloads_with_progress(&output, &profile_progress)?;
            let summary = serde_json::json!({
                "formatVersion": 4,
                "privacySafeSummary": true,
                "archiveScope": report.archive_scope,
                "databaseOffset": batch.offset,
                "databaseLimit": batch.limit,
                "totalDatabaseCount": batch.total_database_count,
                "selectedDatabaseCount": catalog.databases.len(),
                "selectedDatabaseBytes": catalog.databases.iter().map(|database| database.database_byte_count).sum::<u64>(),
                "selectedWriteAheadLogBytes": catalog.databases.iter().map(|database| database.write_ahead_log_byte_count).sum::<u64>(),
                "sourceRowCount": report.integrity.source_row_count,
                "messageSourceRowCount": report.integrity.source_row_count,
                "observedTableRowCount": report.integrity.observed_table_row_count,
                "restoredRowCount": report.integrity.restored_row_count,
                "totalRestoredRecordCount": report.integrity.restored_row_count
                    .saturating_add(report.integrity.cached_moment_count)
                    .saturating_add(report.integrity.cached_moment_interaction_count),
                "cachedMomentCount": report.integrity.cached_moment_count,
                "cachedMomentInteractionCount": report.integrity.cached_moment_interaction_count,
                "cachedSurfaceSemanticGapCount": report.integrity.cached_surface_semantic_gap_count,
                "cachedSurfaceOmittedRowCount": report.integrity.cached_surface_omitted_row_count,
                "rejectedRowCount": report.integrity.rejected_row_count,
                "messageTableCount": report.integrity.message_table_count,
                "messageCandidateGapCount": report.integrity.message_candidate_gap_count,
                "tableRoleCounts": report.integrity.table_role_counts,
                "tableClassificationReasonCounts": report.integrity.table_classification_reason_counts,
                "semanticGapCount": report.integrity.semantic_gap_count,
                "unknownPayloadCount": report.integrity.unknown_payload_count,
                "logicalTypeCounts": report.integrity.logical_type_counts,
                "logicalSubTypeCounts": report.integrity.logical_sub_type_counts,
                "payloadProfiles": payload_profiles,
                "semanticGapReasonCounts": report.integrity.semantic_gap_reason_counts,
                "conversationCount": report.integrity.conversation_count,
                "participantCount": report.integrity.participant_count,
                "accountHolderBound": report.self_participant_id.is_some(),
                "directionCounts": report.integrity.direction_counts,
                "directionConflictCount": report.integrity.direction_conflict_count,
                "rowEquationHolds": report.completion.row_equation_holds,
                "zeroRejectedRows": report.completion.zero_rejected_rows,
                "semanticMessageCoverageComplete": report.completion.semantic_message_coverage_complete,
                "auditReportMatchesArchive": audit.report_matches_archive,
                "auditMessageCount": audit.message_count,
                "auditCachedMomentCount": audit.cached_moment_count,
                "auditCachedMomentInteractionCount": audit.cached_moment_interaction_count,
                "auditRestoredRecordCount": audit.restored_record_count(),
                "auditRejectionCount": audit.rejection_count,
                "auditAccountHolderBound": audit.account_holder_bound,
                "auditDirectionConflictCount": audit.direction_conflict_count,
                "auditDirectionResolutionComplete": audit.completion_evidence.direction_resolution_complete,
                "clientBuildProductionCompatible": audit.client_build_production_compatible
            });
            emit_json_result(&summary, &remaining)?;
        }
        "diagnose-available" => {
            let snapshot = required_path(arguments.next(), "snapshot directory")?;
            let output = required_path(arguments.next(), "diagnostic output directory")?;
            let remaining = arguments.collect::<Vec<_>>();
            let unlock = load_database_unlock(&remaining)?;
            let keys = unlock.exported_keys().ok_or(
                "diagnose-available requires --database-keys-file and does not acquire keys",
            )?;
            let reporter = ProgressReporter::from_arguments(
                &remaining,
                ProgressWorkflow::RestoreAndAudit,
                true,
            )?;
            let catalog = prepare_available_catalog_with_progress(&snapshot, keys, &reporter)?;
            let selection = catalog
                .available_database_selection
                .as_ref()
                .ok_or("available catalog lost its explicit selection evidence")?;
            let report = restore_catalog_with_progress(
                &catalog,
                &RestorationOptions {
                    output_directory: output.clone(),
                    account_root: option_path(&remaining, "--account-root")?,
                    defer_media: !remaining.iter().any(|value| value == "--resolve-media"),
                },
                &reporter,
            )?;
            let audit_progress = PhaseRangeProgress::new(&reporter, 0, 800_000);
            let audit = audit_archive_with_progress(&output, &audit_progress)?;
            let profile_progress = PhaseRangeProgress::new(&reporter, 800_000, 1_000_000);
            let payload_profiles =
                profile_archive_payloads_with_progress(&output, &profile_progress)?;
            let summary = serde_json::json!({
                "formatVersion": 2,
                "privacySafeSummary": true,
                "archiveScope": report.archive_scope,
                "authoritativeDatabaseCoverage": false,
                "availableDatabaseSelection": selection,
                "selectedDatabaseCount": catalog.databases.len(),
                "selectedDatabaseBytes": selection.selected_database_byte_count,
                "selectedWriteAheadLogBytes": selection.selected_write_ahead_log_byte_count,
                "sourceRowCount": report.integrity.source_row_count,
                "messageSourceRowCount": report.integrity.source_row_count,
                "observedTableRowCount": report.integrity.observed_table_row_count,
                "restoredRowCount": report.integrity.restored_row_count,
                "totalRestoredRecordCount": report.integrity.restored_row_count
                    .saturating_add(report.integrity.cached_moment_count)
                    .saturating_add(report.integrity.cached_moment_interaction_count),
                "cachedMomentCount": report.integrity.cached_moment_count,
                "cachedMomentInteractionCount": report.integrity.cached_moment_interaction_count,
                "cachedSurfaceSemanticGapCount": report.integrity.cached_surface_semantic_gap_count,
                "cachedSurfaceOmittedRowCount": report.integrity.cached_surface_omitted_row_count,
                "rejectedRowCount": report.integrity.rejected_row_count,
                "messageTableCount": report.integrity.message_table_count,
                "messageCandidateGapCount": report.integrity.message_candidate_gap_count,
                "tableRoleCounts": report.integrity.table_role_counts,
                "tableClassificationReasonCounts": report.integrity.table_classification_reason_counts,
                "semanticGapCount": report.integrity.semantic_gap_count,
                "unknownPayloadCount": report.integrity.unknown_payload_count,
                "logicalTypeCounts": report.integrity.logical_type_counts,
                "logicalSubTypeCounts": report.integrity.logical_sub_type_counts,
                "payloadProfiles": payload_profiles,
                "semanticGapReasonCounts": report.integrity.semantic_gap_reason_counts,
                "conversationCount": report.integrity.conversation_count,
                "participantCount": report.integrity.participant_count,
                "accountHolderBound": report.self_participant_id.is_some(),
                "directionCounts": report.integrity.direction_counts,
                "directionConflictCount": report.integrity.direction_conflict_count,
                "rowEquationHolds": report.completion.row_equation_holds,
                "zeroRejectedRows": report.completion.zero_rejected_rows,
                "semanticMessageCoverageComplete": report.completion.semantic_message_coverage_complete,
                "auditReportMatchesArchive": audit.report_matches_archive,
                "auditMessageCount": audit.message_count,
                "auditCachedMomentCount": audit.cached_moment_count,
                "auditCachedMomentInteractionCount": audit.cached_moment_interaction_count,
                "auditRestoredRecordCount": audit.restored_record_count(),
                "auditRejectionCount": audit.rejection_count,
                "auditAccountHolderBound": audit.account_holder_bound,
                "auditDirectionConflictCount": audit.direction_conflict_count,
                "auditDirectionResolutionComplete": audit.completion_evidence.direction_resolution_complete,
                "clientBuildProductionCompatible": audit.client_build_production_compatible
            });
            emit_json_result(&summary, &remaining)?;
        }
        "diagnose-archive-payloads" => {
            let archive = required_path(arguments.next(), "archive directory")?;
            let report_path = required_path(arguments.next(), "private diagnostic report")?;
            let remaining = arguments.collect::<Vec<_>>();
            let reporter =
                ProgressReporter::from_arguments(&remaining, ProgressWorkflow::Audit, false)?;
            let report = profile_archive_payloads_with_progress(&archive, &reporter)?;
            write_owner_only_json(&report_path, &report)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "formatVersion": 1,
                    "privacySafe": true,
                    "reportPath": report_path,
                    "messageCount": report.message_count,
                    "relationshipReferenceCount": report.relationship_reference_count,
                    "relationshipIdentifierPresentCount": report.relationship_identifier_present_count,
                    "relationshipIdentifierRecoverableFromDecodedXmlCount": report.relationship_identifier_recoverable_from_decoded_xml_count,
                    "relationshipIdentifierMissingFromDecodedXmlCount": report.relationship_identifier_missing_from_decoded_xml_count,
                    "relationshipDecodedXmlUnavailableCount": report.relationship_decoded_xml_unavailable_count,
                    "adapterTypeProfileCount": report.adapter_type_profiles.len()
                }))?
            );
        }
        "diagnose-archive-schema" => {
            let archive = required_path(arguments.next(), "archive directory")?;
            let report_path = required_path(arguments.next(), "private diagnostic report")?;
            let remaining = arguments.collect::<Vec<_>>();
            let reporter =
                ProgressReporter::from_arguments(&remaining, ProgressWorkflow::Audit, false)?;
            let report = profile_archive_schema_with_progress(&archive, &reporter)?;
            write_owner_only_json(&report_path, &report)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "formatVersion": 1,
                    "privacySafe": true,
                    "reportPath": report_path,
                    "tableCount": report.table_count,
                    "sourceRowCount": report.source_row_count,
                    "otherTableCount": report.other_table_count,
                    "otherSourceRowCount": report.other_source_row_count,
                    "otherFamilyCount": report.other_families.len()
                }))?
            );
        }
        "restore-publish" => {
            let snapshot = required_path(arguments.next(), "snapshot directory")?;
            let output = required_path(arguments.next(), "publication output directory")?;
            let handoff = required_path(arguments.next(), "replica handoff path")?;
            let remaining = arguments.collect::<Vec<_>>();
            let unlock = load_database_unlock(&remaining)?;
            let reporter = ProgressReporter::from_arguments(
                &remaining,
                ProgressWorkflow::RestoreAndAudit,
                unlock.validates_exported_keys(),
            )?;
            let report = restore_snapshot_and_publish_with_progress(
                &snapshot,
                &OfflineRestorePublishOptions {
                    output_archive: output,
                    handoff_path: handoff,
                    previous_snapshot: option_path(&remaining, "--previous-snapshot")?,
                    previous_archive: option_path(&remaining, "--previous-archive")?,
                    account_root: option_path(&remaining, "--account-root")?,
                    defer_media: remaining.iter().any(|value| value == "--defer-media"),
                },
                unlock.material(),
                &reporter,
            )?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "audit-archive" => {
            let archive = required_path(arguments.next(), "archive directory")?;
            let remaining = arguments.collect::<Vec<_>>();
            let reporter =
                ProgressReporter::from_arguments(&remaining, ProgressWorkflow::Audit, false)?;
            let report = audit_archive_with_progress(&archive, &reporter)?;
            emit_json_result(&report, &remaining)?;
        }
        "audit-acquisition-chain" => {
            let previous = required_path(arguments.next(), "previous snapshot directory")?;
            let current = required_path(arguments.next(), "current snapshot directory")?;
            let report = audit_acquisition_chain(&previous, &current)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "audit-connector-log" => {
            let audit_log = required_path(arguments.next(), "connector audit log")?;
            let report = audit_connector_log(&audit_log)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "audit-connector-state" => {
            let replica = required_path(arguments.next(), "replica path")?;
            let policy = required_path(arguments.next(), "tool policy path")?;
            let audit = required_path(arguments.next(), "connector audit log")?;
            let drafts = required_path(arguments.next(), "connector draft directory")?;
            let remaining = arguments.collect::<Vec<_>>();
            if !remaining.iter().any(|value| value == "--replica-key-stdin") {
                return Err("replica keys must be supplied with --replica-key-stdin".into());
            }
            let key = ReplicaKey::read_stdin()?;
            let service = ConnectorService::open(&replica, &key, &policy, &audit, &drafts)?;
            let report = service.audit_state()?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "policy" => {
            let archive = required_path(arguments.next(), "archive directory")?;
            let policy_path = required_path(arguments.next(), "policy path")?;
            let remaining = arguments.collect::<Vec<_>>();
            let enabled = remaining
                .iter()
                .take_while(|value| !value.starts_with("--"))
                .cloned()
                .collect::<BTreeSet<_>>();
            let maximum_page_size = option_usize(&remaining, "--max-page-size")?.unwrap_or(100);
            let policy =
                create_conversation_policy(&archive, &policy_path, enabled, maximum_page_size)?;
            println!("{}", serde_json::to_string_pretty(&policy)?);
        }
        "read" => {
            let archive = required_path(arguments.next(), "archive directory")?;
            let policy = required_path(arguments.next(), "policy path")?;
            let conversation = arguments
                .next()
                .ok_or_else(|| "missing conversation ID".to_string())?;
            let remaining = arguments.collect::<Vec<_>>();
            let cursor = option_string(&remaining, "--cursor")?;
            let limit = option_usize(&remaining, "--limit")?.unwrap_or(100);
            let page =
                read_conversation_page(&archive, &policy, &conversation, cursor.as_deref(), limit)?;
            println!("{}", serde_json::to_string_pretty(&page)?);
        }
        "reconcile" => {
            let previous = required_path(arguments.next(), "previous archive directory")?;
            let current = required_path(arguments.next(), "current archive directory")?;
            let policy = required_path(arguments.next(), "policy path")?;
            let events = required_path(arguments.next(), "events output path")?;
            let report = reconcile_archives(&previous, &current, &policy, &events)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "merge-incremental" => {
            let previous = required_path(arguments.next(), "previous archive directory")?;
            let fragment = required_path(arguments.next(), "incremental fragment directory")?;
            let output = required_path(arguments.next(), "merged archive directory")?;
            let report = merge_incremental_archive(&previous, &fragment, &output)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "replica-bootstrap" => {
            let archive = required_path(arguments.next(), "archive directory")?;
            let replica = required_path(arguments.next(), "replica path")?;
            let remaining = arguments.collect::<Vec<_>>();
            if !remaining.iter().any(|value| value == "--replica-key-stdin") {
                return Err("replica keys must be supplied with --replica-key-stdin".into());
            }
            let reporter = ProgressReporter::from_arguments(
                &remaining,
                ProgressWorkflow::ReplicaApply,
                false,
            )?;
            let key = ReplicaKey::read_stdin()?;
            let report = bootstrap_replica_with_progress(&archive, &replica, &key, &reporter)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "replica-status" => {
            let replica = required_path(arguments.next(), "replica path")?;
            let remaining = arguments.collect::<Vec<_>>();
            if !remaining.iter().any(|value| value == "--replica-key-stdin") {
                return Err("replica keys must be supplied with --replica-key-stdin".into());
            }
            let key = ReplicaKey::read_stdin()?;
            let report = replica_status(&replica, &key)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "audit-replica" => {
            let replica = required_path(arguments.next(), "replica path")?;
            let remaining = arguments.collect::<Vec<_>>();
            if !remaining.iter().any(|value| value == "--replica-key-stdin") {
                return Err("replica keys must be supplied with --replica-key-stdin".into());
            }
            require_progress_file_outside_replica_namespace(&remaining, &replica)?;
            let reporter = ProgressReporter::from_arguments(
                &remaining,
                ProgressWorkflow::ReplicaAudit,
                false,
            )?;
            let key = ReplicaKey::read_stdin()?;
            let report = audit_replica_with_progress(&replica, &key, &reporter)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "audit-replica-backup" => {
            let backup = required_path(arguments.next(), "replica backup path")?;
            let remaining = arguments.collect::<Vec<_>>();
            if !remaining.iter().any(|value| value == "--replica-key-stdin") {
                return Err("replica keys must be supplied with --replica-key-stdin".into());
            }
            require_progress_file_outside_replica_namespace(&remaining, &backup)?;
            let reporter = ProgressReporter::from_arguments(
                &remaining,
                ProgressWorkflow::ReplicaAudit,
                false,
            )?;
            let key = ReplicaKey::read_stdin()?;
            let report = audit_replica_backup_with_progress(&backup, &key, &reporter)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "prepare-replica-recovery" => {
            let backup = required_path(arguments.next(), "replica backup path")?;
            let candidate = required_path(arguments.next(), "new recovery candidate path")?;
            let remaining = arguments.collect::<Vec<_>>();
            if !remaining.iter().any(|value| value == "--replica-key-stdin") {
                return Err("replica keys must be supplied with --replica-key-stdin".into());
            }
            let key = ReplicaKey::read_stdin()?;
            let report = prepare_replica_recovery(&backup, &candidate, &key)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "replica-sync" => {
            let archive = required_path(arguments.next(), "archive directory")?;
            let replica = required_path(arguments.next(), "replica path")?;
            let remaining = arguments.collect::<Vec<_>>();
            if !remaining.iter().any(|value| value == "--replica-key-stdin") {
                return Err("replica keys must be supplied with --replica-key-stdin".into());
            }
            let reporter = ProgressReporter::from_arguments(
                &remaining,
                ProgressWorkflow::ReplicaApply,
                false,
            )?;
            let key = ReplicaKey::read_stdin()?;
            let report = synchronize_replica_with_progress(&archive, &replica, &key, &reporter)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "replica-publish" => {
            let archive = required_path(arguments.next(), "replica-eligible archive directory")?;
            let handoff = required_path(arguments.next(), "replica handoff path")?;
            let remaining = arguments.collect::<Vec<_>>();
            let generation = required_u64_option(&remaining, "--generation")?;
            let report = publish_replica_handoff(&archive, &handoff, generation)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "replica-archive-quarantine" => {
            let handoff = required_path(arguments.next(), "replica handoff path")?;
            let quarantine = required_path(arguments.next(), "archive quarantine directory")?;
            let remaining = arguments.collect::<Vec<_>>();
            let retain_publications =
                option_usize(&remaining, "--retain-publications")?.unwrap_or(2);
            let report =
                quarantine_retired_replica_archives(&handoff, &quarantine, retain_publications)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "replica-archive-restore" => {
            let handoff = required_path(arguments.next(), "replica handoff path")?;
            let quarantine = required_path(arguments.next(), "archive quarantine directory")?;
            let remaining = arguments.collect::<Vec<_>>();
            let generation = required_u64_option(&remaining, "--generation")?;
            let report = restore_quarantined_replica_archive(&handoff, &quarantine, generation)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "compose-latency-evidence" => {
            let snapshot_report = required_path(arguments.next(), "private snapshot report")?;
            let offline_report = required_path(arguments.next(), "private offline report")?;
            let follower_report = required_path(arguments.next(), "private follower report")?;
            let handoff = required_path(arguments.next(), "private replica handoff")?;
            let report = compose_latency_evidence_sample(
                &snapshot_report,
                &offline_report,
                &follower_report,
                &handoff,
            )?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "summarize-latency-evidence" => {
            let samples = required_path(arguments.next(), "private latency sample array")?;
            let report = summarize_latency_evidence_samples(&samples)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "replica-follow-once" => {
            let handoff = required_path(arguments.next(), "replica handoff path")?;
            let state = required_path(arguments.next(), "replica follow state path")?;
            let replica = required_path(arguments.next(), "replica path")?;
            let remaining = arguments.collect::<Vec<_>>();
            if !remaining.iter().any(|value| value == "--replica-key-stdin") {
                return Err("replica keys must be supplied with --replica-key-stdin".into());
            }
            let key = ReplicaKey::read_stdin()?;
            let report = follow_replica_once(&handoff, &state, &replica, &key)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "replica-follow" => {
            let handoff = required_path(arguments.next(), "replica handoff path")?;
            let state = required_path(arguments.next(), "replica follow state path")?;
            let replica = required_path(arguments.next(), "replica path")?;
            let remaining = arguments.collect::<Vec<_>>();
            if !remaining.iter().any(|value| value == "--replica-key-stdin") {
                return Err("replica keys must be supplied with --replica-key-stdin".into());
            }
            let poll_milliseconds = option_u64(&remaining, "--poll-milliseconds")?.unwrap_or(1_000);
            if !(100..=60_000).contains(&poll_milliseconds) {
                return Err("--poll-milliseconds must be between 100 and 60000".into());
            }
            let maximum_polls = option_u64(&remaining, "--maximum-polls")?;
            if maximum_polls == Some(0) {
                return Err("--maximum-polls must be positive".into());
            }
            let key = ReplicaKey::read_stdin()?;
            let mut last_marker = None;
            let mut polls = 0_u64;
            loop {
                let marker = handoff_poll_marker(&handoff)?;
                if marker.is_some() && marker != last_marker {
                    let report = follow_replica_once(&handoff, &state, &replica, &key)?;
                    let mut output = io::stdout().lock();
                    serde_json::to_writer(&mut output, &report)?;
                    output.write_all(b"\n")?;
                    output.flush()?;
                    last_marker = marker;
                }
                polls = polls.saturating_add(1);
                if maximum_polls.is_some_and(|maximum| polls >= maximum) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(poll_milliseconds));
            }
        }
        "replica-follow-status" => {
            let handoff = required_path(arguments.next(), "replica handoff path")?;
            let state = required_path(arguments.next(), "replica follow state path")?;
            let replica = required_path(arguments.next(), "replica path")?;
            let remaining = arguments.collect::<Vec<_>>();
            if !remaining.iter().any(|value| value == "--replica-key-stdin") {
                return Err("replica keys must be supplied with --replica-key-stdin".into());
            }
            let key = ReplicaKey::read_stdin()?;
            let report = replica_follower_status(&handoff, &state, &replica, &key)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "replica-changes" => {
            let replica = required_path(arguments.next(), "replica path")?;
            let remaining = arguments.collect::<Vec<_>>();
            if !remaining.iter().any(|value| value == "--replica-key-stdin") {
                return Err("replica keys must be supplied with --replica-key-stdin".into());
            }
            let cursor = option_string(&remaining, "--cursor")?;
            let limit = option_usize(&remaining, "--limit")?.unwrap_or(100);
            let key = ReplicaKey::read_stdin()?;
            let report = get_replica_changes(&replica, &key, cursor.as_deref(), limit)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "replica-search" => {
            let replica = required_path(arguments.next(), "replica path")?;
            let filter_path = required_path(arguments.next(), "private filter JSON path")?;
            let remaining = arguments.collect::<Vec<_>>();
            if !remaining.iter().any(|value| value == "--replica-key-stdin") {
                return Err("replica keys must be supplied with --replica-key-stdin".into());
            }
            let cursor = option_string(&remaining, "--cursor")?;
            let limit = option_usize(&remaining, "--limit")?.unwrap_or(100);
            let filter = load_replica_message_filter(&filter_path)?;
            let key = ReplicaKey::read_stdin()?;
            let report =
                search_replica_messages(&replica, &key, &filter, cursor.as_deref(), limit)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "replica-cached-moments" => {
            let replica = required_path(arguments.next(), "replica path")?;
            let remaining = arguments.collect::<Vec<_>>();
            if !remaining.iter().any(|value| value == "--replica-key-stdin") {
                return Err("replica keys must be supplied with --replica-key-stdin".into());
            }
            let filter = ReplicaCachedMomentFilter {
                author_id: option_string(&remaining, "--author")?,
                not_before_unix: option_i64(&remaining, "--not-before-unix")?,
                not_after_unix: option_i64(&remaining, "--not-after-unix")?,
                content_type: option_i64(&remaining, "--content-type")?,
            };
            let cursor = option_string(&remaining, "--cursor")?;
            let limit = option_usize(&remaining, "--limit")?.unwrap_or(100);
            let key = ReplicaKey::read_stdin()?;
            let report =
                search_replica_cached_moments(&replica, &key, &filter, cursor.as_deref(), limit)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "replica-message" => {
            let replica = required_path(arguments.next(), "replica path")?;
            let canonical_id = arguments
                .next()
                .ok_or_else(|| "missing canonical message ID".to_string())?;
            let remaining = arguments.collect::<Vec<_>>();
            if !remaining.iter().any(|value| value == "--replica-key-stdin") {
                return Err("replica keys must be supplied with --replica-key-stdin".into());
            }
            let key = ReplicaKey::read_stdin()?;
            let report = get_replica_message(&replica, &key, &canonical_id)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "replica-conversations" => {
            let replica = required_path(arguments.next(), "replica path")?;
            let remaining = arguments.collect::<Vec<_>>();
            if !remaining.iter().any(|value| value == "--replica-key-stdin") {
                return Err("replica keys must be supplied with --replica-key-stdin".into());
            }
            let limit = option_usize(&remaining, "--limit")?.unwrap_or(100);
            let key = ReplicaKey::read_stdin()?;
            let report = list_replica_conversations(&replica, &key, limit)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "replica-coverage" => {
            let replica = required_path(arguments.next(), "replica path")?;
            let remaining = arguments.collect::<Vec<_>>();
            if !remaining.iter().any(|value| value == "--replica-key-stdin") {
                return Err("replica keys must be supplied with --replica-key-stdin".into());
            }
            let key = ReplicaKey::read_stdin()?;
            let report = replica_coverage(&replica, &key)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "ai-query" => {
            let replica = required_path(arguments.next(), "replica path")?;
            let policy = required_path(arguments.next(), "tool policy path")?;
            let audit = required_path(arguments.next(), "connector audit log")?;
            let request_path = required_path(arguments.next(), "private AI query JSON path")?;
            let remaining = arguments.collect::<Vec<_>>();
            if !remaining.iter().any(|value| value == "--replica-key-stdin") {
                return Err("replica keys must be supplied with --replica-key-stdin".into());
            }
            let request = load_ai_query_request(&request_path)?;
            let key = ReplicaKey::read_stdin()?;
            let response = query_ai_context(&replica, &key, &policy, &audit, request)?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        "ai-export" => {
            let replica = required_path(arguments.next(), "replica path")?;
            let policy = required_path(arguments.next(), "tool policy path")?;
            let audit = required_path(arguments.next(), "connector audit log")?;
            let output = required_path(arguments.next(), "AI context output directory")?;
            let remaining = arguments.collect::<Vec<_>>();
            if !remaining.iter().any(|value| value == "--replica-key-stdin") {
                return Err("replica keys must be supplied with --replica-key-stdin".into());
            }
            let requester = required_option(&remaining, "--requester")?;
            let destination =
                parse_connector_destination(option_string(&remaining, "--destination")?)?;
            require_progress_file_outside(&remaining, &[(&output, "AI context output directory")])?;
            let reporter =
                ProgressReporter::from_arguments(&remaining, ProgressWorkflow::AiExport, false)?;
            let key = ReplicaKey::read_stdin()?;
            let manifest = export_ai_context(
                &replica,
                &key,
                &policy,
                &audit,
                &output,
                &requester,
                destination,
                &reporter,
            )?;
            println!("{}", serde_json::to_string_pretty(&manifest)?);
        }
        "audit-ai-context" => {
            let bundle = required_path(arguments.next(), "AI context bundle directory")?;
            let remaining = arguments.collect::<Vec<_>>();
            require_progress_file_outside(&remaining, &[(&bundle, "AI context bundle")])?;
            let reporter = ProgressReporter::from_arguments(
                &remaining,
                ProgressWorkflow::ContextAudit,
                false,
            )?;
            let report = audit_ai_context_with_progress(&bundle, &reporter)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "ai-memory-export" => {
            let bundle = required_path(arguments.next(), "AI context bundle directory")?;
            let output = required_path(arguments.next(), "AI memory output directory")?;
            let remaining = arguments.collect::<Vec<_>>();
            require_progress_file_outside(
                &remaining,
                &[
                    (&bundle, "AI context bundle"),
                    (&output, "AI memory output directory"),
                ],
            )?;
            let defaults = AiMemoryExportOptions::default();
            let options = AiMemoryExportOptions {
                maximum_messages_per_chunk: option_usize(&remaining, "--max-messages-per-chunk")?
                    .unwrap_or(defaults.maximum_messages_per_chunk),
                maximum_text_bytes_per_chunk: option_usize(
                    &remaining,
                    "--max-text-bytes-per-chunk",
                )?
                .unwrap_or(defaults.maximum_text_bytes_per_chunk),
            };
            let reporter = ProgressReporter::from_arguments(
                &remaining,
                ProgressWorkflow::MemoryProjection,
                false,
            )?;
            let manifest = export_ai_memory_with_progress(&bundle, &output, options, &reporter)?;
            println!("{}", serde_json::to_string_pretty(&manifest)?);
        }
        "audit-ai-memory" => {
            let memory = required_path(arguments.next(), "AI memory output directory")?;
            let remaining = arguments.collect::<Vec<_>>();
            require_progress_file_outside(&remaining, &[(&memory, "AI memory output directory")])?;
            let reporter =
                ProgressReporter::from_arguments(&remaining, ProgressWorkflow::MemoryAudit, false)?;
            let report = audit_ai_memory_with_progress(&memory, &reporter)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "connector-serve" => {
            let replica = required_path(arguments.next(), "replica path")?;
            let policy = required_path(arguments.next(), "tool policy path")?;
            let audit = required_path(arguments.next(), "audit log path")?;
            let drafts = required_path(arguments.next(), "draft directory")?;
            let socket = required_path(arguments.next(), "Unix socket path")?;
            let remaining = arguments.collect::<Vec<_>>();
            if !remaining.iter().any(|value| value == "--replica-key-stdin") {
                return Err("replica keys must be supplied with --replica-key-stdin".into());
            }
            let key = ReplicaKey::read_stdin()?;
            let service = ConnectorService::open(&replica, &key, &policy, &audit, &drafts)?;
            serve_unix(&service, &socket)?;
        }
        "connector-call" => {
            let socket = required_path(arguments.next(), "Unix socket path")?;
            let request_path = required_path(arguments.next(), "private request JSON path")?;
            let request = load_connector_request(&request_path)?;
            let response = send_unix_request(&socket, &request)?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        "tool-policy" => {
            let archive = required_path(arguments.next(), "archive directory")?;
            let policy_path = required_path(arguments.next(), "tool policy path")?;
            let remaining = arguments.collect::<Vec<_>>();
            let conversations = remaining
                .iter()
                .take_while(|value| !value.starts_with("--"))
                .cloned()
                .collect::<BTreeSet<_>>();
            let all_conversations = remaining.iter().any(|value| value == "--all-conversations");
            if all_conversations && !conversations.is_empty() {
                return Err(
                    "--all-conversations is mutually exclusive with conversation IDs".into(),
                );
            }
            let capabilities = match option_string(&remaining, "--capabilities")? {
                Some(value) => parse_capabilities(&value)?,
                None if conversations.is_empty() && !all_conversations => BTreeSet::new(),
                None => return Err("missing --capabilities".into()),
            };
            let message_fields = match option_string(&remaining, "--fields")? {
                Some(value) => parse_message_fields(&value)?,
                None if conversations.is_empty() && !all_conversations => BTreeSet::new(),
                None => return Err("missing --fields".into()),
            };
            let not_before_unix = option_i64(&remaining, "--not-before-unix")?;
            let not_after_unix = option_i64(&remaining, "--not-after-unix")?;
            let allow_remote_model = remaining
                .iter()
                .any(|value| value == "--allow-remote-model");
            let conversation_scope = ConversationToolScope {
                capabilities: capabilities.clone(),
                message_fields: message_fields.clone(),
                not_before_unix,
                not_after_unix,
                allow_remote_model,
            };
            let scopes = conversations
                .into_iter()
                .map(|conversation| (conversation, conversation_scope.clone()))
                .collect::<BTreeMap<_, _>>();
            let cached_moments_scope = if remaining
                .iter()
                .any(|value| value == "--enable-cached-moments")
            {
                let fields = option_string(&remaining, "--cached-fields")?
                    .ok_or_else(|| "missing --cached-fields".to_string())
                    .and_then(|value| parse_cached_moment_fields(&value))?;
                Some(CachedMomentsToolScope {
                    fields,
                    not_before_unix: option_i64(&remaining, "--cached-not-before-unix")?,
                    not_after_unix: option_i64(&remaining, "--cached-not-after-unix")?,
                    allow_remote_model: remaining
                        .iter()
                        .any(|value| value == "--allow-cached-remote-model"),
                })
            } else {
                None
            };
            let maximum_result_count = option_usize(&remaining, "--max-results")?.unwrap_or(100);
            let maximum_message_summary_bytes =
                option_usize(&remaining, "--max-summary-bytes")?.unwrap_or(4_096);
            let maximum_draft_bytes =
                option_usize(&remaining, "--max-draft-bytes")?.unwrap_or(16_384);
            let policy = if all_conversations {
                create_all_conversations_tool_policy_with_cached_moments(
                    &archive,
                    &policy_path,
                    conversation_scope,
                    cached_moments_scope,
                    maximum_result_count,
                    maximum_message_summary_bytes,
                    maximum_draft_bytes,
                )?
            } else {
                create_tool_policy_with_cached_moments(
                    &archive,
                    &policy_path,
                    scopes,
                    cached_moments_scope,
                    maximum_result_count,
                    maximum_message_summary_bytes,
                    maximum_draft_bytes,
                )?
            };
            println!("{}", serde_json::to_string_pretty(&policy)?);
        }
        "tool-list" => {
            let archive = required_path(arguments.next(), "archive directory")?;
            let policy = required_path(arguments.next(), "tool policy path")?;
            let audit = required_path(arguments.next(), "audit log path")?;
            let remaining = arguments.collect::<Vec<_>>();
            let destination = parse_destination(option_string(&remaining, "--destination")?)?;
            let requester = required_option(&remaining, "--requester")?;
            let service = LocalToolService::open(&archive, &policy, &audit, &requester)?;
            let result = service.list_enabled_conversations(destination)?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        "tool-recent" => {
            let archive = required_path(arguments.next(), "archive directory")?;
            let policy = required_path(arguments.next(), "tool policy path")?;
            let audit = required_path(arguments.next(), "audit log path")?;
            let conversation = arguments
                .next()
                .ok_or_else(|| "missing conversation ID".to_string())?;
            let remaining = arguments.collect::<Vec<_>>();
            let destination = parse_destination(option_string(&remaining, "--destination")?)?;
            let limit = option_usize(&remaining, "--limit")?.unwrap_or(20);
            let requester = required_option(&remaining, "--requester")?;
            let service = LocalToolService::open(&archive, &policy, &audit, &requester)?;
            let result = service.read_recent_messages(&conversation, limit, destination)?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        "tool-search" => {
            let archive = required_path(arguments.next(), "archive directory")?;
            let policy = required_path(arguments.next(), "tool policy path")?;
            let audit = required_path(arguments.next(), "audit log path")?;
            let remaining = arguments.collect::<Vec<_>>();
            if !remaining.iter().any(|value| value == "--query-stdin") {
                return Err("tool search queries must be supplied with --query-stdin".into());
            }
            let destination = parse_destination(option_string(&remaining, "--destination")?)?;
            let conversation = option_string(&remaining, "--conversation")?;
            let limit = option_usize(&remaining, "--limit")?.unwrap_or(20);
            let requester = required_option(&remaining, "--requester")?;
            let mut query = read_utf8_stdin_limited(1_024)?;
            while query.ends_with(['\n', '\r']) {
                query.pop();
            }
            let service = LocalToolService::open(&archive, &policy, &audit, &requester)?;
            let result =
                service.search_messages(&query, conversation.as_deref(), limit, destination)?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        "tool-draft" => {
            let archive = required_path(arguments.next(), "archive directory")?;
            let policy = required_path(arguments.next(), "tool policy path")?;
            let audit = required_path(arguments.next(), "audit log path")?;
            let drafts = required_path(arguments.next(), "draft directory")?;
            let conversation = arguments
                .next()
                .ok_or_else(|| "missing conversation ID".to_string())?;
            let remaining = arguments.collect::<Vec<_>>();
            if !remaining.iter().any(|value| value == "--body-stdin") {
                return Err("draft bodies must be supplied with --body-stdin".into());
            }
            let requester = required_option(&remaining, "--requester")?;
            let body = read_utf8_stdin_limited(256 * 1_024)?;
            let service = LocalToolService::open(&archive, &policy, &audit, &requester)?;
            let result = service.create_draft(&conversation, &body, &drafts)?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        _ => {
            eprintln!(
                concat!(
                    "Usage:\n",
                    "  greenbubbles-restore synthetic-benchmark <private-work-directory> [--samples <n>] [--small-messages <n>] [--large-messages <n>] [--burst-messages <n>]\n",
                    "  greenbubbles-restore preflight <snapshot> [--progress-file <private-ndjson>] [--progress-json | --quiet-progress]\n",
                    "  greenbubbles-restore probe <snapshot> [--passphrase-stdin | --database-keys-file <owner-only-json>] [--progress-file <private-ndjson>] [--progress-json | --quiet-progress]\n",
                    "  greenbubbles-restore restore <snapshot> <output> [--account-root <path>] [--defer-media] [--passphrase-stdin | --database-keys-file <owner-only-json>] [--progress-file <private-ndjson>] [--progress-json | --quiet-progress]\n",
                    "  greenbubbles-restore diagnose-batch <snapshot> <diagnostic-output> [--database-offset <n>] [--database-limit <n>] [--resolve-media --account-root <path>] [--passphrase-stdin | --database-keys-file <owner-only-json>] [--summary-file <private-json>] [--progress-file <private-ndjson>] [--progress-json | --quiet-progress]\n",
                    "  greenbubbles-restore diagnose-available <snapshot> <diagnostic-output> --database-keys-file <owner-only-json> [--resolve-media --account-root <path>] [--summary-file <private-json>] [--progress-file <private-ndjson>] [--progress-json | --quiet-progress]\n",
                    "  greenbubbles-restore diagnose-archive-payloads <archive> <private-report-json> [--progress-file <private-ndjson>] [--progress-json | --quiet-progress]\n",
                    "  greenbubbles-restore diagnose-archive-schema <archive> <private-report-json> [--progress-file <private-ndjson>] [--progress-json | --quiet-progress]\n",
                    "  greenbubbles-restore restore-publish <snapshot> <publication-output> <handoff-file> [--previous-snapshot <path> --previous-archive <path>] [--account-root <path>] [--defer-media] [--passphrase-stdin | --database-keys-file <owner-only-json>] [--progress-file <private-ndjson>] [--progress-json | --quiet-progress]\n",
                    "  greenbubbles-restore audit-archive <archive> [--summary-file <private-json>] [--progress-file <private-ndjson>] [--progress-json | --quiet-progress]\n",
                    "  greenbubbles-restore audit-acquisition-chain <previous-snapshot> <current-snapshot>\n",
                    "  greenbubbles-restore audit-connector-log <connector-audit-log>\n",
                    "  greenbubbles-restore audit-connector-state <replica-path> <policy-file> <connector-audit-log> <draft-directory> --replica-key-stdin\n",
                    "  greenbubbles-restore policy <archive> <policy-file> <conversation-id>... [--max-page-size <n>]\n",
                    "  greenbubbles-restore read <archive> <policy-file> <conversation-id> [--cursor <cursor>] [--limit <n>]\n",
                    "  greenbubbles-restore reconcile <previous-archive> <current-archive> <policy-file> <events-output>\n",
                    "  greenbubbles-restore merge-incremental <previous-archive> <fragment-archive> <output-archive>\n",
                    "  greenbubbles-restore replica-bootstrap <archive> <replica-path> --replica-key-stdin [--progress-file <private-ndjson>] [--progress-json | --quiet-progress]\n",
                    "  greenbubbles-restore replica-status <replica-path> --replica-key-stdin\n",
                    "  greenbubbles-restore audit-replica <replica-path> --replica-key-stdin [--progress-file <private-ndjson>] [--progress-json | --quiet-progress]\n",
                    "  greenbubbles-restore audit-replica-backup <pre-migration-backup-path> --replica-key-stdin [--progress-file <private-ndjson>] [--progress-json | --quiet-progress]\n",
                    "  greenbubbles-restore prepare-replica-recovery <pre-migration-backup-path> <new-candidate-path> --replica-key-stdin\n",
                    "  greenbubbles-restore replica-sync <archive> <replica-path> --replica-key-stdin [--progress-file <private-ndjson>] [--progress-json | --quiet-progress]\n",
                    "  greenbubbles-restore replica-publish <replica-eligible-archive> <handoff-file> --generation <positive-integer>\n",
                    "  greenbubbles-restore replica-archive-quarantine <handoff-file> <quarantine-directory> [--retain-publications <n, minimum 2>]\n",
                    "  greenbubbles-restore replica-archive-restore <handoff-file> <quarantine-directory> --generation <positive-integer>\n",
                    "  greenbubbles-restore compose-latency-evidence <private-snapshot-report> <private-offline-report> <private-follower-report> <private-handoff-file>\n",
                    "  greenbubbles-restore summarize-latency-evidence <private-sample-array-json>\n",
                    "  greenbubbles-restore replica-follow-once <handoff-file> <follow-state-file> <replica-path> --replica-key-stdin\n",
                    "  greenbubbles-restore replica-follow <handoff-file> <follow-state-file> <replica-path> --replica-key-stdin [--poll-milliseconds <100..60000>] [--maximum-polls <n>]\n",
                    "  greenbubbles-restore replica-follow-status <handoff-file> <follow-state-file> <replica-path> --replica-key-stdin\n",
                    "  greenbubbles-restore replica-changes <replica-path> --replica-key-stdin [--cursor <cursor>] [--limit <n>]\n",
                    "  greenbubbles-restore replica-search <replica-path> <private-filter-json> --replica-key-stdin [--cursor <cursor>] [--limit <n>]\n",
                    "  greenbubbles-restore replica-cached-moments <replica-path> --replica-key-stdin [--author <opaque-id>] [--content-type <n>] [--not-before-unix <seconds>] [--not-after-unix <seconds>] [--cursor <cursor>] [--limit <n>]\n",
                    "  greenbubbles-restore replica-message <replica-path> <canonical-id> --replica-key-stdin\n",
                    "  greenbubbles-restore replica-conversations <replica-path> --replica-key-stdin [--limit <n>]\n",
                    "  greenbubbles-restore replica-coverage <replica-path> --replica-key-stdin\n",
                    "  greenbubbles-restore ai-query <replica-path> <policy-file> <connector-audit-log> <private-request-json> --replica-key-stdin\n",
                    "  greenbubbles-restore ai-export <replica-path> <policy-file> <connector-audit-log> <new-output-directory> --replica-key-stdin --requester <id> [--destination local|remote] [--progress-file <private-ndjson>] [--progress-json | --quiet-progress]\n",
                    "  greenbubbles-restore audit-ai-context <AI-context-bundle-directory> [--progress-file <private-ndjson>] [--progress-json | --quiet-progress]\n",
                    "  greenbubbles-restore ai-memory-export <AI-context-bundle-directory> <new-output-directory> [--max-messages-per-chunk <n>] [--max-text-bytes-per-chunk <n>] [--progress-file <private-ndjson>] [--progress-json | --quiet-progress]\n",
                    "  greenbubbles-restore audit-ai-memory <AI-memory-output-directory> [--progress-file <private-ndjson>] [--progress-json | --quiet-progress]\n",
                    "  greenbubbles-restore connector-serve <replica-path> <policy-file> <audit-log> <draft-directory> <socket-path> --replica-key-stdin\n",
                    "  greenbubbles-restore connector-call <socket-path> <private-request-json>\n",
                    "  greenbubbles-restore tool-policy <archive> <policy-file> ([<conversation-id>...] | --all-conversations) [--capabilities list,read,search,draft] [--fields sender,created-at,direction,type,content,attachments,relationships] [--not-before-unix <seconds>] [--not-after-unix <seconds>] [--allow-remote-model] [--enable-cached-moments --cached-fields author,created-at,type,content,title,description,url,media-count,like-count,comment-count] [--cached-not-before-unix <seconds>] [--cached-not-after-unix <seconds>] [--allow-cached-remote-model] [--max-results <n>] [--max-summary-bytes <n>] [--max-draft-bytes <n>]\n",
                    "  greenbubbles-restore tool-list <archive> <policy-file> <audit-log> --requester <id> [--destination local|remote]\n",
                    "  greenbubbles-restore tool-recent <archive> <policy-file> <audit-log> <conversation-id> --requester <id> [--limit <n>] [--destination local|remote]\n",
                    "  greenbubbles-restore tool-search <archive> <policy-file> <audit-log> --requester <id> --query-stdin [--conversation <id>] [--limit <n>] [--destination local|remote]\n",
                    "  greenbubbles-restore tool-draft <archive> <policy-file> <audit-log> <draft-directory> <conversation-id> --requester <id> --body-stdin"
                )
            );
        }
    }
    Ok(())
}

fn ai_command_help(command: &str) -> Option<&'static str> {
    match command {
        "audit-replica" => Some(concat!(
            "Usage:\n",
            "  greenbubbles-restore audit-replica <replica-path> --replica-key-stdin [--progress-file <private-ndjson>] [--progress-json | --quiet-progress]\n\n",
            "Runs a read-only, aggregate-only deep audit of the encrypted serving replica.\n",
            "Progress includes replica bytes, canonical/link/change totals, exact row progress,\n",
            "stage and overall percentages, and elapsed time without exposing private content.\n\n",
            "Options:\n",
            "  --replica-key-stdin  Require the replica key on standard input\n",
            "  --progress-file <path>  Create an owner-only NDJSON progress log\n",
            "  --progress-json      Emit NDJSON progress on standard error\n",
            "  --quiet-progress     Suppress human progress on standard error\n",
            "  -h, --help           Show this help\n",
        )),
        "audit-replica-backup" => Some(concat!(
            "Usage:\n",
            "  greenbubbles-restore audit-replica-backup <pre-migration-backup-path> --replica-key-stdin [--progress-file <private-ndjson>] [--progress-json | --quiet-progress]\n\n",
            "Runs the historical-schema deep audit without migrating or rewriting the backup.\n",
            "It reports the same privacy-safe byte, row, stage, percentage, and elapsed-time\n",
            "progress as the current-replica audit.\n\n",
            "Options:\n",
            "  --replica-key-stdin  Require the replica key on standard input\n",
            "  --progress-file <path>  Create an owner-only NDJSON progress log\n",
            "  --progress-json      Emit NDJSON progress on standard error\n",
            "  --quiet-progress     Suppress human progress on standard error\n",
            "  -h, --help           Show this help\n",
        )),
        "ai-query" => Some(concat!(
            "Usage:\n",
            "  greenbubbles-restore ai-query <replica-path> <policy-file> <connector-audit-log> <private-request-json> --replica-key-stdin\n\n",
            "Runs one policy-scoped, read-only JSON request against the encrypted replica.\n",
            "The request file must be an owner-only regular file. The replica key is read only\n",
            "from standard input; query text and keys must not be supplied as arguments.\n",
            "The JSON response is written to standard output.\n\n",
            "Options:\n",
            "  --replica-key-stdin  Require the replica key on standard input\n",
            "  -h, --help           Show this help\n",
        )),
        "ai-export" => Some(concat!(
            "Usage:\n",
            "  greenbubbles-restore ai-export <replica-path> <policy-file> <connector-audit-log> <new-output-directory> --replica-key-stdin --requester <id> [--destination local|remote] [--progress-file <private-ndjson>] [--progress-json | --quiet-progress]\n\n",
            "Exports one atomic, checkpoint-consistent, policy-scoped AI context bundle.\n",
            "The output directory must not already exist. Progress is written to standard error,\n",
            "and the final manifest is written as JSON to standard output.\n\n",
            "Options:\n",
            "  --replica-key-stdin       Require the replica key on standard input\n",
            "  --requester <id>          Stable local requester identity\n",
            "  --destination <target>    local (default) or remote\n",
            "  --progress-file <path>    Create an owner-only NDJSON progress log\n",
            "  --progress-json           Emit NDJSON progress on standard error\n",
            "  --quiet-progress          Suppress human progress on standard error\n",
            "  -h, --help                Show this help\n",
        )),
        "audit-ai-context" => Some(concat!(
            "Usage:\n",
            "  greenbubbles-restore audit-ai-context <AI-context-bundle-directory> [--progress-file <private-ndjson>] [--progress-json | --quiet-progress]\n\n",
            "Verifies the bundle inventory, permissions, schemas, hashes, counts, identities,\n",
            "references, freshness, checkpoint, and policy binding without printing content.\n\n",
            "Options:\n",
            "  --progress-file <path>  Create an owner-only NDJSON progress log\n",
            "  --progress-json         Emit NDJSON progress on standard error\n",
            "  --quiet-progress        Suppress human progress on standard error\n",
            "  -h, --help              Show this help\n",
        )),
        "ai-memory-export" => Some(concat!(
            "Usage:\n",
            "  greenbubbles-restore ai-memory-export <AI-context-bundle-directory> <new-output-directory> [--max-messages-per-chunk <n>] [--max-text-bytes-per-chunk <n>] [--progress-file <private-ndjson>] [--progress-json | --quiet-progress]\n\n",
            "Projects an integrity-bound AI context bundle into deterministic, bounded\n",
            "conversation chunks for personal-memory systems. The atomic owner-only output\n",
            "contains Mem0-compatible JSON message batches and QMD-compatible Markdown.\n",
            "Damaged individual records are omitted with limitation counts; source file\n",
            "digest or checkpoint tampering still fails closed.\n\n",
            "Options:\n",
            "  --max-messages-per-chunk <n>   1..1000; default 64\n",
            "  --max-text-bytes-per-chunk <n> 256..1048576; default 49152\n",
            "  --progress-file <path>         Create an owner-only NDJSON progress log\n",
            "  --progress-json                Emit NDJSON progress on standard error\n",
            "  --quiet-progress               Suppress human progress on standard error\n",
            "  -h, --help                     Show this help\n",
        )),
        "audit-ai-memory" => Some(concat!(
            "Usage:\n",
            "  greenbubbles-restore audit-ai-memory <AI-memory-output-directory> [--progress-file <private-ndjson>] [--progress-json | --quiet-progress]\n\n",
            "Verifies the projection identity, owner-only inventory, hashes, bounded chunk\n",
            "schemas, source citations, and every Markdown document without printing content.\n\n",
            "Options:\n",
            "  --progress-file <path>  Create an owner-only NDJSON progress log\n",
            "  --progress-json         Emit NDJSON progress on standard error\n",
            "  --quiet-progress        Suppress human progress on standard error\n",
            "  -h, --help              Show this help\n",
        )),
        _ => None,
    }
}

enum OwnedDatabaseUnlock {
    None,
    Passphrase(DatabasePassphrase),
    ExportedKeys(DatabaseKeySet),
}

impl OwnedDatabaseUnlock {
    fn material(&self) -> DatabaseUnlockMaterial<'_> {
        match self {
            Self::None => DatabaseUnlockMaterial::None,
            Self::Passphrase(value) => DatabaseUnlockMaterial::Passphrase(value),
            Self::ExportedKeys(value) => DatabaseUnlockMaterial::ExportedKeys(value),
        }
    }

    fn validates_exported_keys(&self) -> bool {
        matches!(self, Self::ExportedKeys(_))
    }

    fn exported_keys(&self) -> Option<&DatabaseKeySet> {
        match self {
            Self::ExportedKeys(value) => Some(value),
            Self::None | Self::Passphrase(_) => None,
        }
    }
}

fn load_database_unlock(
    arguments: &[String],
) -> Result<OwnedDatabaseUnlock, Box<dyn std::error::Error>> {
    let passphrase_stdin = arguments.iter().any(|value| value == "--passphrase-stdin");
    let key_file = option_path(arguments, "--database-keys-file")?;
    if passphrase_stdin && key_file.is_some() {
        return Err(
            "choose one database unlock source: --passphrase-stdin or --database-keys-file".into(),
        );
    }
    if passphrase_stdin {
        Ok(OwnedDatabaseUnlock::Passphrase(
            DatabasePassphrase::read_stdin()?,
        ))
    } else if let Some(path) = key_file {
        Ok(OwnedDatabaseUnlock::ExportedKeys(DatabaseKeySet::load(
            &path,
        )?))
    } else {
        Ok(OwnedDatabaseUnlock::None)
    }
}

enum ProgressOutput {
    Human,
    Json,
    Quiet,
}

#[derive(Clone, Copy)]
enum ProgressWorkflow {
    Preflight,
    Probe,
    Restore,
    RestoreAndAudit,
    Audit,
    ReplicaApply,
    ReplicaAudit,
    AiExport,
    ContextAudit,
    MemoryProjection,
    MemoryAudit,
}

impl ProgressWorkflow {
    fn phases(self, validates_exported_keys: bool) -> Vec<ProgressPhase> {
        if matches!(self, Self::AiExport) {
            return vec![ProgressPhase::ContextExport];
        }
        if matches!(self, Self::ContextAudit) {
            return vec![ProgressPhase::ContextAudit];
        }
        if matches!(self, Self::MemoryProjection) {
            return vec![ProgressPhase::MemoryProjection];
        }
        if matches!(self, Self::MemoryAudit) {
            return vec![ProgressPhase::MemoryAudit];
        }
        if matches!(self, Self::Preflight) {
            return vec![ProgressPhase::SnapshotVerification];
        }
        if matches!(self, Self::Audit) {
            return vec![ProgressPhase::ArchiveAudit];
        }
        if matches!(self, Self::ReplicaApply) {
            return vec![
                ProgressPhase::ArchiveAudit,
                ProgressPhase::ReplicaApplication,
            ];
        }
        if matches!(self, Self::ReplicaAudit) {
            return vec![ProgressPhase::ReplicaAudit];
        }
        let mut phases = vec![ProgressPhase::SnapshotVerification];
        if validates_exported_keys {
            phases.push(ProgressPhase::KeyValidation);
        }
        phases.push(ProgressPhase::DatabasePreparation);
        if !matches!(self, Self::Probe) {
            phases.extend([
                ProgressPhase::RecordPlanning,
                ProgressPhase::RecordRestoration,
                ProgressPhase::ArchiveFinalization,
            ]);
        }
        if matches!(self, Self::RestoreAndAudit) {
            phases.push(ProgressPhase::ArchiveAudit);
        }
        phases
    }
}

struct ProgressReporter {
    output: ProgressOutput,
    workflow_phases: Vec<ProgressPhase>,
    progress_file: Option<Mutex<ProgressFileState>>,
    progress_file_failed: AtomicBool,
    human_state: Mutex<HumanProgressState>,
}

struct ProgressFileState {
    writer: BufWriter<File>,
    last_synchronized_at: Instant,
}

#[derive(Default)]
struct HumanProgressState {
    last_emitted_at: Option<Instant>,
    phase: Option<ProgressPhase>,
    database_index: Option<usize>,
}

struct PhaseRangeProgress<'a> {
    observer: &'a dyn ProgressObserver,
    start: u64,
    end: u64,
}

impl<'a> PhaseRangeProgress<'a> {
    fn new(observer: &'a dyn ProgressObserver, start: u64, end: u64) -> Self {
        debug_assert!(start <= end && end <= 1_000_000);
        Self {
            observer,
            start,
            end,
        }
    }
}

impl ProgressObserver for PhaseRangeProgress<'_> {
    fn observe(&self, mut event: ProgressEvent) {
        const RESOLUTION: u64 = 1_000_000;
        let local = if event.phase_total > 0 {
            (event.phase_completed.min(event.phase_total) as u128 * RESOLUTION as u128
                / event.phase_total as u128) as u64
        } else if event.state == ProgressState::Completed {
            RESOLUTION
        } else {
            0
        };
        let span = self.end.saturating_sub(self.start);
        event.phase_completed = self.start.saturating_add(
            u64::try_from(local as u128 * span as u128 / RESOLUTION as u128).unwrap_or(span),
        );
        event.phase_total = RESOLUTION;
        self.observer.observe(event);
    }
}

impl ProgressReporter {
    fn from_arguments(
        arguments: &[String],
        workflow: ProgressWorkflow,
        validates_exported_keys: bool,
    ) -> Result<Self, String> {
        let json = arguments.iter().any(|value| value == "--progress-json");
        let quiet = arguments.iter().any(|value| value == "--quiet-progress");
        if json && quiet {
            return Err("choose at most one of --progress-json and --quiet-progress".to_string());
        }
        let output = if json {
            ProgressOutput::Json
        } else if quiet {
            ProgressOutput::Quiet
        } else {
            ProgressOutput::Human
        };
        let progress_file = option_path(arguments, "--progress-file")?
            .map(|path| {
                owner_only_create_new_writer(&path)
                    .map(|writer| {
                        Mutex::new(ProgressFileState {
                            writer,
                            last_synchronized_at: Instant::now(),
                        })
                    })
                    .map_err(|error| format!("could not create private progress file: {error}"))
            })
            .transpose()?;
        Ok(Self {
            output,
            workflow_phases: workflow.phases(validates_exported_keys),
            progress_file,
            progress_file_failed: AtomicBool::new(false),
            human_state: Mutex::new(HumanProgressState::default()),
        })
    }
}

impl ProgressObserver for ProgressReporter {
    fn observe(&self, mut event: ProgressEvent) {
        event.attach_workflow(&self.workflow_phases);
        if let Some(progress_file) = &self.progress_file {
            if self.progress_file_failed.load(Ordering::Relaxed) {
                return self.emit_display(event);
            }
            let write_result = (|| -> Result<(), Box<dyn std::error::Error>> {
                let mut state = progress_file
                    .lock()
                    .map_err(|_| "private progress file lock was poisoned")?;
                serde_json::to_writer(&mut state.writer, &event)?;
                state.writer.write_all(b"\n")?;
                state.writer.flush()?;
                let now = Instant::now();
                let workflow_completed = event.workflow_completed.is_some()
                    && event.workflow_completed == event.workflow_total;
                if workflow_completed
                    || now.saturating_duration_since(state.last_synchronized_at)
                        >= Duration::from_secs(5)
                {
                    state.writer.get_ref().sync_data()?;
                    state.last_synchronized_at = now;
                }
                Ok(())
            })();
            if let Err(error) = write_result {
                if !self.progress_file_failed.swap(true, Ordering::Relaxed) {
                    eprintln!("error: could not append private progress event: {error}");
                }
            }
        }
        self.emit_display(event);
    }
}

impl ProgressReporter {
    fn emit_display(&self, event: ProgressEvent) {
        match self.output {
            ProgressOutput::Quiet => {}
            ProgressOutput::Json => {
                if let Ok(value) = serde_json::to_string(&event) {
                    eprintln!("{value}");
                }
            }
            ProgressOutput::Human => {
                let should_emit = self
                    .human_state
                    .lock()
                    .map(|mut state| should_emit_human_progress(&event, &mut state, Instant::now()))
                    .unwrap_or(true);
                if should_emit {
                    eprintln!("{}", human_progress(&event));
                }
            }
        }
    }
}

fn should_emit_human_progress(
    event: &ProgressEvent,
    state: &mut HumanProgressState,
    now: Instant,
) -> bool {
    const MINIMUM_PERIODIC_INTERVAL: Duration = Duration::from_secs(1);

    let phase_changed = state.phase != Some(event.phase);
    state.phase = Some(event.phase);
    let database_changed =
        event.database_index.is_some() && state.database_index != event.database_index;
    if event.database_index.is_some() {
        state.database_index = event.database_index;
    }

    // A real corpus can contain thousands of tiny hashed message tables. Keep
    // every event in JSON/progress files, but collapse their start/completion
    // chatter in the default console to a periodic cumulative-row update.
    let high_frequency_operation = matches!(
        event.operation.as_str(),
        "inspectTable" | "restoreMessageTable" | "restoreCachedSurfaceTable"
    );
    let milestone = phase_changed
        || database_changed
        || event.state == ProgressState::Planned
        || (!high_frequency_operation && event.state != ProgressState::Advanced);
    let periodic = state
        .last_emitted_at
        .is_none_or(|last| now.saturating_duration_since(last) >= MINIMUM_PERIODIC_INTERVAL);
    let emit = milestone || periodic;
    if emit {
        state.last_emitted_at = Some(now);
    }
    emit
}

fn human_progress(event: &ProgressEvent) -> String {
    let state = match event.state {
        ProgressState::Planned => "plan",
        ProgressState::Started => "start",
        ProgressState::Advanced => "progress",
        ProgressState::Completed => "done",
    };
    let workflow = event
        .workflow_completed
        .zip(event.workflow_total)
        .map_or_else(
            || "n/a".to_string(),
            |(completed, total)| percentage(completed, total, event.state),
        );
    let phase = percentage(event.phase_completed, event.phase_total, event.state);
    let current = percentage(event.completed, event.total, event.state);
    let mut fields = vec![format!(
        "[greenbubbles {state}] {:?} {} — workflow {workflow}, phase {phase}, current {current}",
        event.phase, event.operation
    )];
    if let (Some(index), Some(count)) = (event.workflow_phase_index, event.workflow_phase_count) {
        fields.push(format!("phase {index}/{count}"));
    }
    if let (Some(index), Some(count)) = (event.stage_index, event.stage_count) {
        fields.push(format!("stage {index}/{count}"));
    }
    if let (Some(index), Some(count)) = (event.database_index, event.database_count) {
        fields.push(format!("database {index}/{count}"));
    } else if let Some(count) = event.database_count {
        fields.push(format!("{count} databases"));
    }
    if let (Some(index), Some(count)) = (event.file_index, event.file_count) {
        fields.push(format!("file {index}/{count}"));
    } else if let Some(count) = event.file_count {
        fields.push(format!("{count} files"));
    }
    if let Some(path) = &event.logical_path {
        fields.push(path.clone());
    }
    if let Some(family) = &event.storage_family {
        fields.push(family.clone());
    }
    if let Some(method) = &event.database_key_match_method {
        fields.push(format!("key match {method}"));
    }
    if let Some(state) = &event.database_unlock_state {
        fields.push(format!("unlock {state}"));
    }
    if let Some(count) = event.available_database_count {
        fields.push(format!("{count} available"));
    }
    if let Some(count) = event.unavailable_database_count {
        fields.push(format!("{count} unavailable"));
    }
    if let Some(bytes) = event.database_byte_count {
        let wal = event.write_ahead_log_byte_count.unwrap_or(0);
        fields.push(format!(
            "database {}, WAL {}",
            format_bytes(bytes),
            format_bytes(wal)
        ));
    } else if event.unit == ProgressUnit::Bytes {
        fields.push(format!(
            "{} / {}",
            format_bytes(event.completed),
            format_bytes(event.total)
        ));
    } else {
        let unit = match event.unit {
            ProgressUnit::Records => "records",
            ProgressUnit::Items => "items",
            ProgressUnit::Bytes => unreachable!("byte progress handled above"),
        };
        fields.push(format!("{} / {} {unit}", event.completed, event.total));
    }
    if let (Some(completed), Some(total)) = (event.file_completed_byte_count, event.file_byte_count)
    {
        fields.push(format!(
            "file read {} / {}",
            format_bytes(completed),
            format_bytes(total)
        ));
    } else if let Some(bytes) = event.file_byte_count {
        fields.push(format!("file size {}", format_bytes(bytes)));
    }
    if let Some(bytes) = event.source_byte_count {
        fields.push(format!("source {}", format_bytes(bytes)));
    }
    if let Some(bytes) = event.estimated_archive_byte_count {
        fields.push(format!("estimated archive {}", format_bytes(bytes)));
    }
    if let Some(bytes) = event.estimated_staging_byte_count {
        fields.push(format!("estimated staging {}", format_bytes(bytes)));
    }
    if let Some(bytes) = event.estimated_peak_byte_count {
        fields.push(format!("estimated peak {}", format_bytes(bytes)));
    }
    if let Some(bytes) = event.available_free_byte_count {
        fields.push(format!("free {}", format_bytes(bytes)));
    }
    if let Some(bytes) = event.required_free_byte_count {
        fields.push(format!("required free {}", format_bytes(bytes)));
    }
    if let Some(bytes) = event.staging_file_byte_count {
        fields.push(format!("staging on disk {}", format_bytes(bytes)));
    }
    if let (Some(compressed), Some(uncompressed)) = (
        event.staged_compressed_byte_count,
        event.staged_uncompressed_byte_count,
    ) {
        fields.push(format!(
            "staged payload {} compressed / {} source JSON",
            format_bytes(compressed),
            format_bytes(uncompressed)
        ));
    }
    if let Some(bytes) = event.published_archive_byte_count {
        fields.push(format!("archive written {}", format_bytes(bytes)));
    }
    if let Some(bytes) = event.archive_byte_count {
        fields.push(format!("archive input {}", format_bytes(bytes)));
    }
    if let Some(bytes) = event.replica_file_byte_count {
        fields.push(format!("encrypted replica {}", format_bytes(bytes)));
    }
    if let Some(tables) = event.table_count {
        fields.push(format!("{tables} tables"));
    }
    if let Some(table) = &event.table_name {
        fields.push(format!("table {table}"));
    }
    if let Some(role) = &event.table_role {
        fields.push(format!("role {role}"));
    }
    if let Some(columns) = &event.table_columns {
        fields.push(format!(
            "{} columns [{}]",
            columns.len(),
            columns.join(", ")
        ));
    }
    if let Some(frames) = event.write_ahead_log_frame_count {
        let description = match event.operation.as_str() {
            "scanWriteAheadLog" => "WAL frames scanned",
            "applyWriteAheadLog" | "applyPlaintextWriteAheadLog" => "WAL frames applied",
            _ => "WAL frames",
        };
        fields.push(format!("{frames} {description}"));
    }
    if let Some(records) = event.restored_record_count {
        fields.push(format!("{records} restored"));
    }
    if let Some(records) = event.source_record_count {
        fields.push(format!("{records} source records"));
    }
    if let Some(records) = event.conversation_record_count {
        fields.push(format!("{records} source conversations"));
    }
    if let Some(records) = event.message_record_count {
        fields.push(format!("{records} source messages"));
    }
    if let Some(records) = event.canonical_record_count {
        fields.push(format!("{records} canonical records"));
    }
    if let Some(records) = event.link_record_count {
        fields.push(format!("{records} canonical links"));
    }
    if let Some(records) = event.change_record_count {
        fields.push(format!("{records} change rows"));
    }
    if let Some(records) = event.processed_conversation_count {
        fields.push(format!("{records} conversations processed"));
    }
    if let Some(records) = event.processed_message_count {
        fields.push(format!("{records} messages processed"));
    }
    if let Some(records) = event.emitted_chunk_count {
        fields.push(format!("{records} chunks emitted"));
    }
    if let Some(records) = event.emitted_document_count {
        fields.push(format!("{records} documents emitted"));
    }
    if let Some(bytes) = event.emitted_byte_count {
        fields.push(format!("{} emitted", format_bytes(bytes)));
    }
    if let Some(records) = event.verified_chunk_count {
        fields.push(format!("{records} chunks verified"));
    }
    if let Some(records) = event.verified_document_count {
        fields.push(format!("{records} documents verified"));
    }
    if let Some(bytes) = event.verified_byte_count {
        fields.push(format!("{} verified", format_bytes(bytes)));
    }
    if let Some(records) = event.verified_record_count {
        fields.push(format!("{records} canonical records verified"));
    }
    if let Some(records) = event.verified_link_count {
        fields.push(format!("{records} canonical links verified"));
    }
    if let Some(records) = event.verified_change_count {
        fields.push(format!("{records} change rows verified"));
    }
    if let Some(records) = event.rejected_record_count {
        fields.push(format!("{records} rejected"));
    }
    if let Some(gaps) = event.semantic_gap_count {
        fields.push(format!("{gaps} semantic gaps"));
    }
    if let Some(milliseconds) = event.elapsed_milliseconds {
        fields.push(format!("{:.1}s", milliseconds as f64 / 1_000.0));
    }
    fields.join(" | ")
}

fn percentage(completed: u64, total: u64, state: ProgressState) -> String {
    if total == 0 {
        return if state == ProgressState::Completed {
            "100.0%".to_string()
        } else {
            "0.0%".to_string()
        };
    }
    format!("{:.1}%", completed.min(total) as f64 * 100.0 / total as f64)
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn emit_json_result<T: serde::Serialize>(
    value: &T,
    arguments: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(path) = option_path(arguments, "--summary-file")? {
        if option_path(arguments, "--progress-file")?.as_ref() == Some(&path) {
            return Err("--summary-file and --progress-file must be different paths".into());
        }
        write_owner_only_json(&path, value)?;
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "privacySafeSummary": true,
                "summaryPath": path
            }))?
        );
    } else {
        println!("{}", serde_json::to_string_pretty(value)?);
    }
    Ok(())
}

fn write_owner_only_json<T: serde::Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut writer = owner_only_create_new_writer(path)?;
    serde_json::to_writer_pretty(&mut writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    Ok(())
}

fn owner_only_create_new_writer(path: &Path) -> io::Result<BufWriter<File>> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent_metadata = std::fs::symlink_metadata(parent)?;
    if !parent_metadata.is_dir()
        || parent_metadata.file_type().is_symlink()
        || parent_metadata.uid() != unsafe { libc::geteuid() }
        || parent_metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "report parent must be an owner-only, owner-controlled directory",
        ));
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "report output is not an owner-only regular file",
        ));
    }
    Ok(BufWriter::new(file))
}

fn require_progress_file_outside(
    arguments: &[String],
    protected_roots: &[(&Path, &str)],
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(progress_file) = option_path(arguments, "--progress-file")? else {
        return Ok(());
    };
    let progress_file = resolved_path_for_comparison(&progress_file)?;
    for (root, description) in protected_roots {
        let root = resolved_path_for_comparison(root)?;
        if progress_file == root || progress_file.starts_with(&root) {
            return Err(format!("--progress-file must be outside the {description}").into());
        }
    }
    Ok(())
}

fn require_progress_file_outside_replica_namespace(
    arguments: &[String],
    replica_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(progress_file) = option_path(arguments, "--progress-file")? else {
        return Ok(());
    };
    let progress_file = resolved_path_for_comparison(&progress_file)?;
    let replica = resolved_path_for_comparison(replica_path)?;
    let mut protected = vec![replica.clone()];
    let replica_name = replica
        .file_name()
        .ok_or("replica path has no final component")?
        .to_string_lossy();
    let parent = replica.parent().ok_or("replica path has no parent")?;
    for suffix in ["-wal", "-shm", "-journal"] {
        protected.push(parent.join(format!("{replica_name}{suffix}")));
    }
    if protected.contains(&progress_file) {
        return Err("--progress-file must not overlap the replica storage namespace".into());
    }
    Ok(())
}

fn resolved_path_for_comparison(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()?.join(path)
    };
    if std::fs::symlink_metadata(&absolute).is_ok() {
        return std::fs::canonicalize(absolute);
    }
    let parent = absolute.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "path has no parent directory")
    })?;
    let file_name = absolute.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "path has no final component")
    })?;
    Ok(std::fs::canonicalize(parent)?.join(file_name))
}

fn option_string(arguments: &[String], option: &str) -> Result<Option<String>, String> {
    let Some(index) = arguments.iter().position(|value| value == option) else {
        return Ok(None);
    };
    arguments
        .get(index + 1)
        .filter(|value| !value.starts_with("--"))
        .cloned()
        .map(Some)
        .ok_or_else(|| format!("missing value for {option}"))
}

fn required_option(arguments: &[String], option: &str) -> Result<String, String> {
    option_string(arguments, option)?.ok_or_else(|| format!("missing {option}"))
}

fn required_u64_option(arguments: &[String], option: &str) -> Result<u64, String> {
    required_option(arguments, option)?
        .parse::<u64>()
        .map_err(|_| format!("invalid positive integer for {option}"))
        .and_then(|value| {
            (value > 0)
                .then_some(value)
                .ok_or_else(|| format!("invalid positive integer for {option}"))
        })
}

fn option_usize(arguments: &[String], option: &str) -> Result<Option<usize>, String> {
    option_string(arguments, option)?
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| format!("invalid integer for {option}"))
        })
        .transpose()
}

fn option_u64(arguments: &[String], option: &str) -> Result<Option<u64>, String> {
    option_string(arguments, option)?
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| format!("invalid integer for {option}"))
        })
        .transpose()
}

fn option_i64(arguments: &[String], option: &str) -> Result<Option<i64>, String> {
    option_string(arguments, option)?
        .map(|value| {
            value
                .parse::<i64>()
                .map_err(|_| format!("invalid integer for {option}"))
        })
        .transpose()
}

fn option_path(arguments: &[String], option: &str) -> Result<Option<PathBuf>, String> {
    let Some(index) = arguments.iter().position(|value| value == option) else {
        return Ok(None);
    };
    arguments
        .get(index + 1)
        .filter(|value| !value.starts_with("--"))
        .map(PathBuf::from)
        .map(Some)
        .ok_or_else(|| format!("missing value for {option}"))
}

fn required_path(value: Option<String>, name: &str) -> Result<PathBuf, String> {
    value
        .map(PathBuf::from)
        .ok_or_else(|| format!("missing {name}"))
}

fn parse_capabilities(value: &str) -> Result<BTreeSet<ToolCapability>, String> {
    let mut result = BTreeSet::new();
    for capability in value.split(',') {
        result.insert(match capability {
            "list" => ToolCapability::ListConversations,
            "read" => ToolCapability::ReadRecentMessages,
            "search" => ToolCapability::SearchMessages,
            "draft" => ToolCapability::CreateDraft,
            _ => return Err(format!("unsupported tool capability: {capability}")),
        });
    }
    if result.is_empty() {
        return Err("at least one tool capability is required".to_string());
    }
    Ok(result)
}

fn parse_destination(value: Option<String>) -> Result<ToolDataDestination, String> {
    match value.as_deref().unwrap_or("local") {
        "local" => Ok(ToolDataDestination::LocalModel),
        "remote" => Ok(ToolDataDestination::RemoteModel),
        value => Err(format!("unsupported data destination: {value}")),
    }
}

fn parse_connector_destination(value: Option<String>) -> Result<ConnectorDestination, String> {
    match value.as_deref().unwrap_or("local") {
        "local" => Ok(ConnectorDestination::Local),
        "remote" => Ok(ConnectorDestination::RemoteModel),
        value => Err(format!("unsupported connector destination: {value}")),
    }
}

fn parse_message_fields(value: &str) -> Result<BTreeSet<ToolMessageField>, String> {
    let mut result = BTreeSet::new();
    for field in value.split(',') {
        result.insert(match field {
            "sender" => ToolMessageField::Sender,
            "created-at" => ToolMessageField::CreatedAt,
            "direction" => ToolMessageField::Direction,
            "type" => ToolMessageField::MessageType,
            "content" => ToolMessageField::Content,
            "attachments" => ToolMessageField::Attachments,
            "relationships" => ToolMessageField::Relationships,
            _ => return Err(format!("unsupported message field: {field}")),
        });
    }
    Ok(result)
}

fn parse_cached_moment_fields(value: &str) -> Result<BTreeSet<CachedMomentField>, String> {
    let mut result = BTreeSet::new();
    for field in value.split(',') {
        result.insert(match field {
            "author" => CachedMomentField::Author,
            "created-at" => CachedMomentField::CreatedAt,
            "type" => CachedMomentField::ContentType,
            "content" => CachedMomentField::ContentDescription,
            "title" => CachedMomentField::Title,
            "description" => CachedMomentField::Description,
            "url" => CachedMomentField::ContentUrl,
            "media-count" => CachedMomentField::MediaCount,
            "like-count" => CachedMomentField::LikeCount,
            "comment-count" => CachedMomentField::CommentCount,
            _ => return Err(format!("unsupported cached Moment field: {field}")),
        });
    }
    if result.is_empty() {
        return Err("at least one cached Moment field is required".to_string());
    }
    Ok(result)
}

fn read_utf8_stdin_limited(
    maximum_bytes: u64,
) -> Result<Zeroizing<String>, Box<dyn std::error::Error>> {
    let mut bytes = Zeroizing::new(Vec::new());
    io::stdin()
        .lock()
        .take(maximum_bytes + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum_bytes {
        return Err(format!("standard input exceeds {maximum_bytes} bytes").into());
    }
    Ok(Zeroizing::new(String::from_utf8(bytes.to_vec())?))
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct HandoffPollMarker {
    device: u64,
    inode: u64,
    byte_count: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

fn handoff_poll_marker(
    path: &std::path::Path,
) -> Result<Option<HandoffPollMarker>, Box<dyn std::error::Error>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err("replica handoff hint is not a regular file".into());
            }
            Ok(Some(HandoffPollMarker {
                device: metadata.dev(),
                inode: metadata.ino(),
                byte_count: metadata.len(),
                modified_seconds: metadata.mtime(),
                modified_nanoseconds: metadata.mtime_nsec(),
            }))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_progress_distinguishes_scanned_and_applied_wal_frames() {
        let mut event = ProgressEvent::new(
            ProgressPhase::DatabasePreparation,
            ProgressState::Completed,
            "scanWriteAheadLog",
            ProgressUnit::Bytes,
            32,
            32,
            32,
            64,
        );
        event.write_ahead_log_frame_count = Some(9);
        assert!(human_progress(&event).contains("9 WAL frames scanned"));

        event.operation = "applyWriteAheadLog".to_string();
        event.write_ahead_log_frame_count = Some(0);
        assert!(human_progress(&event).contains("0 WAL frames applied"));
    }

    #[test]
    fn human_progress_throttles_tiny_tables_but_keeps_database_milestones() {
        let now = Instant::now();
        let mut state = HumanProgressState::default();
        let mut event = ProgressEvent::new(
            ProgressPhase::RecordRestoration,
            ProgressState::Started,
            "restoreMessageTable",
            ProgressUnit::Records,
            0,
            1,
            0,
            10,
        );
        event.database_index = Some(1);
        event.database_count = Some(2);
        assert!(should_emit_human_progress(&event, &mut state, now));

        event.state = ProgressState::Completed;
        event.completed = 1;
        event.phase_completed = 1;
        assert!(!should_emit_human_progress(
            &event,
            &mut state,
            now + Duration::from_millis(10)
        ));
        assert!(should_emit_human_progress(
            &event,
            &mut state,
            now + Duration::from_secs(1)
        ));

        event.state = ProgressState::Started;
        event.completed = 0;
        event.database_index = Some(2);
        assert!(should_emit_human_progress(
            &event,
            &mut state,
            now + Duration::from_millis(1_010)
        ));
    }
}
