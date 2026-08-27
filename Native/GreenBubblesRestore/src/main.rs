use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::io::{self, Read};
use std::path::PathBuf;

use greenbubbles_restore::{
    archive::{create_conversation_policy, read_conversation_page},
    benchmark::{run_synthetic_benchmark, SyntheticBenchmarkConfig},
    connector::{ConnectorDestination, ConnectorService},
    merge::merge_incremental_archive,
    preflight_snapshot, prepare_catalog,
    reconcile::reconcile_archives,
    replica::{
        bootstrap_replica, get_replica_changes, get_replica_message, list_replica_conversations,
        load_replica_message_filter, replica_coverage, replica_status,
        search_replica_cached_moments, search_replica_messages, synchronize_replica,
        ReplicaCachedMomentFilter,
    },
    restore_catalog,
    tools::{
        create_tool_policy_with_cached_moments, CachedMomentField, CachedMomentsToolScope,
        ConversationToolScope, LocalToolService, ToolCapability, ToolDataDestination,
        ToolMessageField,
    },
    transport::{load_connector_request, run_mcp_adapter, send_unix_request, serve_unix},
    DatabasePassphrase, ReplicaKey, RestorationOptions,
};
use zeroize::Zeroizing;

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args().skip(1);
    let command = arguments.next().unwrap_or_else(|| "help".to_string());
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
            let use_passphrase = arguments.any(|value| value == "--passphrase-stdin");
            let passphrase = if use_passphrase {
                Some(DatabasePassphrase::read_stdin()?)
            } else {
                None
            };
            let catalog = prepare_catalog(&snapshot, passphrase.as_ref())?;
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
            let report = preflight_snapshot(&snapshot)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "restore" => {
            let snapshot = required_path(arguments.next(), "snapshot directory")?;
            let output = required_path(arguments.next(), "output directory")?;
            let remaining = arguments.collect::<Vec<_>>();
            let use_passphrase = remaining.iter().any(|value| value == "--passphrase-stdin");
            let account_root = option_path(&remaining, "--account-root")?;
            let defer_media = remaining.iter().any(|value| value == "--defer-media");
            let passphrase = if use_passphrase {
                Some(DatabasePassphrase::read_stdin()?)
            } else {
                None
            };
            let catalog = prepare_catalog(&snapshot, passphrase.as_ref())?;
            let report = restore_catalog(
                &catalog,
                &RestorationOptions {
                    output_directory: output,
                    account_root,
                    defer_media,
                },
            )?;
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
            let key = ReplicaKey::read_stdin()?;
            let report = bootstrap_replica(&archive, &replica, &key)?;
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
        "replica-sync" => {
            let archive = required_path(arguments.next(), "archive directory")?;
            let replica = required_path(arguments.next(), "replica path")?;
            let remaining = arguments.collect::<Vec<_>>();
            if !remaining.iter().any(|value| value == "--replica-key-stdin") {
                return Err("replica keys must be supplied with --replica-key-stdin".into());
            }
            let key = ReplicaKey::read_stdin()?;
            let report = synchronize_replica(&archive, &replica, &key)?;
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
        "connector-mcp" => {
            let socket = required_path(arguments.next(), "Unix socket path")?;
            let remaining = arguments.collect::<Vec<_>>();
            let requester = required_option(&remaining, "--requester")?;
            let destination =
                parse_connector_destination(option_string(&remaining, "--destination")?)?;
            run_mcp_adapter(&socket, &requester, destination)?;
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
            let capabilities = match option_string(&remaining, "--capabilities")? {
                Some(value) => parse_capabilities(&value)?,
                None if conversations.is_empty() => BTreeSet::new(),
                None => return Err("missing --capabilities".into()),
            };
            let message_fields = option_string(&remaining, "--fields")?
                .map(|value| parse_message_fields(&value))
                .transpose()?
                .unwrap_or_default();
            let not_before_unix = option_i64(&remaining, "--not-before-unix")?;
            let not_after_unix = option_i64(&remaining, "--not-after-unix")?;
            let allow_remote_model = remaining
                .iter()
                .any(|value| value == "--allow-remote-model");
            let scopes = conversations
                .into_iter()
                .map(|conversation| {
                    (
                        conversation,
                        ConversationToolScope {
                            capabilities: capabilities.clone(),
                            message_fields: message_fields.clone(),
                            not_before_unix,
                            not_after_unix,
                            allow_remote_model,
                        },
                    )
                })
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
            let policy = create_tool_policy_with_cached_moments(
                &archive,
                &policy_path,
                scopes,
                cached_moments_scope,
                option_usize(&remaining, "--max-results")?.unwrap_or(100),
                option_usize(&remaining, "--max-summary-bytes")?.unwrap_or(4_096),
                option_usize(&remaining, "--max-draft-bytes")?.unwrap_or(16_384),
            )?;
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
                    "  greenbubbles-restore preflight <snapshot>\n",
                    "  greenbubbles-restore probe <snapshot> [--passphrase-stdin]\n",
                    "  greenbubbles-restore restore <snapshot> <output> [--account-root <path>] [--defer-media] [--passphrase-stdin]\n",
                    "  greenbubbles-restore policy <archive> <policy-file> <conversation-id>... [--max-page-size <n>]\n",
                    "  greenbubbles-restore read <archive> <policy-file> <conversation-id> [--cursor <cursor>] [--limit <n>]\n",
                    "  greenbubbles-restore reconcile <previous-archive> <current-archive> <policy-file> <events-output>\n",
                    "  greenbubbles-restore merge-incremental <previous-archive> <fragment-archive> <output-archive>\n",
                    "  greenbubbles-restore replica-bootstrap <archive> <replica-path> --replica-key-stdin\n",
                    "  greenbubbles-restore replica-status <replica-path> --replica-key-stdin\n",
                    "  greenbubbles-restore replica-sync <archive> <replica-path> --replica-key-stdin\n",
                    "  greenbubbles-restore replica-changes <replica-path> --replica-key-stdin [--cursor <cursor>] [--limit <n>]\n",
                    "  greenbubbles-restore replica-search <replica-path> <private-filter-json> --replica-key-stdin [--cursor <cursor>] [--limit <n>]\n",
                    "  greenbubbles-restore replica-cached-moments <replica-path> --replica-key-stdin [--author <opaque-id>] [--content-type <n>] [--not-before-unix <seconds>] [--not-after-unix <seconds>] [--cursor <cursor>] [--limit <n>]\n",
                    "  greenbubbles-restore replica-message <replica-path> <canonical-id> --replica-key-stdin\n",
                    "  greenbubbles-restore replica-conversations <replica-path> --replica-key-stdin [--limit <n>]\n",
                    "  greenbubbles-restore replica-coverage <replica-path> --replica-key-stdin\n",
                    "  greenbubbles-restore connector-serve <replica-path> <policy-file> <audit-log> <draft-directory> <socket-path> --replica-key-stdin\n",
                    "  greenbubbles-restore connector-call <socket-path> <private-request-json>\n",
                    "  greenbubbles-restore connector-mcp <socket-path> --requester <id> [--destination local|remote]\n",
                    "  greenbubbles-restore tool-policy <archive> <policy-file> [<conversation-id>...] [--capabilities list,read,search,draft] [--fields sender,created-at,direction,type,content,attachments,relationships] [--not-before-unix <seconds>] [--not-after-unix <seconds>] [--allow-remote-model] [--enable-cached-moments --cached-fields author,created-at,type,content,title,description,url,media-count,like-count,comment-count] [--cached-not-before-unix <seconds>] [--cached-not-after-unix <seconds>] [--allow-cached-remote-model] [--max-results <n>] [--max-summary-bytes <n>] [--max-draft-bytes <n>]\n",
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

fn option_usize(arguments: &[String], option: &str) -> Result<Option<usize>, String> {
    option_string(arguments, option)?
        .map(|value| {
            value
                .parse::<usize>()
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
