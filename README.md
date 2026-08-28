# GreenBubbles

GreenBubbles is an experimental, local-first bridge for making a user's own
WeChat data available to narrowly scoped AI tools on macOS.

The project currently implements passive, read-only discovery, consistent
database snapshots, and an offline restoration engine. That pipeline does
**not** inject code into WeChat, call private network APIs, or send messages.

Separately, and only under explicit owner authorization, the
`greenbubbles-acquire` helper can capture the owner's own database passphrase
by attaching `lldb` to the owner's own running WeChat client during a
logout/re-login. It is gated by an explicit `--owner-authorized` flag, a manual
owner-run re-sign of the client, and a pinned-build check, and it proves
correctness against every database's SQLCipher4 page-1 HMAC. It exists because
the owner reversed the project's previous blanket prohibition on
debugger-based acquisition on 2026-08-27. See
[docs/PASSPHRASE_ACQUISITION.md](docs/PASSPHRASE_ACQUISITION.md).

## Current milestone

The current passive-read slice provides:

- discovers known WeChat application and sandbox locations on macOS;
- inventories likely databases, SQLite sidecars, indexes, and media by metadata;
- redacts filesystem paths by default;
- supports synthetic test roots so format research never needs live user data;
- never opens a live database or media source for writing;
- validates and decrypts owner-authorized snapshot copies using a passphrase
  supplied through standard input only;
- consumes an already exported owner-only per-database key set without invoking
  an acquisition/export helper, authenticates each database independently,
  continues around unavailable databases, and preserves explicit freshness or
  stale-coverage evidence through replica synchronization;
- accepts signed official macOS WeChat `4.1` and later for passive restoration
  without pinning publication to one executable hash;
- reports continuous workflow, phase, database/file, byte, table, record,
  finalization, and audit progress in human-readable or NDJSON form;
- retains every message row and raw SQLite value while adding typed payloads;
- merges message shards into deterministic per-conversation order;
- resolves downloaded images, videos, documents, posters, and database-backed
  voice payloads to verified local artifact records;
- records non-downloaded, ambiguous, unsafe, or undecodable artifacts
  explicitly instead of silently omitting them.
- exposes verified source and decoded artifact locations through a
  conversation- and time-scoped local-only CLI operation, with a fresh
  descriptor/digest check before every path release;
- restores the compatible client's passive local Moments cache and interactions
  with raw provenance, explicit partial-cache semantics, encrypted replica
  storage, and a separately authorized minimized CLI/service view;
- produces policy-scoped AI context as checkpoint-consistent static JSONL and
  one-shot JSON queries, with normalized contacts, chat metadata, per-record
  source-database freshness, explicit coverage, and a repository agent skill;
- inventories the pinned signed app bundle's URL, extension, XPC, app-group,
  and internal-service metadata without live-process interaction, while keeping
  authenticated reads explicitly unavailable.
- inventories static evidence of official backup, migration, device-transfer,
  and file-export workflows without invoking WeChat or claiming that their
  formats or conversation/media coverage are compatible.
- defines and tests a pure offline action-safety contract for future gate,
  build, adapter, approval, idempotency, rate, kill-switch, and lifecycle
  checks without exposing approval, attempt, or send operations.

Separately from that passive slice, one explicitly gated active helper exists:
owner-authorized passphrase capture (`greenbubbles-acquire`), validated live on
2026-08-27 against the owner's own account on the pinned build (26/26
databases HMAC-verified). It requires root, a manual owner-run client re-sign,
and the `--owner-authorized` flag, and fails closed on any unpinned build. See
[docs/PASSPHRASE_ACQUISITION.md](docs/PASSPHRASE_ACQUISITION.md).

See [PLAN.md](PLAN.md) for the phased roadmap and safety gates.
The implemented downstream protocol and validation evidence are in
[docs/SOURCE_CONNECTOR_CONTRACT.md](docs/SOURCE_CONNECTOR_CONTRACT.md),
[docs/DOWNSTREAM_CONSUMER.md](docs/DOWNSTREAM_CONSUMER.md), and
[docs/ECOSYSTEM_VALIDATION.md](docs/ECOSYSTEM_VALIDATION.md). The bounded static
active-read assessment is in
[docs/ACTIVE_READ_FEASIBILITY.md](docs/ACTIVE_READ_FEASIBILITY.md). The
acquisition assessment and the three-path owner-controlled acquisition model
are in
[docs/ACQUISITION_FEASIBILITY.md](docs/ACQUISITION_FEASIBILITY.md); the gated
active capture path is documented in
[docs/PASSPHRASE_ACQUISITION.md](docs/PASSPHRASE_ACQUISITION.md). Every
remaining external gate and its required resumption evidence is mapped in
[docs/GATE_READINESS.md](docs/GATE_READINESS.md). Aggregate evidence from the
owner-authorized, content-free local snapshot validation is in
[docs/LOCAL_ACQUISITION_VALIDATION.md](docs/LOCAL_ACQUISITION_VALIDATION.md).
The unapproved private-development incident and complaint response workflow is
in [docs/OPERATIONAL_RESPONSE_PLAN.md](docs/OPERATIONAL_RESPONSE_PLAN.md); it
does not substitute for named owners or counsel/security approval.
The factual source/binary dependency boundary, nested native-code notices, and
publication categories are in
[docs/DISTRIBUTION_INVENTORY.md](docs/DISTRIBUTION_INVENTORY.md). This is not a
public-release approval; the repository remains unlicensed for public use and
private by design. The deliberately non-operational Phase 4 safety foundation
and its remaining adapter/live-evidence requirements are in
[docs/ACTION_SAFETY_CONTRACT.md](docs/ACTION_SAFETY_CONTRACT.md).

## Build and test

```sh
swift build
swift test
swift run greenbubbles accounts
swift run greenbubbles account-storage --max-artifacts 100000
swift run greenbubbles acquisition-surfaces
swift run greenbubbles discover
swift run greenbubbles integration-surfaces
swift run greenbubbles inventory
swift run greenbubbles snapshot
swift run greenbubbles-public-article /private/owner-only-request.json
swift run greenbubbles-acquire preflight
swift run greenbubbles-acquire capture --output <owner-only-passphrase-file> \
  --owner-authorized
swift run greenbubbles-acquire verify --passphrase-stdin
swift scripts/check-pinned-build-profile.swift
swift scripts/check-distribution-inventory.swift
swift scripts/check-secret-hygiene.swift
cd Native/GreenBubblesRestore
cargo test --locked --all-targets
```

One-time repository setup: enable the pre-commit secret-hygiene hook so
extracted key material can never enter local history:

```sh
git config core.hooksPath scripts/git-hooks
```

The hook runs `scripts/check-secret-hygiene.swift --staged` against staged
content; the same check runs over all tracked files in CI. It blocks banned
secret-file names and secret-shaped content patterns (raw-key literals, JSON
passphrase fields, `PRAGMA key` hex literals); ordinary 64-hex strings such as
pinned build hashes remain allowed.

`greenbubbles-acquire` is the owner-authorized active acquisition helper,
separate from the passive pipeline. `preflight` reports pinned-build,
hardening, process, privilege, and salt-inventory readiness and prints the
exact manual re-sign command when required; `capture` refuses without
`--owner-authorized`, waits for an owner logout/re-login, and writes the
passphrase only to the owner-specified `--output` file (mode `0600`, parent
`0700`, no silent overwrite without `--overwrite`); `verify` re-checks a stored
passphrase from standard input against every database's page-1 HMAC. The
passphrase never appears on the command line, in JSON reports, or in logs. See
[docs/PASSPHRASE_ACQUISITION.md](docs/PASSPHRASE_ACQUISITION.md).

`inventory` reports opaque path identifiers by default. For local debugging,
`--include-paths` may be used explicitly. Do not paste that output into issues
or logs because paths can contain stable account identifiers. Opaque identifiers
are stable hashes intended for correlation, not a substitute for access control.

`account-storage` emits aggregate filesystem metadata only: database-family
candidate counts and attachment-candidate counts/bytes by broad type. It does
not output attachment filenames or paths and does not open database or
attachment content. `reachedAttachmentLimit` or metadata issues make the
enumeration incomplete; even a complete enumeration does not prove that a file
belongs to a message or that restoration can decode it.

`integration-surfaces` reads only signed metadata from the exact pinned WeChat
build and emits no application path. It reports inbound and internal boundaries;
it does not invoke them and does not claim an authenticated message or Moments
read API. An unknown build fails closed.

`acquisition-surfaces` performs a bounded read-only scan of one regular,
single-link resource in that same exact pinned bundle. It reports static clues
for official user-mediated backup/restore, history migration, device transfer,
and file export. It neither invokes those workflows nor proves a portable
plaintext export, backup compatibility, or complete conversation/attachment
coverage. See
[docs/ACQUISITION_FEASIBILITY.md](docs/ACQUISITION_FEASIBILITY.md).

`greenbubbles-public-article` is a separately compiled, cookie-free,
fail-closed helper for one ordinary public `https://mp.weixin.qq.com/s...`
page. It checks robots, authentication, paywall signals, origin, redirects,
file permissions, and size limits; it has no replica/restoration dependency and
does not crawl subresources. The official robots policy observed on 2026-08-27
disallows `/s`, so the current command stops before fetching any article. See
[docs/PUBLIC_ARTICLE_FETCH.md](docs/PUBLIC_ARTICLE_FETCH.md).

```sh
swift run greenbubbles inventory --include-paths
swift run greenbubbles inventory --root /path/to/synthetic/fixture
```

`snapshot` opens candidate sources with read-only file descriptors, copies each
database/WAL/SHM set into an owner-only temporary directory, rejects concurrent
mutation, prints a redacted manifest, and automatically removes the copy when
the command exits.

On APFS, snapshot acquisition first uses descriptor-based atomic copy-on-write
file clones and records `atomicCopyOnWriteClone` on each captured entry. It
captures a database and its WAL/SHM sidecars as one bounded group, requires the
database to remain unchanged through that group, and hashes the immutable clones
after capture. A verified byte copy is used on volumes without clone support and
retains the stricter whole-group mutation check. Unselected source sets must
remain identical to the prior manifest in either mode.

For format work, first select the opaque ID reported by `accounts`. Supplying a
snapshot base is an explicit request to preserve the encrypted snapshot instead
of deleting it at process exit:

```sh
swift run greenbubbles snapshot --account <opaque-id> \
  --snapshot-base "$HOME/Library/Application Support/GreenBubbles/Snapshots"
```

For subsequent change-proportional snapshots, supply the prior manifest. The
planner revisits changed/recent sets and automatically schedules a full
integrity scan when the carried full-scan anchor is seven days old:

```sh
swift run greenbubbles snapshot --account <opaque-id> \
  --previous-manifest /private/prior-snapshot/manifest.json \
  --snapshot-base /private/next-snapshot-base
```

Use `--integrity-scan` to force a scan immediately or
`--integrity-scan-interval-seconds <n>` to change the maximum interval.

The preserved directory is mode `0700`; copied files and its manifest are mode
`0600`. Remove it when no longer needed.

Snapshot command report format 2 includes monotonic planning, acquisition, and
total durations in milliseconds. These relative values can be combined with the
offline operator and follower timing reports during a controlled latency run.
The snapshot report still contains a manifest and, for a preserved snapshot, a
private local path; keep the complete report owner-private and publish only a
reviewed aggregate.

After the same published generation is applied, compose a bound aggregate-only
stage sample from owner-only report files and the private current handoff:

```sh
greenbubbles-restore compose-latency-evidence \
  <private-snapshot-report.json> <private-offline-report.json> \
  <private-follower-report.json> <private-current-handoff.json>
```

Reviewed samples can be summarized from an owner-only JSON array with
`summarize-latency-evidence`. The output reports p50/p95 stage values but always
states that source-persistence time, inter-command delay, and disposable-case
attribution are missing, so it cannot falsely satisfy the 60-second end-to-end
gate. See [docs/LATENCY_EVIDENCE.md](docs/LATENCY_EVIDENCE.md).

The native restoration engine works only from such a snapshot. A database
passphrase must never be placed on the command line. A passphrase file
produced by `greenbubbles-acquire capture --output <file>` can be piped
directly, keeping the value out of arguments and shell history:

```sh
cat <owner-only-passphrase-file> | cargo run \
  --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
  restore <snapshot-directory> <private-output-directory> \
  --account-root <authorized-account-directory> --passphrase-stdin
```

The full restoration command set:

```sh
cargo run --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
  preflight <snapshot-directory>

cargo run --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
  restore <snapshot-directory> <private-output-directory> \
  --account-root <authorized-account-directory> --passphrase-stdin

cargo run --locked \
  --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
  audit-archive <private-output-directory>

cargo run --locked \
  --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
  audit-acquisition-chain <previous-snapshot> <current-snapshot>

cargo run --locked \
  --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
  audit-connector-log <owner-only-connector-audit.ndjson>

cargo run --locked \
  --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
  audit-connector-state <replica> <policy> <audit-log> <draft-directory> \
  --replica-key-stdin
```

Long-running restoration commands show progress on standard error by default,
while keeping the final JSON result on standard output. The human display has
three distinct percentages: a monotonic whole-workflow stage position, the
byte/record progress inside the current phase, and the current file, database,
table, or ledger item. It also reports phase and database ordinals, database
and WAL sizes, WAL frames scanned/applied, table and row counts, restored and
rejected records, semantic gaps, and elapsed time. The workflow percentage is
a stage position, not a wall-clock ETA; byte and record percentages remain the
authoritative measure within each stage. To keep schemas with thousands of
tiny hashed tables readable, the human view coalesces repetitive table chatter
to periodic cumulative-row updates while retaining every event in JSON output
and an explicitly requested progress file.

Use `--progress-json` for NDJSON progress on standard error, or
`--progress-file <new-private-ndjson>` to retain the same events while the
human display remains active. `--quiet-progress` suppresses the display but not
an explicitly requested progress file. Progress and summary files are created
without overwriting, require an existing owner-controlled `0700` parent, and
are written as owner-only `0600` files. They contain no keys or message bodies,
but remain private operational evidence.

When an owner already has a per-database key export, GreenBubbles can validate
and use it directly without running or invoking any acquisition/export helper:

```sh
greenbubbles-restore restore-publish \
  <snapshot-directory> <new-private-archive> <private-handoff.json> \
  --database-keys-file <existing-owner-only-key-json>

greenbubbles-restore diagnose-available \
  <snapshot-directory> <private-diagnostic-archive> \
  --database-keys-file <existing-owner-only-key-json> \
  --summary-file <new-private-summary.json> \
  --progress-file <new-private-progress.ndjson>

greenbubbles-restore diagnose-archive-schema \
  <private-diagnostic-archive> <new-private-schema-report.json> \
  --progress-file <new-private-schema-progress.ndjson>

greenbubbles-restore diagnose-archive-payloads \
  <private-diagnostic-archive> <new-private-payload-report.json> \
  --progress-file <new-private-payload-progress.ndjson>
```

`diagnose-available` verifies the complete snapshot first, authenticates each
existing key against the encrypted first page, restores every database that
can be authenticated, and names the count, byte size, logical path, and reason
for every unavailable database. The result is always a `diagnosticSubset` and
sets `authoritativeDatabaseCoverage` to false, even if the supplied key set
happens to cover every database; use normal `restore`/`restore-publish` for
fault-tolerant replica-eligible output. `diagnose-archive-schema` reads only the
GreenBubbles coverage ledger and emits bounded schema-family aggregates; it
never prints row values, source identities, or payload byte samples.
`diagnose-archive-payloads` independently scans the canonical message ledger,
reports byte and record progress, profiles storage/semantic shapes, and audits
whether relationship identifiers are present, recoverable from already decoded
raw XML, genuinely absent there, or lack decoded XML. Its report remains
aggregate-only and owner-only.

The normal `restore` and `restore-publish` paths use the same independent key
authentication but are fault tolerant: one unavailable database no longer
aborts healthy database restoration or publication. Full-snapshot output is
marked `partialDatabaseCoverage`, with exact fresh/unavailable counts and
source-set evidence. Incremental merge carries prior records for an unavailable
changed database as explicitly stale, so replica synchronization cannot mistake
temporary unavailability for deletion. When that database becomes available in
a later generation, its fresh records replace the stale set automatically.
`replica-bootstrap`, `replica-sync`, `replica-status`, and `replica-coverage`
surface archive scope and aggregate total/fresh/unavailable/stale database
coverage, so a running system is visibly degraded rather than appearing halted
or silently complete.

The latest owner-local aggregate validation of this workflow authenticated 25
of 26 databases and explicitly retained the one unavailable database. Across
the selected set, GreenBubbles classified all 6,542 tables and 9,529,301
observed rows as 6,291 message tables or 251 known auxiliary tables, with zero
generic or unhandled candidates. It restored and independently audited
1,854,110 messages plus 69,190 cached-SNS records (1,923,300 source records)
with zero rejected rows, duplicate canonical identities, unknown payloads, or
cached-surface semantic gaps. Two malformed subtype `49:19` values remain raw-
retained semantic gaps. A follow-up GreenBubbles payload audit classified all
193,503 relationship references: 1 identifier was already present, 192,991 are
recoverable from source-preserving decoded XML, 511 are genuinely absent from
that XML, and 0 lack decoded-XML evidence. This is diagnostic evidence, not a
production-ready archive because it was intentionally created as a
`diagnosticSubset`, media was deferred, and it predates the fault-tolerant
publication/relationship corrections. The observed signed 4.1.13 client is now
inside the supported passive-restoration family; the unavailable icon database
would be explicit partial coverage rather than a pipeline-wide failure in a new
publication run.

`preflight` verifies every copied database/WAL/SHM digest and reports the
current source-set count, copied database storage families, signed 4.1+-client
compatibility, and whether the copied databases require a passphrase. It does
not decrypt a database, inspect tables or rows, emit source paths, or accept a
secret. For incremental snapshots, “current source sets” describes the complete
authoritative inventory while “copied databases” describes only the selected
change fragment.

The output directory is owner-only and contains canonical message NDJSON,
artifact NDJSON with exact verified local locations, a rejection ledger, a
schema/type coverage report, account-scoped conversation and participant
records, and an integrity/completion report. It also contains losslessly decoded
image derivatives, raw SILK voice payloads, and playable voice derivatives when
decoding succeeds. These files are plaintext private data: keep them out of
Git, issue attachments, shell transcripts, and model prompts.

`audit-archive` independently reopens all ledgers, reproduces their counts and
relationships, validates source-preserving encodings and schema profiles, and
descriptor-verifies every recorded downloaded/materialized/decoded file against
its stored identity, size, timestamps, and SHA-256. Its output contains only
aggregate counts and independently derived per-component completion verdicts,
including explicit external-attestation limitations. A moved, evicted,
substituted, or changed media file fails closed. See
[docs/ARCHIVE_AUDIT.md](docs/ARCHIVE_AUDIT.md).

`audit-acquisition-chain` digest-verifies two owner-only snapshots and proves
that baseline continuity, client build, changed/reconciliation/deleted set
classification, and selected copied entries agree with both complete source
inventories. Its output is aggregate-only. See
[docs/ACQUISITION_CHAIN_AUDIT.md](docs/ACQUISITION_CHAIN_AUDIT.md).

For an already acquired format-3 snapshot, the offline operator can combine
restoration, acquisition-chain verification, independent archive audits,
incremental merge, and monotonic publication without exposing the passphrase on
the command line:

```sh
greenbubbles-restore restore-publish \
  <snapshot> <new-authoritative-archive> <private-handoff.json> \
  --previous-snapshot <previous-snapshot> \
  --previous-archive <previous-authoritative-archive> \
  --account-root <authorized-account-root> --passphrase-stdin
```

Omit both previous inputs only for a bootstrap. The command requires the signed
WeChat identity and a marketing version of 4.1 or later, never touches live
WeChat state or the encrypted replica, and publishes only after the resulting
authoritative or explicit partial-database archive passes its independent
audit.
See [docs/OFFLINE_PIPELINE.md](docs/OFFLINE_PIPELINE.md).

`audit-connector-log` verifies the body-free connector journal's owner-only
file boundary, event structure, unique identities, account consistency, event
digests, and predecessor chain. Its report is aggregate-only and explicitly
counts any unchained legacy prefix. See
[docs/CONNECTOR_AUDIT.md](docs/CONNECTOR_AUDIT.md).

`audit-connector-state` additionally opens the encrypted replica and current
policy, recomputes every immutable draft identity, classifies expired/stale
drafts, cross-links draft/request/review records, and rejects gated action
stages. The replica key is accepted only through standard input, and the report
contains no identities or bodies.

Production completeness is deliberately strict. The restoration report must
satisfy `source rows = restored rows + rejected rows`, with zero rejections,
zero duplicate canonical identities, no unknown observed message types, and no
unexplained media state. See [docs/RESTORATION_SPEC.md](docs/RESTORATION_SPEC.md).
The coverage ledger also carries deterministic whole-profile and per-table
schema fingerprints; data-row changes do not alter them, while structural drift
does.

For low-latency text publication, media traversal and decoding can be deferred:

```sh
cargo run --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
  restore <snapshot> <private-text-archive> --defer-media --passphrase-stdin
```

This produces every canonical message immediately, gives media messages an
explicit deferred artifact record, sets `mediaPhase` to `deferred`, and cannot
claim complete restoration. Run restoration again from the same immutable
snapshot without `--defer-media` (and with the authorized account root) to
produce a fully resolved media archive. It retains the source fingerprint;
`replica-sync` recognizes the changed restoration revision and commits the
artifact/message enrichment without mixing pagination checkpoints.

The release-mode synthetic benchmark and fault harness is documented in
[docs/SYNTHETIC_BENCHMARK.md](docs/SYNTHETIC_BENCHMARK.md).

## Encrypted canonical replica

The restored archive can be bootstrapped into a one-account SQLCipher replica.
Use a new high-entropy 32-byte key that is distinct from the WeChat database
passphrase. The key is accepted only through standard input:

```sh
mkdir -m 700 /path/to/private-replica-directory
printf '%s' '<64-hex-character-random-replica-key>' | cargo run \
  --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
  replica-bootstrap <private-output-directory> \
  /path/to/private-replica-directory/greenbubbles.db --replica-key-stdin

printf '%s' '<64-hex-character-random-replica-key>' | cargo run \
  --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
  replica-status /path/to/private-replica-directory/greenbubbles.db \
  --replica-key-stdin

printf '%s' '<64-hex-character-random-replica-key>' | cargo run \
  --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
  replica-sync <new-private-output-directory> \
  /path/to/private-replica-directory/greenbubbles.db --replica-key-stdin

printf '%s' '<64-hex-character-random-replica-key>' | cargo run \
  --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
  replica-changes /path/to/private-replica-directory/greenbubbles.db \
  --replica-key-stdin --limit 100

printf '%s' '<64-hex-character-random-replica-key>' | cargo run \
  --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
  audit-replica /path/to/private-replica-directory/greenbubbles.db \
  --replica-key-stdin

printf '%s' '<64-hex-character-random-replica-key>' | cargo run \
  --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
  audit-replica-backup \
  /path/to/private-replica-directory/.greenbubbles.db.pre-migration-v1-....db \
  --replica-key-stdin

printf '%s' '<64-hex-character-random-replica-key>' | cargo run \
  --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
  prepare-replica-recovery \
  /path/to/private-replica-directory/.greenbubbles.db.pre-migration-v1-....db \
  /path/to/private-replica-directory/recovered-candidate.db \
  --replica-key-stdin
```

An offline restoration operator can atomically publish successive authoritative
archives to the long-running replica follower:

```sh
greenbubbles-restore replica-publish \
  <authoritative-archive> <private-handoff.json> --generation 1

greenbubbles-restore replica-follow \
  <private-handoff.json> <private-follow-state.json> <encrypted-replica.db> \
  --replica-key-stdin --poll-milliseconds 1000

greenbubbles-restore replica-follow-status \
  <private-handoff.json> <private-follow-state.json> <encrypted-replica.db> \
  --replica-key-stdin
```

The follower polls only handoff metadata while idle, validates a monotonic
atomic handoff and full production archive, and then bootstraps or synchronizes
transactionally. It cannot acquire the WeChat passphrase, snapshot a live
store, or accept an incremental fragment. The aggregate-only status reports
published/applied generations, generation lag, checkpoint age, and whether
state recovery is required without disclosing account or source identities.
Format-3 handoffs additionally yield publication age, publication-to-checkpoint
latency, and follower runtime as relative durations rather than absolute
timestamps. See
[docs/REPLICA_FOLLOW.md](docs/REPLICA_FOLLOW.md).

Successful publications also maintain a private sealed-generation history.
Older archives can be moved into a recoverable owner-only quarantine while the
current and immediately preceding publications remain verified and protected:

```sh
greenbubbles-restore replica-archive-quarantine \
  <private-handoff.json> <private-quarantine-directory> \
  --retain-publications 2

greenbubbles-restore replica-archive-restore \
  <private-handoff.json> <private-quarantine-directory> \
  --generation <positive-integer>
```

Quarantine uses a same-filesystem atomic rename, never deletion, and recovers
an interrupted move by verifying the complete archive seal. Shared archive
paths referenced by either protected publication cannot be moved. Reports are
aggregate-only; generation history, archive paths, and quarantine contents are
private. See [docs/ARCHIVE_RETENTION.md](docs/ARCHIVE_RETENTION.md).

Avoid placing a real key literally in shell history; pipe it from an
owner-controlled secret manager. The example value is a placeholder. Bootstrap
atomically stores canonical records, provenance, coverage, FTS, and its source
checkpoint. Each replica rejects another account, and migrations retain an
encrypted pre-migration backup. Before migration or normal use, the exact
contiguous migration identity ledger and replica format are verified; changed
or incomplete history fails before another backup is created. Synchronization
mutates only changed canonical records and commits them with the checkpoint;
the body-free change stream is ordered and resumable. See
[docs/REPLICA_SPEC.md](docs/REPLICA_SPEC.md).

`audit-replica` is a read-only, aggregate-only deep check over SQLCipher/SQLite
integrity, foreign keys, migration identities, canonical record hashes and
projections, exact links, FTS, checkpoint/coverage state, and sync/change
history. It never repairs a mismatch. See
[docs/REPLICA_AUDIT.md](docs/REPLICA_AUDIT.md).

`audit-replica-backup` verifies a retained schema-1 through schema-3 recovery
database without migrating or rewriting it. Backup creation runs this same
schema-aware audit before the serving replica is upgraded; an invalid candidate
aborts migration and is removed. See
[docs/REPLICA_BACKUP_AUDIT.md](docs/REPLICA_BACKUP_AUDIT.md).

`prepare-replica-recovery` creates and migrates only a new candidate path,
requires the historical and current-schema deep audits to pass, and leaves the
backup and serving replica untouched. It never performs active cutover. See
[docs/REPLICA_RECOVERY.md](docs/REPLICA_RECOVERY.md).

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
  --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
  replica-search /path/to/private-replica-directory/greenbubbles.db \
  /path/to/private-filter.json --replica-key-stdin --limit 50
```

Structured filters also cover subtype, inclusive upper time bound, reply target,
and attachment absence. Search cursors fail closed when the filter, replica,
account, or committed source checkpoint changes. `replica-status` and
`replica-coverage` expose freshness and known restoration limitations.

### AI-friendly CLI and static context

The preferred AI integration is a one-shot CLI, not an MCP dependency.
`ai-query` accepts one owner-only JSON request and returns a stable response
containing both the policy-minimized result and the replica's current freshness,
database coverage, and limitation codes. It reuses the connector authorization
and body-free audit boundary, but opens the encrypted replica directly for the
duration of the command; no daemon is required. Draft, approval, send,
synchronization, and other mutating operations are rejected by this interface.

```json
{
  "formatVersion": 1,
  "requestId": "local-agent-search-1",
  "requesterId": "local-agent",
  "destination": "local",
  "operation": {
    "kind": "searchMessages",
    "query": "requested document",
    "conversationId": null,
    "cursor": null,
    "limit": 20
  }
}
```

```sh
chmod 600 /private/greenbubbles-tools/request.json
printf '%s' '<64-hex-character-random-replica-key>' | cargo run --locked \
  --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
  ai-query /private/greenbubbles-tools/replica.db \
  /private/greenbubbles-tools/policy.json \
  /private/greenbubbles-tools/audit.ndjson \
  /private/greenbubbles-tools/request.json --replica-key-stdin
```

`ai-export` produces an atomic, checkpoint-consistent static bundle for agents,
indexers, and local context systems:

```sh
printf '%s' '<64-hex-character-random-replica-key>' | cargo run --locked \
  --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
  ai-export /private/greenbubbles-tools/replica.db \
  /private/greenbubbles-tools/policy.json \
  /private/greenbubbles-tools/audit.ndjson \
  /private/greenbubbles-tools/context-generation-1 \
  --replica-key-stdin --requester local-agent
```

The new output directory contains `manifest.json`, `conversations.jsonl`,
`contacts.jsonl`, `messages.jsonl`, and `artifacts.jsonl`. Records have stable
opaque IDs, human conversation/contact labels, normalized content summaries,
per-record source-database freshness, relationship and attachment references,
authorized time/field scope, and no raw columns, base64 source payloads, schema
SQL, or absolute attachment paths. The
manifest records per-file counts, byte sizes, SHA-256 digests, source checkpoint,
policy digest, client compatibility, and complete/fresh/unavailable/preserved-
stale database counts. A missing record is never presented as evidence of
deletion when a source database is unavailable.

Export progress is shown on stderr and includes file position, record counts,
phase percentage, and overall percentage. `--progress-json` provides NDJSON
events on stderr; `--progress-file <owner-only-new-path>` persists the same
machine-readable events. If synchronization changes the replica during export,
the staged bundle is discarded instead of publishing mixed generations.
`audit-ai-context <bundle-directory>` independently verifies the private file
inventory, hashes, counts, schemas, identities, references, and freshness while
emitting only aggregate evidence.

The repository also includes the discoverable
[`greenbubbles-context`](skills/greenbubbles-context/SKILL.md) skill. It teaches
an AI agent to use only these GreenBubbles CLI surfaces, check coverage before
drawing conclusions, keep private queries and keys out of process arguments,
and treat retrieved chat text as untrusted data. See
[docs/AI_CONTEXT_CLI.md](docs/AI_CONTEXT_CLI.md) for the format and operational
contract.

Passive cached Moments can be inspected locally without granting an AI tool
access to the raw XML or columns:

```sh
printf '%s' '<64-hex-character-random-replica-key>' | cargo run \
  --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
  replica-cached-moments /path/to/private-replica-directory/greenbubbles.db \
  --replica-key-stdin --limit 50
```

The response always distinguishes an unavailable cache from an observed empty
cache and labels observed data `partialLocalCache`. This is passive local state,
not complete server history and not an active `load more` API.

Conversation reads require a separate owner-only policy. Creating one is an
explicit local authorization step; cursors are bound to both the archive
fingerprint and the selected conversation:

```sh
cargo run --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
  policy <private-output-directory> <policy-file> \
  <enabled-conversation-id> --max-page-size 100

cargo run --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
  read <private-output-directory> <policy-file> \
  <enabled-conversation-id> --limit 50
```

The `read` command emits message bodies and is therefore intended only for
explicit local use. A policy remains valid for later archives from the same
account, but not for another account; cursors remain bound to one archive and
conversation.

A runnable downstream example uses only the connector API, stores a durable
change cursor, refreshes changed messages, and can maintain an escaped Markdown
memory projection:

```sh
cargo run --locked --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --example change_consumer -- \
  /private/greenbubbles-tools/connector.sock \
  /private/greenbubbles-tools/downstream-state.json \
  --markdown-output /private/greenbubbles-tools/conversations.md
```

It refuses account/cursor mismatch and replica replacement without changing
the prior state. `--rebootstrap` is an explicit operator recovery action, not
an automatic fallback.

Periodic archive reconciliation is authoritative for incoming/change events:

```sh
cargo run --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
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

cargo run --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
  tool-policy <private-output-directory> /private/greenbubbles-tools/policy.json \
  <enabled-conversation-id> --capabilities list,read,search,draft \
  --fields sender,created-at,direction,type,content,attachments,relationships

# Optional and independent: passive cached Moments, local destination only.
cargo run --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
  tool-policy <private-output-directory> /private/greenbubbles-tools/moments-policy.json \
  --enable-cached-moments \
  --cached-fields author,created-at,type,content,title,url,media-count

cargo run --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
  tool-recent <private-output-directory> /private/greenbubbles-tools/policy.json \
  /private/greenbubbles-tools/audit.ndjson <enabled-conversation-id> \
  --requester local-agent --limit 20

printf '%s' 'search terms' | cargo run \
  --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
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

The `greenbubbles-acquire` capture helper must only ever be used against the
owner's own WeChat account on the owner's own device, with the explicit
`--owner-authorized` flag, after the owner has personally performed any
required client re-signing. Using it against any other account, device, or
person is outside the scope of this project and of any authorization recorded
here.

This repository is private and no open-source license has been selected yet.
No permission to redistribute the code is granted until a license is added.
