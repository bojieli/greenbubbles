use std::collections::BTreeSet;
use std::env;
use std::path::PathBuf;

use greenbubbles_restore::{
    archive::{create_conversation_policy, read_conversation_page},
    prepare_catalog,
    reconcile::reconcile_archives,
    restore_catalog, DatabasePassphrase, RestorationOptions,
};

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
        _ => {
            eprintln!(
                "Usage:\n  greenbubbles-restore probe <snapshot> [--passphrase-stdin]\n  greenbubbles-restore restore <snapshot> <output> [--account-root <path>] [--passphrase-stdin]\n  greenbubbles-restore policy <archive> <policy-file> <conversation-id>... [--max-page-size <n>]\n  greenbubbles-restore read <archive> <policy-file> <conversation-id> [--cursor <cursor>] [--limit <n>]\n  greenbubbles-restore reconcile <previous-archive> <current-archive> <policy-file> <events-output>"
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

fn option_usize(arguments: &[String], option: &str) -> Result<Option<usize>, String> {
    option_string(arguments, option)?
        .map(|value| {
            value
                .parse::<usize>()
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
