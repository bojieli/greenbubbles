use std::process::Command;

#[test]
fn ai_commands_expose_help_without_opening_private_inputs() {
    for (command, expected) in [
        (
            "audit-replica",
            "deep audit of the encrypted serving replica",
        ),
        (
            "audit-replica-backup",
            "without migrating or rewriting the backup",
        ),
        ("ai-query", "policy-scoped, read-only JSON request"),
        (
            "ai-export",
            "checkpoint-consistent, policy-scoped AI context bundle",
        ),
        ("audit-ai-context", "without printing content"),
        (
            "ai-memory-export",
            "Mem0-compatible JSON message batches and QMD-compatible Markdown",
        ),
        ("audit-ai-memory", "without printing content"),
    ] {
        for help_flag in ["--help", "-h"] {
            let output = Command::new(env!("CARGO_BIN_EXE_greenbubbles"))
                .args([command, help_flag])
                .output()
                .expect("AI CLI help command should run");

            assert!(
                output.status.success(),
                "{command} {help_flag} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.stderr.is_empty());
            let stdout = String::from_utf8(output.stdout).expect("help should be UTF-8");
            assert!(stdout.starts_with("Usage:\n"));
            assert!(stdout.contains(expected));
            if command != "ai-query" {
                assert!(stdout.contains("--progress-file"));
                assert!(stdout.contains("--progress-json"));
                assert!(stdout.contains("--quiet-progress"));
            }
        }
    }
}

#[test]
fn help_topic_exposes_ai_command_help() {
    let output = Command::new(env!("CARGO_BIN_EXE_greenbubbles"))
        .args(["help", "ai-query"])
        .output()
        .expect("AI CLI help topic should run");

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(String::from_utf8(output.stdout)
        .expect("help should be UTF-8")
        .contains("greenbubbles ai-query"));
}
