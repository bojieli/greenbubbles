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

Replica schema version 3 stores:

- the account and current source fingerprint;
- canonical conversations, participants, memberships, messages, artifacts,
  message-artifact links, and message relationships;
- each full canonical JSON record and its SHA-256 digest;
- normalized fields needed for exact filters and FTS5 text;
- the restoration report and complete schema/type coverage document;
- source checkpoints, synchronization runs, and an ordered change log.

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

Every numbered migration is transactional and recorded with a migration
identity digest. Before upgrading an existing non-empty schema, GreenBubbles
uses SQLite's online backup API to create a same-key encrypted, mode-`0600`
pre-migration database in the replica directory. The backup filename—not its
absolute location—is the only backup reference exposed in normal reports.

Synthetic tests prove that plaintext headers, message text, and stable artifact
paths do not appear in the database bytes; unkeyed and wrong-key reads fail;
cross-account bootstrap fails; same-checkpoint bootstrap is idempotent; and a
schema-1 database is backed up in encrypted form before migration to schema 3.

## Transactional reconciliation and changes

`replica-sync` compares canonical SHA-256 record identities inside an immediate
encrypted transaction. It mutates only added, changed, or removed conversations,
participants, messages, and artifacts. Message FTS rows, quote/reply/recall
relationships, and artifact links are replaced only when their message changes.
An encrypted `sync_seen` table makes deletions explicit without writing IDs to
plaintext temporary files.

Coverage, restoration completion, the sync-run record, entity change events,
current account fingerprint, and source checkpoint commit in that same
transaction. Invalid or truncated JSON after earlier valid rows therefore rolls
back all provisional changes and leaves the prior checkpoint authoritative.
Repeating a committed source fingerprint is an idempotent no-op.

`replica-changes` returns ordered, body-free entity metadata with a base64url
cursor bound to the opaque account ID, a random replica-generation ID, and last
sequence. Cursors remain valid across later synchronizations of that replica;
cross-account use and reuse against a replacement replica fail closed.
Downstream consumers bootstrap canonical data through scoped APIs, then use
this stream to know which stable entities require refresh.

## Exact retrieval and health

`replica-search` combines encrypted FTS5 with deterministic structured filters:
conversation, sender/participant, direction, logical type/subtype, inclusive
time range, relationship target, and attachment presence. Its filter document
is an owner-only JSON file so private search terms need not appear in process
arguments. Results are canonical lossless records, not generated summaries.

Message cursors bind the exact filter digest, account, replica generation, and
current source fingerprint. Changing the query or committing another source
checkpoint invalidates pagination rather than producing a mixed-state page.
`replica-message`, `replica-conversations`, and `replica-coverage` provide
stable JSON access to exact canonical data and machine-readable coverage.

`replica-status` exposes the schema/cipher, opaque account and source
fingerprints, exact client-build compatibility state and mismatched fields,
canonical counts, authoritative checkpoint age, completion state,
source/restored row counts, semantic/message-candidate gaps, missing and
undecoded artifacts, entity gaps, and the calculated semantic-decoder coverage
ratio. The evidence is persisted inside the encrypted coverage state rather
than inferred from the client that happens to be installed when status is
queried. A current replica with known gaps is reported as
`currentWithCoverageGaps`; it is never labeled complete merely because the
latest synchronization committed successfully.
