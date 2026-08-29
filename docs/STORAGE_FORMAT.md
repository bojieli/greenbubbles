# What WeChat 4.1 writes to disk

This is the compatibility profile the passive adapter was built against. It is
evidence about the versions that were actually observed — not a promise about
one you have not tried.

The general rule throughout: anything not understood is recorded as a gap and
keeps `fullRestorationAchieved` false. Nothing is guessed into completeness.

## Which client

Passive restoration supports official signed macOS WeChat `4.1` and later, when
every identity check holds:

- bundle and signing identifier `com.tencent.xinWeChat`;
- a numerically valid marketing version of at least `4.1`;
- Team ID `5A4RE8SF68`;
- Hardened Runtime and a valid strict code signature.

Executable, CodeDirectory, build number and architecture evidence are recorded
for audit and incident diagnosis, but an ordinary update inside the signed 4.1+
family does not block restoration. The pinned `4.1.13`/`269579` fingerprint
reports `supportedPinned`; other qualifying versions report
`supportedCompatible`.

The debugger-based [acquisition helper](PASSPHRASE_ACQUISITION.md) stays
exact-build-bound, because attaching to a live process has a much narrower
safety boundary than reading files.

The snapshotter opens the executable read-only without following symlinks,
hashes it while checking file identity for mutation, invokes `codesign` and
`lipo` directly without a shell, and stores the evidence in the manifest.
Anything else is classified `unsupported` or `missing`, naming the field that
failed. Old format-1 synthetic fixtures are classified `legacySyntheticFixture`
and can never establish production support.

An unsupported build may still be parsed to retain authorized raw evidence —
but the resulting archive cannot set `fullRestorationAchieved`, and no future
active-read or write adapter may be enabled from it. Malformed fingerprints are
rejected before any database is prepared.

## The encrypted database family

The WCDB/SQLCipher-4 profile observed in macOS WeChat 4.1.x:

- 4096-byte pages;
- AES-256-CBC page encryption;
- PBKDF2-HMAC-SHA512, 256,000 iterations;
- HMAC-SHA512 page authentication;
- 80 reserved bytes per page;
- one 32-byte account passphrase shared across the account's stores, with a
  distinct per-file salt;
- encrypted WAL frames applied to the decrypted copy before reading.

The profile is explicit rather than probed. A plaintext `SQLite format 3`
header selects ordinary SQLite; **any** non-SQLite header is treated as the
pinned encrypted family and must decrypt successfully. There is no
guess-another-format fallback after a failure.

Passphrases enter through standard input and live only in zeroized process
memory. The engine has no runtime network client. Decrypted databases exist
only inside an owner-only temporary directory and are removed when the catalog
is dropped.

### Per-database key sets

`--database-keys-file` accepts an already-exported owner-only set of
per-database keys. It is opened without following symlinks; must be one
current-user-owned, single-link, mode-`0600` regular file within a bounded
size; is parsed into zeroizing memory; and every candidate is authenticated
against SQLCipher page 1 before use. Exact logical-path matches win; a
relocated entry is accepted only when exactly one candidate authenticates.

Restoration does not run, invoke, or depend on any key-acquisition tool.

### Partial coverage is a state, not a failure

Restoration is fault-tolerant at the database boundary. With an exported key
set it authenticates each database independently, continues with all healthy
ones, and records every unavailable database with its source set, logical path,
database and WAL byte counts, and reason.

A full-snapshot result containing any unavailable database is
`partialDatabaseCoverage` — not silently authoritative, but independently
auditable and replica-eligible. During incremental publication, a temporarily
unavailable changed source set keeps its prior canonical records as
`preservedStaleSourceSetIDs`. **It is never interpreted as deletion.** A later
successful restoration replaces the stale set and can return the archive to
authoritative coverage.

`diagnose-available` keeps the stricter `diagnosticSubset` label because it is
for bounded inspection, not publication, and diagnostic batches stay ineligible
for replica mutation.

### Before asking for a passphrase

```sh
greenbubbles preflight <snapshot>
```

validates the cross-language manifest, verifies the digest of every copied
database/WAL/SHM entry, and reads only the **first 16 bytes** of each copied
database through a regular-file, single-link, no-symlink descriptor. It reports
ordinary SQLite versus the pinned WCDB family and the resulting passphrase
requirement, without decrypting or enumerating any schema or content.

(Swift's encoded acronym keys — `snapshotID`, `sourceSetID`, `deviceID`,
`fileID`, `SHA256` — are canonical; the Rust reader also accepts the older
`Id`/`Sha256` spellings so existing synthetic archives stay readable.)

## Capture manifests

**Format 3** separates the complete source-set inventory from the database sets
copied in one acquisition. Database, WAL and SHM presence plus full file
identity determine change selection. It records bootstrap, incremental or
integrity-scan mode, the bounded reconciliation window, selected sets, deleted
sets, and a verified SHA-256 for every current source file. Rust validates that
the selected entries exactly match the inventory before opening anything.

**Format 4** is the current exporter contract. Before copying, the exporter
resolves the one account directory beneath `db_storage` that contains every
supplied database and sidecar, derives the source account identifier from it,
records that only in the private manifest, and exposes a SHA-256 identity
derived from the canonical account-root path. Common `wxid_*_XXXX` suffixes are
removed deterministically; a non-`wxid` suffix is removed only when an
independent `all_users/login/<candidate>` directory confirms it.

**Contact names, message contents, group ownership and traffic patterns are
never used to determine identity.** Moving the account root changes the
path-derived opaque ID and therefore requires a fresh bootstrap rather than an
incremental chain. The selected directory must be a real directory, not a
symlink, checked before its canonical path is used.

The complete format-4 binding is included in the inventory source fingerprint.
Incremental planning rejects a format-3 predecessor with no binding, any
predecessor whose binding differs, a database outside the chosen account root,
mixed account roots, and a sidecar from another root. Restoration converts the
private identifier into one account-scoped opaque `selfParticipantId`; the raw
identifier never enters an ordinary report or an AI bundle.

### How files are captured

On APFS, each source is opened `O_RDONLY | O_NOFOLLOW | O_CLOEXEC` and that
descriptor is passed to `fclonefileat`. The copy-on-write clone captures one
file atomically without widening the capture window while a large file is
hashed. A database and its WAL/SHM sidecars are captured consecutively and the
database identity must stay stable through the group; sidecars may keep
advancing after their own atomic capture. Each clone is mode `0600`, hashed
only after the whole group is captured, and records
`captureMethod: atomicCopyOnWriteClone`.

Unsupported filesystems fall back to a descriptor byte copy, where every source
in the group must remain stable for the whole copy.

Acquisition evidence format 2 records the last bootstrap or integrity-scan
anchor. The planner automatically selects every current set after a configured
maximum interval, while incrementals preserve the anchor. **This scheduling
lives in the manifest and is never inferred from a filesystem wake-up hint.**

## Message and auxiliary stores

Message tables are discovered by *both* hashed `Msg_`/`Chat_` naming and
required column signatures, so ordinary, business and chatbot schema variants
are all included. Field aliases resolve dynamically. Every column is retained
with its original SQLite storage class and bytes.

Every table in every prepared database is recorded in the schema coverage
ledger. A message-like table that does not meet the supported signature stays
an explicit **completion-blocking candidate** until its role is proved and an
adapter or auxiliary classification is added. That is the mechanism that stops
an unknown table from quietly disappearing.

Coverage format 3 fingerprints every table from ordered `PRAGMA table_xinfo`
metadata plus its related `sqlite_schema` table, index and trigger objects,
with a second digest binding the complete ordered logical-path/table profile.
These SHA-256 values expose exact schema drift without publishing the
underlying SQL, and row mutations do not affect them. Older archives
deserialize with absent fingerprint evidence and cannot be silently upgraded to
a claimed observed profile.

Known auxiliary chains:

```text
message row
  → MessageResourceInfo (local/server ID, packed-info bytes)
  → MD5 / title metadata
  → account-scoped msg or business media tree

voice message
  → VoiceInfo (server ID, then local-ID fallback)
  → raw Tencent SILK payload
```

`MessageResourceInfo`, `VoiceInfo`, session, contact and group columns are
matched through verified aliases rather than one fixed schema. Artifact
provenance records the resource database set, logical path, opaque table ID,
exact table name and row ID as one indivisible group; the archive audit
resolves that group back to complete schema coverage, and a partial or
substituted auxiliary-table identity fails closed.

## Media

Encrypted image sources are retained, with support for legacy single-byte XOR,
V1 fixed-key AES, and V2 per-account-key `.dat` variants. Images, stickers,
video payloads and posters, documents, thumbnails and raw voice blobs are
verified and recorded. Voice transcoding to Ogg Opus is attempted and **never**
replaces the SILK source.

Not-downloaded, remote-only, expired, deleted, corrupt, ambiguous, unsafe and
key-unavailable are distinct states. The adapter does not infer which remote
state applies unless local metadata proves it — a generic local miss stays
`notDownloaded` rather than becoming a confident claim about a server.

## Nested message XML

Merged histories (`49:19`) and Finder/channel media (`49:51`, `49:63`) retain
the decoder's complete `raw_xml` and also carry a versioned `normalized_xml`
projection when the graph parses safely.

The projection is an ordered **generic** XML tree, not a guessed private
schema: element and attribute names, namespace URIs and declarations, text,
comments and processing instructions are all retained. XML embedded in
`recorditem`, `content` or `recordxml` text is recursively projected, so a
forwarded message's children do not stay an opaque string.

The parser disables DTD processing and bounds each document to 8 MiB and
100,000 nodes, with at most four levels of embedding. **Raw XML remains
authoritative.** A malformed, incomplete, oversized or over-deep graph keeps
its raw value and an explicit semantic gap rather than being discarded or
labelled complete. Archive audit regenerates every present projection from the
raw XML and requires an exact match.

## An example of the profiler catching a real bug

Payload-profile format 2 records, for each canonical relationship, whether an
identifier was already present, is recoverable from the typed decoder's
source-preserving raw XML, is absent from that XML, or has no decoded XML
evidence at all. It emits no identifier, body, source identity or byte sample.

This caught an actual adapter error: the relationship extractor was searching a
*compressed* source column even though the exact decoded quote XML had already
been retained. Current restoration parses identifiers from decoded XML while
keeping the original source-column bytes as provenance.

On the owner-local aggregate, the profiler classified all 193,503 relationship
references: 1 identifier already present, 192,991 recoverable from decoded XML,
511 absent from decoded XML, 0 lacking decoded-XML evidence. Aggregate
diagnostic counts only — no identifiers, no XML.

## Progress, and why it does not lie

Restoration is observable from snapshot verification through independent
archive audit. Progress format 3 reports seven named phases where applicable:
snapshot verification, key validation, database preparation, record planning,
record restoration, archive finalization, archive audit. Each event carries a
monotonic workflow-stage position plus phase-local and current-item
completed/total values — and the stage position is deliberately **not**
presented as a wall-clock ETA.

Byte-level events cover every database, WAL and SHM digest verification,
decryption and copying, encrypted-WAL scan, and committed-frame apply.
Record-level events cover table planning, each message table, cached Moments
and interactions, canonical ledger writing, and audit ledgers. Finalization
reserves explicit work for entities, cached surfaces, coverage and the archive
report, so the visible phase cannot round to `100.0%` while material work
remains.

### Space, checked before anything is created

After row planning and before creating an archive file, restoration emits a
storage preflight: selected source bytes, message and observed-table record
counts, estimated archive bytes, estimated compressed-spool bytes, estimated
peak bytes, free bytes, and required free bytes. The estimate uses documented
saturating expansion allowances for lossless JSON and base64 projections,
per-record metadata and indexes, plus a ten-percent (minimum 64 MiB) operating
reserve. Insufficient space fails **before** the output directory exists.

The ordering spool is an owner-only, archive-local temporary SQLite database;
each canonical JSON envelope is compressed independently with Zstandard level 1
so ordered emission stays streaming and corruption stays row-local. Its guard
removes only the `.staging-*` directory, on normal completion or a propagated
error — never partially published archive evidence, never another path.

`report.json.storage` records the initial estimate and free-space evidence,
peak measured spool bytes, compressed versus uncompressed payload totals, and
the exact final archive byte count. Independent audit remeasures the archive
and rejects an inconsistent storage equation, an unsafe file, a retained
ordering spool, or a final byte-count mismatch.

### Where progress goes

Human progress goes to standard error by default; the same schema is available
as NDJSON via `--progress-json` or a create-new owner-only `--progress-file`,
with final summaries on standard output or a separate `--summary-file`.

Progress contains no keys or row values, but database logical paths and schema
metadata still make it private evidence. The human reporter throttles
repetitive per-table events into periodic cumulative updates while always
printing phase and database milestones; NDJSON and the progress file keep the
full stream. The file is flushed after every event, synchronized at least every
five seconds and at completion, and a write or sync failure is reported once
and latched so it can neither flood stderr nor stall the restoration. Offline
publication forwards the same ledger events during its final audit, so a
successful restore cannot vanish into an unreported validation pass.

## What stays uncertain

Observed-but-unknown message types, generic app subtypes, nested XML that fails
bounded normalization, sender-less rows with no explicit direction,
sender/account conflicts with explicit direction flags, failed group protobufs,
ambiguous relationships and unavailable media decoders all remain
machine-readable coverage gaps.

None of them prevent raw retention. All of them keep `fullRestorationAchieved`
false until the exact observed corpus is understood. See
[KNOWN_LIMITATIONS.md](KNOWN_LIMITATIONS.md).
