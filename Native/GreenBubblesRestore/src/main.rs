use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::io::{self, Read};
use std::path::PathBuf;

use greenbubbles_restore::{
    archive::{create_conversation_policy, read_conversation_page},
    prepare_catalog,
    reconcile::reconcile_archives,
    replica::{bootstrap_replica, replica_status},
    restore_catalog,
    tools::{
        create_tool_policy, ConversationToolScope, LocalToolService, ToolCapability,
        ToolDataDestination, ToolMessageField,
    },
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
                "databaseCount": catalog.databases.len(),
                "storageFamilies": catalog.storage_family_counts(),
                "databases": catalog.databases,
            });
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "restore" => {
            let snapshot = required_path(arguments.next(), "snapshot directory")?;
            let output = required_path(arguments.next(), "output directory")?;
            let remaining = arguments.collect::<Vec<_>>();
            let use_passphrase = remaining.iter().any(|value| value == "--passphrase-stdin");
            let account_root = option_path(&remaining, "--account-root")?;
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
        "tool-policy" => {
            let archive = required_path(arguments.next(), "archive directory")?;
            let policy_path = required_path(arguments.next(), "tool policy path")?;
            let remaining = arguments.collect::<Vec<_>>();
            let conversations = remaining
                .iter()
                .take_while(|value| !value.starts_with("--"))
                .cloned()
                .collect::<BTreeSet<_>>();
            let capabilities = option_string(&remaining, "--capabilities")?
                .ok_or_else(|| "missing --capabilities".to_string())
                .and_then(|value| parse_capabilities(&value))?;
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
            let policy = create_tool_policy(
                &archive,
                &policy_path,
                scopes,
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
                "Usage:\n  greenbubbles-restore probe <snapshot> [--passphrase-stdin]\n  greenbubbles-restore restore <snapshot> <output> [--account-root <path>] [--passphrase-stdin]\n  greenbubbles-restore policy <archive> <policy-file> <conversation-id>... [--max-page-size <n>]\n  greenbubbles-restore read <archive> <policy-file> <conversation-id> [--cursor <cursor>] [--limit <n>]\n  greenbubbles-restore reconcile <previous-archive> <current-archive> <policy-file> <events-output>\n  greenbubbles-restore replica-bootstrap <archive> <replica-path> --replica-key-stdin\n  greenbubbles-restore replica-status <replica-path> --replica-key-stdin\n  greenbubbles-restore tool-policy <archive> <policy-file> <conversation-id>... --capabilities list,read,search,draft [--fields sender,created-at,direction,type,content,attachments,relationships] [--not-before-unix <seconds>] [--not-after-unix <seconds>] [--allow-remote-model] [--max-results <n>] [--max-summary-bytes <n>] [--max-draft-bytes <n>]\n  greenbubbles-restore tool-list <archive> <policy-file> <audit-log> --requester <id> [--destination local|remote]\n  greenbubbles-restore tool-recent <archive> <policy-file> <audit-log> <conversation-id> --requester <id> [--limit <n>] [--destination local|remote]\n  greenbubbles-restore tool-search <archive> <policy-file> <audit-log> --requester <id> --query-stdin [--conversation <id>] [--limit <n>] [--destination local|remote]\n  greenbubbles-restore tool-draft <archive> <policy-file> <audit-log> <draft-directory> <conversation-id> --requester <id> --body-stdin"
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
