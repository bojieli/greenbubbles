# GreenBubbles

GreenBubbles is an experimental, local-first bridge for making a user's own
WeChat data available to narrowly scoped AI tools on macOS.

The project currently implements passive, read-only discovery, consistent
database snapshots, and an offline restoration engine. It does **not** inject
code into WeChat, call private network APIs, or send messages.

## Current milestone

The current passive-read slice provides:

- discovers known WeChat application and sandbox locations on macOS;
- inventories likely databases, SQLite sidecars, indexes, and media by metadata;
- redacts filesystem paths by default;
- supports synthetic test roots so format research never needs live user data;
- never opens a live database or media source for writing;
- validates and decrypts owner-authorized snapshot copies using a passphrase
  supplied through standard input only;
- retains every message row and raw SQLite value while adding typed payloads;
- merges message shards into deterministic per-conversation order;
- resolves downloaded images, videos, documents, posters, and database-backed
  voice payloads to verified local artifact records;
- records non-downloaded, ambiguous, unsafe, or undecodable artifacts
  explicitly instead of silently omitting them.

See [PLAN.md](PLAN.md) for the phased roadmap and safety gates.

## Build and test

```sh
swift build
swift test
swift run greenbubbles accounts
swift run greenbubbles discover
swift run greenbubbles inventory
swift run greenbubbles snapshot
cd Native/GreenBubblesRestore
cargo test --locked --all-targets
```

`inventory` reports opaque path identifiers by default. For local debugging,
`--include-paths` may be used explicitly. Do not paste that output into issues
or logs because paths can contain stable account identifiers. Opaque identifiers
are stable hashes intended for correlation, not a substitute for access control.

```sh
swift run greenbubbles inventory --include-paths
swift run greenbubbles inventory --root /path/to/synthetic/fixture
```

`snapshot` opens candidate sources with read-only file descriptors, copies each
database/WAL/SHM set into an owner-only temporary directory, rejects concurrent
mutation, prints a redacted manifest, and automatically removes the copy when
the command exits.

For format work, first select the opaque ID reported by `accounts`. Supplying a
snapshot base is an explicit request to preserve the encrypted snapshot instead
of deleting it at process exit:

```sh
swift run greenbubbles snapshot --account <opaque-id> \
  --snapshot-base "$HOME/Library/Application Support/GreenBubbles/Snapshots"
```

The preserved directory is mode `0700`; copied files and its manifest are mode
`0600`. Remove it when no longer needed.

The native restoration engine works only from such a snapshot. A database
passphrase must never be placed on the command line:

```sh
cargo run --manifest-path Native/GreenBubblesRestore/Cargo.toml -- \
  restore <snapshot-directory> <private-output-directory> \
  --account-root <authorized-account-directory> --passphrase-stdin
```

The output directory is owner-only and contains canonical message NDJSON,
artifact NDJSON with exact verified local locations, a rejection ledger, a
schema/type coverage report, account-scoped conversation and participant
records, and an integrity/completion report. It also contains losslessly decoded
image derivatives, raw SILK voice payloads, and playable voice derivatives when
decoding succeeds. These files are plaintext private data: keep them out of
Git, issue attachments, shell transcripts, and model prompts.

Production completeness is deliberately strict. The restoration report must
satisfy `source rows = restored rows + rejected rows`, with zero rejections,
zero duplicate canonical identities, no unknown observed message types, and no
unexplained media state. See [docs/RESTORATION_SPEC.md](docs/RESTORATION_SPEC.md).

## Encrypted canonical replica

The restored archive can be bootstrapped into a one-account SQLCipher replica.
Use a new high-entropy 32-byte key that is distinct from the WeChat database
passphrase. The key is accepted only through standard input:

```sh
mkdir -m 700 /path/to/private-replica-directory
printf '%s' '<64-hex-character-random-replica-key>' | cargo run \
  --manifest-path Native/GreenBubblesRestore/Cargo.toml -- \
  replica-bootstrap <private-output-directory> \
  /path/to/private-replica-directory/greenbubbles.db --replica-key-stdin

printf '%s' '<64-hex-character-random-replica-key>' | cargo run \
  --manifest-path Native/GreenBubblesRestore/Cargo.toml -- \
  replica-status /path/to/private-replica-directory/greenbubbles.db \
  --replica-key-stdin

printf '%s' '<64-hex-character-random-replica-key>' | cargo run \
  --manifest-path Native/GreenBubblesRestore/Cargo.toml -- \
  replica-sync <new-private-output-directory> \
  /path/to/private-replica-directory/greenbubbles.db --replica-key-stdin

printf '%s' '<64-hex-character-random-replica-key>' | cargo run \
  --manifest-path Native/GreenBubblesRestore/Cargo.toml -- \
  replica-changes /path/to/private-replica-directory/greenbubbles.db \
  --replica-key-stdin --limit 100
```

Avoid placing a real key literally in shell history; pipe it from an
owner-controlled secret manager. The example value is a placeholder. Bootstrap
atomically stores canonical records, provenance, coverage, FTS, and its source
checkpoint. Each replica rejects another account, and migrations retain an
encrypted pre-migration backup. Synchronization mutates only changed canonical
records and commits them with the checkpoint; the body-free change stream is
ordered and resumable. See [docs/REPLICA_SPEC.md](docs/REPLICA_SPEC.md).

Exact retrieval uses an owner-only JSON filter. Any field can be omitted:

```json
{
  "conversationId": "<opaque-id>",
  "senderId": "<opaque-id>",
  "direction": "incoming",
  "logicalType": 1,
  "notBeforeUnix": 1700000000,
  "hasAttachment": true,
  "fullTextQuery": "requested document"
}
```

```sh
chmod 600 /path/to/private-filter.json
printf '%s' '<64-hex-character-random-replica-key>' | cargo run \
  --manifest-path Native/GreenBubblesRestore/Cargo.toml -- \
  replica-search /path/to/private-replica-directory/greenbubbles.db \
  /path/to/private-filter.json --replica-key-stdin --limit 50
```

Structured filters also cover subtype, inclusive upper time bound, reply target,
and attachment absence. Search cursors fail closed when the filter, replica,
account, or committed source checkpoint changes. `replica-status` and
`replica-coverage` expose freshness and known restoration limitations.

Conversation reads require a separate owner-only policy. Creating one is an
explicit local authorization step; cursors are bound to both the archive
fingerprint and the selected conversation:

```sh
cargo run --manifest-path Native/GreenBubblesRestore/Cargo.toml -- \
  policy <private-output-directory> <policy-file> \
  <enabled-conversation-id> --max-page-size 100

cargo run --manifest-path Native/GreenBubblesRestore/Cargo.toml -- \
  read <private-output-directory> <policy-file> \
  <enabled-conversation-id> --limit 50
```

The `read` command emits message bodies and is therefore intended only for
explicit local use. A policy remains valid for later archives from the same
account, but not for another account; cursors remain bound to one archive and
conversation.

Periodic archive reconciliation is authoritative for incoming/change events:

```sh
cargo run --manifest-path Native/GreenBubblesRestore/Cargo.toml -- \
  reconcile <previous-archive> <current-archive> <policy-file> <events-output>
```

It emits deterministic, body-free `added`, `changed`, and `removed` event
metadata only for enabled conversations. Filesystem and optional notification
hints merely decide when to run this reconciliation. See
[docs/NOTIFICATION_HINTS.md](docs/NOTIFICATION_HINTS.md).

An experimental local AI-tool kernel adds operation, account, conversation,
field, time-range, and local/remote-destination checks. It has no send
capability. Create its private working directory first, then grant only the
needed fields and operations:

```sh
mkdir -m 700 /private/greenbubbles-tools /private/greenbubbles-tools/drafts

cargo run --manifest-path Native/GreenBubblesRestore/Cargo.toml -- \
  tool-policy <private-output-directory> /private/greenbubbles-tools/policy.json \
  <enabled-conversation-id> --capabilities list,read,search,draft \
  --fields sender,created-at,direction,type,content,attachments,relationships

cargo run --manifest-path Native/GreenBubblesRestore/Cargo.toml -- \
  tool-recent <private-output-directory> /private/greenbubbles-tools/policy.json \
  /private/greenbubbles-tools/audit.ndjson <enabled-conversation-id> \
  --requester local-agent --limit 20

printf '%s' 'search terms' | cargo run \
  --manifest-path Native/GreenBubblesRestore/Cargo.toml -- \
  tool-search <private-output-directory> /private/greenbubbles-tools/policy.json \
  /private/greenbubbles-tools/audit.ndjson --requester local-agent --query-stdin
```

Remote-model release is denied unless the policy was created with the explicit
`--allow-remote-model` flag. Raw source fields and paths are never part of the
minimized tool response. Search queries, message bodies, and draft bodies are
omitted from the append-only audit JSONL. See
[docs/AI_TOOL_BOUNDARY.md](docs/AI_TOOL_BOUNDARY.md).

## Scope and authorization

Use GreenBubbles only with data and accounts you own or are explicitly
authorized to access. Group chats contain other people's data even when the
database belongs to the local user. The connector must enforce per-conversation
consent and data minimization before any model integration is enabled.

This repository is private and no open-source license has been selected yet.
No permission to redistribute the code is granted until a license is added.
