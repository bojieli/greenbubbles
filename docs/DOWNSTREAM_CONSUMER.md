# Resumable downstream consumer example

`examples/change_consumer.rs` is a runnable, host-neutral consumer of the Unix
connector API. It never opens the encrypted replica or restoration archive.
It demonstrates the bootstrap/catch-up sequence, persists a change cursor and
policy-minimized records atomically, refreshes changed canonical messages, and
removes recalled/deleted or newly unauthorized records.

Build or run it from the Rust package:

```text
cargo run --locked --example change_consumer -- \
  /private/greenbubbles/connector.sock \
  /private/greenbubbles/downstream-state.json \
  --markdown-output /private/greenbubbles/conversations.md
```

The state and optional Markdown parent directory must already be mode `0700`.
Outputs are mode `0600` and atomically replaced. The Markdown projection is a
deterministic downstream memory view over fields already minimized by local
policy; it labels and HTML-escapes source text as untrusted. It performs no LLM
summarization and makes no claim beyond the connector's coverage.

Run the same command after synchronization. The consumer resumes its stored
cursor and processes only later invalidations. If the account differs or the
replica-generation-bound cursor is rejected, it exits without modifying the
state. After independently verifying that the replacement replica and account
are intended, the operator may request a full rebuild:

```text
cargo run --locked --example change_consumer -- \
  /private/greenbubbles/connector.sock \
  /private/greenbubbles/downstream-state.json \
  --markdown-output /private/greenbubbles/conversations.md \
  --rebootstrap
```

The integration test runs this workflow against the real Unix service,
re-runs it as an idle resume, confirms mode-`0600` JSON/Markdown output, swaps
in a newly generated replica, verifies cursor rejection leaves bytes unchanged,
and then verifies explicit rebootstrap.

