# Encrypted canonical replica

The canonical replica is the serving surface for future synchronization,
retrieval, local API, and MCP operations. WeChat databases remain acquisition
inputs; consumers never receive raw SQL access to either source or replica.

## Encryption and account isolation

Each replica file contains exactly one opaque account ID. Opening or importing
an archive for another account fails before content mutation. A separate,
high-entropy 32-byte replica key is supplied through standard input and held in
zeroized memory. It must not be reused as the WeChat database passphrase.

The file, WAL, SHM, and pre-migration backup are SQLCipher-encrypted and live
inside an owner-only directory. Temporary SQLite storage is forced to memory;
foreign keys, secure deletion, full synchronous commits, and encrypted WAL are
enabled. The connector rejects symlinked, multiply linked, group-readable, or
world-readable existing replica files. Exact artifact locations and full raw
canonical records therefore remain inside the encrypted boundary.

The caller is responsible for generating, storing, and recovering the replica
key with an appropriate local secret manager. GreenBubbles does not print it,
accept it as a command argument, or silently fall back to plaintext.

## Schema and provenance

Replica schema version 4 stores:

- the account and current source fingerprint;
- canonical conversations, participants, memberships, messages, artifacts,
  message-artifact links, and message relationships;
- each full canonical JSON record and its SHA-256 digest;
- normalized fields needed for exact filters and FTS5 text;
- the restoration report and complete schema/type coverage document;
- source checkpoints, synchronization runs, and an ordered change log.
- passive cached Moments/interactions and their explicit partial-cache coverage.

Unknown payloads, original source identities, raw SQLite values, exact verified
artifact paths, semantic gaps, and missing-media states remain in the encrypted
record JSON. FTS is an accelerator over normalized/local text, never the source
of truth.

## Transaction and migration invariants

Bootstrap inserts all canonical records, joins, coverage, synchronization run,
and authoritative source checkpoint in one immediate transaction. A crash
cannot commit the checkpoint without its records. Repeating the same bootstrap
is idempotent; presenting a different fingerprint requires the synchronization
path rather than silently replacing the replica.

Before bootstrap imports a production-format archive, it runs the independent
archive audit itself. Row accounting, identities, schema provenance,
relationship states, artifact state coherence, resource-row provenance, and
every currently recorded source/derivative file must therefore pass again at
the initial serving boundary. Synchronization remains change-proportional: it
descriptor/digest-verifies every added or changed artifact rather than rereading
all unchanged media, while explicit/periodic archive audits retain the full
integrity-scan role. Legacy synthetic format-2 fixtures remain isolated test
inputs; they are not accepted as production restoration evidence.

Before either bootstrap or synchronization imports a production-format
archive, it runs the independent archive audit itself. Row accounting,
identities, schema provenance, relationship states, artifact state coherence,
resource-row provenance, and every currently recorded source/derivative file
must therefore pass again at the serving boundary. Legacy synthetic format-2
fixtures remain isolated test inputs; they are not accepted as production
restoration evidence.

Every numbered migration is transactional and recorded with a migration
identity digest. Before upgrading an existing non-empty schema, GreenBubbles
uses SQLite's online backup API to create a same-key encrypted, mode-`0600`
pre-migration database in the replica directory. The backup filename—not its
absolute location—is the only backup reference exposed in normal reports.

Synthetic tests prove that plaintext headers, message text, and stable artifact
paths do not appear in the database bytes; unkeyed and wrong-key reads fail;
cross-account bootstrap fails; same-checkpoint bootstrap is idempotent; and a
schema-1 database is backed up in encrypted form before migration to schema 4.

## Transactional reconciliation and changes

`replica-sync` compares canonical SHA-256 record identities inside an immediate
encrypted transaction. It mutates only added, changed, or removed conversations,
participants, messages, artifacts, cached Moments, and cached Moment
interactions. Message FTS rows, quote/reply/recall
relationships, and artifact links are replaced only when their message changes.
An encrypted `sync_seen` table makes deletions explicit without writing IDs to
plaintext temporary files.

Coverage, restoration completion, the sync-run record, entity change events,
current account fingerprint, and source checkpoint commit in that same
transaction. Invalid or truncated JSON after earlier valid rows therefore rolls
back all provisional changes and leaves the prior checkpoint authoritative.
A committed archive is an idempotent no-op only when both its source
fingerprint and its restoration revision match the stored state. The revision
binds client compatibility, integrity/completion evidence, archive scope, and
the complete decoder coverage document. This permits a decoder upgrade or
media-enrichment pass to reconcile canonical records even when the underlying
source snapshot fingerprint is unchanged.

Synchronization runs are classified as `incrementalMerge`, `integrityScan`,
`fullScan`, or legacy `reconcile`; bootstrap remains a distinct run kind. The
classification and timings are stored in the encrypted replica rather than
inferred from wake-up hints.

`replica-changes` returns ordered, body-free entity metadata with a base64url
cursor bound to the opaque account ID, a random replica-generation ID, and last
sequence. Cursors remain valid across later synchronizations of that replica;
cross-account use and reuse against a replacement replica fail closed.
Downstream consumers bootstrap canonical data through scoped APIs, then use
this stream to know which stable entities require refresh.

## Change-proportional acquisition

Snapshot manifest format 3 carries a complete inventory of database/WAL/SHM
sets while copying only sets selected for the current run. A bootstrap and an
explicit integrity scan select every current set. A normal incremental plan
selects sets whose file identity changed, plus unchanged sets modified inside a
bounded reconciliation window; it also records source sets that disappeared.
The planner verifies the whole inventory before and after copying so a source
outside the selected subset cannot mutate unnoticed.

An unchanged file's SHA-256 may be carried forward only when its device, inode,
size, modification seconds, and modification nanoseconds are all unchanged.
The manifest source fingerprint covers the complete current inventory and its
content digests, not merely the copied fragment. Repeating a no-op plan
therefore retains the same authoritative source identity.

Acquisition evidence format 2 also carries the last full-scan anchor. When a
prior manifest is supplied, the snapshot CLI automatically selects integrity
scan mode once that anchor reaches the configured maximum age (seven days by
default, adjustable with `--integrity-scan-interval-seconds`). A bootstrap and
each integrity scan establish a new anchor; incrementals carry it forward.
`--integrity-scan` remains an immediate override. Frequent wake-up-driven
incrementals therefore cannot postpone a full comparison forever.

Incremental restoration fragments are deliberately not accepted directly by
`replica-bootstrap` or `replica-sync`. `merge-incremental` first binds the
fragment to the exact prior source fingerprint, removes prior records only from
selected or deleted source sets, and combines it with untouched canonical
state. It then recomputes conversation-wide ordering, cross-shard relationship
resolution, referenced artifacts, schema/type coverage, integrity counts, and
the row equation before marking the result authoritative.

The merge is staged in an owner-only sibling directory, syncs its files, and is
renamed into place only after validation. Connector-owned materialized and
decoded media needed by the merged history is copied into the new archive and
verified against its recorded SHA-256, so deleting the input fragment cannot
silently break artifact locations. Only the resulting authoritative archive can
advance the encrypted replica checkpoint.

## Exact retrieval and health

`replica-search` combines encrypted FTS5 with deterministic structured filters:
conversation, sender/participant, direction, logical type/subtype, inclusive
time range, relationship target, and attachment presence. Its filter document
is an owner-only JSON file so private search terms need not appear in process
arguments. Results are canonical lossless records, not generated summaries.

Message cursor format 2 binds the exact filter digest, account, replica
generation, source fingerprint, and a monotonically advancing checkpoint
revision. Changing the query or committing any reconciliation—including a
same-source decoder or media upgrade—invalidates pagination rather than
producing a mixed-state page. Change cursors deliberately remain resumable
across those commits.
`replica-message`, `replica-conversations`, and `replica-coverage` provide
stable JSON access to exact canonical data and machine-readable coverage. The
coverage includes the observed schema-profile and per-table fingerprints, so a
downstream operator can compare decoder runs without receiving schema SQL.

`replica-cached-moments` provides author/type/time filters and bounded,
checkpoint-consistent pagination. Its cursor binds the filter, account, random
replica generation, source fingerprint, and checkpoint revision. The response
distinguishes `unavailable`, `availableEmpty`, and `available`; it also reports
the exact observation time and `partialLocalCache` label. The raw canonical CLI
surface remains local/private. Connector and MCP consumers receive a separately
authorized minimized view instead.

`replica-status` exposes the schema/cipher, opaque account and source
fingerprints, exact client-build compatibility state and mismatched fields,
checkpoint revision, acquisition mode, media phase, decoder identity/version, canonical
counts, authoritative checkpoint age, latest synchronization kind/start/
duration, latest integrity-scan time and age, completion state,
source/restored row counts, semantic/message-candidate gaps, missing and
undecoded artifacts, entity gaps, and the calculated semantic-decoder coverage
ratio. The evidence is persisted inside the encrypted coverage state rather
than inferred from the client that happens to be installed when status is
queried. A current replica with known gaps is reported as
`currentWithCoverageGaps`; it is never labeled complete merely because the
latest synchronization committed successfully.
