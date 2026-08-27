use std::env;
use std::path::PathBuf;

use greenbubbles_restore::{
    prepare_catalog, restore_catalog, DatabasePassphrase, RestorationOptions,
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
        _ => {
            eprintln!(
                "Usage:\n  greenbubbles-restore probe <snapshot> [--passphrase-stdin]\n  greenbubbles-restore restore <snapshot> <output> [--account-root <path>] [--passphrase-stdin]"
            );
        }
    }
    Ok(())
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
