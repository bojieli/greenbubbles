//! Emits one immutable action draft with its content-addressed identity.
//!
//! Text drafts normally come from the replica-backed connector. The
//! no-navigation send mode needs a draft too, but its recipient is whichever
//! conversation is already open, so there is nothing for the connector to
//! resolve. This tool fills that gap for operators and for live exercises: it
//! takes a skeleton, derives the identity the loader will demand, and writes an
//! owner-only file.
//!
//! ```text
//! cargo run --example emit_action_draft -- <skeleton.json> <draft-directory>
//! ```

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use greenbubbles_restore::connector::{action_draft_identity, ActionDraft};

fn main() {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let [skeleton, directory] = arguments.as_slice() else {
        eprintln!("usage: emit_action_draft <skeleton.json> <draft-directory>");
        std::process::exit(2);
    };
    let bytes = std::fs::read(skeleton).expect("the skeleton is readable");
    let mut draft: ActionDraft = serde_json::from_slice(&bytes).expect("the skeleton parses");
    draft.draft_id = action_draft_identity(&draft);
    let path = PathBuf::from(directory).join(format!("{}.json", draft.draft_id));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .expect("the draft path is new");
    file.write_all(&serde_json::to_vec_pretty(&draft).expect("the draft serializes"))
        .expect("the draft is written");
    println!(
        "{}",
        serde_json::json!({ "draftId": draft.draft_id, "path": path })
    );
}
