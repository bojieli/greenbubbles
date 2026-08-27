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

Replica schema version 2 stores:

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
schema-1 database is backed up in encrypted form before migration to schema 2.
