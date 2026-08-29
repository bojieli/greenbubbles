# Command-line reference

The native `greenbubbles` executable is the query, snapshot, restoration,
replica, connector and audit engine behind the history browser. Its built-in
help is the canonical syntax:

```sh
greenbubbles help
greenbubbles help ai-query
greenbubbles send --help
```

Help topics exist for `profile`, `source`, `conversations`, `messages`,
`message`, `attachment`, `snapshot`, `send`, the three `connector-*-direct`
commands, `audit-replica`, `audit-replica-backup`, `ai-query`, `ai-export`,
`ai-memory-export`, `audit-ai-context` and `audit-ai-memory`. An unrecognized
topic falls back to the full usage listing rather than erroring.

This page maps command families to the workflow they belong to. It does not
turn an advanced forensic or action command into a recommended first run — for
ordinary use start with the [user guide](USER_GUIDE.md).

Do not confuse it with `greenbubbles-discover`, the small Swift helper that
locates installations, accounts and candidate artefacts *before* any key is
supplied. It opens no database contents.

## Build

```sh
cargo build --locked --release --manifest-path Native/GreenBubbles/Cargo.toml

GB_CLI="Native/GreenBubbles/target/release/greenbubbles"
"$GB_CLI" help
```

## Command map

| Family | Commands | Use |
| --- | --- | --- |
| Query profiles | `profile path/template/list/show/validate/set-default` | Store private source and credential references for repeated queries |
| Direct resources | `source status`, `conversations list`, `messages list/search`, `message get` | Read one bounded live or snapshot page without creating a restoration |
| Exact attachments | `attachment inspect/materialize` | Inspect one message; copy one selected artefact to a new private path |
| Recoverable snapshots | `snapshot recovery-kit/local-credential/create/create-capture/verify/rewrap/rekey/retention` | Create, reopen, rotate, verify and retain independently encrypted snapshots |
| Offline restoration | `preflight`, `probe`, `restore`, `restore-publish` | Validate and restore an owner-authorized immutable capture |
| Diagnostics | `diagnose-batch`, `diagnose-available`, `diagnose-archive-schema`, `diagnose-archive-payloads` | Produce privacy-minimized structural evidence for incomplete or changing formats |
| Archive audit and merge | `audit-archive`, `audit-acquisition-chain`, `reconcile`, `merge-incremental` | Verify archive integrity and combine change-proportional generations |
| Replica lifecycle | `replica-bootstrap/status/sync/publish/follow*`, `audit-replica*`, `prepare-replica-recovery` | Maintain and recover the encrypted serving replica |
| Replica reads | `replica-conversations/search/message/coverage/changes/cached-moments` | Query replica-only enrichment and change surfaces |
| Direct AI connector | `connector-policy-direct`, `connector-query-direct`, `connector-serve-direct` | Apply source-bound policy and audit directly to live or snapshot queries |
| Replica AI connector | `tool-policy/list/recent/search/draft`, `connector-serve/call` | Serve policy-scoped replica reads and non-executing drafts |
| AI interchange | `ai-query`, `ai-export`, `audit-ai-context`, `ai-memory-export`, `audit-ai-memory` | Create and verify minimized, citation-preserving AI context |
| Operational evidence | `synthetic-benchmark`, `compose-latency-evidence`, `summarize-latency-evidence`, `audit-connector-log/state` | Generate or verify aggregate release and service evidence |
| Sending | `send …` | Inspect the separate experimental adapter; public builds stay closed |

## How secrets reach a process

Every direct source command takes exactly one access mode:

| Mode | Option | Secret input |
| --- | --- | --- |
| Live encrypted WeChat | `--passphrase-stdin` | key as the first stdin line |
| Snapshot, portable | `--snapshot-recovery-kit <file>` | owner-only file; nothing on stdin |
| Snapshot, local | `--snapshot-local-credential <file>` | owner-only file; nothing on stdin |
| Snapshot, passphrase | `--snapshot-passphrase-stdin` | passphrase as the first stdin line |
| Legacy snapshot raw key | `--snapshot-key-stdin` | key as the first stdin line |
| Synthetic / plaintext | `--decrypted` | none |

When a search also uses stdin, the key or passphrase is the first line and the
query is the remaining UTF-8 input. In file-backed and plaintext modes, stdin
contains only the query.

**Never put a key, passphrase, recovery phrase, replica key or private search
text in a shell argument.** Prefer an owner-only file redirected into stdin
over an interactive shell literal — the literal enters shell history, and
arguments are visible to every process on the machine.

A [query profile](QUERY_PROFILES.md) removes most of this ceremony from daily
use.

## Bounds and how to read a response

- Conversation and message lists default to 100 items, hard maximum 500.
- Search has a hard maximum of 200 results. Fallback source scanning examines a
  bounded window and may need a continuation followed through *empty* result
  pages — an empty page is not the end of the search.
- There is no `--all` direct-query option, and there will not be one.
- Opaque cursors and message IDs are bound to their source, operation,
  conversation and filters. Do not reuse one across those boundaries; it will
  be rejected rather than silently reinterpreted.
- Read `ok`, `consistency`, `warnings`, `coverage` and `page` before
  interpreting any result.
- **Missing data under stale, unavailable or partial coverage is not evidence
  of deletion.** This is the single most important reading rule, and the one an
  automated caller is most likely to get wrong.

Envelope and paging details are in [ARCHITECTURE.md](ARCHITECTURE.md); the AI
request contract is in [AI_CONTEXT_CLI.md](AI_CONTEXT_CLI.md).

## Progress output

Most restoration, audit, snapshot and export commands accept
`--progress-json`, `--quiet-progress`, or an owner-only create-new
`--progress-file`. Progress events omit message content, credentials and source
paths — but an operational report can still reveal private aggregate facts, so
treat one as private until you have read it.

## Where to go next

| Task | Document |
| --- | --- |
| Repeated queries without retyping | [Query profiles](QUERY_PROFILES.md) |
| Backups and recovery | [Recoverable snapshots](RECOVERABLE_SNAPSHOTS.md) |
| Offline restoration and publication | [Restoration specification](RESTORATION_SPEC.md) |
| The serving replica | [Replica specification](REPLICA_SPEC.md) · [operations](REPLICA_OPERATIONS.md) |
| The local request contract | [Connector API](CONNECTOR_API.md) |
| Giving an AI access | [AI context CLI](AI_CONTEXT_CLI.md) |
| Verifying any of the above | [Auditing](AUDITING.md) |
| The closed send path | [Send adapter](SEND_ADAPTER.md) |
