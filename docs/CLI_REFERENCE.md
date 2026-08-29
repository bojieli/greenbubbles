# Command-line reference

The native <code>greenbubbles</code> executable is the query, snapshot,
restoration, replica, connector, and audit engine behind the SwiftUI history
browser. Its built-in help is the canonical syntax reference:

```sh
greenbubbles help
greenbubbles help ai-query
greenbubbles send --help
```

Help topics exist for <code>profile</code>, <code>source</code>,
<code>conversations</code>, <code>messages</code>, <code>message</code>,
<code>attachment</code>, <code>snapshot</code>, <code>send</code>, the three
<code>connector-*-direct</code> commands, <code>audit-replica</code>,
<code>audit-replica-backup</code>, <code>ai-query</code>,
<code>ai-export</code>, <code>ai-memory-export</code>,
<code>audit-ai-context</code>, and <code>audit-ai-memory</code>. An unrecognized
topic falls back to the full usage listing rather than reporting an error.

This page maps command families to their intended workflow. It does not turn
advanced forensic or action commands into a recommended first-run path. For
ordinary use, start with the [user guide](USER_GUIDE.md).

Do not confuse it with <code>greenbubbles-discover</code>, the small Swift
helper that locates WeChat installations, accounts, and candidate artifacts
before any key is supplied. That helper opens no database contents and is
documented alongside the workflows that use it, such as the
[live database sanity check](LIVE_DATABASE_SANITY_CHECK.md) and
[notification hints](NOTIFICATION_HINTS.md).

## Build

From the repository root:

```sh
cargo build --locked --release \
  --manifest-path Native/GreenBubbles/Cargo.toml

GB_CLI="Native/GreenBubbles/target/release/greenbubbles"
"$GB_CLI" help
```

## Command map

| Family                  | Commands                                                                                                                                                 | Use                                                                              |
| ----------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------- |
| Query profiles          | <code>profile path/template/list/show/validate/set-default</code>                                                                                        | Store private source and credential-file references for repeated bounded queries |
| Direct resources        | <code>source status</code>, <code>conversations list</code>, <code>messages list/search</code>, <code>message get</code>                                 | Read a bounded live or snapshot page without creating a restoration              |
| Exact attachments       | <code>attachment inspect/materialize</code>                                                                                                              | Inspect one message and copy one selected local artifact to a new private path   |
| Recoverable snapshots   | <code>snapshot recovery-kit/local-credential/create/create-capture/verify/rewrap/rekey/retention</code>                                                  | Create, reopen, rotate, verify, and retain independently encrypted snapshots     |
| Offline restoration     | <code>preflight</code>, <code>probe</code>, <code>restore</code>, <code>restore-publish</code>                                                           | Validate and restore an owner-authorized immutable capture                       |
| Diagnostics             | <code>diagnose-batch</code>, <code>diagnose-available</code>, <code>diagnose-archive-schema</code>, <code>diagnose-archive-payloads</code>               | Produce privacy-minimized structural evidence for incomplete or changing formats |
| Archive audit and merge | <code>audit-archive</code>, <code>audit-acquisition-chain</code>, <code>reconcile</code>, <code>merge-incremental</code>                                 | Verify archive integrity and combine change-proportional generations             |
| Replica lifecycle       | <code>replica-bootstrap/status/sync/publish/follow*</code>, <code>audit-replica*</code>, <code>prepare-replica-recovery</code>                           | Maintain and recover the encrypted canonical serving replica                     |
| Replica reads           | <code>replica-conversations/search/message/coverage/changes/cached-moments</code>                                                                        | Query replica-only enrichment and change surfaces                                |
| Direct AI connector     | <code>connector-policy-direct</code>, <code>connector-query-direct</code>, <code>connector-serve-direct</code>                                           | Apply source-bound policy and audit directly to live or snapshot queries         |
| Replica AI connector    | <code>tool-policy/list/recent/search/draft</code>, <code>connector-serve/call</code>                                                                     | Serve policy-scoped replica reads and non-executing drafts                       |
| AI interchange          | <code>ai-query</code>, <code>ai-export</code>, <code>audit-ai-context</code>, <code>ai-memory-export</code>, <code>audit-ai-memory</code>                | Create and verify minimized, citation-preserving AI context                      |
| Operational evidence    | <code>synthetic-benchmark</code>, <code>compose-latency-evidence</code>, <code>summarize-latency-evidence</code>, <code>audit-connector-log/state</code> | Generate or verify aggregate release and service evidence                        |
| Sending                 | <code>send …</code>                                                                                                                                      | Inspect the separate experimental action adapter; public builds remain closed    |

## Access modes and secret transport

Direct source commands accept exactly one access mode:

| Mode                             | Option                                                | Secret input                                |
| -------------------------------- | ----------------------------------------------------- | ------------------------------------------- |
| Live encrypted WeChat            | <code>--passphrase-stdin</code>                       | Key as the first standard-input line        |
| Recoverable snapshot, portable   | <code>--snapshot-recovery-kit &lt;file&gt;</code>     | Owner-only file; no key on standard input   |
| Recoverable snapshot, local      | <code>--snapshot-local-credential &lt;file&gt;</code> | Owner-only file; no key on standard input   |
| Recoverable snapshot, passphrase | <code>--snapshot-passphrase-stdin</code>              | Passphrase as the first standard-input line |
| Legacy snapshot raw key          | <code>--snapshot-key-stdin</code>                     | Key as the first standard-input line        |
| Synthetic/plaintext fixture      | <code>--decrypted</code>                              | No secret                                   |

When a search also uses standard input, the key or passphrase is the first line
and the query is the remaining UTF-8 input. In file-backed and plaintext modes,
standard input contains only the query.

Never place a key, passphrase, recovery phrase, replica key, or private search
text in shell arguments. Prefer an owner-only file redirected into standard
input over an interactive shell literal, which may enter shell history.

## Bounds and response semantics

- Conversation and message lists default to 100 items and have a hard maximum
  of 500.
- Search has a hard maximum of 200 results; fallback source scanning examines
  a bounded window and may require following a continuation through empty
  result pages.
- There is no <code>--all</code> direct-query option.
- Opaque cursors and message IDs are bound to their source, operation,
  conversation, and filter. Do not reuse them across those boundaries.
- Inspect <code>ok</code>, <code>consistency</code>, <code>warnings</code>,
  <code>coverage</code>, and <code>page</code> before interpreting a result.
- Missing data under stale, unavailable, or partial coverage is not evidence
  of deletion or nonexistence.

For response envelopes and paging behavior, see
[AI context CLI](AI_CONTEXT_CLI.md) and
[live query architecture](LIVE_QUERY_ARCHITECTURE.md).

## Advanced workflows

- [Query profiles](QUERY_PROFILES.md)
- [Recoverable snapshots](RECOVERABLE_SNAPSHOTS.md)
- [Offline restoration](OFFLINE_PIPELINE.md)
- [Replica specification](REPLICA_SPEC.md)
- [Replica follow mode](REPLICA_FOLLOW.md)
- [Connector API](CONNECTOR_API.md)
- [AI context and memory](AI_CONTEXT_CLI.md)
- [Send adapter](SEND_ADAPTER.md)

Most restoration, audit, snapshot, and export commands accept
<code>--progress-json</code>, <code>--quiet-progress</code>, or an owner-only
create-new <code>--progress-file</code>. Progress events deliberately omit
message content, credentials, and source paths, but operational reports can
still reveal private aggregate facts and should remain private unless reviewed.
