# Live Query and Recoverable Snapshot Architecture

Status: accepted design; implementation in progress
Last updated: 2026-08-28

## 1. Decision

GreenBubbles will serve WeChat history through bounded, typed, read-only queries
against the original SQLite/WCDB databases. JSON is a response format, not the
primary persistence format.

The default path is:

```text
Live encrypted WeChat SQLite/WCDB
        |
        v
Read-only schema adapter and decoder
        |
        v
Bounded keyset pagination
        |
        v
Small, versioned JSON response
```

The same query contract will also work against optional snapshots:

```text
Live WeChat DB --short capture--> stable source clone
                                      |
                                      v
                          logical SQLite backup
                                      |
                                      v
                     GreenBubbles-keyed SQLCipher DBs
                                      |
                                      v
                           same read-only adapter
```

A durable snapshot must not depend on WeChat's database key. Snapshot creation
decrypts records through SQLite and writes them into a new database protected by
a GreenBubbles-controlled recovery key. Losing access to the running WeChat
application must therefore not make an otherwise intact backup unreadable.

The existing canonical JSON/JSONL restoration remains available as an explicit
forensic or interchange export during migration. It is no longer the required
serving path and must not be created as a side effect of an ordinary query.

## 2. Why the architecture is changing

The current restoration pipeline performs substantially more work than an
interactive history query needs. For every source row it can preserve both raw
and typed projections, base64-encode binary values, serialize JSON, compress and
stage that JSON, resolve relationships, serialize it again, import it into a
second database, update indexes, and populate full-text search tables. Media
restoration can additionally hash, decrypt, write, verify, and sync derivatives.

Measurements from the current owner-authorized corpus make the mismatch clear:

| Item | Observed size or count |
|---|---:|
| 26 source database groups | about 2.98 GB (2.78 GiB) |
| Source database files | about 2.92 GB |
| Source WAL files | about 60.4 MB |
| Restored messages | 1,855,548 |
| Message tables | 6,292 |
| Text-only canonical archive | about 13.50 GB |
| `messages.ndjson` alone | about 12.71 GB |
| Temporary staging SQLite peak | about 7.42 GB |
| Zstandard-compressed staged payload | about 4.74 GB |
| Eager media derivatives in one run | roughly 30 GB |
| Replica bootstrap WAL in one run | roughly 18 GB |

The source databases are compact because SQLite stores typed integers and BLOBs
without JSON field names or base64 expansion. The restored form duplicates
information for provenance, compatibility, audit, indexing, and presentation.
Those properties can be useful for a forensic export, but they are a poor price
to pay before returning one page of one conversation.

## 3. Product goals

The redesign has the following product requirements:

1. Return the first useful page without restoring the full corpus.
2. Keep normal response sizes suitable for a person, script, or language model.
3. Make every source access demonstrably read-only.
4. Avoid long-lived read transactions that interfere with WeChat's WAL
   checkpoint and recycling behavior.
5. Use keyset cursors so later pages do not become slower or shift like offset
   pages.
6. Preserve typed message decoding and tolerate individual unreadable shards.
7. Resolve attachments only when explicitly inspected or materialized.
8. Support repeatable queries through optional immutable snapshots.
9. Make durable snapshots recoverable without WeChat or its key material.
10. Keep an explicit, auditable JSONL export for interoperability and forensic
    preservation without making it the canonical online store.

## 4. Non-goals

The AI-facing CLI will not:

- accept arbitrary SQL;
- expose an unrestricted `--all` operation;
- hold a source transaction open while a caller or model processes a response;
- promise one atomic point in time across all WeChat databases during a live
  query;
- eagerly copy or decode all media;
- treat a copied encrypted WeChat file as a sufficient long-term backup;
- silently place decrypted databases or keys in ordinary temporary directories.

## 5. Product surface

The intended resource-oriented commands are:

```text
conversations list <database-root> [--limit N] [--cursor TOKEN]
messages list <database-root> --conversation ID [--limit N] [--cursor TOKEN]
messages search <database-root> --query-stdin [--conversation ID]
                [--limit N] [--cursor TOKEN]
message get <database-root> --conversation ID --message ID
attachment inspect <database-root> --message ID --attachment ID
attachment materialize <database-root> --message ID --attachment ID
                       --output PATH
snapshot create <live-database-root> <new-snapshot-directory>
snapshot verify <snapshot-directory>
snapshot rekey <snapshot-directory>
export jsonl <source> <new-output-directory>
```

Initial implementation may expose a documented subset, but command semantics
and response contracts must converge on this resource model. Every data command
accepts exactly one database access mode:

```text
--passphrase-stdin    encrypted live WeChat source; key read from standard input
--decrypted           explicitly allow a plaintext SQLite source
--snapshot-key-stdin  GreenBubbles-protected snapshot; key read from standard input
```

Secrets are never accepted in command arguments or emitted in responses, logs,
errors, manifests, or cursors. Query text is read from standard input so shell
history and process listings do not disclose it.

### 5.1 Bounds

The first implementation uses these conservative defaults:

| Limit | Default | Hard maximum |
|---|---:|---:|
| Conversations per response | 100 | 500 |
| Messages per response | 100 | 500 |
| Search hits per response | 50 | 200 |
| One projected text field | 16 KiB | 16 KiB |
| Serialized JSON response | 8 MiB | 8 MiB |
| Query text | 16 KiB | 16 KiB |

Limits can be revisited using measured latency and memory data, but callers
cannot override the hard maximums. An explicit offline export is the supported
way to process an entire corpus.

## 6. Versioned JSON contract

All commands return an envelope rather than an unqualified JSON array:

```json
{
  "schema": "greenbubbles.query.v1",
  "formatVersion": 1,
  "operation": "messages.list",
  "ok": true,
  "source": {
    "mode": "liveEncrypted",
    "identity": "sha256:opaque-prefix"
  },
  "consistency": {
    "guarantee": "perDatabaseReadStatement",
    "databaseCount": 4,
    "crossDatabaseAtomic": false,
    "observedAtUnixMilliseconds": 1787880000000
  },
  "page": {
    "limit": 100,
    "returned": 100,
    "hasMore": true,
    "nextCursor": "opaque-token"
  },
  "warnings": [],
  "items": []
}
```

Errors use the same schema and operation fields with `ok: false`, a stable error
code, and a safe description. Content, source paths, SQL, and key material must
not appear in error details.

Compatibility rules are:

- additive optional fields do not change `formatVersion`;
- removing or changing the meaning of a field requires a new version;
- cursors carry their own version and are rejected by incompatible operations;
- unknown input options fail closed rather than being ignored;
- source schema incompatibility is reported explicitly.

## 7. Live database adapter

The pinned `wx-db` dependency already opens SQLCipher databases with
`SQLITE_OPEN_READ_ONLY`, applies `sqlite3_key()`, validates the key, and sets
`PRAGMA query_only = ON`. GreenBubbles will reuse this code for encrypted live
access and its existing decoders for typed message content.

The adapter has four layers:

1. **Source validation**: canonicalize the selected account database root, ensure
   it is a current-user-owned directory, reject unsafe file types, and derive an
   opaque source identity without returning the path.
2. **Schema routing**: resolve `contact/contact.db`, `session/session.db`, and the
   numbered `message/message_N.db` shards. Table identifiers are computed from
   the conversation ID and never supplied as raw SQL by the caller.
3. **Bounded query**: push a compound keyset predicate and `LIMIT + 1` into each
   relevant SQLite statement. Merge only the bounded shard windows in memory.
4. **Projection**: decode typed values, omit implementation-only raw columns,
   truncate oversized presentation fields on UTF-8 boundaries, and serialize a
   bounded envelope.

Connections are command-scoped in the CLI. Each statement completes before JSON
serialization and before control returns to a language model. A later daemon may
pool connections, but it must reopen them after source changes and must retain
the same short-statement rule.

### 7.1 Consistency semantics

SQLite guarantees one stable view for an individual read statement. WeChat data
is split across databases, so a live message page that touches several shards is
not one globally atomic snapshot. The truthful guarantees are:

| Query | Guarantee |
|---|---|
| Conversation page | one statement in `session.db` |
| Message page in one shard | one statement in that shard |
| Message page across shards | one statement per shard, then deterministic merge |
| Live search across native FTS and shards | operation-specific, reported explicitly |
| Snapshot query | stable snapshot generation; statement-level within it |

The response always reports `databaseCount` and `crossDatabaseAtomic`. A caller
that needs stable multi-page or cross-database results should create a snapshot
and run the same query against that generation.

### 7.2 WAL behavior

A read-only SQLite connection still participates in WAL visibility. A long read
transaction can pin an old WAL frame and prevent checkpoint progress. Therefore:

- do not begin a transaction around language-model work;
- select one bounded page and finalize the statement immediately;
- close command-scoped connections before serializing the response when
  practical;
- impose a busy timeout and a wall-clock deadline;
- never copy only a `.db` file while it has uncheckpointed WAL state.

If policy requires zero interaction with live WAL/SHM state, the adapter can
query an APFS clone of only the required database and sidecars. That is a query
isolation option, not a full restoration.

## 8. Pagination and identifiers

Offset pagination is not used for the new commands. Cursors are URL-safe base64
encodings of a small versioned structure. They are opaque to callers and bind:

- operation kind;
- opaque source identity;
- query filters and conversation identity;
- the last returned compound ordering key;
- cursor format version.

Conversation ordering is:

```text
(sort_timestamp DESC, username ASC)
```

Message ordering is total and deterministic across shards:

```text
(sort_seq DESC, create_time DESC, server_id DESC, shard_id DESC, rowid DESC)
```

Including shard and row identity prevents unsynchronized messages with a zero or
repeated server ID from being skipped. A page queries keys strictly older than
the cursor. The implementation fetches at most `limit + 1` rows per relevant
shard, merges by the same compound key, returns `limit`, and uses the extra row
only to compute `hasMore`.

Cursor contents are not authorization. The CLI validates every cursor binding
and applies access policy independently. A future long-running service should
authenticate cursors with an installation-local MAC to prevent tampering.

## 9. Search

Search is optional infrastructure, not a reason to duplicate the full corpus.
The preference order is:

1. use compatible native WeChat FTS databases read-only;
2. use bounded indexed predicates where appropriate;
3. maintain a compact local FTS cache containing stable source references and
   normalized searchable text only.

The fallback cache does not store the complete canonical message JSON. It is
incremental, rebuildable, versioned, and keyed by source database identity plus
row identity. Search results are hydrated from the source adapter in a second
bounded step. Cache freshness and missed-shard warnings are included in every
response.

## 10. Attachments and media

Message pages return lightweight artifact references and availability metadata.
They do not decode every attachment.

`attachment inspect` may read headers and metadata without creating a durable
derivative. `attachment materialize` decrypts or converts exactly one selected
artifact into a new owner-only output path, verifies its digest, and reports the
result. Implementations must avoid an individual `fsync` for every file in a
bulk operation; an explicit export can batch durable writes and sync directory
boundaries.

## 11. Recoverable snapshot format

### 11.1 Independence from WeChat

An APFS clone of encrypted WeChat files is useful as a short-lived, consistent
capture, but it is not by itself a durable recovery product. It still requires
WeChat's raw key and SQLCipher parameters.

A GreenBubbles durable snapshot contains logical database copies created through
SQLite's backup API:

1. Acquire a stable source set using coordinated DB/WAL/SHM clone semantics.
2. Open the captured source read-only with the WeChat key.
3. Open a newly created destination database with a random GreenBubbles snapshot
   data-encryption key.
4. Use `sqlite3_backup` so SQLite reads decrypted logical pages from the source
   and writes pages encrypted under the destination key.
5. Checkpoint and close the destination so it has no required WAL sidecar.
6. Verify `cipher_integrity_check`, `integrity_check`, required schema, row-count
   evidence, and manifest hashes using only the snapshot key.
7. Remove the temporary encrypted source clone according to retention policy.

No plaintext SQLite staging file is required. `--decrypted` snapshot output is
an explicit portability export for an already protected destination and carries
a prominent warning.

### 11.2 Key hierarchy

Each snapshot generation receives a random 256-bit data-encryption key (DEK).
The DEK is independent of every WeChat key. It is wrapped using one or more
recovery protectors:

- an application passphrase processed by a memory-hard KDF such as Argon2id;
- a random recovery key that the owner can export and store offline;
- optionally a platform Keychain/Secure Enclave protector for convenience.

The manifest stores only KDF parameters, protector identifiers, salts, wrapped
DEKs, and authenticated metadata. It never stores a plaintext key or passphrase.
At least one portable recovery protector must be offered; a device-only Keychain
entry is not sufficient for a backup.

Cryptographic container details must be assigned a format version and reviewed
before release. Standard, maintained primitives are required; GreenBubbles will
not invent encryption algorithms. Rekeying should normally rewrap the DEK rather
than rewriting every database.

### 11.3 Recovery proof

`snapshot verify` must support a strict recovery test that deliberately uses no
WeChat key. It opens every database from the snapshot protector, verifies all
integrity checks and hashes, runs representative typed queries, and produces a
content-free report. A snapshot is not reported as recoverable until this test
passes.

The manifest records:

- snapshot and format versions;
- creation time and source build compatibility;
- opaque account binding;
- database inventory, logical roles, byte sizes, and SHA-256 digests;
- per-database capture window and consistency scope;
- source row-count evidence where inexpensive;
- protector metadata and recovery-verification result;
- optional parent generation for incremental history.

### 11.4 Immutability and retention

Immutability is a property of a published generation, not a requirement to keep
querying stale data. A generation is written into a new directory, verified,
fsynced, and atomically published. Existing generations are never updated in
place. Retention deletes only whole, explicitly selected generations after a
new recovery-verified generation exists.

Content encryption and filesystem access control are separate layers. Snapshot
directories remain owner-only and may additionally use filesystem snapshots,
backup media controls, or an immutable storage service.

## 12. Security and authorization

Read-only SQL is necessary but not sufficient. The product boundary also
requires:

- typed operations with allowlisted filters;
- current-user ownership checks for source and secret files;
- `O_NOFOLLOW`/`O_CLOEXEC` where file descriptors are opened directly;
- no raw source paths in normal JSON;
- per-operation hard limits, deadlines, and response-size enforcement;
- conversation and field authorization before returning content;
- audit events that contain identities and counts, not message bodies;
- zeroization of keys and passphrases after use;
- no network access in the local query path;
- explicit destination policy before content is sent to a remote model.

Direct CLI use initially inherits the invoking owner's filesystem authority. The
connector/service layer must continue to enforce requester, conversation, time,
field, and destination policies for AI consumers.

## 13. Failure behavior

The adapter distinguishes:

- invalid arguments or cursor bindings;
- unsafe source paths or permissions;
- incorrect keys;
- unsupported source schema/build;
- busy/timeout conditions;
- individual shard failures;
- decode omissions;
- response-bound violations.

An unreadable required database fails its resource operation. An unreadable
message shard can yield a partial page only when the response names the skipped
shard by opaque ID and marks coverage incomplete. Silent omission is forbidden.

## 14. Migration plan

### Phase 1: bounded live reads

- Add source validation, encrypted/decrypted open modes, opaque source identity,
  versioned envelopes, cursor codec, and response bounds.
- Implement keyset-paginated conversation listing.
- Implement keyset-paginated message listing across shards with typed decoding.
- Add synthetic plaintext and SQLCipher integration tests plus CLI help tests.

### Phase 2: complete query product

- Add single-message retrieval and contact/name enrichment.
- Add native FTS probing and bounded search.
- Add lazy attachment inspection and one-artifact materialization.
- Route the connector's read operations through the same adapter and policies.

### Phase 3: independent snapshots

- Add stable source capture and logical SQLite backup into independently keyed
  SQLCipher destinations.
- Implement passphrase and portable recovery-key protectors.
- Add verify, recovery-test, rekey, atomic publication, and retention commands.
- Run the exact same query adapter against snapshot roots.

### Phase 4: optional index and incremental behavior

- Add a compact reference/text FTS cache only where native FTS is insufficient.
- Track source identities and high-water keys per shard.
- Update proportionally to changed shards and invalidate on incompatible schema
  changes.

### Phase 5: retire mandatory restoration

- Make live or snapshot query the default History/AI path.
- Relabel canonical JSONL as an explicit export/audit format.
- Stop creating staging, full replica, FTS, or media derivatives for ordinary
  reads.
- Keep old readers during a documented compatibility window, then remove the
  mandatory restoration path after migration evidence is complete.

## 15. Verification and acceptance criteria

The redesign is complete only when all of the following are demonstrated:

1. A first conversation page and first message page can be returned from an
   owner-authorized encrypted live source without creating a canonical archive,
   replica, staging database, or media derivative.
2. Every query connection is opened read-only with `query_only` enabled, and a
   test proves writes fail.
3. Repeated cursor paging over fixtures returns every expected row exactly once,
   including duplicate timestamps, duplicate server IDs, and messages split
   across shards.
4. Limits, invalid cursors, oversized fields, unsafe paths, wrong keys, schema
   drift, and damaged shards have explicit tests.
5. Live-query latency and disk-write evidence show no corpus-sized write
   amplification.
6. A snapshot made from encrypted sources can be verified and queried using only
   its GreenBubbles recovery material after the WeChat key is withheld.
7. Snapshot creation leaves no plaintext SQLite database behind by default.
8. Snapshot generations pass database integrity, manifest hash, permission,
   atomic-publication, interrupted-write, rekey, and restore drills.
9. Search and attachment access remain bounded and lazy.
10. The History app and AI connector no longer require the JSON restoration path
    for ordinary browsing and retrieval.

## 16. Consequences

The main benefit is proportional work: asking for 100 messages reads and decodes
a bounded neighborhood instead of transforming 1.8 million rows. Disk usage for
ordinary access becomes effectively zero beyond small logs or an optional index.

The tradeoff is that live pages do not represent one global instant across all
WeChat databases, and schema adapters must evolve with WeChat releases. The
response makes that consistency limit explicit, while recoverable snapshots
provide the stable alternative. This is a better separation of concerns than
paying the cost of a forensic export for every interactive read.
