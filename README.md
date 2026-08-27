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
schema/type coverage report, and an integrity report. It also contains
losslessly decoded image derivatives and raw SILK voice payloads when locally
available. These files are plaintext private data: keep them out of Git, issue
attachments, shell transcripts, and model prompts.

Production completeness is deliberately strict. The restoration report must
satisfy `source rows = restored rows + rejected rows`, with zero rejections,
zero duplicate canonical identities, no unknown observed message types, and no
unexplained media state. See [docs/RESTORATION_SPEC.md](docs/RESTORATION_SPEC.md).

## Scope and authorization

Use GreenBubbles only with data and accounts you own or are explicitly
authorized to access. Group chats contain other people's data even when the
database belongs to the local user. The connector must enforce per-conversation
consent and data minimization before any model integration is enabled.

This repository is private and no open-source license has been selected yet.
No permission to redistribute the code is granted until a license is added.
