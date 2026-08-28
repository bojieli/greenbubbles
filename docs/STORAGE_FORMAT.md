# WeChat 4.1+ local-storage compatibility profile

This document records the evidence used by the current passive restoration
adapter. It is a compatibility profile, not a promise about untested WeChat
versions.

## Supported client family

Snapshot manifest format 2 carries signed-client evidence gathered before any
database restoration. Passive restoration supports official signed macOS
WeChat versions `4.1` and later when all of these identity checks hold:

- bundle/signing identifier `com.tencent.xinWeChat`;
- a numerically valid marketing version at least `4.1`;
- Team ID `5A4RE8SF68`;
- Hardened Runtime and a valid strict code signature.

The executable, CodeDirectory, build number, and architecture evidence remain
recorded for audit and incident diagnosis, but an ordinary update within the
signed `4.1+` family does not block restoration or publication. The pinned
`4.1.13`/`269579` fingerprint is reported as `supportedPinned`; other
qualifying versions are reported as `supportedCompatible`. The separately
gated debugger-based passphrase-acquisition helper remains exact-build-bound
because its live attachment behavior has a narrower safety boundary.

The snapshotter opens the executable read-only without following symlinks,
hashes it while checking file identity for mutation, invokes `codesign` and
`lipo` directly without a shell, and stores the evidence in the snapshot
manifest. Restoration otherwise classifies evidence as `unsupported` or
`missing` and names the incompatible identity/version field. Old format-1
synthetic fixtures are classified separately as `legacySyntheticFixture` and
never establish production support.

An unsupported or missing build may still be parsed to retain authorized raw
evidence, but an archive produced from that snapshot cannot set
`fullRestorationAchieved` and no future active-read or write adapter may be
enabled from it. Malformed fingerprints are rejected before database
preparation.

## Supported encrypted database family

The adapter targets the WCDB/SQLCipher-4 profile observed in macOS WeChat
4.1.x:

- 4096-byte pages;
- AES-256-CBC page encryption;
- PBKDF2-HMAC-SHA512 with 256,000 iterations;
- HMAC-SHA512 page authentication;
- 80 reserved bytes per page;
- one 32-byte database passphrase shared by the account stores, with a distinct
  per-file salt;
- encrypted WAL frames applied to the decrypted copy before reading.

The encrypted-page profile is explicit in the restoration dependency. A plaintext
`SQLite format 3` header selects ordinary SQLite; any non-SQLite header is
treated as the pinned encrypted family and must decrypt successfully. It is not
guessed as another format after failure.

Passphrases enter through standard input and live only in zeroized process
memory. The engine has no runtime network client. Decrypted databases exist
only in an owner-only temporary directory and are removed when the catalog is
dropped.

GreenBubbles can alternatively consume an already exported owner-only set of
per-database encryption keys through `--database-keys-file`. It opens that file
without following symlinks, requires one current-user-owned single-link `0600`
regular file within a bounded size, parses key material into zeroizing memory,
and authenticates candidates against SQLCipher page 1 before use. Exact
logical-path matches are preferred; a relocated entry is accepted only when
exactly one candidate authenticates. The restoration process does not run,
invoke, or depend on a key-acquisition/export tool.

Normal restoration is fault tolerant at the database boundary. With an
exported key set it authenticates every database independently, continues with
all healthy databases, and records every unavailable database with its source
set, logical path, database/WAL byte counts, and reason. A full-snapshot result
with any unavailable database is `partialDatabaseCoverage`, not silently
authoritative, but it is independently auditable and replica-eligible. During
incremental publication, a temporarily unavailable changed source set keeps
its prior canonical records as `preservedStaleSourceSetIDs`; it is never
interpreted as deletion. A later successful restoration replaces that stale
set and can return the archive to authoritative coverage.

The explicitly diagnostic `diagnose-available` command retains its stricter
`diagnosticSubset` label because it is intended for bounded inspection rather
than publication. Diagnostic batches remain ineligible for replica mutation.

Before a passphrase is requested, `greenbubbles-restore preflight <snapshot>`
loads and validates the cross-language manifest, verifies the digest of every
copied database/WAL/SHM entry, and reads only the first 16 bytes of each copied
database through a regular-file, single-link, no-symlink descriptor. It reports
ordinary SQLite versus the pinned WCDB/SQLCipher family and the resulting
passphrase requirement without decrypting or enumerating any schema or content.
Swift's encoded acronym keys (`snapshotID`, `sourceSetID`, `deviceID`, `fileID`,
and `SHA256` fields) are canonical; the Rust reader also accepts the older
`Id`/`Sha256` spellings so existing synthetic archives remain readable.

Snapshot manifest format 3 separates the complete source-set inventory from
the database sets copied in a particular acquisition. Database, WAL, and SHM
presence and full file identity determine change selection. The format records
bootstrap, incremental, or integrity-scan mode, the bounded reconciliation
window, selected sets, deleted sets, and a verified SHA-256 for every current
source file. Rust validates that the selected entries exactly match this
inventory before opening a database.

Snapshot manifest format 4 is the current exporter contract. Before copying,
the exporter resolves the one account directory containing every supplied
database and sidecar beneath `db_storage`. It derives the source account
identifier from that selected directory, records it only in the private
manifest, and exposes a SHA-256 account identity derived from the canonical
account-root path. Common `wxid_*_XXXX` directory suffixes are removed
deterministically; a non-`wxid` suffix is removed only when the independent
`all_users/login/<candidate>` directory confirms it. Contact names, message
contents, group ownership, and traffic patterns are never used.
Moving the selected account root changes this path-derived opaque account ID
and therefore requires a fresh bootstrap rather than an incremental chain.
The selected account directory itself must be a real directory, not a symbolic
link; this is checked before its canonical path is used for the account digest.

The complete format-4 account binding is included in the inventory source
fingerprint. Incremental planning rejects a format-3 predecessor without a
binding and any predecessor whose binding differs. It also rejects a database
outside the chosen account root, mixed account roots, or a WAL/SHM sidecar from
another root. Restoration converts the private source identifier to one
account-scoped opaque `selfParticipantId`; the raw identifier does not enter
ordinary reports or AI bundles.

On APFS, the snapshotter opens each source with `O_RDONLY`, `O_NOFOLLOW`, and
`O_CLOEXEC`, then passes that descriptor to `fclonefileat`. The copy-on-write
clone captures one file atomically without widening the capture window while a
large file is hashed. A database and its WAL/SHM sidecars are captured
consecutively; the database identity must remain stable through the group.
Sidecars may continue advancing after their atomic capture. Each clone is mode
`0600`, is hashed only after the group is captured, and records
`captureMethod: atomicCopyOnWriteClone`. Unsupported filesystems fall back to a
descriptor byte copy, for which every source in the group must remain stable
through the complete copy. The finalized authoritative inventory uses the
captured fingerprints and digests for selected sets and carried verified
evidence for unselected sets.

Acquisition evidence format 2 records the last bootstrap/integrity-scan anchor.
The planner automatically selects every current set after the configured
maximum interval, while incrementals preserve the anchor. This scheduling
metadata is part of the manifest, not inferred from filesystem wake-up hints.

## Restoration progress contract

Long-running GreenBubbles restoration is observable from snapshot verification
through independent archive audit. Progress-event format 3 reports seven named
phases where applicable: snapshot verification, key validation, database
preparation, record planning, record restoration, archive finalization, and
archive audit. Each event carries a monotonic workflow-stage position plus
phase-local and current-item completed/total values. The stage position is not
presented as a wall-clock ETA.

Byte-level events cover every database, WAL, and SHM digest verification,
database decryption/copying, encrypted-WAL scan, and committed-frame apply.
Record-level events cover table planning, each message table, cached Moments
and interactions, canonical ledger writing, and independent audit ledgers.
Events also carry available/unavailable key counts, database/file ordinals and
sizes, storage family, table role/schema metadata, restored/rejected counts,
semantic gaps, and elapsed milliseconds. Finalization reserves explicit work
for entities, cached surfaces, coverage, and the archive report, so the visible
phase cannot round to `100.0%` while material work remains.

After row planning and before creating an archive file, restoration emits a
storage preflight containing selected source bytes, message and observed-table
record counts, estimated archive bytes, estimated compressed-spool bytes,
estimated peak bytes, free bytes, and required free bytes. The estimate uses
documented saturating expansion allowances for lossless JSON/base64 projections,
per-record metadata, indexes, and a ten-percent (minimum 64 MiB) operating
reserve. Insufficient space fails before the output directory is created.
Progress then reports the actual SQLite spool size, compressed and source-JSON
payload bytes, current free/required bytes, and published archive bytes.

The ordering spool is an owner-only, archive-local temporary SQLite database;
each canonical JSON envelope is compressed independently with Zstandard level
1 so ordered emission remains streaming and corruption is row-local. Its guard
removes only the `.staging-*` directory on normal completion or any propagated
error. It never deletes partially published archive evidence or another path.
Synthetic large-spool tests verify byte-for-byte decompression, measured disk
reduction, and this cleanup boundary. `report.json.storage` records the initial
estimate and free-space evidence, peak measured spool bytes, compressed versus
uncompressed payload totals, and the exact final archive byte count. Independent
audit remeasures the archive and rejects inconsistent storage equations, unsafe
files, a retained ordering spool, or a final byte-count mismatch.

Human progress is written to standard error by default. The same schema is
available as NDJSON through `--progress-json` or a create-new owner-only file
through `--progress-file`; final summaries remain on standard output or a
separate owner-only `--summary-file`. Progress contains no keys or row values,
but database logical paths and schema metadata still make it private evidence.
The human reporter throttles repetitive per-table events to periodic
cumulative updates and always prints phase/database milestones; NDJSON and the
progress file retain the full event stream. Offline publication forwards the
same ledger byte/record events during its final independent archive audit, so a
successful restore cannot disappear into an unreported validation pass.

Payload-profile format 2 adds aggregate relationship-identifier evidence. For
each canonical relationship, it records whether an identifier was already
present, can be recovered from the typed decoder's source-preserving raw XML,
is absent from that XML, or lacks decoded XML evidence. It emits no identifier,
message body, source identity, or byte sample. This distinction caught a real
adapter error where the relationship extractor searched a compressed source
column even though the exact decoded quote XML was already retained. Current
restoration parses identifiers from decoded XML while preserving the original
source-column bytes as provenance. On the owner-local aggregate, the profiler
classified all 193,503 relationship references: 1 identifier was already
present, 192,991 are recoverable from decoded XML, 511 are absent from decoded
XML, and 0 lack decoded-XML evidence. These are aggregate diagnostic counts;
the profiler emits none of the identifiers or XML.

## Message and auxiliary stores

Message tables are discovered by both hashed `Msg_`/`Chat_` naming and required
column signatures, allowing ordinary, business, and chatbot schema variants to
be included. Field aliases are resolved dynamically. Every column is retained
using its original SQLite storage class and bytes.

Every table in every prepared database is also recorded in the schema coverage
ledger. Message-like tables that do not meet the supported adapter signature
remain explicit completion-blocking candidates until their role is proved and
an adapter or auxiliary classification is added.

Coverage format 3 fingerprints every table from ordered `PRAGMA table_xinfo`
metadata plus its related `sqlite_schema` table, index, and trigger objects. A
second digest binds the complete ordered logical-path/table profile. These
SHA-256 values expose exact schema drift without publishing the underlying SQL.
Row mutations do not affect them. Cached-surface coverage format 2 uses the same
fingerprints, and authoritative incremental merges recompute both profiles.
Older archives deserialize with absent fingerprint evidence and cannot be
silently upgraded to a claimed observed profile.

Known auxiliary chains include:

```text
message row
  -> MessageResourceInfo (local/server ID, packed-info bytes)
  -> MD5/title metadata
  -> account-scoped msg or business media tree

voice message
  -> VoiceInfo (server ID, then local-ID fallback)
  -> raw Tencent SILK payload
```

`MessageResourceInfo`, `VoiceInfo`, session, contact, and group columns are
matched through verified aliases instead of one fixed schema. Their source rows
are retained in the local archive where they contribute to a normalized
entity. Artifact provenance records the resource database set, logical path,
opaque table ID, exact table name, and row ID as one indivisible group. The
independent archive audit resolves that group back to complete schema coverage;
a partial or substituted auxiliary-table identity fails closed.

## Media variants

The adapter retains encrypted image sources and supports legacy single-byte XOR,
V1 fixed-key AES, and V2 per-account-key `.dat` variants. It verifies and
records images, stickers, video payloads and posters, documents, thumbnails,
and raw voice blobs. Voice transcoding to Ogg Opus is attempted without ever
replacing the SILK source.

Not-downloaded, remote-only, expired, deleted, corrupt, ambiguous, unsafe, and
key-unavailable media are distinct states. The current adapter does not infer
which remote state applies unless local metadata proves it; a generic local miss
therefore remains `notDownloaded`.

## Nested message XML

App-message subtypes for merged histories (`49:19`) and Finder/channel media
(`49:51` and `49:63`) retain the decoder's complete `raw_xml` value and also
carry a versioned `normalized_xml` projection when the graph can be parsed
safely. The projection is an ordered generic XML tree rather than a guessed
private schema: it retains element and attribute names, namespace URIs and
declarations, text, comments, and processing instructions. XML documents
embedded in `recorditem`, `content`, or `recordxml` text are recursively
projected so forwarded-message children do not remain an opaque string.

The parser disables DTD processing and bounds each document to 8 MiB and
100,000 nodes, with at most four embedded-document levels. The raw XML remains
authoritative. Malformed, structurally incomplete, oversized, or over-depth
graphs keep that raw value and an explicit semantic gap instead of being
discarded or labeled complete. Archive audit independently regenerates every
present projection from the raw XML. It also accepts older partial archives
that predate this projection, but a record labeled complete must contain an
exact reproducible projection.

## Uncertainty and completion

Observed-but-unknown message types, generic app subtypes, nested XML that fails
bounded structural normalization, sender-less rows with no explicit direction,
sender/account conflicts with explicit direction flags, failed group protobufs,
ambiguous relationships, and unavailable media decoders remain machine-readable
coverage gaps. They do not prevent raw retention, but they keep
`fullRestorationAchieved` false until the exact observed corpus is understood.
