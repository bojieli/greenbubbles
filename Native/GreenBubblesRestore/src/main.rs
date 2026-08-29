#![recursion_limit = "256"]

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};
use std::time::{Duration, Instant};

use greenbubbles_restore::{
    acquisition_audit::audit_acquisition_chain,
    action::ExternalApprovalEvidence,
    ai_context::{
        audit_ai_context_with_progress, export_ai_context, load_ai_query_request, query_ai_context,
    },
    ai_memory::{
        audit_ai_memory_with_progress, export_ai_memory_with_progress, AiMemoryExportOptions,
    },
    archive::{create_conversation_policy, read_conversation_page},
    audit::audit_archive_with_progress,
    benchmark::{run_synthetic_benchmark, SyntheticBenchmarkConfig},
    connector::load_action_draft,
    connector::{audit_connector_log, ConnectorDestination, ConnectorService},
    diagnostic::{profile_archive_payloads_with_progress, profile_archive_schema_with_progress},
    direct_connector::DirectConnectorService,
    follow::{
        follow_replica_once, publish_replica_handoff, quarantine_retired_replica_archives,
        replica_follower_status, restore_quarantined_replica_archive,
    },
    latency::{compose_latency_evidence_sample, summarize_latency_evidence_samples},
    live_attachment::{
        inspect_image_attachment, inspect_message_attachment, materialize_image_attachment,
        materialize_message_attachment, serialize_attachment_error, AttachmentKind,
        LiveAttachmentError,
    },
    live_query::{
        find_conversations as find_live_conversations, get_message as get_live_message,
        get_search_result_message as get_live_search_result_message,
        list_conversations as list_live_conversations, list_messages as list_live_messages,
        search_messages as search_live_messages, serialize_query_error, serialize_query_response,
        source_status as live_source_status, LiveQueryError, LiveQuerySource, QueryDatabaseAccess,
        DEFAULT_PAGE_LIMIT, DEFAULT_SEARCH_LIMIT, MAX_PAGE_LIMIT, MAX_SEARCH_QUERY_BYTES,
    },
    merge::merge_incremental_archive,
    operator::{restore_snapshot_and_publish_with_progress, OfflineRestorePublishOptions},
    preflight_snapshot_with_progress, prepare_available_catalog_with_progress,
    prepare_catalog_batch_with_progress, prepare_catalog_with_progress,
    query_profile::{
        default_query_profile_path, read_private_32_byte_credential,
        read_private_snapshot_passphrase, QueryProfile, QueryProfileAccess, QueryProfileError,
        QueryProfileStore, QUERY_PROFILE_FORMAT_VERSION, QUERY_PROFILE_SCHEMA,
    },
    reconcile::reconcile_archives,
    recoverable_snapshot::{
        create_recoverable_snapshot, create_recoverable_snapshot_from_stable_capture,
        create_recoverable_snapshot_from_stable_capture_with_recovery_words_and_optional_protectors,
        create_recoverable_snapshot_with_recovery_words_and_optional_protectors,
        quarantine_recoverable_snapshot_generation, recoverable_snapshot_data_root,
        rekey_recoverable_snapshot, restore_quarantined_snapshot_generation,
        rewrap_recoverable_snapshot_protectors_with_optional_protectors,
        unlock_recoverable_snapshot_with_local_credential,
        unlock_recoverable_snapshot_with_passphrase,
        unlock_recoverable_snapshot_with_recovery_words, verify_recoverable_snapshot,
        verify_recoverable_snapshot_with_local_credential,
        verify_recoverable_snapshot_with_passphrase,
        verify_recoverable_snapshot_with_recovery_words, RecoverableSnapshotError,
    },
    replica::{
        audit_replica_backup_with_progress, audit_replica_with_progress,
        bootstrap_replica_with_progress, get_replica_changes, get_replica_message,
        list_replica_conversations, load_replica_message_filter, prepare_replica_recovery,
        replica_coverage, replica_status, search_replica_cached_moments, search_replica_messages,
        synchronize_replica_with_progress, ReplicaCachedMomentFilter,
    },
    restore_catalog_with_progress,
    send_adapter::{
        observe_send_in_replica, unix_nanoseconds as adapter_unix_nanoseconds,
        ProcessSendDispatcher, SendAdapter, SendDispatcher, SEND_ADAPTER_ID, SEND_ADAPTER_VERSION,
    },
    send_profile::{
        load_calibration_profile, load_compatibility_matrix, sign_calibration_profile,
        sign_compatibility_matrix, signing_key_public_hex, CalibrationProfileBody,
        CompatibilityMatrixBody, SendTrustRoot,
    },
    snapshot_protector::{SnapshotLocalCredential, SnapshotPassphrase, SnapshotRecoveryWords},
    tools::{
        create_all_conversations_tool_policy_with_cached_moments, create_direct_tool_policy,
        create_tool_policy_with_cached_moments, CachedMomentField, CachedMomentsToolScope,
        ConversationToolScope, LocalToolService, ToolCapability, ToolDataDestination,
        ToolMessageField,
    },
    transport::{load_connector_request, send_unix_request, serve_unix},
    DatabaseKeySet, DatabasePassphrase, DatabaseUnlockMaterial, ProgressEvent, ProgressObserver,
    ProgressPhase, ProgressState, ProgressUnit, ReplicaKey, RestorationOptions, RestoreError,
    SnapshotKey,
};
use zeroize::Zeroizing;

fn main() {
    let query_operation = process_query_operation();
    let attachment_operation = process_attachment_operation();
    if let Err(error) = run() {
        if let Some(operation) = query_operation {
            let (code, message, retryable) = query_error_details(error.as_ref());
            println!(
                "{}",
                serialize_query_error(operation, code, message, retryable)
            );
            eprintln!("error: bounded query failed; see the JSON error on standard output");
        } else if let Some(operation) = attachment_operation {
            let (code, message, retryable) = attachment_error_details(error.as_ref());
            println!(
                "{}",
                serialize_attachment_error(operation, code, message, retryable)
            );
            eprintln!(
                "error: bounded attachment request failed; see the JSON error on standard output"
            );
        } else {
            eprintln!("error: {error}");
        }
        std::process::exit(2);
    }
}

fn process_attachment_operation() -> Option<&'static str> {
    let arguments = env::args().skip(1).take(2).collect::<Vec<_>>();
    match arguments.as_slice() {
        [command, subcommand] if command == "attachment" && subcommand == "inspect" => {
            Some("attachment.inspect")
        }
        [command, subcommand] if command == "attachment" && subcommand == "materialize" => {
            Some("attachment.materialize")
        }
        _ => None,
    }
}

fn process_query_operation() -> Option<&'static str> {
    let arguments = env::args().skip(1).take(2).collect::<Vec<_>>();
    match arguments.as_slice() {
        [command, subcommand] if command == "conversations" && subcommand == "list" => {
            Some("conversations.list")
        }
        [command, subcommand] if command == "source" && subcommand == "status" => {
            Some("source.status")
        }
        [command, subcommand] if command == "messages" && subcommand == "list" => {
            Some("messages.list")
        }
        [command, subcommand] if command == "messages" && subcommand == "search" => {
            Some("messages.search")
        }
        [command, subcommand] if command == "message" && subcommand == "get" => Some("message.get"),
        _ => None,
    }
}

fn query_error_details(
    error: &(dyn std::error::Error + 'static),
) -> (&'static str, &'static str, bool) {
    if let Some(error) = error.downcast_ref::<LiveQueryError>() {
        return match error {
            LiveQueryError::InvalidArgument(_) => (
                "invalidQuery",
                "The bounded query arguments are invalid.",
                false,
            ),
            LiveQueryError::UnsafeSource(_) => (
                "unsafeSource",
                "The selected database source failed path or ownership validation.",
                false,
            ),
            LiveQueryError::Database(_) => (
                "databaseUnavailable",
                "The database could not complete the bounded read-only operation with the supplied access material.",
                true,
            ),
            LiveQueryError::InvalidCursor(_) => (
                "invalidCursor",
                "The cursor is invalid or does not belong to this operation and source.",
                false,
            ),
            LiveQueryError::NotFound(_) => (
                "messageNotFound",
                "The selected message is no longer available from this source.",
                false,
            ),
            LiveQueryError::SearchUnavailable(_) => (
                "searchUnavailable",
                "No compatible bounded search path is available for this source.",
                false,
            ),
            LiveQueryError::ResponseTooLarge { .. } => (
                "responseTooLarge",
                "The projected response exceeded the fixed serialization limit.",
                false,
            ),
        };
    }
    if matches!(
        error.downcast_ref::<RestoreError>(),
        Some(
            RestoreError::InvalidPassphrase
                | RestoreError::InvalidSnapshotKey
                | RestoreError::InvalidReplicaKey
        )
    ) {
        return (
            "invalidAccessMaterial",
            "The supplied key material is not a valid bounded secret input.",
            false,
        );
    }
    if error.downcast_ref::<RecoverableSnapshotError>().is_some() {
        return (
            "invalidSnapshot",
            "The selected recoverable snapshot failed manifest or storage validation.",
            false,
        );
    }
    if error.downcast_ref::<QueryProfileError>().is_some() {
        return (
            "invalidProfile",
            "The selected local query profile could not be loaded or validated.",
            false,
        );
    }
    (
        "invalidRequest",
        "The query invocation is invalid or incomplete.",
        false,
    )
}

fn attachment_error_details(
    error: &(dyn std::error::Error + 'static),
) -> (&'static str, &'static str, bool) {
    if let Some(error) = error.downcast_ref::<LiveAttachmentError>() {
        return match error {
            LiveAttachmentError::InvalidArgument(_) => (
                "invalidAttachmentRequest",
                "The bounded attachment arguments are invalid.",
                false,
            ),
            LiveAttachmentError::UnsafeSource(_) => (
                "unsafeSource",
                "The selected attachment source failed path or ownership validation.",
                false,
            ),
            LiveAttachmentError::Unavailable(_) => (
                "attachmentUnavailable",
                "The requested attachment candidate is not currently available.",
                true,
            ),
            LiveAttachmentError::SourceChanged => (
                "sourceChanged",
                "The attachment source changed during the bounded read.",
                true,
            ),
            LiveAttachmentError::Decode(_) => (
                "decodeFailed",
                "The selected attachment could not be decoded safely.",
                false,
            ),
            LiveAttachmentError::Output(_) => (
                "outputRejected",
                "The decoded attachment output could not be published safely.",
                false,
            ),
            LiveAttachmentError::Io(_) => (
                "ioUnavailable",
                "The bounded attachment operation could not complete its file access.",
                true,
            ),
        };
    }
    (
        "invalidAttachmentRequest",
        "The bounded attachment invocation is invalid or incomplete.",
        false,
    )
}

fn run_query_profile_command(arguments: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let subcommand = arguments.first().map(String::as_str).unwrap_or("help");
    if subcommand == "help"
        || subcommand == "--help"
        || subcommand == "-h"
        || arguments
            .iter()
            .skip(1)
            .any(|value| matches!(value.as_str(), "--help" | "-h"))
    {
        println!("{}", query_profile_command_help());
        return Ok(());
    }
    match subcommand {
        "path" => {
            require_exact_argument_count(&arguments, 1, "profile path takes no arguments")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "schema": QUERY_PROFILE_SCHEMA,
                    "formatVersion": QUERY_PROFILE_FORMAT_VERSION,
                    "configurationFile": default_query_profile_path()?,
                }))?
            );
        }
        "template" => {
            require_exact_argument_count(&arguments, 1, "profile template takes no arguments")?;
            println!("{}", query_profile_template()?);
        }
        "list" => {
            require_exact_argument_count(&arguments, 1, "profile list takes no arguments")?;
            let (configuration_file, store) = QueryProfileStore::load_default()?;
            let profiles = store
                .profiles
                .iter()
                .map(|(name, profile)| {
                    serde_json::json!({
                        "name": name,
                        "default": store.default_profile.as_deref() == Some(name.as_str()),
                        "sourceRoot": profile.source_root,
                        "accessMode": profile.access.mode_name(),
                    })
                })
                .collect::<Vec<_>>();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "schema": QUERY_PROFILE_SCHEMA,
                    "formatVersion": QUERY_PROFILE_FORMAT_VERSION,
                    "configurationFile": configuration_file,
                    "defaultProfile": store.default_profile,
                    "profiles": profiles,
                }))?
            );
        }
        "show" => {
            require_exact_argument_count(&arguments, 2, "profile show requires one name")?;
            let (configuration_file, store) = QueryProfileStore::load_default()?;
            let (name, profile) = store.select(Some(&arguments[1]))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "schema": QUERY_PROFILE_SCHEMA,
                    "formatVersion": QUERY_PROFILE_FORMAT_VERSION,
                    "configurationFile": configuration_file,
                    "name": name,
                    "default": store.default_profile.as_deref() == Some(name.as_str()),
                    "sourceRoot": profile.source_root,
                    "access": profile.access,
                }))?
            );
        }
        "validate" => {
            if arguments.len() > 2 {
                return Err("profile validate accepts at most one name".into());
            }
            let requested = arguments.get(1).map(String::as_str);
            let (configuration_file, invocation) =
                load_configured_query_invocation(requested)?;
            let source = invocation.access.open_source(&invocation.source_root)?;
            let status = live_source_status(&source)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "schema": "greenbubbles.query-profile-validation.v1",
                    "formatVersion": 1,
                    "ok": true,
                    "configurationFile": configuration_file,
                    "profile": invocation.profile_name,
                    "sourceMode": status.source.mode,
                    "databaseCount": status.database_count,
                    "totalSqliteStorageBytes": status.total_sqlite_storage_bytes,
                }))?
            );
        }
        "set-default" => {
            require_exact_argument_count(
                &arguments,
                2,
                "profile set-default requires one name",
            )?;
            let (configuration_file, mut store) = QueryProfileStore::load_default()?;
            store.set_default(&arguments[1])?;
            store.replace_private_file(&configuration_file)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "schema": QUERY_PROFILE_SCHEMA,
                    "formatVersion": QUERY_PROFILE_FORMAT_VERSION,
                    "configurationFile": configuration_file,
                    "defaultProfile": arguments[1],
                    "updated": true,
                }))?
            );
        }
        _ => {
            return Err(format!(
                "unsupported profile subcommand: {subcommand}; expected path, template, list, show, validate, or set-default"
            )
            .into())
        }
    }
    Ok(())
}

fn require_exact_argument_count(
    arguments: &[String],
    expected: usize,
    message: &str,
) -> Result<(), String> {
    if arguments.len() == expected {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn query_profile_template() -> Result<String, Box<dyn std::error::Error>> {
    let home = env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/Users/you"));
    let credential_directory = home.join(".greenbubbles/credentials");
    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "schema": QUERY_PROFILE_SCHEMA,
        "formatVersion": QUERY_PROFILE_FORMAT_VERSION,
        "defaultProfile": "live",
        "profiles": {
            "live": {
                "sourceRoot": "/ABSOLUTE/PATH/TO/WECHAT/db_storage",
                "access": {
                    "mode": "liveWeChatKeyFile",
                    "credentialFile": credential_directory.join("wechat-database-key")
                }
            },
            "archive": {
                "sourceRoot": "/ABSOLUTE/PATH/TO/RECOVERABLE-SNAPSHOT",
                "access": {
                    "mode": "snapshotLocalCredential",
                    "credentialFile": credential_directory.join("snapshot-local-credential")
                }
            }
        }
    }))?)
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
        "profile" => {
            run_query_profile_command(arguments.collect::<Vec<_>>())?;
        }
        "source" => {
            let subcommand = arguments
                .next()
                .ok_or("missing source subcommand; expected 'status'")?;
            if subcommand != "status" {
                return Err(format!(
                    "unsupported source subcommand: {subcommand}; expected 'status'"
                )
                .into());
            }
            if matches!(arguments.peek().map(String::as_str), Some("--help" | "-h")) {
                println!("{}", source_status_help());
                return Ok(());
            }
            let (database_root, remaining) =
                split_optional_query_source(arguments.collect::<Vec<_>>());
            if remaining
                .iter()
                .any(|value| matches!(value.as_str(), "--help" | "-h"))
            {
                println!("{}", source_status_help());
                return Ok(());
            }
            validate_command_options(
                &remaining,
                &[
                    "--profile",
                    "--snapshot-recovery-kit",
                    "--snapshot-local-credential",
                ],
                &[
                    "--passphrase-stdin",
                    "--snapshot-key-stdin",
                    "--snapshot-passphrase-stdin",
                    "--decrypted",
                ],
            )?;
            let invocation = resolve_query_invocation(database_root, &remaining)?;
            let source = invocation.access.open_source(&invocation.source_root)?;
            let response = live_source_status(&source)?;
            println!("{}", serialize_query_response(&response)?);
        }
        "conversations" => {
            let subcommand = arguments
                .next()
                .ok_or("missing conversations subcommand; expected 'list'")?;
            if subcommand != "list" {
                return Err(format!(
                    "unsupported conversations subcommand: {subcommand}; expected 'list'"
                )
                .into());
            }
            if matches!(arguments.peek().map(String::as_str), Some("--help" | "-h")) {
                println!("{}", conversations_command_help());
                return Ok(());
            }
            let (database_root, remaining) =
                split_optional_query_source(arguments.collect::<Vec<_>>());
            if remaining
                .iter()
                .any(|value| matches!(value.as_str(), "--help" | "-h"))
            {
                println!("{}", conversations_command_help());
                return Ok(());
            }
            validate_command_options(
                &remaining,
                &[
                    "--profile",
                    "--limit",
                    "--cursor",
                    "--snapshot-recovery-kit",
                    "--snapshot-local-credential",
                ],
                &[
                    "--passphrase-stdin",
                    "--snapshot-key-stdin",
                    "--snapshot-passphrase-stdin",
                    "--decrypted",
                ],
            )?;
            let invocation = resolve_query_invocation(database_root, &remaining)?;
            let source = invocation.access.open_source(&invocation.source_root)?;
            let limit = option_usize(&remaining, "--limit")?.unwrap_or(DEFAULT_PAGE_LIMIT);
            let cursor = option_string(&remaining, "--cursor")?;
            let response = list_live_conversations(&source, limit, cursor.as_deref())?;
            println!("{}", serialize_query_response(&response)?);
        }
        "messages" => {
            let subcommand = arguments
                .next()
                .ok_or("missing messages subcommand; expected 'list' or 'search'")?;
            if matches!(arguments.peek().map(String::as_str), Some("--help" | "-h")) {
                println!("{}", messages_subcommand_help(&subcommand)?);
                return Ok(());
            }
            let (database_root, remaining) =
                split_optional_query_source(arguments.collect::<Vec<_>>());
            if remaining
                .iter()
                .any(|value| matches!(value.as_str(), "--help" | "-h"))
            {
                println!("{}", messages_subcommand_help(&subcommand)?);
                return Ok(());
            }
            match subcommand.as_str() {
                "list" => {
                    validate_command_options(
                        &remaining,
                        &[
                            "--profile",
                            "--conversation",
                            "--limit",
                            "--cursor",
                            "--snapshot-recovery-kit",
                            "--snapshot-local-credential",
                        ],
                        &[
                            "--passphrase-stdin",
                            "--snapshot-key-stdin",
                            "--snapshot-passphrase-stdin",
                            "--decrypted",
                        ],
                    )?;
                    let invocation = resolve_query_invocation(database_root, &remaining)?;
                    let conversation = required_option(&remaining, "--conversation")?;
                    let source = invocation.access.open_source(&invocation.source_root)?;
                    let limit = option_usize(&remaining, "--limit")?.unwrap_or(DEFAULT_PAGE_LIMIT);
                    let cursor = option_string(&remaining, "--cursor")?;
                    let response =
                        list_live_messages(&source, &conversation, limit, cursor.as_deref())?;
                    println!("{}", serialize_query_response(&response)?);
                }
                "search" => {
                    validate_command_options(
                        &remaining,
                        &[
                            "--profile",
                            "--conversation",
                            "--limit",
                            "--cursor",
                            "--snapshot-recovery-kit",
                            "--snapshot-local-credential",
                        ],
                        &[
                            "--passphrase-stdin",
                            "--snapshot-key-stdin",
                            "--snapshot-passphrase-stdin",
                            "--decrypted",
                            "--query-stdin",
                        ],
                    )?;
                    if !remaining.iter().any(|value| value == "--query-stdin") {
                        return Err("message search requires --query-stdin".into());
                    }
                    let invocation = resolve_query_invocation(database_root, &remaining)?;
                    let mut query = read_utf8_stdin_limited(MAX_SEARCH_QUERY_BYTES as u64)?;
                    while query.ends_with(['\n', '\r']) {
                        query.pop();
                    }
                    let source = invocation.access.open_source(&invocation.source_root)?;
                    let conversation = option_string(&remaining, "--conversation")?;
                    let limit =
                        option_usize(&remaining, "--limit")?.unwrap_or(DEFAULT_SEARCH_LIMIT);
                    let cursor = option_string(&remaining, "--cursor")?;
                    let response = search_live_messages(
                        &source,
                        &query,
                        conversation.as_deref(),
                        limit,
                        cursor.as_deref(),
                    )?;
                    println!("{}", serialize_query_response(&response)?);
                }
                _ => {
                    return Err(format!(
                        "unsupported messages subcommand: {subcommand}; expected 'list' or 'search'"
                    )
                    .into())
                }
            }
        }
        "message" => {
            let subcommand = arguments
                .next()
                .ok_or("missing message subcommand; expected 'get'")?;
            if subcommand != "get" {
                return Err(format!(
                    "unsupported message subcommand: {subcommand}; expected 'get'"
                )
                .into());
            }
            if matches!(arguments.peek().map(String::as_str), Some("--help" | "-h")) {
                println!("{}", message_get_help());
                return Ok(());
            }
            let (database_root, remaining) =
                split_optional_query_source(arguments.collect::<Vec<_>>());
            if remaining
                .iter()
                .any(|value| matches!(value.as_str(), "--help" | "-h"))
            {
                println!("{}", message_get_help());
                return Ok(());
            }
            validate_command_options(
                &remaining,
                &[
                    "--profile",
                    "--conversation",
                    "--message",
                    "--snapshot-recovery-kit",
                    "--snapshot-local-credential",
                ],
                &[
                    "--passphrase-stdin",
                    "--snapshot-key-stdin",
                    "--snapshot-passphrase-stdin",
                    "--decrypted",
                ],
            )?;
            let invocation = resolve_query_invocation(database_root, &remaining)?;
            let conversation = required_option(&remaining, "--conversation")?;
            let message_id = required_option(&remaining, "--message")?;
            let source = invocation.access.open_source(&invocation.source_root)?;
            let response = match get_live_message(&source, &conversation, &message_id) {
                Ok(response) => response,
                Err(LiveQueryError::InvalidCursor(_)) => {
                    get_live_search_result_message(&source, &conversation, &message_id)?
                }
                Err(error) => return Err(error.into()),
            };
            println!("{}", serialize_query_response(&response)?);
        }
        "attachment" => {
            let subcommand = arguments
                .next()
                .ok_or("missing attachment subcommand; expected 'inspect' or 'materialize'")?;
            if matches!(arguments.peek().map(String::as_str), Some("--help" | "-h")) {
                println!("{}", attachment_subcommand_help(&subcommand)?);
                return Ok(());
            }
            let account_root =
                required_path(arguments.next(), "WeChat account or snapshot source root")?;
            let remaining = arguments.collect::<Vec<_>>();
            if remaining
                .iter()
                .any(|value| matches!(value.as_str(), "--help" | "-h"))
            {
                println!("{}", attachment_subcommand_help(&subcommand)?);
                return Ok(());
            }
            match subcommand.as_str() {
                "inspect" => {
                    validate_command_options(
                        &remaining,
                        &[
                            "--conversation",
                            "--md5",
                            "--message",
                            "--kind",
                            "--snapshot-recovery-kit",
                            "--snapshot-local-credential",
                        ],
                        &[
                            "--passphrase-stdin",
                            "--snapshot-key-stdin",
                            "--snapshot-passphrase-stdin",
                            "--decrypted",
                        ],
                    )?;
                    let conversation = required_option(&remaining, "--conversation")?;
                    let kind = option_string(&remaining, "--kind")?
                        .map(|value| value.parse::<AttachmentKind>())
                        .transpose()?
                        .unwrap_or(AttachmentKind::Image);
                    let message_id = option_string(&remaining, "--message")?;
                    let response = if let Some(message_id) = message_id {
                        if option_string(&remaining, "--md5")?.is_some() {
                            return Err(
                                "--message derives its locator from the exact source row; do not also pass --md5"
                                    .into(),
                            );
                        }
                        let access = load_live_query_access(&account_root, &remaining)?;
                        let source = access.open_attachment_source(&account_root)?;
                        let filesystem_root = attachment_account_root(&account_root);
                        inspect_message_attachment(
                            filesystem_root.as_deref(),
                            &source,
                            &conversation,
                            &message_id,
                            kind,
                        )?
                    } else {
                        if kind != AttachmentKind::Image {
                            return Err(
                                "voice, video, and document inspection requires --message"
                                    .into(),
                            );
                        }
                        reject_database_access_options(&remaining)?;
                        let source_md5 = required_option(&remaining, "--md5")?;
                        inspect_image_attachment(&account_root, &conversation, &source_md5)?
                    };
                    println!("{}", serde_json::to_string_pretty(&response)?);
                }
                "materialize" => {
                    validate_command_options(
                        &remaining,
                        &[
                            "--conversation",
                            "--md5",
                            "--message",
                            "--kind",
                            "--attachment",
                            "--output",
                            "--snapshot-recovery-kit",
                            "--snapshot-local-credential",
                        ],
                        &[
                            "--passphrase-stdin",
                            "--snapshot-key-stdin",
                            "--snapshot-passphrase-stdin",
                            "--decrypted",
                        ],
                    )?;
                    let conversation = required_option(&remaining, "--conversation")?;
                    let kind = option_string(&remaining, "--kind")?
                        .map(|value| value.parse::<AttachmentKind>())
                        .transpose()?
                        .unwrap_or(AttachmentKind::Image);
                    let message_id = option_string(&remaining, "--message")?;
                    let attachment_id = required_option(&remaining, "--attachment")?;
                    let output = PathBuf::from(required_option(&remaining, "--output")?);
                    let response = if let Some(message_id) = message_id {
                        if option_string(&remaining, "--md5")?.is_some() {
                            return Err(
                                "--message derives its locator from the exact source row; do not also pass --md5"
                                    .into(),
                            );
                        }
                        let access = load_live_query_access(&account_root, &remaining)?;
                        let source = access.open_attachment_source(&account_root)?;
                        let filesystem_root = attachment_account_root(&account_root);
                        materialize_message_attachment(
                            filesystem_root.as_deref(),
                            &source,
                            &conversation,
                            &message_id,
                            kind,
                            &attachment_id,
                            &output,
                        )?
                    } else {
                        if kind != AttachmentKind::Image {
                            return Err(
                                "voice, video, and document materialization requires --message"
                                    .into(),
                            );
                        }
                        reject_database_access_options(&remaining)?;
                        let source_md5 = required_option(&remaining, "--md5")?;
                        materialize_image_attachment(
                            &account_root,
                            &conversation,
                            &source_md5,
                            &attachment_id,
                            &output,
                        )?
                    };
                    println!("{}", serde_json::to_string_pretty(&response)?);
                }
                _ => {
                    return Err(format!(
                        "unsupported attachment subcommand: {subcommand}; expected 'inspect' or 'materialize'"
                    )
                    .into())
                }
            }
        }
        "snapshot" => {
            let subcommand = arguments
                .next()
                .ok_or(
                    "missing snapshot subcommand; expected 'recovery-kit', 'local-credential', 'create', 'create-capture', 'verify', 'rewrap', 'retention', or 'rekey'",
                )?;
            if matches!(arguments.peek().map(String::as_str), Some("--help" | "-h")) {
                println!("{}", snapshot_subcommand_help(&subcommand)?);
                return Ok(());
            }
            match subcommand.as_str() {
                "local-credential" => {
                    let action = arguments.next().ok_or(
                        "missing local-credential action; expected 'create' or 'validate'",
                    )?;
                    if matches!(arguments.peek().map(String::as_str), Some("--help" | "-h")) {
                        println!("{}", snapshot_local_credential_help());
                        return Ok(());
                    }
                    let path = required_path(arguments.next(), "private local-credential file")?;
                    let remaining = arguments.collect::<Vec<_>>();
                    validate_command_options(&remaining, &[], &[])?;
                    let report = match action.as_str() {
                        "create" => SnapshotLocalCredential::write_new_private_file(&path)?,
                        "validate" => SnapshotLocalCredential::validate_private_file(&path)?,
                        _ => {
                            return Err(format!(
                                "unsupported local-credential action: {action}; expected 'create' or 'validate'"
                            )
                            .into())
                        }
                    };
                    println!("{}", serde_json::to_string_pretty(&report)?);
                }
                "recovery-kit" => {
                    let action = arguments
                        .next()
                        .ok_or("missing recovery-kit action; expected 'create' or 'validate'")?;
                    if matches!(arguments.peek().map(String::as_str), Some("--help" | "-h")) {
                        println!("{}", snapshot_recovery_kit_help());
                        return Ok(());
                    }
                    let path = required_path(arguments.next(), "private recovery-kit file")?;
                    let remaining = arguments.collect::<Vec<_>>();
                    validate_command_options(&remaining, &[], &[])?;
                    let report = match action.as_str() {
                        "create" => SnapshotRecoveryWords::write_new_private_file(&path)?,
                        "validate" => SnapshotRecoveryWords::validate_private_file(&path)?,
                        _ => {
                            return Err(format!(
                                "unsupported recovery-kit action: {action}; expected 'create' or 'validate'"
                            )
                            .into())
                        }
                    };
                    println!("{}", serde_json::to_string_pretty(&report)?);
                }
                "create" => {
                    let source_root = required_path(arguments.next(), "WeChat database root")?;
                    let output = required_path(arguments.next(), "new snapshot directory")?;
                    let remaining = arguments.collect::<Vec<_>>();
                    if remaining
                        .iter()
                        .any(|value| matches!(value.as_str(), "--help" | "-h"))
                    {
                        println!("{}", snapshot_create_help());
                        return Ok(());
                    }
                    validate_command_options(
                        &remaining,
                        &[
                            "--snapshot-recovery-kit",
                            "--snapshot-local-credential",
                        ],
                        &[
                            "--source-passphrase-stdin",
                            "--source-decrypted",
                            "--snapshot-key-stdin",
                            "--snapshot-passphrase-stdin",
                        ],
                    )?;
                    let raw_key = remaining
                        .iter()
                        .any(|value| value == "--snapshot-key-stdin");
                    let recovery_kit = option_path(&remaining, "--snapshot-recovery-kit")?;
                    let local_credential =
                        option_path(&remaining, "--snapshot-local-credential")?;
                    let passphrase = remaining
                        .iter()
                        .any(|value| value == "--snapshot-passphrase-stdin");
                    if usize::from(raw_key) + usize::from(recovery_kit.is_some()) != 1 {
                        return Err(
                            "snapshot create requires exactly one of --snapshot-key-stdin or --snapshot-recovery-kit"
                                .into(),
                        );
                    }
                    if (local_credential.is_some() || passphrase) && recovery_kit.is_none() {
                        return Err(
                            "optional local or passphrase protection requires --snapshot-recovery-kit so 24-word recovery is retained"
                                .into(),
                        );
                    }
                    let source_access = load_snapshot_create_source_access(&remaining)?;
                    let manifest = if let Some(recovery_kit) = recovery_kit {
                        let recovery_words =
                            SnapshotRecoveryWords::read_private_file(&recovery_kit)?;
                        let local_credential = local_credential
                            .as_ref()
                            .map(|path| SnapshotLocalCredential::read_private_file(path))
                            .transpose()?;
                        let passphrase = passphrase
                            .then(SnapshotPassphrase::read_stdin)
                            .transpose()?;
                        create_recoverable_snapshot_with_recovery_words_and_optional_protectors(
                            &source_root,
                            source_access.material(),
                            &output,
                            &recovery_words,
                            local_credential.as_ref(),
                            passphrase.as_ref(),
                        )?
                    } else {
                        let snapshot_key = SnapshotKey::read_stdin()?;
                        create_recoverable_snapshot(
                            &source_root,
                            source_access.material(),
                            &output,
                            &snapshot_key,
                        )?
                    };
                    println!("{}", serde_json::to_string_pretty(&manifest)?);
                }
                "create-capture" => {
                    let capture =
                        required_path(arguments.next(), "stable acquisition snapshot")?;
                    let output = required_path(arguments.next(), "new snapshot directory")?;
                    let remaining = arguments.collect::<Vec<_>>();
                    if remaining
                        .iter()
                        .any(|value| matches!(value.as_str(), "--help" | "-h"))
                    {
                        println!("{}", snapshot_create_capture_help());
                        return Ok(());
                    }
                    validate_command_options(
                        &remaining,
                        &[
                            "--snapshot-recovery-kit",
                            "--snapshot-local-credential",
                        ],
                        &[
                            "--source-passphrase-stdin",
                            "--source-decrypted",
                            "--snapshot-key-stdin",
                            "--snapshot-passphrase-stdin",
                        ],
                    )?;
                    let raw_key = remaining
                        .iter()
                        .any(|value| value == "--snapshot-key-stdin");
                    let recovery_kit = option_path(&remaining, "--snapshot-recovery-kit")?;
                    let local_credential =
                        option_path(&remaining, "--snapshot-local-credential")?;
                    let passphrase = remaining
                        .iter()
                        .any(|value| value == "--snapshot-passphrase-stdin");
                    if usize::from(raw_key) + usize::from(recovery_kit.is_some()) != 1 {
                        return Err(
                            "snapshot create-capture requires exactly one of --snapshot-key-stdin or --snapshot-recovery-kit"
                                .into(),
                        );
                    }
                    if (local_credential.is_some() || passphrase) && recovery_kit.is_none() {
                        return Err(
                            "optional local or passphrase protection requires --snapshot-recovery-kit so 24-word recovery is retained"
                                .into(),
                        );
                    }
                    let source_access = load_snapshot_create_source_access(&remaining)?;
                    let manifest = if let Some(recovery_kit) = recovery_kit {
                        let recovery_words =
                            SnapshotRecoveryWords::read_private_file(&recovery_kit)?;
                        let local_credential = local_credential
                            .as_ref()
                            .map(|path| SnapshotLocalCredential::read_private_file(path))
                            .transpose()?;
                        let passphrase = passphrase
                            .then(SnapshotPassphrase::read_stdin)
                            .transpose()?;
                        create_recoverable_snapshot_from_stable_capture_with_recovery_words_and_optional_protectors(
                            &capture,
                            source_access.material(),
                            &output,
                            &recovery_words,
                            local_credential.as_ref(),
                            passphrase.as_ref(),
                        )?
                    } else {
                        let snapshot_key = SnapshotKey::read_stdin()?;
                        create_recoverable_snapshot_from_stable_capture(
                            &capture,
                            source_access.material(),
                            &output,
                            &snapshot_key,
                        )?
                    };
                    println!("{}", serde_json::to_string_pretty(&manifest)?);
                }
                "verify" => {
                    let snapshot = required_path(arguments.next(), "snapshot directory")?;
                    let remaining = arguments.collect::<Vec<_>>();
                    if remaining
                        .iter()
                        .any(|value| matches!(value.as_str(), "--help" | "-h"))
                    {
                        println!("{}", snapshot_verify_help());
                        return Ok(());
                    }
                    validate_command_options(
                        &remaining,
                        &[
                            "--snapshot-recovery-kit",
                            "--snapshot-local-credential",
                        ],
                        &["--snapshot-key-stdin", "--snapshot-passphrase-stdin"],
                    )?;
                    let raw_key = remaining
                        .iter()
                        .any(|value| value == "--snapshot-key-stdin");
                    let recovery_kit = option_path(&remaining, "--snapshot-recovery-kit")?;
                    let local_credential =
                        option_path(&remaining, "--snapshot-local-credential")?;
                    let passphrase = remaining
                        .iter()
                        .any(|value| value == "--snapshot-passphrase-stdin");
                    if usize::from(raw_key)
                        + usize::from(recovery_kit.is_some())
                        + usize::from(local_credential.is_some())
                        + usize::from(passphrase)
                        != 1
                    {
                        return Err(
                            "snapshot verify requires exactly one of --snapshot-key-stdin, --snapshot-recovery-kit, --snapshot-local-credential, or --snapshot-passphrase-stdin"
                                .into(),
                        );
                    }
                    let report = if let Some(recovery_kit) = recovery_kit {
                        let recovery_words =
                            SnapshotRecoveryWords::read_private_file(&recovery_kit)?;
                        verify_recoverable_snapshot_with_recovery_words(
                            &snapshot,
                            &recovery_words,
                        )?
                    } else if let Some(local_credential) = local_credential {
                        let local_credential =
                            SnapshotLocalCredential::read_private_file(&local_credential)?;
                        verify_recoverable_snapshot_with_local_credential(
                            &snapshot,
                            &local_credential,
                        )?
                    } else if passphrase {
                        let passphrase = SnapshotPassphrase::read_stdin()?;
                        verify_recoverable_snapshot_with_passphrase(&snapshot, &passphrase)?
                    } else {
                        let snapshot_key = SnapshotKey::read_stdin()?;
                        verify_recoverable_snapshot(&snapshot, &snapshot_key)?
                    };
                    println!("{}", serde_json::to_string_pretty(&report)?);
                }
                "retention" => {
                    let action = arguments.next().ok_or(
                        "missing snapshot retention action; expected 'quarantine' or 'restore'",
                    )?;
                    if matches!(arguments.peek().map(String::as_str), Some("--help" | "-h")) {
                        println!("{}", snapshot_retention_action_help(&action)?);
                        return Ok(());
                    }
                    match action.as_str() {
                        "quarantine" => {
                            let retiring =
                                required_path(arguments.next(), "retiring snapshot directory")?;
                            let replacement =
                                required_path(arguments.next(), "replacement snapshot directory")?;
                            let quarantine =
                                required_path(arguments.next(), "snapshot quarantine directory")?;
                            let remaining = arguments.collect::<Vec<_>>();
                            validate_command_options(
                                &remaining,
                                &[
                                    "--retiring-recovery-kit",
                                    "--retiring-local-credential",
                                    "--replacement-recovery-kit",
                                ],
                                &["--retiring-snapshot-passphrase-stdin"],
                            )?;
                            let retiring_recovery_kit =
                                option_path(&remaining, "--retiring-recovery-kit")?;
                            let retiring_local_credential =
                                option_path(&remaining, "--retiring-local-credential")?;
                            let retiring_passphrase = remaining
                                .iter()
                                .any(|value| value == "--retiring-snapshot-passphrase-stdin");
                            if usize::from(retiring_recovery_kit.is_some())
                                + usize::from(retiring_local_credential.is_some())
                                + usize::from(retiring_passphrase)
                                != 1
                            {
                                return Err(
                                    "snapshot retention quarantine requires exactly one retiring-generation protector"
                                        .into(),
                                );
                            }
                            let replacement_recovery_kit = option_path(
                                &remaining,
                                "--replacement-recovery-kit",
                            )?
                            .ok_or(
                                "snapshot retention quarantine requires portable recovery verification of the replacement",
                            )?;
                            let retiring_key = if let Some(path) = retiring_recovery_kit {
                                let words = SnapshotRecoveryWords::read_private_file(&path)?;
                                unlock_recoverable_snapshot_with_recovery_words(
                                    &retiring,
                                    &words,
                                )?
                            } else if let Some(path) = retiring_local_credential.as_ref() {
                                let credential =
                                    SnapshotLocalCredential::read_private_file(path)?;
                                unlock_recoverable_snapshot_with_local_credential(
                                    &retiring,
                                    &credential,
                                )?
                            } else {
                                let passphrase = SnapshotPassphrase::read_stdin()?;
                                unlock_recoverable_snapshot_with_passphrase(
                                    &retiring,
                                    &passphrase,
                                )?
                            };
                            let replacement_words = SnapshotRecoveryWords::read_private_file(
                                &replacement_recovery_kit,
                            )?;
                            let report = quarantine_recoverable_snapshot_generation(
                                &retiring,
                                &retiring_key,
                                &replacement,
                                &replacement_words,
                                &quarantine,
                            )?;
                            println!("{}", serde_json::to_string_pretty(&report)?);
                        }
                        "restore" => {
                            let quarantined =
                                required_path(arguments.next(), "quarantined snapshot directory")?;
                            let restored =
                                required_path(arguments.next(), "restored snapshot directory")?;
                            let remaining = arguments.collect::<Vec<_>>();
                            validate_command_options(
                                &remaining,
                                &[
                                    "--snapshot-recovery-kit",
                                    "--snapshot-local-credential",
                                ],
                                &["--snapshot-passphrase-stdin"],
                            )?;
                            let recovery_kit =
                                option_path(&remaining, "--snapshot-recovery-kit")?;
                            let local_credential =
                                option_path(&remaining, "--snapshot-local-credential")?;
                            let passphrase = remaining
                                .iter()
                                .any(|value| value == "--snapshot-passphrase-stdin");
                            if usize::from(recovery_kit.is_some())
                                + usize::from(local_credential.is_some())
                                + usize::from(passphrase)
                                != 1
                            {
                                return Err(
                                    "snapshot retention restore requires exactly one snapshot protector"
                                        .into(),
                                );
                            }
                            let snapshot_key = if let Some(path) = recovery_kit {
                                let words = SnapshotRecoveryWords::read_private_file(&path)?;
                                unlock_recoverable_snapshot_with_recovery_words(
                                    &quarantined,
                                    &words,
                                )?
                            } else if let Some(path) = local_credential.as_ref() {
                                let credential =
                                    SnapshotLocalCredential::read_private_file(path)?;
                                unlock_recoverable_snapshot_with_local_credential(
                                    &quarantined,
                                    &credential,
                                )?
                            } else {
                                let passphrase = SnapshotPassphrase::read_stdin()?;
                                unlock_recoverable_snapshot_with_passphrase(
                                    &quarantined,
                                    &passphrase,
                                )?
                            };
                            let report = restore_quarantined_snapshot_generation(
                                &quarantined,
                                &snapshot_key,
                                &restored,
                            )?;
                            println!("{}", serde_json::to_string_pretty(&report)?);
                        }
                        _ => {
                            return Err(format!(
                                "unsupported snapshot retention action: {action}; expected 'quarantine' or 'restore'"
                            )
                            .into())
                        }
                    }
                }
                "rewrap" => {
                    let source = required_path(arguments.next(), "source snapshot directory")?;
                    let output = required_path(arguments.next(), "new snapshot directory")?;
                    let remaining = arguments.collect::<Vec<_>>();
                    if remaining
                        .iter()
                        .any(|value| matches!(value.as_str(), "--help" | "-h"))
                    {
                        println!("{}", snapshot_rewrap_help());
                        return Ok(());
                    }
                    validate_command_options(
                        &remaining,
                        &[
                            "--old-snapshot-recovery-kit",
                            "--old-snapshot-local-credential",
                            "--new-snapshot-recovery-kit",
                            "--new-snapshot-local-credential",
                        ],
                        &[
                            "--old-snapshot-passphrase-stdin",
                            "--new-snapshot-passphrase-stdin",
                        ],
                    )?;
                    let old_recovery_kit =
                        option_path(&remaining, "--old-snapshot-recovery-kit")?;
                    let old_local_credential =
                        option_path(&remaining, "--old-snapshot-local-credential")?;
                    let old_passphrase = remaining
                        .iter()
                        .any(|value| value == "--old-snapshot-passphrase-stdin");
                    if usize::from(old_recovery_kit.is_some())
                        + usize::from(old_local_credential.is_some())
                        + usize::from(old_passphrase)
                        != 1
                    {
                        return Err(
                            "snapshot rewrap requires exactly one old recovery kit, old local credential, or old passphrase"
                                .into(),
                        );
                    }
                    let new_recovery_kit = option_path(
                        &remaining,
                        "--new-snapshot-recovery-kit",
                    )?
                    .ok_or(
                        "snapshot rewrap requires --new-snapshot-recovery-kit so portable recovery is retained",
                    )?;
                    let new_local_credential =
                        option_path(&remaining, "--new-snapshot-local-credential")?;
                    let existing_key = if let Some(old_recovery_kit) = old_recovery_kit {
                        let words = SnapshotRecoveryWords::read_private_file(&old_recovery_kit)?;
                        unlock_recoverable_snapshot_with_recovery_words(&source, &words)?
                    } else if let Some(path) = old_local_credential.as_ref() {
                        let credential = SnapshotLocalCredential::read_private_file(path)?;
                        unlock_recoverable_snapshot_with_local_credential(&source, &credential)?
                    } else {
                        let passphrase = SnapshotPassphrase::read_stdin()?;
                        unlock_recoverable_snapshot_with_passphrase(&source, &passphrase)?
                    };
                    let new_words =
                        SnapshotRecoveryWords::read_private_file(&new_recovery_kit)?;
                    let new_local_credential = new_local_credential
                        .as_ref()
                        .map(|path| SnapshotLocalCredential::read_private_file(path))
                        .transpose()?;
                    let new_passphrase = remaining
                        .iter()
                        .any(|value| value == "--new-snapshot-passphrase-stdin")
                        .then(SnapshotPassphrase::read_stdin)
                        .transpose()?;
                    let manifest = rewrap_recoverable_snapshot_protectors_with_optional_protectors(
                        &source,
                        &existing_key,
                        &output,
                        &new_words,
                        new_local_credential.as_ref(),
                        new_passphrase.as_ref(),
                    )?;
                    println!("{}", serde_json::to_string_pretty(&manifest)?);
                }
                "rekey" => {
                    let source = required_path(arguments.next(), "source snapshot directory")?;
                    let output = required_path(arguments.next(), "new snapshot directory")?;
                    let remaining = arguments.collect::<Vec<_>>();
                    if remaining
                        .iter()
                        .any(|value| matches!(value.as_str(), "--help" | "-h"))
                    {
                        println!("{}", snapshot_rekey_help());
                        return Ok(());
                    }
                    validate_command_options(
                        &remaining,
                        &[],
                        &["--old-snapshot-key-stdin", "--new-snapshot-key-stdin"],
                    )?;
                    if !remaining
                        .iter()
                        .any(|value| value == "--old-snapshot-key-stdin")
                        || !remaining
                            .iter()
                            .any(|value| value == "--new-snapshot-key-stdin")
                    {
                        return Err(
                            "snapshot rekey requires --old-snapshot-key-stdin and --new-snapshot-key-stdin"
                                .into(),
                        );
                    }
                    let old_snapshot_key = SnapshotKey::read_stdin()?;
                    let new_snapshot_key = SnapshotKey::read_stdin()?;
                    let manifest = rekey_recoverable_snapshot(
                        &source,
                        &old_snapshot_key,
                        &output,
                        &new_snapshot_key,
                    )?;
                    println!("{}", serde_json::to_string_pretty(&manifest)?);
                }
                _ => {
                    return Err(format!(
                    "unsupported snapshot subcommand: {subcommand}; expected 'recovery-kit', 'local-credential', 'create', 'create-capture', 'verify', 'rewrap', 'retention', or 'rekey'"
                )
                    .into())
                }
            }
        }
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
        "connector-policy-direct" => {
            let database_root = required_path(arguments.next(), "WeChat database root")?;
            let policy_path = required_path(arguments.next(), "new direct connector policy")?;
            let remaining = arguments.collect::<Vec<_>>();
            if remaining
                .iter()
                .any(|value| matches!(value.as_str(), "--help" | "-h"))
            {
                println!("{}", connector_policy_direct_help());
                return Ok(());
            }
            let option_start = remaining
                .iter()
                .position(|value| value.starts_with("--"))
                .unwrap_or(remaining.len());
            let option_arguments = &remaining[option_start..];
            validate_command_options(
                option_arguments,
                &[
                    "--capabilities",
                    "--fields",
                    "--not-before-unix",
                    "--not-after-unix",
                    "--max-results",
                    "--max-summary-bytes",
                    "--snapshot-recovery-kit",
                    "--snapshot-local-credential",
                ],
                &[
                    "--allow-remote-model",
                    "--passphrase-stdin",
                    "--snapshot-key-stdin",
                    "--snapshot-passphrase-stdin",
                    "--decrypted",
                ],
            )?;
            let conversations = remaining[..option_start]
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>();
            if conversations.is_empty() {
                return Err("direct connector policy requires at least one conversation ID".into());
            }
            let capabilities = parse_capabilities(&required_option(&remaining, "--capabilities")?)?;
            if capabilities.contains(&ToolCapability::CreateDraft) {
                return Err("direct connector policy cannot authorize draft creation".into());
            }
            let message_fields = parse_message_fields(&required_option(&remaining, "--fields")?)?;
            let scope = ConversationToolScope {
                capabilities,
                message_fields,
                not_before_unix: option_i64(&remaining, "--not-before-unix")?,
                not_after_unix: option_i64(&remaining, "--not-after-unix")?,
                allow_remote_model: remaining
                    .iter()
                    .any(|value| value == "--allow-remote-model"),
            };
            let access = load_live_query_access(&database_root, &remaining)?;
            let source = access.open_source(&database_root)?;
            let conversation_ids = conversations.iter().cloned().collect::<Vec<_>>();
            for batch in conversation_ids.chunks(MAX_PAGE_LIMIT) {
                let present = find_live_conversations(&source, batch)?;
                if let Some(conversation) = batch
                    .iter()
                    .find(|conversation| !present.contains_key(*conversation))
                {
                    return Err(format!(
                        "conversation is not present in the selected SQLite source: {conversation}"
                    )
                    .into());
                }
            }
            let scopes = conversations
                .into_iter()
                .map(|conversation| (conversation, scope.clone()))
                .collect::<BTreeMap<_, _>>();
            let policy = create_direct_tool_policy(
                &policy_path,
                source.identity(),
                scopes,
                option_usize(&remaining, "--max-results")?.unwrap_or(100),
                option_usize(&remaining, "--max-summary-bytes")?.unwrap_or(4_096),
            )?;
            println!("{}", serde_json::to_string_pretty(&policy)?);
        }
        "connector-serve-direct" => {
            let database_root = required_path(arguments.next(), "WeChat database root")?;
            let policy = required_path(arguments.next(), "direct connector policy")?;
            let audit = required_path(arguments.next(), "audit log path")?;
            let socket = required_path(arguments.next(), "Unix socket path")?;
            let remaining = arguments.collect::<Vec<_>>();
            if remaining
                .iter()
                .any(|value| matches!(value.as_str(), "--help" | "-h"))
            {
                println!("{}", connector_serve_direct_help());
                return Ok(());
            }
            validate_command_options(
                &remaining,
                &["--snapshot-recovery-kit", "--snapshot-local-credential"],
                &[
                    "--passphrase-stdin",
                    "--snapshot-key-stdin",
                    "--snapshot-passphrase-stdin",
                    "--decrypted",
                ],
            )?;
            let access = load_live_query_access(&database_root, &remaining)?;
            let source = access.open_source(&database_root)?;
            let service = DirectConnectorService::open(source, &policy, &audit)?;
            serve_unix(&service, &socket)?;
        }
        "connector-query-direct" => {
            let database_root = required_path(arguments.next(), "WeChat database root")?;
            let policy = required_path(arguments.next(), "direct connector policy")?;
            let audit = required_path(arguments.next(), "audit log path")?;
            let request_path = required_path(arguments.next(), "private request JSON")?;
            let remaining = arguments.collect::<Vec<_>>();
            if remaining
                .iter()
                .any(|value| matches!(value.as_str(), "--help" | "-h"))
            {
                println!("{}", connector_query_direct_help());
                return Ok(());
            }
            validate_command_options(
                &remaining,
                &["--snapshot-recovery-kit", "--snapshot-local-credential"],
                &[
                    "--passphrase-stdin",
                    "--snapshot-key-stdin",
                    "--snapshot-passphrase-stdin",
                    "--decrypted",
                ],
            )?;
            let request = load_connector_request(&request_path)?;
            let access = load_live_query_access(&database_root, &remaining)?;
            let source = access.open_source(&database_root)?;
            let service = DirectConnectorService::open(source, &policy, &audit)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&service.handle(request))?
            );
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
        "send" => {
            run_send_command(arguments)?;
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
                    "  greenbubbles-restore profile path|template|list|show|validate|set-default ...\n",
                    "  greenbubbles-restore source status [--profile <name>]\n",
                    "  greenbubbles-restore source status <source-root> (--passphrase-stdin | --snapshot-recovery-kit <file> | --snapshot-local-credential <file> | --snapshot-passphrase-stdin | --snapshot-key-stdin | --decrypted)\n",
                    "  greenbubbles-restore conversations list [--profile <name>] [--limit <1..500>] [--cursor <opaque-cursor>]\n",
                    "  greenbubbles-restore conversations list <source-root> (--passphrase-stdin | --snapshot-recovery-kit <file> | --snapshot-local-credential <file> | --snapshot-passphrase-stdin | --snapshot-key-stdin | --decrypted) [--limit <1..500>] [--cursor <opaque-cursor>]\n",
                    "  greenbubbles-restore messages list --conversation <id> [--profile <name>] [--limit <1..500>] [--cursor <opaque-cursor>]\n",
                    "  greenbubbles-restore messages list <source-root> --conversation <id> (--passphrase-stdin | --snapshot-recovery-kit <file> | --snapshot-local-credential <file> | --snapshot-passphrase-stdin | --snapshot-key-stdin | --decrypted) [--limit <1..500>] [--cursor <opaque-cursor>]\n",
                    "  greenbubbles-restore messages search --query-stdin [--profile <name>] [--conversation <id>] [--limit <1..200>] [--cursor <opaque-cursor>]\n",
                    "  greenbubbles-restore messages search <source-root> --query-stdin (--passphrase-stdin | --snapshot-recovery-kit <file> | --snapshot-local-credential <file> | --snapshot-passphrase-stdin | --snapshot-key-stdin | --decrypted) [--conversation <id>] [--limit <1..200>] [--cursor <opaque-cursor>]\n",
                    "  greenbubbles-restore message get --conversation <id> --message <opaque-id> [--profile <name>]\n",
                    "  greenbubbles-restore message get <source-root> --conversation <id> --message <opaque-id> (--passphrase-stdin | --snapshot-recovery-kit <file> | --snapshot-local-credential <file> | --snapshot-passphrase-stdin | --snapshot-key-stdin | --decrypted)\n",
                    "  greenbubbles-restore attachment inspect <account-or-source-root> --conversation <id> --message <opaque-id> --kind image|voice|video|document (--passphrase-stdin | --snapshot-recovery-kit <file> | --snapshot-local-credential <file> | --snapshot-passphrase-stdin | --snapshot-key-stdin | --decrypted)\n",
                    "  greenbubbles-restore attachment materialize <account-or-source-root> --conversation <id> --message <opaque-id> --kind image|voice|video|document --attachment <opaque-id> --output <new-path> (--passphrase-stdin | --snapshot-recovery-kit <file> | --snapshot-local-credential <file> | --snapshot-passphrase-stdin | --snapshot-key-stdin | --decrypted)\n",
                    "  greenbubbles-restore snapshot recovery-kit create <new-private-file>\n",
                    "  greenbubbles-restore snapshot local-credential create <new-private-file>\n",
                    "  greenbubbles-restore snapshot create <WeChat-database-root> <new-snapshot-directory> (--source-passphrase-stdin | --source-decrypted) (--snapshot-recovery-kit <file> [--snapshot-local-credential <file>] [--snapshot-passphrase-stdin] | --snapshot-key-stdin)\n",
                    "  greenbubbles-restore snapshot create-capture <stable-acquisition-snapshot> <new-snapshot-directory> (--source-passphrase-stdin | --source-decrypted) (--snapshot-recovery-kit <file> [--snapshot-local-credential <file>] [--snapshot-passphrase-stdin] | --snapshot-key-stdin)\n",
                    "  greenbubbles-restore snapshot verify <snapshot-directory> (--snapshot-recovery-kit <file> | --snapshot-local-credential <file> | --snapshot-passphrase-stdin | --snapshot-key-stdin)\n",
                    "  greenbubbles-restore snapshot rewrap <snapshot-directory> <new-snapshot-directory> (--old-snapshot-recovery-kit <file> | --old-snapshot-local-credential <file> | --old-snapshot-passphrase-stdin) --new-snapshot-recovery-kit <file> [--new-snapshot-local-credential <file>] [--new-snapshot-passphrase-stdin]\n",
                    "  greenbubbles-restore snapshot retention quarantine <retiring> <replacement> <quarantine-directory> (--retiring-recovery-kit <file> | --retiring-local-credential <file> | --retiring-snapshot-passphrase-stdin) --replacement-recovery-kit <file>\n",
                    "  greenbubbles-restore snapshot retention restore <quarantined> <restored-directory> (--snapshot-recovery-kit <file> | --snapshot-local-credential <file> | --snapshot-passphrase-stdin)\n",
                    "  greenbubbles-restore snapshot rekey <snapshot-directory> <new-snapshot-directory> --old-snapshot-key-stdin --new-snapshot-key-stdin\n",
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
                    "  greenbubbles-restore connector-policy-direct <source-root> <new-policy-file> <conversation-id>... --capabilities list,read,search --fields sender,created-at,type,content (--passphrase-stdin | --snapshot-recovery-kit <file> | --snapshot-local-credential <file> | --snapshot-passphrase-stdin | --snapshot-key-stdin | --decrypted) [--not-before-unix <seconds>] [--not-after-unix <seconds>] [--allow-remote-model] [--max-results <n>] [--max-summary-bytes <n>]\n",
                    "  greenbubbles-restore connector-query-direct <source-root> <policy-file> <audit-log> <private-request-json> (--passphrase-stdin | --snapshot-recovery-kit <file> | --snapshot-local-credential <file> | --snapshot-passphrase-stdin | --snapshot-key-stdin | --decrypted)\n",
                    "  greenbubbles-restore connector-serve-direct <source-root> <policy-file> <audit-log> <socket-path> (--passphrase-stdin | --snapshot-recovery-kit <file> | --snapshot-local-credential <file> | --snapshot-passphrase-stdin | --snapshot-key-stdin | --decrypted)\n",
                    "  greenbubbles-restore connector-serve <replica-path> <policy-file> <audit-log> <draft-directory> <socket-path> --replica-key-stdin\n",
                    "  greenbubbles-restore connector-call <socket-path> <private-request-json>\n",
                    "  greenbubbles-restore tool-policy <archive> <policy-file> ([<conversation-id>...] | --all-conversations) [--capabilities list,read,search,draft] [--fields sender,created-at,direction,type,content,attachments,relationships] [--not-before-unix <seconds>] [--not-after-unix <seconds>] [--allow-remote-model] [--enable-cached-moments --cached-fields author,created-at,type,content,title,description,url,media-count,like-count,comment-count] [--cached-not-before-unix <seconds>] [--cached-not-after-unix <seconds>] [--allow-cached-remote-model] [--max-results <n>] [--max-summary-bytes <n>] [--max-draft-bytes <n>]\n",
                    "  greenbubbles-restore tool-list <archive> <policy-file> <audit-log> --requester <id> [--destination local|remote]\n",
                    "  greenbubbles-restore tool-recent <archive> <policy-file> <audit-log> <conversation-id> --requester <id> [--limit <n>] [--destination local|remote]\n",
                    "  greenbubbles-restore tool-search <archive> <policy-file> <audit-log> --requester <id> --query-stdin [--conversation <id>] [--limit <n>] [--destination local|remote]\n",
                    "  greenbubbles-restore tool-draft <archive> <policy-file> <audit-log> <draft-directory> <conversation-id> --requester <id> --body-stdin\n",
                    "  greenbubbles-restore send <subcommand>   (run 'greenbubbles-restore send --help')"
                )
            );
        }
    }
    Ok(())
}

const fn query_profile_command_help() -> &'static str {
    concat!(
        "Usage:\n",
        "  greenbubbles-restore profile path\n",
        "  greenbubbles-restore profile template\n",
        "  greenbubbles-restore profile list\n",
        "  greenbubbles-restore profile show <name>\n",
        "  greenbubbles-restore profile validate [<name>]\n",
        "  greenbubbles-restore profile set-default <name>\n\n",
        "Named query profiles live in ~/.greenbubbles/query-profiles.json by default.\n",
        "The configuration remembers a source root and access mode. It never contains\n",
        "a raw WeChat key, snapshot key, recovery phrase, or passphrase: encrypted modes\n",
        "refer to a separate current-user-owned 0600 credential file inside an owner-only\n",
        "directory. Configuration and credential symlinks are rejected.\n\n",
        "Query use:\n",
        "  omit both source and access arguments to use defaultProfile\n",
        "  --profile <name> selects a different named profile\n",
        "  explicit <source-root> plus one access mode remains supported\n",
        "  profile selection cannot be mixed with explicit source/access arguments\n\n",
        "Access mode names:\n",
        "  liveWeChatKeyFile, snapshotLocalCredential, snapshotRecoveryKit,\n",
        "  snapshotPassphraseFile, snapshotRawKeyFile, decrypted\n\n",
        "GREENBUBBLES_QUERY_PROFILES_FILE may select another absolute owner-only\n",
        "configuration file. Run `profile template` for the strict JSON schema.\n",
    )
}

const fn source_status_help() -> &'static str {
    concat!(
        "Usage:\n",
        "  greenbubbles-restore source status [--profile <name>]\n",
        "  greenbubbles-restore source status <source-root> (--passphrase-stdin | --snapshot-recovery-kit <file> | --snapshot-local-credential <file> | --snapshot-passphrase-stdin | --snapshot-key-stdin | --decrypted)\n\n",
        "Authenticates the required core databases read-only, then returns bounded,\n",
        "content-free storage accounting for every regular .db file and its adjacent\n",
        "WAL, SHM, or rollback-journal sidecars. Sizes are filesystem byte counts; no\n",
        "rows are restored, decoded, exported, indexed, or copied. Absolute paths are\n",
        "not returned. The inventory is capped at 4,096 databases and 100,000 entries.\n\n",
        "Access modes:\n",
        "  no access arguments   Use defaultProfile from the private profile file\n",
        "  --profile <name>      Use one named private query profile\n",
        "  --passphrase-stdin   Read the 32-byte WeChat database key from standard input\n",
        "  --snapshot-local-credential <file>  Use the owner-only local convenience file\n",
        "  --snapshot-recovery-kit <file>      Use the portable 24-word recovery kit\n",
        "  --snapshot-passphrase-stdin         Read the optional Argon2id passphrase\n",
        "  --snapshot-key-stdin Legacy: read an independent raw snapshot key\n",
        "  --decrypted          Explicitly inspect plaintext SQLite database files\n",
        "  -h, --help           Show this help\n",
    )
}

const fn conversations_command_help() -> &'static str {
    concat!(
        "Usage:\n",
        "  greenbubbles-restore conversations list [--profile <name>] [--limit <1..500>] [--cursor <opaque-cursor>]\n",
        "  greenbubbles-restore conversations list <source-root> (--passphrase-stdin | --snapshot-recovery-kit <file> | --snapshot-local-credential <file> | --snapshot-passphrase-stdin | --snapshot-key-stdin | --decrypted) [--limit <1..500>] [--cursor <opaque-cursor>]\n\n",
        "Returns one bounded, keyset-paginated JSON page directly from session.db.\n",
        "The source is opened read-only with SQLite query_only enforcement; no archive,\n",
        "replica, staging database, search index, or media derivative is created.\n\n",
        "Access modes:\n",
        "  no access arguments  Use defaultProfile from the private profile file\n",
        "  --profile <name>     Use one named private query profile\n",
        "  --passphrase-stdin  Read the 32-byte WeChat database key from standard input\n",
        "  --snapshot-local-credential <file>  Use a local snapshot convenience protector\n",
        "  --snapshot-recovery-kit <file>      Use portable 24-word snapshot recovery\n",
        "  --snapshot-passphrase-stdin         Read the optional Argon2id passphrase\n",
        "  --snapshot-key-stdin Legacy raw-key snapshot compatibility\n",
        "  --decrypted         Explicitly query plaintext SQLite database files\n\n",
        "Options:\n",
        "  --limit <n>         Return 1..500 conversations; default 100\n",
        "  --cursor <token>    Continue from an opaque cursor returned by the prior page\n",
        "  -h, --help          Show this help\n",
    )
}

const fn messages_command_help() -> &'static str {
    concat!(
        "Usage:\n",
        "  greenbubbles-restore messages list --conversation <id> [--profile <name>] [--limit <1..500>] [--cursor <opaque-cursor>]\n",
        "  greenbubbles-restore messages list <source-root> --conversation <id> (--passphrase-stdin | --snapshot-recovery-kit <file> | --snapshot-local-credential <file> | --snapshot-passphrase-stdin | --snapshot-key-stdin | --decrypted) [--limit <1..500>] [--cursor <opaque-cursor>]\n\n",
        "  greenbubbles-restore messages search --query-stdin [--profile <name>] [--conversation <id>] [--limit <1..200>] [--cursor <opaque-cursor>]\n",
        "  greenbubbles-restore messages search <source-root> --query-stdin (--passphrase-stdin | --snapshot-recovery-kit <file> | --snapshot-local-credential <file> | --snapshot-passphrase-stdin | --snapshot-key-stdin | --decrypted) [--conversation <id>] [--limit <1..200>] [--cursor <opaque-cursor>]\n\n",
        "Returns one bounded, typed, keyset-paginated JSON page directly from numbered\n",
        "WeChat message shards. Each shard uses a short read-only statement; the response\n",
        "reports cross-database consistency and partial-coverage warnings explicitly. Search\n",
        "prefers compatible native WeChat FTS. If it is unavailable, each response scans at\n",
        "most 500 decoded source messages and returns a continuation without writing an index.\n\n",
        "Access modes:\n",
        "  no access arguments  Use defaultProfile from the private profile file\n",
        "  --profile <name>     Use one named private query profile\n",
        "  --passphrase-stdin  Read the 32-byte WeChat database key from standard input\n",
        "  --snapshot-local-credential <file>  Use a local snapshot convenience protector\n",
        "  --snapshot-recovery-kit <file>      Use portable 24-word snapshot recovery\n",
        "  --snapshot-passphrase-stdin         Read the optional Argon2id passphrase\n",
        "  --snapshot-key-stdin Legacy raw-key snapshot compatibility\n",
        "  --decrypted         Explicitly query plaintext SQLite database files\n\n",
        "Options:\n",
        "  --conversation <id> Exact wxid or chatroom identifier\n",
        "  --limit <n>         Return 1..500 messages; default 100\n",
        "  --cursor <token>    Continue from an opaque cursor returned by the prior page\n",
        "  -h, --help          Show this help\n",
    )
}

const fn messages_search_help() -> &'static str {
    concat!(
        "Usage:\n",
        "  greenbubbles-restore messages search --query-stdin [--profile <name>] [--conversation <id>] [--limit <1..200>] [--cursor <opaque-cursor>]\n",
        "  greenbubbles-restore messages search <source-root> --query-stdin (--passphrase-stdin | --snapshot-recovery-kit <file> | --snapshot-local-credential <file> | --snapshot-passphrase-stdin | --snapshot-key-stdin | --decrypted) [--conversation <id>] [--limit <1..200>] [--cursor <opaque-cursor>]\n\n",
        "Runs one literal, parameterized, keyset-paginated query against a compatible\n",
        "read-only native WeChat message FTS database. It never builds an index or falls\n",
        "back to a corpus-wide message scan. Index freshness is reported as unverified.\n\n",
        "Input ordering:\n",
        "  encrypted live source   input line 1 is the WeChat key; all remaining UTF-8\n",
        "                          input (maximum 16 KiB) is the search query\n",
        "  encrypted snapshot      input line 1 is the snapshot recovery key; all remaining\n",
        "                          UTF-8 input is the search query\n",
        "  snapshot passphrase     input line 1 is the Argon2id passphrase; all remaining\n",
        "                          UTF-8 input is the search query\n",
        "  snapshot protector file all standard input is the search query; protector\n",
        "                          contents and unwrapped key never enter standard input\n",
        "  configured profile      all standard input is the search query; the private\n",
        "                          credential file is read separately\n",
        "  --decrypted             all standard input is the search query\n\n",
        "Options:\n",
        "  --query-stdin       Required; search text is never accepted in an argument\n",
        "  --conversation <id> Restrict hits to one exact wxid or chatroom identifier\n",
        "  --limit <n>         Return 1..200 hits; default 50\n",
        "  --cursor <token>    Continue the exact same search from a prior page\n",
        "  -h, --help          Show this help\n",
    )
}

fn messages_subcommand_help(subcommand: &str) -> Result<&'static str, String> {
    match subcommand {
        "list" => Ok(messages_command_help()),
        "search" => Ok(messages_search_help()),
        _ => Err(format!(
            "unsupported messages subcommand: {subcommand}; expected 'list' or 'search'"
        )),
    }
}

const fn message_get_help() -> &'static str {
    concat!(
        "Usage:\n",
        "  greenbubbles-restore message get --conversation <id> --message <opaque-id> [--profile <name>]\n",
        "  greenbubbles-restore message get <source-root> --conversation <id> --message <opaque-id> (--passphrase-stdin | --snapshot-recovery-kit <file> | --snapshot-local-credential <file> | --snapshot-passphrase-stdin | --snapshot-key-stdin | --decrypted)\n\n",
        "Performs one bounded retrieval by the opaque identity returned from messages\n",
        "list. The identity is bound to the source and conversation. GreenBubbles opens\n",
        "only the named read-only shard and performs one rowid/key lookup; it does not\n",
        "scan the conversation or create an archive, index, replica, or derivative.\n\n",
        "Access modes:\n",
        "  no access arguments   Use defaultProfile from the private profile file\n",
        "  --profile <name>      Use one named private query profile\n",
        "  --passphrase-stdin   Read the 32-byte WeChat database key from standard input\n",
        "  --snapshot-local-credential <file>  Use a local snapshot convenience protector\n",
        "  --snapshot-recovery-kit <file>      Use portable 24-word snapshot recovery\n",
        "  --snapshot-passphrase-stdin         Read the optional Argon2id passphrase\n",
        "  --snapshot-key-stdin Legacy raw-key snapshot compatibility\n",
        "  --decrypted          Explicitly query plaintext SQLite database files\n\n",
        "Options:\n",
        "  --conversation <id>  Exact wxid or chatroom identifier used for listing\n",
        "  --message <id>       Opaque message identity returned by messages list\n",
        "  -h, --help           Show this help\n",
    )
}

const fn attachment_command_help() -> &'static str {
    concat!(
        "Usage:\n",
        "  greenbubbles-restore attachment inspect <account-or-source-root> --conversation <id> --message <opaque-id> --kind image|voice|video|document <access-mode>\n",
        "  greenbubbles-restore attachment materialize <account-or-source-root> --conversation <id> --message <opaque-id> --kind image|voice|video|document --attachment <opaque-id> --output <new-path> <access-mode>\n",
        "  greenbubbles-restore attachment inspect <account-root> --conversation <id> --md5 <hex>\n",
        "  greenbubbles-restore attachment materialize <account-root> --conversation <id> --md5 <hex> --attachment <opaque-id> --output <new-path>\n\n",
        "Inspects or materializes one exact image, voice, video, or document on demand.\n",
        "The preferred message-bound form hydrates one opaque message identity read-only,\n",
        "then performs bounded media-DB, hardlink-metadata, or conversation filesystem\n",
        "lookup. The legacy MD5 form remains available for images and reads no database.\n",
        "Inspection writes nothing. Materialization revalidates the opaque candidate and\n",
        "atomically creates one owner-only output outside the protected source. Source and\n",
        "output paths are never returned in JSON.\n",
    )
}

const fn attachment_inspect_help() -> &'static str {
    concat!(
        "Usage:\n",
        "  greenbubbles-restore attachment inspect <account-or-source-root> --conversation <id> --message <opaque-id> --kind image|voice|video|document <access-mode>\n",
        "  greenbubbles-restore attachment inspect <account-root> --conversation <id> --md5 <hex>\n\n",
        "Returns a small versioned JSON description without decoding or writing media.\n",
        "The preferred form binds the source, conversation, exact opaque message, kind,\n",
        "candidate location or row, and current content/version evidence. Legacy --md5 is\n",
        "image-only, accepts no database access mode, and remains compatibility syntax.\n\n",
        "Access modes for --message:\n",
        "  --passphrase-stdin                   Encrypted live WeChat databases\n",
        "  --snapshot-recovery-kit <file>       Portable 24-word snapshot recovery\n",
        "  --snapshot-local-credential <file>   Local snapshot convenience protector\n",
        "  --snapshot-passphrase-stdin          Optional Argon2id snapshot passphrase\n",
        "  --snapshot-key-stdin                 Legacy raw snapshot key\n",
        "  --decrypted                          Explicit plaintext SQLite source\n\n",
        "Options:\n",
        "  --conversation <id>  Exact wxid or chatroom identifier\n",
        "  --message <id>       Opaque identity returned by messages list/search\n",
        "  --kind <kind>        image, voice, video, or document; default image\n",
        "  --md5 <hex>          Legacy image locator; exactly 32 hexadecimal characters\n",
        "  -h, --help           Show this help\n",
    )
}

const fn attachment_materialize_help() -> &'static str {
    concat!(
        "Usage:\n",
        "  greenbubbles-restore attachment materialize <account-or-source-root> --conversation <id> --message <opaque-id> --kind image|voice|video|document --attachment <opaque-id> --output <new-path> <access-mode>\n",
        "  greenbubbles-restore attachment materialize <account-root> --conversation <id> --md5 <hex> --attachment <opaque-id> --output <new-path>\n\n",
        "Materializes exactly one previously inspected candidate. Images are decoded; a\n",
        "voice payload is converted from SILK to Ogg Opus when supported and otherwise\n",
        "retained as SILK; video and documents are streamed without whole-file buffering.\n",
        "The output parent\n",
        "must already be a current-user-owned directory with no group/other access.\n",
        "Existing outputs are never overwritten, and output inside the protected source is\n",
        "rejected. The response reports format, byte count, and SHA-256, but no paths.\n\n",
        "Access modes for --message are the same as attachment inspect.\n\n",
        "Options:\n",
        "  --conversation <id>  Exact wxid or chatroom identifier\n",
        "  --message <id>       Opaque identity returned by messages list/search\n",
        "  --kind <kind>        image, voice, video, or document; default image\n",
        "  --md5 <hex>          Legacy image locator; exactly 32 hexadecimal characters\n",
        "  --attachment <id>    Opaque candidate identity returned by inspect\n",
        "  --output <path>      New decoded file path; overwrite is not supported\n",
        "  -h, --help           Show this help\n",
    )
}

fn attachment_subcommand_help(subcommand: &str) -> Result<&'static str, String> {
    match subcommand {
        "inspect" => Ok(attachment_inspect_help()),
        "materialize" => Ok(attachment_materialize_help()),
        _ => Err(format!(
            "unsupported attachment subcommand: {subcommand}; expected 'inspect' or 'materialize'"
        )),
    }
}

const fn snapshot_command_help() -> &'static str {
    concat!(
        "Usage:\n",
        "  greenbubbles-restore snapshot recovery-kit (create | validate) <private-file>\n",
        "  greenbubbles-restore snapshot local-credential (create | validate) <private-file>\n",
        "  greenbubbles-restore snapshot create <WeChat-database-root> <new-snapshot-directory> (--source-passphrase-stdin | --source-decrypted) (--snapshot-recovery-kit <private-file> [--snapshot-local-credential <private-file>] [--snapshot-passphrase-stdin] | --snapshot-key-stdin)\n",
        "  greenbubbles-restore snapshot create-capture <stable-acquisition-snapshot> <new-snapshot-directory> (--source-passphrase-stdin | --source-decrypted) (--snapshot-recovery-kit <private-file> [--snapshot-local-credential <private-file>] [--snapshot-passphrase-stdin] | --snapshot-key-stdin)\n",
        "  greenbubbles-restore snapshot verify <snapshot-directory> (--snapshot-recovery-kit <private-file> | --snapshot-local-credential <private-file> | --snapshot-passphrase-stdin | --snapshot-key-stdin)\n\n",
        "  greenbubbles-restore snapshot rewrap <snapshot-directory> <new-snapshot-directory> (--old-snapshot-recovery-kit <private-file> | --old-snapshot-local-credential <private-file> | --old-snapshot-passphrase-stdin) --new-snapshot-recovery-kit <private-file> [--new-snapshot-local-credential <private-file>] [--new-snapshot-passphrase-stdin]\n",
        "  greenbubbles-restore snapshot retention quarantine <retiring> <replacement> <quarantine-directory> (--retiring-recovery-kit <private-file> | --retiring-local-credential <private-file> | --retiring-snapshot-passphrase-stdin) --replacement-recovery-kit <private-file>\n",
        "  greenbubbles-restore snapshot retention restore <quarantined> <restored-directory> (--snapshot-recovery-kit <private-file> | --snapshot-local-credential <private-file> | --snapshot-passphrase-stdin)\n",
        "  greenbubbles-restore snapshot rekey <snapshot-directory> <new-snapshot-directory> --old-snapshot-key-stdin --new-snapshot-key-stdin\n\n",
        "Creates and verifies durable logical SQLite backups whose encryption key is\n",
        "independent of WeChat. Use 'snapshot create --help' or 'snapshot verify --help'\n",
        "for secret-input ordering and recovery details.\n",
    )
}

const fn snapshot_local_credential_help() -> &'static str {
    concat!(
        "Usage:\n",
        "  greenbubbles-restore snapshot local-credential create <new-private-file>\n",
        "  greenbubbles-restore snapshot local-credential validate <private-file>\n\n",
        "Creates or validates a random local convenience credential. The file is\n",
        "exclusively created mode 0600 beneath an owner-only directory and is never\n",
        "printed. It wraps the snapshot database key independently; it contains neither\n",
        "the SQLCipher key nor the 24 recovery words. This file is not a backup: keep the\n",
        "portable recovery words separately so deletion or loss of this device is safe.\n",
    )
}

const fn snapshot_recovery_kit_help() -> &'static str {
    concat!(
        "Usage:\n",
        "  greenbubbles-restore snapshot recovery-kit create <new-private-file>\n",
        "  greenbubbles-restore snapshot recovery-kit validate <private-file>\n\n",
        "Creates or validates a standard 24-word English BIP-39 recovery kit. Creation\n",
        "uses 256 random bits, validates the checksum, exclusively creates a mode-0600\n",
        "single-link file beneath an owner-only directory, and syncs it before success.\n",
        "The JSON report contains no words, raw key, base64 key, or path. Read and copy\n",
        "the private file to an independent recovery location before snapshot creation.\n",
    )
}

const fn snapshot_create_help() -> &'static str {
    concat!(
        "Usage:\n",
        "  greenbubbles-restore snapshot create <WeChat-database-root> <new-snapshot-directory> (--source-passphrase-stdin | --source-decrypted) (--snapshot-recovery-kit <private-file> [--snapshot-local-credential <private-file>] [--snapshot-passphrase-stdin] | --snapshot-key-stdin)\n\n",
        "Copies every regular .db file through SQLite's logical backup API into a new\n",
        "SQLCipher database protected by a random key independent from WeChat. Prefer a\n",
        "24-word recovery kit, which wraps that key without becoming the database key.\n",
        "No plaintext database staging file is created. Every destination is reopened,\n",
        "integrity-checked without the WeChat key, hashed, and atomically published.\n\n",
        "Source modes:\n",
        "  --source-passphrase-stdin  Read the 32-byte WeChat key from input line 1\n",
        "  --source-decrypted         Explicitly read plaintext SQLite source databases\n\n",
        "Recovery protection:\n",
        "  --snapshot-recovery-kit <file>      Preferred portable 24-word protector\n",
        "  --snapshot-local-credential <file>  Optional local convenience protector; this\n",
        "                                      is accepted only alongside the recovery kit\n",
        "  --snapshot-passphrase-stdin         Optional Argon2id protector; read after the\n",
        "                                      source key, or as line 1 for plaintext source\n",
        "  --snapshot-key-stdin       Legacy raw-key format. Read line 2 for an encrypted\n",
        "                             source, or line 1 for a decrypted source\n",
        "  -h, --help                 Show this help\n",
    )
}

const fn snapshot_create_capture_help() -> &'static str {
    concat!(
        "Usage:\n",
        "  greenbubbles-restore snapshot create-capture <stable-acquisition-snapshot> <new-snapshot-directory> (--source-passphrase-stdin | --source-decrypted) (--snapshot-recovery-kit <private-file> [--snapshot-local-credential <private-file>] [--snapshot-passphrase-stdin] | --snapshot-key-stdin)\n\n",
        "Converts a complete owner-only filesystem capture produced by GreenBubbles'\n",
        "database/WAL/SHM snapshotter. It verifies every capture hash before use, opens\n",
        "captured databases read-only, writes logical pages directly into separately\n",
        "keyed SQLCipher databases, and verifies the capture again before publication.\n",
        "Incremental fragments are rejected because they are not complete generations.\n",
        "No plaintext database staging file is created.\n\n",
        "Source modes:\n",
        "  --source-passphrase-stdin  Read the 32-byte WeChat key from input line 1\n",
        "  --source-decrypted         Explicitly read a plaintext SQLite capture\n\n",
        "Recovery protection:\n",
        "  --snapshot-recovery-kit <file>      Preferred portable 24-word protector\n",
        "  --snapshot-local-credential <file>  Optional local convenience protector; this\n",
        "                                      is accepted only alongside the recovery kit\n",
        "  --snapshot-passphrase-stdin         Optional Argon2id protector; read after the\n",
        "                                      source key, or as line 1 for plaintext capture\n",
        "  --snapshot-key-stdin       Legacy raw-key format; read line 2 for encrypted\n",
        "                             captures, or line 1 for plaintext\n",
        "  -h, --help                 Show this help\n",
    )
}

const fn snapshot_verify_help() -> &'static str {
    concat!(
        "Usage:\n",
        "  greenbubbles-restore snapshot verify <snapshot-directory> (--snapshot-recovery-kit <private-file> | --snapshot-local-credential <private-file> | --snapshot-passphrase-stdin | --snapshot-key-stdin)\n\n",
        "Verifies owner-only permissions, exact inventory, manifest hashes, encrypted\n",
        "headers, absence of required WAL/SHM/journal files, and SQLite integrity using\n",
        "only an independent snapshot protector. No WeChat key is accepted. A local\n",
        "credential is valid only when the manifest still contains portable recovery.\n\n",
        "Options:\n",
        "  --snapshot-recovery-kit <file>      Use the portable 24-word recovery kit\n",
        "  --snapshot-local-credential <file>  Use the owner-only local convenience file\n",
        "  --snapshot-passphrase-stdin         Read the Argon2id passphrase as one line\n",
        "  --snapshot-key-stdin Legacy: read the raw recovery key from standard input\n",
        "  -h, --help            Show this help\n",
    )
}

const fn snapshot_rekey_help() -> &'static str {
    concat!(
        "Usage:\n",
        "  greenbubbles-restore snapshot rekey <snapshot-directory> <new-snapshot-directory> --old-snapshot-key-stdin --new-snapshot-key-stdin\n\n",
        "Verifies an immutable source generation using only its old recovery key, then\n",
        "logically copies each database directly from old-key SQLCipher into new-key\n",
        "SQLCipher. No plaintext database is created. A separate generation is verified\n",
        "with only the new key and atomically published; the source remains untouched.\n\n",
        "Input ordering:\n",
        "  line 1  Existing 32-byte snapshot recovery key\n",
        "  line 2  Distinct new 32-byte snapshot recovery key\n\n",
        "Options:\n",
        "  --old-snapshot-key-stdin  Required; confirms line 1 semantics\n",
        "  --new-snapshot-key-stdin  Required; confirms line 2 semantics\n",
        "  -h, --help                Show this help\n",
    )
}

const fn snapshot_rewrap_help() -> &'static str {
    concat!(
        "Usage:\n",
        "  greenbubbles-restore snapshot rewrap <snapshot-directory> <new-snapshot-directory> (--old-snapshot-recovery-kit <private-file> | --old-snapshot-local-credential <private-file> | --old-snapshot-passphrase-stdin) --new-snapshot-recovery-kit <private-file> [--new-snapshot-local-credential <private-file>] [--new-snapshot-passphrase-stdin]\n\n",
        "Authenticates and fully verifies an immutable format-2 source generation, then\n",
        "byte-copies its already encrypted SQLCipher databases into a new generation.\n",
        "The database key and ciphertext bytes do not change; only the snapshot identity\n",
        "and authenticated protector envelope change. The source remains untouched.\n\n",
        "A new portable 24-word recovery kit is mandatory. A new local credential and\n",
        "Argon2id passphrase are optional and never substitute for portable recovery. If\n",
        "both passphrase flags are used, stdin line 1 is the old passphrase and line 2 is\n",
        "the new passphrase. No key, recovery words, or database plaintext enters stdin.\n",
    )
}

const fn snapshot_retention_help() -> &'static str {
    concat!(
        "Usage:\n",
        "  greenbubbles-restore snapshot retention quarantine <retiring> <replacement> <quarantine-directory> (--retiring-recovery-kit <private-file> | --retiring-local-credential <private-file> | --retiring-snapshot-passphrase-stdin) --replacement-recovery-kit <private-file>\n",
        "  greenbubbles-restore snapshot retention restore <quarantined> <restored-directory> (--snapshot-recovery-kit <private-file> | --snapshot-local-credential <private-file> | --snapshot-passphrase-stdin)\n\n",
        "Retention moves only a whole verified generation and never deletes it. Quarantine\n",
        "requires a newer linked replacement and proves that replacement through its\n",
        "portable 24-word recovery path. The move is same-filesystem, atomic, fsynced,\n",
        "re-verified at the destination, and rolled back automatically on failure.\n",
    )
}

const fn snapshot_retention_quarantine_help() -> &'static str {
    concat!(
        "Usage:\n",
        "  greenbubbles-restore snapshot retention quarantine <retiring> <replacement> <quarantine-directory> (--retiring-recovery-kit <private-file> | --retiring-local-credential <private-file> | --retiring-snapshot-passphrase-stdin) --replacement-recovery-kit <private-file>\n\n",
        "The retiring generation must verify with either of its protectors. The replacement\n",
        "must be newer, linked by parent identity or stable source identity, and verify\n",
        "using its portable recovery words. A retiring passphrase is read as stdin line 1.\n",
        "No age-only or local-only retirement is allowed.\n",
    )
}

const fn snapshot_retention_restore_help() -> &'static str {
    concat!(
        "Usage:\n",
        "  greenbubbles-restore snapshot retention restore <quarantined> <restored-directory> (--snapshot-recovery-kit <private-file> | --snapshot-local-credential <private-file> | --snapshot-passphrase-stdin)\n\n",
        "Authenticates the quarantined generation, atomically moves the whole directory to\n",
        "a new non-existing path on the same filesystem, fsyncs both parents, and verifies\n",
        "it again. A passphrase is read as stdin line 1. A failed post-move verification\n",
        "is rolled back automatically.\n",
    )
}

fn snapshot_retention_action_help(action: &str) -> Result<&'static str, String> {
    match action {
        "quarantine" => Ok(snapshot_retention_quarantine_help()),
        "restore" => Ok(snapshot_retention_restore_help()),
        _ => Err(format!(
            "unsupported snapshot retention action: {action}; expected 'quarantine' or 'restore'"
        )),
    }
}

fn snapshot_subcommand_help(subcommand: &str) -> Result<&'static str, String> {
    match subcommand {
        "recovery-kit" => Ok(snapshot_recovery_kit_help()),
        "local-credential" => Ok(snapshot_local_credential_help()),
        "create" => Ok(snapshot_create_help()),
        "create-capture" => Ok(snapshot_create_capture_help()),
        "verify" => Ok(snapshot_verify_help()),
        "rewrap" => Ok(snapshot_rewrap_help()),
        "retention" => Ok(snapshot_retention_help()),
        "rekey" => Ok(snapshot_rekey_help()),
        _ => Err(format!(
            "unsupported snapshot subcommand: {subcommand}; expected 'recovery-kit', 'local-credential', 'create', 'create-capture', 'verify', 'rewrap', 'retention', or 'rekey'"
        )),
    }
}

const fn connector_policy_direct_help() -> &'static str {
    concat!(
        "Usage:\n",
        "  greenbubbles-restore connector-policy-direct <source-root> <new-policy-file> <conversation-id>... --capabilities list,read,search --fields sender,created-at,type,content (--passphrase-stdin | --snapshot-recovery-kit <file> | --snapshot-local-credential <file> | --snapshot-passphrase-stdin | --snapshot-key-stdin | --decrypted) [--not-before-unix <seconds>] [--not-after-unix <seconds>] [--allow-remote-model] [--max-results <n>] [--max-summary-bytes <n>]\n\n",
        "Creates an owner-only policy in the direct SQLite identifier namespace. Every\n",
        "conversation is verified against the selected source before publication. The policy\n",
        "is bound to that source identity and cannot authorize draft or replica-only operations.\n",
    )
}

const fn connector_serve_direct_help() -> &'static str {
    concat!(
        "Usage:\n",
        "  greenbubbles-restore connector-serve-direct <source-root> <policy-file> <audit-log> <socket-path> (--passphrase-stdin | --snapshot-recovery-kit <file> | --snapshot-local-credential <file> | --snapshot-passphrase-stdin | --snapshot-key-stdin | --decrypted)\n\n",
        "Serves policy-scoped ordinary reads directly from live or snapshot SQLite. Requests\n",
        "remain typed, bounded, keyset-paginated, read-only, destination-scoped, and recorded\n",
        "in the same append-only privacy-safe audit format. Replica-only surfaces fail closed.\n",
    )
}

const fn connector_query_direct_help() -> &'static str {
    concat!(
        "Usage:\n",
        "  greenbubbles-restore connector-query-direct <source-root> <policy-file> <audit-log> <private-request-json> (--passphrase-stdin | --snapshot-recovery-kit <file> | --snapshot-local-credential <file> | --snapshot-passphrase-stdin | --snapshot-key-stdin | --decrypted)\n\n",
        "Runs one owner-only JSON connector request directly against live or snapshot SQLite\n",
        "and returns one bounded JSON response. No archive, replica, daemon, or corpus-sized\n",
        "JSON conversion is required; the normal connector policy and audit boundary remains.\n",
    )
}

fn ai_command_help(command: &str) -> Option<&'static str> {
    match command {
        "profile" => Some(query_profile_command_help()),
        "source" => Some(source_status_help()),
        "conversations" => Some(conversations_command_help()),
        "messages" => Some(messages_command_help()),
        "message" => Some(message_get_help()),
        "attachment" => Some(attachment_command_help()),
        "snapshot" => Some(snapshot_command_help()),
        "connector-policy-direct" => Some(connector_policy_direct_help()),
        "connector-query-direct" => Some(connector_query_direct_help()),
        "connector-serve-direct" => Some(connector_serve_direct_help()),
        "send" => Some(send_command_help()),
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

enum OwnedLiveQueryAccess {
    LiveEncrypted(DatabasePassphrase),
    SnapshotEncrypted(SnapshotKey),
    Decrypted,
}

struct ResolvedQueryInvocation {
    source_root: PathBuf,
    access: OwnedLiveQueryAccess,
    profile_name: Option<String>,
}

impl OwnedLiveQueryAccess {
    fn open_source<'a>(
        &'a self,
        selected_root: &Path,
    ) -> Result<LiveQuerySource<'a>, Box<dyn std::error::Error>> {
        match self {
            Self::LiveEncrypted(key) => Ok(LiveQuerySource::open(
                selected_root,
                QueryDatabaseAccess::LiveEncrypted(key.expose_for_database_operation()),
            )?),
            Self::SnapshotEncrypted(key) => {
                let data_root = recoverable_snapshot_data_root(selected_root)?;
                Ok(LiveQuerySource::open(
                    &data_root,
                    QueryDatabaseAccess::SnapshotEncrypted(key.expose_for_snapshot_operation()),
                )?)
            }
            Self::Decrypted => Ok(LiveQuerySource::open(
                selected_root,
                QueryDatabaseAccess::Decrypted,
            )?),
        }
    }

    fn open_attachment_source<'a>(
        &'a self,
        selected_root: &Path,
    ) -> Result<LiveQuerySource<'a>, Box<dyn std::error::Error>> {
        match self {
            Self::SnapshotEncrypted(_) => self.open_source(selected_root),
            Self::LiveEncrypted(_) | Self::Decrypted => {
                let database_root = if selected_root.join("db_storage").is_dir() {
                    selected_root.join("db_storage")
                } else {
                    selected_root.to_path_buf()
                };
                self.open_source(&database_root)
            }
        }
    }
}

fn split_optional_query_source(mut arguments: Vec<String>) -> (Option<PathBuf>, Vec<String>) {
    let source_root = arguments
        .first()
        .filter(|value| !value.starts_with("--"))
        .map(PathBuf::from);
    if source_root.is_some() {
        arguments.remove(0);
    }
    (source_root, arguments)
}

fn resolve_query_invocation(
    explicit_source_root: Option<PathBuf>,
    arguments: &[String],
) -> Result<ResolvedQueryInvocation, Box<dyn std::error::Error>> {
    let requested_profile = option_string(arguments, "--profile")?;
    if let Some(source_root) = explicit_source_root {
        if requested_profile.is_some() {
            return Err(
                "--profile cannot be combined with an explicit source root or access mode".into(),
            );
        }
        let access = load_live_query_access(&source_root, arguments)?;
        return Ok(ResolvedQueryInvocation {
            source_root,
            access,
            profile_name: None,
        });
    }
    if contains_database_access_option(arguments) {
        return Err(
            "database access options require an explicit source root and cannot override a profile"
                .into(),
        );
    }
    let (_, invocation) = load_configured_query_invocation(requested_profile.as_deref())?;
    Ok(invocation)
}

fn load_configured_query_invocation(
    requested_profile: Option<&str>,
) -> Result<(PathBuf, ResolvedQueryInvocation), Box<dyn std::error::Error>> {
    let (configuration_file, store) = QueryProfileStore::load_default()?;
    let (profile_name, profile) = store.select(requested_profile)?;
    let access = load_query_profile_access(profile)?;
    Ok((
        configuration_file,
        ResolvedQueryInvocation {
            source_root: profile.source_root.clone(),
            access,
            profile_name: Some(profile_name),
        },
    ))
}

fn load_query_profile_access(
    profile: &QueryProfile,
) -> Result<OwnedLiveQueryAccess, QueryProfileError> {
    match &profile.access {
        QueryProfileAccess::LiveWeChatKeyFile(access) => {
            let key = read_private_32_byte_credential(&access.credential_file)?;
            Ok(OwnedLiveQueryAccess::LiveEncrypted(
                DatabasePassphrase::from_bytes(*key),
            ))
        }
        QueryProfileAccess::SnapshotRawKeyFile(access) => {
            let key = read_private_32_byte_credential(&access.credential_file)?;
            Ok(OwnedLiveQueryAccess::SnapshotEncrypted(
                SnapshotKey::from_bytes(*key),
            ))
        }
        QueryProfileAccess::SnapshotPassphraseFile(access) => {
            let passphrase = read_private_snapshot_passphrase(&access.credential_file)?;
            let key =
                unlock_recoverable_snapshot_with_passphrase(&profile.source_root, &passphrase)
                    .map_err(|_| {
                        QueryProfileError::InvalidCredential(
                            "snapshot passphrase could not unlock the selected snapshot".into(),
                        )
                    })?;
            Ok(OwnedLiveQueryAccess::SnapshotEncrypted(key))
        }
        QueryProfileAccess::SnapshotRecoveryKit(access) => {
            let words = SnapshotRecoveryWords::read_private_file(&access.credential_file).map_err(
                |_| {
                    QueryProfileError::InvalidCredential(
                        "recovery kit could not be loaded as a private credential".into(),
                    )
                },
            )?;
            let key = unlock_recoverable_snapshot_with_recovery_words(&profile.source_root, &words)
                .map_err(|_| {
                    QueryProfileError::InvalidCredential(
                        "recovery kit could not unlock the selected snapshot".into(),
                    )
                })?;
            Ok(OwnedLiveQueryAccess::SnapshotEncrypted(key))
        }
        QueryProfileAccess::SnapshotLocalCredential(access) => {
            let credential = SnapshotLocalCredential::read_private_file(&access.credential_file)
                .map_err(|_| {
                    QueryProfileError::InvalidCredential(
                        "local snapshot credential could not be loaded privately".into(),
                    )
                })?;
            let key = unlock_recoverable_snapshot_with_local_credential(
                &profile.source_root,
                &credential,
            )
            .map_err(|_| {
                QueryProfileError::InvalidCredential(
                    "local credential could not unlock the selected snapshot".into(),
                )
            })?;
            Ok(OwnedLiveQueryAccess::SnapshotEncrypted(key))
        }
        QueryProfileAccess::Decrypted(_) => Ok(OwnedLiveQueryAccess::Decrypted),
    }
}

fn contains_database_access_option(arguments: &[String]) -> bool {
    const OPTIONS: [&str; 6] = [
        "--passphrase-stdin",
        "--snapshot-key-stdin",
        "--snapshot-passphrase-stdin",
        "--snapshot-recovery-kit",
        "--snapshot-local-credential",
        "--decrypted",
    ];
    arguments
        .iter()
        .any(|argument| OPTIONS.contains(&argument.as_str()))
}

fn attachment_account_root(selected_root: &Path) -> Option<PathBuf> {
    if selected_root.join("msg").is_dir() && selected_root.join("db_storage").is_dir() {
        return Some(selected_root.to_path_buf());
    }
    selected_root
        .file_name()
        .is_some_and(|name| name == "db_storage")
        .then(|| selected_root.parent())
        .flatten()
        .filter(|parent| parent.join("msg").is_dir())
        .map(Path::to_path_buf)
}

fn reject_database_access_options(arguments: &[String]) -> Result<(), String> {
    const ACCESS_OPTIONS: [&str; 6] = [
        "--passphrase-stdin",
        "--snapshot-key-stdin",
        "--snapshot-passphrase-stdin",
        "--snapshot-recovery-kit",
        "--snapshot-local-credential",
        "--decrypted",
    ];
    if arguments
        .iter()
        .any(|argument| ACCESS_OPTIONS.contains(&argument.as_str()))
    {
        return Err(
            "database access options require --message; legacy image-MD5 lookup reads no database"
                .into(),
        );
    }
    Ok(())
}

fn load_live_query_access(
    selected_root: &Path,
    arguments: &[String],
) -> Result<OwnedLiveQueryAccess, Box<dyn std::error::Error>> {
    let encrypted = arguments.iter().any(|value| value == "--passphrase-stdin");
    let snapshot_key = arguments
        .iter()
        .any(|value| value == "--snapshot-key-stdin");
    let snapshot_passphrase = arguments
        .iter()
        .any(|value| value == "--snapshot-passphrase-stdin");
    let recovery_kit = option_path(arguments, "--snapshot-recovery-kit")?;
    let local_credential = option_path(arguments, "--snapshot-local-credential")?;
    let decrypted = arguments.iter().any(|value| value == "--decrypted");
    if usize::from(encrypted)
        + usize::from(snapshot_key)
        + usize::from(snapshot_passphrase)
        + usize::from(recovery_kit.is_some())
        + usize::from(local_credential.is_some())
        + usize::from(decrypted)
        != 1
    {
        return Err(
            "choose exactly one database access mode: --passphrase-stdin, --snapshot-key-stdin, --snapshot-passphrase-stdin, --snapshot-recovery-kit, --snapshot-local-credential, or --decrypted".into(),
        );
    }
    if encrypted {
        Ok(OwnedLiveQueryAccess::LiveEncrypted(
            DatabasePassphrase::read_stdin()?,
        ))
    } else if snapshot_key {
        Ok(OwnedLiveQueryAccess::SnapshotEncrypted(
            SnapshotKey::read_stdin()?,
        ))
    } else if snapshot_passphrase {
        let passphrase = SnapshotPassphrase::read_stdin()?;
        Ok(OwnedLiveQueryAccess::SnapshotEncrypted(
            unlock_recoverable_snapshot_with_passphrase(selected_root, &passphrase)?,
        ))
    } else if let Some(recovery_kit) = recovery_kit {
        let recovery_words = SnapshotRecoveryWords::read_private_file(&recovery_kit)?;
        Ok(OwnedLiveQueryAccess::SnapshotEncrypted(
            unlock_recoverable_snapshot_with_recovery_words(selected_root, &recovery_words)?,
        ))
    } else if let Some(local_credential) = local_credential {
        let local_credential = SnapshotLocalCredential::read_private_file(&local_credential)?;
        Ok(OwnedLiveQueryAccess::SnapshotEncrypted(
            unlock_recoverable_snapshot_with_local_credential(selected_root, &local_credential)?,
        ))
    } else {
        Ok(OwnedLiveQueryAccess::Decrypted)
    }
}

enum OwnedSnapshotCreateSourceAccess {
    LiveEncrypted(DatabasePassphrase),
    Decrypted,
}

impl OwnedSnapshotCreateSourceAccess {
    fn material(&self) -> QueryDatabaseAccess<'_> {
        match self {
            Self::LiveEncrypted(key) => {
                QueryDatabaseAccess::LiveEncrypted(key.expose_for_database_operation())
            }
            Self::Decrypted => QueryDatabaseAccess::Decrypted,
        }
    }
}

fn load_snapshot_create_source_access(
    arguments: &[String],
) -> Result<OwnedSnapshotCreateSourceAccess, Box<dyn std::error::Error>> {
    let encrypted = arguments
        .iter()
        .any(|value| value == "--source-passphrase-stdin");
    let decrypted = arguments.iter().any(|value| value == "--source-decrypted");
    if encrypted == decrypted {
        return Err(
            "choose exactly one snapshot source mode: --source-passphrase-stdin or --source-decrypted"
                .into(),
        );
    }
    if encrypted {
        Ok(OwnedSnapshotCreateSourceAccess::LiveEncrypted(
            DatabasePassphrase::read_stdin()?,
        ))
    } else {
        Ok(OwnedSnapshotCreateSourceAccess::Decrypted)
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

fn validate_command_options(
    arguments: &[String],
    value_options: &[&str],
    flags: &[&str],
) -> Result<(), String> {
    let value_options = value_options.iter().copied().collect::<BTreeSet<_>>();
    let flags = flags.iter().copied().collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    let mut index = 0usize;
    while index < arguments.len() {
        let option = arguments[index].as_str();
        if !option.starts_with("--") {
            return Err(format!("unexpected positional argument: {option}"));
        }
        if !seen.insert(option) {
            return Err(format!("option may be supplied only once: {option}"));
        }
        if value_options.contains(option) {
            let value = arguments
                .get(index + 1)
                .filter(|value| !value.starts_with("--"))
                .ok_or_else(|| format!("missing value for {option}"))?;
            if value.is_empty() {
                return Err(format!("empty value for {option}"));
            }
            index += 2;
        } else if flags.contains(option) {
            index += 1;
        } else {
            return Err(format!("unsupported option: {option}"));
        }
    }
    Ok(())
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

/// Dispatches the `send` command group: the deterministic UI-automation send
/// adapter's control plane. Every subcommand is fail-closed, writes only
/// owner-only files, and never accepts message text or key material in an
/// argument.
fn run_send_command(
    mut arguments: std::iter::Peekable<impl Iterator<Item = String>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let subcommand = arguments
        .next()
        .ok_or("missing send subcommand; run 'greenbubbles-restore send --help'")?;
    if matches!(arguments.peek().map(String::as_str), Some("--help" | "-h")) {
        println!("{}", send_command_help());
        return Ok(());
    }
    match subcommand.as_str() {
        "config-template" => {
            println!("{}", send_config_template()?);
        }
        "profile-template" => {
            println!("{}", send_profile_template()?);
        }
        "profile-keygen" => {
            let output = required_path(arguments.next(), "new private signing-seed file")?;
            let mut seed = Zeroizing::new([0_u8; 32]);
            getrandom::fill(seed.as_mut_slice())
                .map_err(|_| "the operating system refused to provide random bytes")?;
            write_owner_only_json(
                &output,
                &serde_json::json!({
                    "formatVersion": 1,
                    "algorithm": "ed25519",
                    "signingKeySeedHex": hex::encode(seed.as_slice()),
                    "publicKeyHex": signing_key_public_hex(&seed),
                }),
            )?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "publicKeyHex": signing_key_public_hex(&seed),
                    "pinInstruction":
                        "build the verifying binaries with GREENBUBBLES_SEND_RELEASE_PUBLIC_KEYS set to this value",
                }))?
            );
        }
        "profile-sign" | "matrix-sign" => {
            let body_path = required_path(arguments.next(), "unsigned document")?;
            let output = required_path(arguments.next(), "new signed document")?;
            let remaining = arguments.collect::<Vec<_>>();
            validate_command_options(&remaining, &["--signing-key-file"], &[])?;
            let key_path = PathBuf::from(required_option(&remaining, "--signing-key-file")?);
            let seed = load_signing_key_seed(&key_path)?;
            if subcommand == "profile-sign" {
                let body: CalibrationProfileBody =
                    serde_json::from_slice(&read_owner_only_document(&body_path)?)?;
                write_owner_only_json(&output, &sign_calibration_profile(&body, &seed)?)?;
            } else {
                let body: CompatibilityMatrixBody =
                    serde_json::from_slice(&read_owner_only_document(&body_path)?)?;
                write_owner_only_json(&output, &sign_compatibility_matrix(&body, &seed)?)?;
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "signed": output.display().to_string(),
                    "publicKeyHex": signing_key_public_hex(&seed),
                }))?
            );
        }
        "profile-verify" | "matrix-verify" => {
            let document = required_path(arguments.next(), "signed document")?;
            let remaining = arguments.collect::<Vec<_>>();
            validate_command_options(&remaining, &["--development-trust-root"], &[])?;
            let trust_root = load_send_trust_root(&remaining)?;
            let now = send_unix_seconds()?;
            if subcommand == "profile-verify" {
                let verified = load_calibration_profile(&document, &trust_root, now)?;
                println!("{}", serde_json::to_string_pretty(&verified)?);
            } else {
                let verified = load_compatibility_matrix(&document, &trust_root, now)?;
                println!("{}", serde_json::to_string_pretty(&verified)?);
            }
        }
        "doctor" => {
            let config = required_path(arguments.next(), "send configuration")?;
            let remaining = arguments.collect::<Vec<_>>();
            validate_command_options(&remaining, &[], &["--no-helper"])?;
            let adapter = SendAdapter::load(&config)?;
            let dispatcher = send_dispatcher(&adapter, &remaining)?;
            let report = adapter.doctor(
                dispatcher.as_ref().map(|value| value.as_ref()),
                send_unix_nanoseconds()?,
            )?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "selftest" => {
            let config = required_path(arguments.next(), "send configuration")?;
            let adapter = SendAdapter::load(&config)?;
            let dispatcher = ProcessSendDispatcher::new(&adapter.config().helper)?;
            let report = adapter.calibration_selftest(&dispatcher, send_unix_nanoseconds()?)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "approval-binding" => {
            let config = required_path(arguments.next(), "send configuration")?;
            let draft_path = required_path(arguments.next(), "immutable draft")?;
            let adapter = SendAdapter::load(&config)?;
            let draft = load_action_draft(&draft_path)?;
            let binding = adapter
                .expected_approval_binding(&draft)
                .ok_or("the draft cannot be bound to this adapter configuration")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "draftId": draft.draft_id,
                    "conversationId": draft.conversation_id,
                    "humanRecipient": draft.recipient.human_label,
                    "renderedTextSha256": draft.rendered_text_sha256,
                    "immutableBindingSha256": binding,
                }))?
            );
        }
        "approve" => {
            let config = required_path(arguments.next(), "send configuration")?;
            let draft_path = required_path(arguments.next(), "immutable draft")?;
            let output = required_path(arguments.next(), "new approval-evidence file")?;
            let remaining = arguments.collect::<Vec<_>>();
            validate_command_options(
                &remaining,
                &["--approver", "--validity-seconds"],
                &["--confirm"],
            )?;
            if !remaining.iter().any(|value| value == "--confirm") {
                return Err(
                    "approval requires an explicit --confirm after reviewing the printed recipient"
                        .into(),
                );
            }
            let approver = required_option(&remaining, "--approver")?;
            let validity = option_u64(&remaining, "--validity-seconds")?.unwrap_or(600);
            if approver.is_empty() || !(60..=3_600).contains(&validity) {
                return Err("approver must be named and validity must be 60..3600 seconds".into());
            }
            let adapter = SendAdapter::load(&config)?;
            let draft = load_action_draft(&draft_path)?;
            let binding = adapter
                .expected_approval_binding(&draft)
                .ok_or("the draft cannot be bound to this adapter configuration")?;
            let now = send_unix_nanoseconds()?;
            let mut nonce = [0_u8; 32];
            getrandom::fill(&mut nonce)
                .map_err(|_| "the operating system refused to provide random bytes")?;
            let approval = ExternalApprovalEvidence {
                approval_id: hex::encode(<sha2::Sha256 as sha2::Digest>::digest(
                    [
                        b"greenbubbles.send.approval-identity.v1".as_slice(),
                        binding.as_bytes(),
                        approver.as_bytes(),
                        &now.to_le_bytes(),
                        &nonce,
                    ]
                    .concat(),
                )),
                immutable_binding_sha256: binding,
                approver_id: approver,
                approved_at_unix_nanoseconds: now,
                expires_at_unix_nanoseconds: now
                    .saturating_add(u128::from(validity).saturating_mul(1_000_000_000)),
            };
            eprintln!(
                "approving a send to \"{}\" ({} bytes, sha256 {})",
                draft.recipient.human_label,
                draft.rendered_text.len(),
                draft.rendered_text_sha256
            );
            write_owner_only_json(&output, &approval)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "approvalId": approval.approval_id,
                    "draftId": draft.draft_id,
                    "expiresAtUnixNanoseconds": approval.expires_at_unix_nanoseconds,
                    "idempotencyKey":
                        adapter.idempotency_key(&draft.draft_id, &approval.approval_id),
                }))?
            );
        }
        "precheck" | "submit" => {
            let config = required_path(arguments.next(), "send configuration")?;
            let draft_path = required_path(arguments.next(), "immutable draft")?;
            let approval_path = required_path(arguments.next(), "approval evidence")?;
            let remaining = arguments.collect::<Vec<_>>();
            validate_command_options(&remaining, &[], &["--no-helper"])?;
            let adapter = SendAdapter::load(&config)?;
            let draft = load_action_draft(&draft_path)?;
            let approval: ExternalApprovalEvidence =
                serde_json::from_slice(&read_owner_only_document(&approval_path)?)?;
            let now = send_unix_nanoseconds()?;
            if subcommand == "precheck" {
                let dispatcher = send_dispatcher(&adapter, &remaining)?;
                let report = adapter.precheck_from_disk(
                    &draft,
                    &approval,
                    dispatcher.as_ref().map(|value| value.as_ref()),
                    now,
                )?;
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                let dispatcher = ProcessSendDispatcher::new(&adapter.config().helper)?;
                let report = adapter.execute(&draft, &approval, &dispatcher, now)?;
                println!("{}", serde_json::to_string_pretty(&report)?);
            }
        }
        "recall-window" => {
            let config = required_path(arguments.next(), "send configuration")?;
            let remaining = arguments.collect::<Vec<_>>();
            validate_command_options(&remaining, &["--idempotency-key"], &[])?;
            let adapter = SendAdapter::load(&config)?;
            let key = required_option(&remaining, "--idempotency-key")?;
            let report = adapter.recall_window(&key, send_unix_nanoseconds()?)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "outbox-status" => {
            let config = required_path(arguments.next(), "send configuration")?;
            let adapter = SendAdapter::load(&config)?;
            let now = send_unix_nanoseconds()?;
            let (summary, recovery, pending) = adapter.outbox_status(now)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "outbox": summary,
                    "recovery": recovery,
                    "pendingReconciliation": pending,
                }))?
            );
        }
        "reconcile" => {
            let config = required_path(arguments.next(), "send configuration")?;
            let draft_path = required_path(arguments.next(), "immutable draft")?;
            let remaining = arguments.collect::<Vec<_>>();
            validate_command_options(
                &remaining,
                &[
                    "--idempotency-key",
                    "--replica",
                    "--observation",
                    "--lookback-seconds",
                ],
                &["--replica-key-stdin"],
            )?;
            let adapter = SendAdapter::load(&config)?;
            let draft = load_action_draft(&draft_path)?;
            let key = required_option(&remaining, "--idempotency-key")?;
            let now = send_unix_nanoseconds()?;
            let observation = match option_string(&remaining, "--observation")? {
                Some(path) => {
                    serde_json::from_slice(&read_owner_only_document(&PathBuf::from(path))?)?
                }
                None => {
                    let replica = PathBuf::from(required_option(&remaining, "--replica")?);
                    if !remaining.iter().any(|value| value == "--replica-key-stdin") {
                        return Err("replica reconciliation requires --replica-key-stdin".into());
                    }
                    let (_, _, pending) = adapter.outbox_status(now)?;
                    let entry = pending
                        .into_iter()
                        .find(|entry| entry.idempotency_key == key)
                        .ok_or("no parked attempt matches this idempotency key")?;
                    let lookback = option_i64(&remaining, "--lookback-seconds")?
                        .unwrap_or(300)
                        .max(0);
                    let replica_key = ReplicaKey::read_stdin()?;
                    observe_send_in_replica(&replica, &replica_key, &entry, lookback, now)?
                }
            };
            let report = adapter.reconcile(&observation, Some(&draft), now)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        _ => {
            return Err(format!(
                "unsupported send subcommand: {subcommand}; run 'greenbubbles-restore send --help'"
            )
            .into())
        }
    }
    Ok(())
}

/// Builds a process dispatcher unless the caller explicitly opted out. Opting
/// out never makes the send path *more* permissive: a missing helper status is
/// itself a PRECHECK failure.
fn send_dispatcher(
    adapter: &SendAdapter,
    remaining: &[String],
) -> Result<Option<Box<dyn SendDispatcher>>, Box<dyn std::error::Error>> {
    if remaining.iter().any(|value| value == "--no-helper") {
        return Ok(None);
    }
    Ok(Some(Box::new(ProcessSendDispatcher::new(
        &adapter.config().helper,
    )?)))
}

fn load_send_trust_root(remaining: &[String]) -> Result<SendTrustRoot, Box<dyn std::error::Error>> {
    match option_string(remaining, "--development-trust-root")? {
        Some(path) => Ok(SendTrustRoot::load_development(&PathBuf::from(path))?),
        None => Ok(SendTrustRoot::pinned()
            .map_err(|_| "the pinned release trust root is malformed in this build")?),
    }
}

fn load_signing_key_seed(path: &Path) -> Result<Zeroizing<[u8; 32]>, Box<dyn std::error::Error>> {
    let document: serde_json::Value = serde_json::from_slice(&read_owner_only_document(path)?)?;
    let encoded = document
        .get("signingKeySeedHex")
        .and_then(serde_json::Value::as_str)
        .ok_or("signing-key file does not contain signingKeySeedHex")?;
    let bytes = hex::decode(encoded).map_err(|_| "signing-key seed is not hexadecimal")?;
    let seed: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| "signing-key seed must be exactly 32 bytes")?;
    Ok(Zeroizing::new(seed))
}

fn read_owner_only_document(path: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.nlink() != 1
        || metadata.len() > 1_048_576
    {
        return Err(format!(
            "{} must be a bounded, owner-only, single-link regular file",
            path.display()
        )
        .into());
    }
    Ok(std::fs::read(path)?)
}

fn send_unix_nanoseconds() -> Result<u128, Box<dyn std::error::Error>> {
    Ok(adapter_unix_nanoseconds()?)
}

fn send_unix_seconds() -> Result<u64, Box<dyn std::error::Error>> {
    Ok((adapter_unix_nanoseconds()? / 1_000_000_000) as u64)
}

const fn send_command_help() -> &'static str {
    concat!(
        "Usage:\n",
        "  greenbubbles-restore send config-template\n",
        "  greenbubbles-restore send profile-template\n",
        "  greenbubbles-restore send profile-keygen <new-private-seed-file>\n",
        "  greenbubbles-restore send profile-sign <unsigned-profile> <new-signed-profile> --signing-key-file <file>\n",
        "  greenbubbles-restore send profile-verify <signed-profile> [--development-trust-root <file>]\n",
        "  greenbubbles-restore send matrix-sign <unsigned-matrix> <new-signed-matrix> --signing-key-file <file>\n",
        "  greenbubbles-restore send matrix-verify <signed-matrix> [--development-trust-root <file>]\n",
        "  greenbubbles-restore send doctor <config> [--no-helper]\n",
        "  greenbubbles-restore send selftest <config>\n",
        "  greenbubbles-restore send approval-binding <config> <draft-file>\n",
        "  greenbubbles-restore send approve <config> <draft-file> <new-approval-file> --approver <id> [--validity-seconds <60..3600>] --confirm\n",
        "  greenbubbles-restore send precheck <config> <draft-file> <approval-file> [--no-helper]\n",
        "  greenbubbles-restore send submit <config> <draft-file> <approval-file>\n",
        "  greenbubbles-restore send outbox-status <config>\n",
        "  greenbubbles-restore send recall-window <config> --idempotency-key <hex>\n",
        "  greenbubbles-restore send reconcile <config> <draft-file> --idempotency-key <hex> (--replica <path> --replica-key-stdin [--lookback-seconds <n>] | --observation <file>)\n\n",
        "The send adapter drives the real WeChat client's user interface through a\n",
        "privilege-separated, first-party input helper. It is fail-closed at every\n",
        "step: an unsigned or expired calibration profile, an unverified macOS/WeChat\n",
        "build pair, a missing TCC grant, a recipient whose on-screen title does not\n",
        "match the approved draft, or an unreconciled earlier attempt all keep the\n",
        "send path shut. `observedSent` is created only by replica reconciliation.\n\n",
        "Rollout stages:\n",
        "  dryRun       Run every step including both verification gates, stop before Return\n",
        "  selfSend     Send only to the account's own File Transfer conversation\n",
        "  allowListed  Send to one additional reviewed conversation under volume caps\n\n",
        "Options:\n",
        "  --no-helper  Evaluate without contacting the input helper (never permits a send)\n",
        "  --confirm    Required acknowledgement when recording local approval evidence\n",
        "  -h, --help   Show this help\n",
    )
}

/// A documented configuration skeleton. It is deliberately emitted in the
/// safest possible state: dry run, kill switch engaged, no gate evidence.
fn send_config_template() -> Result<String, Box<dyn std::error::Error>> {
    let home = env::var("HOME").unwrap_or_else(|_| "/Users/you".to_string());
    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "formatVersion": 1,
        "accountId": "REPLACE-WITH-THE-REPLICA-ACCOUNT-ID",
        "rolloutStage": "dryRun",
        "globalKillSwitchEngaged": true,
        "gate": {
            "gateDecisionId": "0".repeat(64),
            "acquisitionGatePassed": false,
            "restorationGatePassed": false,
            "mechanismApproved": false,
            "legalReviewApproved": false
        },
        "adapter": {
            "adapterId": SEND_ADAPTER_ID,
            "adapterVersion": SEND_ADAPTER_VERSION,
            "clientBuildProfileId": "wechat-macos-4.1.13-269579"
        },
        "allowList": {
            "accountIds": ["REPLACE-WITH-THE-REPLICA-ACCOUNT-ID"],
            "conversationIds": ["filehelper"],
            "capabilities": ["textSend"]
        },
        "selfSendConversationId": "filehelper",
        "searchKeyOverrides": {},
        "attemptWindowSeconds": 3600,
        "maximumAttemptsPerWindow": 3,
        "circuitBreakerFailureThreshold": 3,
        "circuitBreakerCooldownSeconds": 900,
        "capabilityValiditySeconds": 120,
        "reconciliationGraceSeconds": 900,
        "recallWindowSeconds": 120,
        "expectedMacosBuild": "25G83",
        "expectedMacosMajor": 26,
        "expectedWechatBuild": "4.1.13.269579",
        "calibrationProfilePath": format!("{home}/.greenbubbles/send/calibration-profile.json"),
        "compatibilityMatrixPath": format!("{home}/.greenbubbles/send/compatibility-matrix.json"),
        "outboxDirectory": format!("{home}/.greenbubbles/send/outbox"),
        "auditLogPath": format!("{home}/.greenbubbles/connector-audit.ndjson"),
        "draftDirectory": format!("{home}/.greenbubbles/drafts"),
        "helper": {
            "dispatcherExecutable":
                "/Applications/GreenBubbles.app/Contents/MacOS/greenbubbles-send",
            "dispatcherArguments": [],
            "machServiceName": "me.greenbubbles.InputHelper",
            "statusTimeoutMilliseconds": 5000,
            "selftestTimeoutMilliseconds": 20000,
            "sendTimeoutMilliseconds": 45000
        }
    }))?)
}

/// An unsigned calibration-profile skeleton keyed to the pinned client build.
/// Anchors and regions are the spike's measured values and must be re-measured
/// and re-signed for every new WeChat layout.
fn send_profile_template() -> Result<String, Box<dyn std::error::Error>> {
    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "schema": 1,
        "profileId": "wechat-4.1.13.269579-macos-26",
        "wechatBundleIdentifier": "com.tencent.xinWeChat",
        "wechatMarketingVersion": "4.1.13",
        "wechatBuild": "4.1.13.269579",
        "clientBuildProfileId": "wechat-macos-4.1.13-269579",
        "macosMajor": 26,
        "anchors": {
            "searchBox": {"xPartsPerMillion": 235000, "yPartsPerMillion": 36000},
            "firstResultRow": {"xPartsPerMillion": 235000, "yPartsPerMillion": 115000},
            "composeBox": {"xPartsPerMillion": 715000, "yPartsPerMillion": 870000}
        },
        "ocrRegions": {
            "title": {
                "xPartsPerMillion": 440000, "yPartsPerMillion": 20000,
                "widthPartsPerMillion": 300000, "heightPartsPerMillion": 50000
            },
            "compose": {
                "xPartsPerMillion": 400000, "yPartsPerMillion": 830000,
                "widthPartsPerMillion": 560000, "heightPartsPerMillion": 110000
            },
            "newestOutgoing": {
                "xPartsPerMillion": 620000, "yPartsPerMillion": 700000,
                "widthPartsPerMillion": 280000, "heightPartsPerMillion": 200000
            }
        },
        "selftest": {
            "focusIndicator": "search_caret",
            "minimumTitleConfidencePartsPerMillion": 900000
        },
        "issuedAtUnixSeconds": 0,
        "expiresAtUnixSeconds": 0
    }))?)
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
