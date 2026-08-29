# The encrypted replica

The canonical replica is the serving surface: synchronization, one-shot CLI
retrieval, static AI-context export, and the optional local API all read from
it. WeChat's databases stay acquisition inputs, and no consumer ever gets raw
SQL against either the source or the replica.

Running one is [REPLICA_OPERATIONS.md](REPLICA_OPERATIONS.md); verifying one is
[AUDITING.md](AUDITING.md).

## Encryption and account isolation

Each replica file holds exactly one opaque account ID. Opening or importing an
archive for a different account fails **before** any content mutation.

The replica key is a separate high-entropy 32-byte secret, supplied on standard
input and held in zeroized memory. **It must not be the WeChat database
passphrase.** GreenBubbles never prints it, never accepts it as an argument,
and never silently falls back to plaintext — generating, storing and recovering
it with a local secret manager is the caller's job.

The database, WAL, SHM and any pre-migration backup are SQLCipher-encrypted
inside an owner-only directory. Temporary SQLite storage is forced to memory;
foreign keys, secure deletion, full synchronous commits and encrypted WAL are
all enabled. The connector rejects an existing replica file that is symlinked,
multiply linked, or group- or world-readable. Exact artifact locations and full
raw canonical records therefore never leave the encrypted boundary.

## What schema 5 stores

- the account, the current source fingerprint, and the opaque account-holder
  participant identity;
- canonical conversations, participants, memberships, messages, artifacts,
  message-artifact links and message relationships;
- each full canonical JSON record and its SHA-256 digest;
- normalized fields for exact filters, plus FTS5 text;
- the restoration report and the complete schema and type coverage document;
- source checkpoints, synchronization runs and an ordered change log;
- passive cached Moments and interactions with their explicit partial-cache
  coverage.

Unknown payloads, original source identities, raw SQLite values, exact verified
artifact paths, semantic gaps and missing-media states all stay in the
encrypted record JSON. **FTS is an accelerator over normalized text, never the
source of truth.**

## Transactions and migrations

Bootstrap inserts all canonical records, joins, coverage, the synchronization
run and the authoritative checkpoint in **one** immediate transaction, so a
crash cannot commit a checkpoint without its records. Repeating the same
bootstrap is idempotent; presenting a different fingerprint requires the
synchronization path rather than silently replacing the replica.

Before either bootstrap or synchronization imports a production-format archive,
it runs the independent archive audit itself. Row accounting, identities,
schema provenance, relationship states, artifact state coherence, resource-row
provenance and every currently recorded file must pass **again** at the serving
boundary — the archive having passed once, earlier, elsewhere, is not
sufficient. Synchronization stays change-proportional: it descriptor- and
digest-verifies every added or changed artifact rather than rereading all
unchanged media, and explicit or periodic archive audits keep the full
integrity-scan role. Legacy synthetic format-2 fixtures are isolated test
inputs and are never accepted as production restoration evidence.

Every numbered migration is transactional and recorded with a migration
identity digest. Before upgrading a non-empty schema, GreenBubbles uses
SQLite's online backup API to create a same-key encrypted mode-`0600`
pre-migration database in the replica directory, converts it to
rollback-journal mode, closes it, and runs an independent schema-aware
read-only content audit *before* touching the serving schema. A failed
candidate is removed and migration never begins. Only the backup **filename**,
not its absolute location, appears in normal reports.

Opening a non-empty replica verifies the singleton schema row, replica format,
the exact contiguous migration sequence, positive recorded timestamps, and the
compiled identity digest for every migration that schema claims — and verifies
the same ledger again after applying migrations. A missing, extra, reordered,
malformed or changed entry fails before a new backup or migration is attempted.
The operator restores a known-good encrypted backup or explicitly
rebootstraps; GreenBubbles does not bless unexplained state.

That is tamper and corruption detection. It is **not** a signature against an
attacker who can replace the entire encrypted replica and its key.

Upgrading a populated schema-1 replica backfills the schema-2 FTS projection
from canonical messages and creates a checkpoint, a matching reconciliation run
and an initial checkpoint change event from the already committed identity and
counts. A migrated database must still pass the full current-schema audit: **a
newer schema number is not evidence of a usable serving replica.**

Synthetic tests prove that plaintext headers, message text and stable artifact
paths do not appear in the database bytes; that unkeyed and wrong-key reads
fail; that cross-account bootstrap fails; that same-checkpoint bootstrap is
idempotent; that a schema-1 database is backed up in encrypted form before
migrating to schema 5; and that changed migration digests, invalid format
versions and malformed timestamps are rejected without creating a misleading
new backup.

## Reconciliation and the change log

`replica-sync` compares canonical SHA-256 record identities inside an immediate
encrypted transaction, mutating only added, changed or removed conversations,
participants, messages, artifacts, cached Moments and cached interactions.
Message FTS rows, quote/reply/recall relationships and artifact links are
replaced only when their message changes. An encrypted `sync_seen` table makes
deletions explicit without writing IDs to a plaintext temporary file.

Coverage, restoration completion, the sync-run record, entity change events,
the current account fingerprint and the source checkpoint all commit in that
same transaction. Invalid or truncated JSON after earlier valid rows therefore
rolls everything back and leaves the prior checkpoint authoritative.

A committed archive is an idempotent no-op **only** when both its source
fingerprint and its restoration revision match stored state. The revision binds
client compatibility, integrity and completion evidence, archive scope, and the
complete decoder coverage document — which is what lets a decoder upgrade or a
media-enrichment pass reconcile canonical records even when the underlying
source snapshot has not changed at all.

Runs are classified `incrementalMerge`, `integrityScan`, `fullScan` or legacy
`reconcile`, with bootstrap a distinct kind. **The classification and timings
are stored in the encrypted replica, never inferred from a wake-up hint.**

`replica-changes` returns ordered, body-free entity metadata with a base64url
cursor bound to the opaque account ID, a random replica-generation ID and the
last sequence. Cursors stay valid across later synchronizations of that
replica; cross-account use and reuse against a *replacement* replica fail
closed. Downstream consumers bootstrap through scoped APIs, then use this
stream to learn which stable entities need refreshing.

## Change-proportional acquisition

Format-3 manifests carry a complete inventory of database/WAL/SHM sets while
copying only the sets selected for the current run. Format 4 keeps that
inventory and binds every acquisition to the one account directory containing
all selected databases and sidecars, storing the source account identifier only
in the private manifest, exposing only account-scoped opaque identities
downstream, and including the binding in the source fingerprint.

A bootstrap and an explicit integrity scan select every current set. A normal
incremental selects sets whose file identity changed, plus unchanged sets
modified inside a bounded reconciliation window, and records sets that
disappeared. The planner verifies the whole inventory before *and* after
copying, so a source outside the selected subset cannot mutate unnoticed.

An incremental format-4 snapshot must carry exactly the same integrity-bound
account evidence as its predecessor. A missing legacy binding, a changed
account directory, a database outside that account's `db_storage`, or a set
spanning multiple account roots all fail before copying — as does a symbolic
account directory, checked before canonicalization could obscure it. Legacy
format-3 snapshots stay readable but cannot silently become a bound incremental
chain. **Because the opaque account ID derives from the canonical account path,
moving that root changes the ID and requires a fresh bootstrap.** A path move
is never treated as an incremental continuation.

An unchanged file's SHA-256 may be carried forward only when device, inode,
size, modification seconds *and* modification nanoseconds are all unchanged.
The manifest source fingerprint covers the complete current inventory and its
content digests — not merely the copied fragment — so repeating a no-op plan
retains the same authoritative source identity.

Acquisition evidence format 2 also carries the last full-scan anchor. When a
prior manifest is supplied, the snapshot CLI automatically selects integrity
scan mode once that anchor reaches the configured maximum age (seven days by
default, `--integrity-scan-interval-seconds` to change it). Bootstrap and each
integrity scan establish a new anchor; incrementals carry it forward, and
`--integrity-scan` is an immediate override. Frequent hint-driven incrementals
therefore cannot postpone a full comparison forever.

Snapshot command report format 2 separately measures monotonic planning,
descriptor-based acquisition and total command time. It contains private
manifest and path material, so it is **not** itself a publishable benchmark —
only reviewed aggregate durations may be combined with publication and
follower deltas. See [MEASUREMENTS.md](MEASUREMENTS.md).

### Fragments are not accepted directly

`replica-bootstrap` and `replica-sync` deliberately refuse incremental
restoration fragments and diagnostic subsets. `merge-incremental` binds the
fragment to the exact prior source fingerprint, removes prior records only from
successfully restored or deleted source sets, and combines it with untouched
canonical state. A selected database that is temporarily unavailable retains
its prior records as an explicit stale source set. The merge then recomputes
conversation-wide ordering, cross-shard relationship resolution, referenced
artifacts, schema and type coverage, integrity counts and the row equation
before marking the result authoritative or `partialDatabaseCoverage`.

The merge is staged in an owner-only sibling directory, fsynced, and renamed
into place only after validation. Connector-owned materialized and decoded
media needed by the merged history is copied into the new archive and verified
against its recorded SHA-256, so deleting the input fragment cannot silently
break artifact locations.

Bootstrap, synchronization, status and coverage responses expose archive scope
plus authoritative, total, fresh, unavailable and preserved-stale database
counts. The detailed *private* archive report retains each unavailable source
set's logical path, storage family, sizes and reason; ordinary replica status
stays aggregate-only.

## Retrieval

`replica-search` combines encrypted FTS5 with deterministic structured filters:
conversation, sender or participant, direction, logical type and subtype,
inclusive time range, relationship target, and attachment presence. Its filter
document is an owner-only JSON file, so private search terms need not appear in
process arguments. **Results are canonical lossless records, not generated
summaries.**

Message cursor format 2 binds the exact filter digest, account, replica
generation, source fingerprint and a monotonically advancing checkpoint
revision. Changing the query, or committing any reconciliation — including a
same-source decoder or media upgrade — invalidates pagination rather than
producing a mixed-state page. Change cursors deliberately stay resumable across
those commits.

`replica-message`, `replica-conversations` and `replica-coverage` give stable
JSON access to exact canonical data and machine-readable coverage, including
the observed schema-profile and per-table fingerprints so a downstream operator
can compare decoder runs without ever receiving schema SQL.

`replica-cached-moments` offers author, type and time filters with bounded
checkpoint-consistent pagination, its cursor bound to filter, account, replica
generation, source fingerprint and checkpoint revision. Responses distinguish
`unavailable`, `availableEmpty` and `available`, and always report the
observation time and the `partialLocalCache` label. This raw CLI surface stays
local and private; policy-scoped AI and connector consumers get a separately
authorized minimized view. Cached table read failures are isolated — healthy
tables still publish, while `omittedRowCount`, per-table `availability` and
`limitationCode`, and aggregate `limitationCodes` carry the gap through archive
audit, coverage, status and query responses.

## Health

`replica-status` exposes the schema and cipher, opaque account and source
fingerprints, exact client-build compatibility and any mismatched fields,
checkpoint revision, acquisition mode, media phase, decoder identity and
version, canonical counts, authoritative checkpoint age, latest synchronization
kind, start and duration, latest integrity-scan time and age, completion state,
source and restored row counts, semantic and message-candidate gaps, missing
and undecoded artifacts, entity gaps, and the calculated semantic-decoder
coverage ratio. It reports `cachedSurfaceOmittedRowCount` when planned optional
cached rows could not be read.

Bootstrap, synchronization and audits also report omitted malformed
relationship and artifact-reference counts with typed `limitationCodes`; those
optional links never prevent the containing canonical message from committing.

Restoration and cached-surface evidence is persisted inside the encrypted
coverage state, **not** inferred from whichever client happens to be installed
when you ask. A current replica with known gaps reports
`currentWithCoverageGaps`. It is never labelled complete merely because the
latest synchronization succeeded.

## Artifact authorization

Normally this uses the `message_artifact` projection. If that optional index is
absent, incomplete or unreadable, the connector scans the bounded authorized
canonical message records instead, and a missing artifact metadata row becomes
a typed metadata-unavailable result. **If the canonical records cannot prove
the reference, access is denied** — the fallback degrades what you learn, never
who is allowed to ask.
