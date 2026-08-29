# Live Query and Recoverable Snapshot Architecture

Status: accepted design; implementation complete and verified
Last updated: 2026-08-29

Verified implementation on 2026-08-29:

- bounded `conversations list`, `messages list`, and exact `message get`
  commands for live encrypted, explicit plaintext, and independently encrypted
  snapshot sources;
- content-free `source status` accounting for database and SQLite sidecar bytes;
- version-1 response envelopes and source-bound keyset cursors;
- strict read-only/query-only opens, response and field caps, and partial-shard
  coverage reporting;
- bounded native WeChat FTS search with literal standard-input queries and
  source/filter-bound keyset cursors;
- a no-write decoded-source fallback when native FTS is absent, capped at 500
  messages and 16 conversations per response with resumable empty windows;
- lazy exact-message image, voice, video, and document inspection plus
  one-candidate materialization, with no path disclosure or inspection-side
  writes; the original image-MD5 form remains compatible;
- version-2 recoverable snapshot creation and verification using a random
  SQLCipher DEK wrapped by portable 24-word recovery material and optional
  owner-only local convenience material;
- atomic recovery-key rotation into a separate verified snapshot generation;
- complete acquisition-capture conversion, with capture hashes checked before
  use and again before atomic publication;
- native History app direct mode for source-size inspection, bounded
  conversation/message pagination, exact search-result retrieval, and native
  FTS against live, snapshot, or explicitly plaintext sources;
- a source-bound direct connector for `listConversations`, `getMessages`,
  `searchMessages`, and `getMessage`, with field/time/destination authorization,
  opaque keyset cursors, summary/result caps, and the existing chained audit;
- one-shot `connector-query-direct` and reusable `connector-serve-direct`
  entry points, neither of which creates or opens a canonical replica;
- bounded optional contact display-name enrichment for conversations, message
  senders, native search, fallback search, and exact hydration, including
  schema variants and truthful independent-database consistency reporting;
- recovery integration tests that delete the WeChat source before verification
  and querying, plus wrong-key and tamper tests.

Measured fallback latency does not currently justify a compact search
accelerator. Argon2id passphrase, macOS Keychain, and owner-only hidden-file
recovery wrappers are implemented under the phases below.

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
source status <database-root>
conversations list <database-root> [--limit N] [--cursor TOKEN]
messages list <database-root> --conversation ID [--limit N] [--cursor TOKEN]
messages search <database-root> --query-stdin [--conversation ID]
                [--limit N] [--cursor TOKEN]
message get <database-root> --conversation ID --message ID
attachment inspect <account-or-source-root> --conversation ID
                   --message OPAQUE_MESSAGE_ID
                   --kind image|voice|video|document <access-mode>
attachment materialize <account-or-source-root> --conversation ID
                       --message OPAQUE_MESSAGE_ID
                       --kind image|voice|video|document
                       --attachment OPAQUE_CANDIDATE_ID --output PATH
                       <access-mode>
# compatibility-only image form; reads no database
attachment inspect <account-root> --conversation ID --md5 HEX
attachment materialize <account-root> --conversation ID --md5 HEX
                       --attachment OPAQUE_CANDIDATE_ID --output PATH
connector-policy-direct <source> <new-policy> <conversation-ID>...
connector-query-direct <source> <policy> <audit> <private-request-JSON>
connector-serve-direct <source> <policy> <audit> <private-socket>
snapshot create <live-database-root> <new-snapshot-directory>
snapshot create-capture <stable-acquisition-snapshot> <new-snapshot-directory>
snapshot verify <snapshot-directory>
snapshot rewrap <snapshot-directory> <new-snapshot-directory>
snapshot rekey <legacy-snapshot-directory> <new-snapshot-directory>
export jsonl <source> <new-output-directory>
```

Initial implementation may expose a documented subset, but command semantics
and response contracts must converge on this resource model. Every database
query command accepts exactly one database access mode:

```text
--passphrase-stdin                 encrypted live WeChat source; key on stdin
--snapshot-local-credential FILE  ordinary local snapshot reopening
--snapshot-recovery-kit FILE      portable 24-word snapshot recovery
--snapshot-passphrase-stdin       optional Argon2id snapshot passphrase
--snapshot-key-stdin              legacy raw-key snapshot compatibility
--decrypted                        explicitly allow a plaintext SQLite source
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
| Unique contact IDs in one enrichment read | 500 | 500 |
| One lazy image source | 128 MiB | 128 MiB |
| One lazy voice source | 32 MiB | 32 MiB |
| Cumulative voice candidates inspected | 128 MiB | 128 MiB |
| One decoded audio output | 128 MiB | 128 MiB |
| One lazy video source | 2 GiB | 2 GiB |
| One lazy document source | 512 MiB | 512 MiB |
| Attachment candidates | 256 | 256 |

Attachment inspection additionally caps one conversation scan at 4,096 child
directories and 100,000 filesystem entries. Voice lookup reads at most 256
exact-server-ID rows across a bounded media-database inventory. Those are fixed
safety bounds, not caller-adjustable page sizes.

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

Conversation items may include `displayName`; message and search items may
include `senderDisplayName`. These are additive presentation fields. Stable raw
`id` and `sender` values remain present and are the fallback whenever optional
enrichment is missing. The adapter resolves at most 500 unique IDs with one
read-only `IN (...)` statement against `contact/contact.db`, accepting
`username`/`user_name`, `remark`/`remark_name`, and
`nick_name`/`nickname` variants. Precedence is remark, nickname, alias, then the
raw identifier at presentation time. It decodes SQLite text or BLOB values and
truncates on a UTF-8 boundary.

Missing rows or an absent/incompatible contact schema never fail the primary
message read. They emit `contactDisplayNameUnresolved` or
`contactEnrichmentUnavailable`, retain raw identifiers, and mark enrichment
coverage incomplete.

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
| Conversation page | one statement in `session.db`, then one bounded statement in `contact.db` when items exist |
| Message page in one shard | one statement in that shard, then one bounded statement in `contact.db` when senders exist |
| Message page across shards | one statement per shard, deterministic merge, then bounded contact enrichment |
| Native live search | one native FTS statement, then bounded contact enrichment |
| Decoded fallback search | operation-specific bounded shard/session/contact statements, reported explicitly |
| Snapshot query | stable snapshot generation; statement-level within it |

The response always reports `databaseCount` and `crossDatabaseAtomic`.
Enriched results normally read both the primary database and `contact.db`, so
they deliberately report `crossDatabaseAtomic: false`; the friendly name and
the message/session row can race on a live source without corrupting either raw
identity. A caller that needs stable multi-page or cross-database results should
create a snapshot and run the same query against that generation.

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
2. scan a fixed decoded source window without writes and return an opaque
   continuation, including when the window contains no match;
3. optionally maintain a compact local FTS cache containing stable source
   references and normalized searchable text only when measured latency makes
   it worthwhile.

The implemented no-write fallback examines at most 500 source messages and 16
conversations per response. It orders conversations by identifier and messages
newest-first within each conversation. It never claims that an empty window is
the end of the search when a continuation remains, and its result identity is a
normal source-message identity that can be hydrated exactly. Responses mark
coverage incomplete and report the decoded source-row count. This path trades
latency for zero corpus-sized write amplification.

If later field measurements justify a fallback cache, it must not store the
complete canonical message JSON. It must be encrypted, incremental, rebuildable,
versioned, and keyed by source database identity plus row identity. Search hits
would still be hydrated from the source adapter in a second bounded step, with
cache freshness and missed-shard warnings in every response.

### 9.1 Measured fallback latency and cache decision

The reproducible release-mode benchmark is an ignored integration test so it
does not make routine test runs timing-sensitive:

```sh
cargo test --release --test live_query_cli \
  fallback_search_latency_evidence_for_the_fixed_500_message_window -- \
  --ignored --nocapture --test-threads=1
```

On 2026-08-29, an Apple M2 Max running macOS 26.6.2 measured 20 end-to-end CLI
samples after three warmups for each synthetic case. Every case forced native
FTS absence, searched for a miss so the whole bounded window was decoded, and
compared the complete source file inventory before and after the run.

| Source and window | Payload | Initial p95 | Optimized p95 | Final verification p95 |
|---|---:|---:|---:|---:|
| Plaintext, one conversation, 500 messages | 256 B | 8.486 ms | 6.261 ms | 8.023 ms |
| SQLCipher, one conversation, 500 messages | 256 B | 345.292 ms | 245.626 ms | 240.373 ms |
| SQLCipher, one conversation, 500 messages | 8 KiB | 356.039 ms | 247.861 ms | 245.720 ms |
| SQLCipher, 16 conversations, 500 messages | 1 KiB | 4,387.747 ms | 351.648 ms | 352.490 ms |

The initial multi-conversation cost came from reopening the same SQLCipher
shards and repeating their key setup for every conversation, not from text
matching. The optimized fallback opens each shard once per request, reuses those
read-only connections across the bounded conversation window, and performs one
contact-name enrichment only for returned hits. The benchmark observed no
persistent writes before or after either implementation.

The final acceptance rerun used the same 20-sample, three-warmup protocol on
2026-08-29 and again observed no persistent writes. The worst verified p95 is
about 352 ms, so these measurements do not justify a persistent encrypted text
cache. GreenBubbles therefore retains native FTS first and the zero-write
decoded fallback second. The benchmark remains available for future schema,
hardware, and real-corpus evidence; a cache remains an optional future response
only if bounded no-write latency materially regresses.

## 10. Attachments and media

Message pages return lightweight artifact references and availability metadata.
They do not decode every attachment.

`attachment inspect` may read headers and metadata without creating a durable
derivative. `attachment materialize` decrypts or converts exactly one selected
artifact into a new owner-only output path, verifies its digest, and reports the
result. Implementations must avoid an individual `fsync` for every file in a
bulk operation; an explicit export can batch durable writes and sync directory
boundaries.

The implemented version-1 lazy path accepts an exact source-bound message
identity plus the conversation and requested artifact kind. It hydrates only
that source row, rejects a kind mismatch, derives locators from decoded content
rather than process arguments, and binds every candidate identity to the source
message and current row/file evidence. A stale identity cannot be reused for a
different source, conversation, message, kind, media row, or changed file.

Image lookup uses the decoded 32-hex MD5 and the fixed bounded conversation
scan. It supports legacy XOR plus the pinned V1/V2 WeChat image decoders. Voice
lookup performs bounded exact-server-ID queries against read-only `VoiceInfo`
tables in the media shards. Materialization attempts SILK-to-Ogg-Opus conversion
and safely retains the raw SILK payload when conversion is unavailable or the
payload is not decodable. Video and document lookup first consults bounded
read-only `hardlink.db` metadata. Its fixed-depth conversation-scoped filesystem
fallback uses only the decoded MD5 and, for documents, a title basename; the
document title never appears in command arguments. Video and document bytes are
streamed into the one output instead of being buffered as a whole.

Every filesystem candidate must be a bounded, current-user-owned regular file
beneath real non-symlink account directories and is opened with no-follow
semantics. Materialization re-runs inventory, requires the same opaque identity,
detects source version changes, and atomically creates a single mode-`0600`
output in an existing owner-only directory outside the protected source. It
refuses overwrite and leaves no partial output after a failed read. Neither
inspection nor error/success JSON returns a source or output path.

The original `--conversation` plus `--md5` image syntax remains compatible and
reads no database. Message-bound lookup requires exactly one normal database
access mode. A live/decrypted account root can resolve filesystem artifacts and
its `db_storage`; a database-only recoverable snapshot can resolve database-
resident voice payloads but does not claim to contain external video/document/
image files unless a future snapshot format explicitly inventories them.

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

- a mandatory portable 24-word BIP-39 recovery mnemonic generated from 256
  random bits and protected by the mnemonic checksum;
- optionally an application passphrase processed by Argon2id;
- optionally a random local-unlock credential held in macOS Keychain for
  convenience;
- optionally that same kind of local credential in an owner-only hidden file as
  a portable-platform fallback.

The manifest stores only KDF parameters, protector identifiers, salts, wrapped
DEKs, and authenticated metadata. It never stores a plaintext key or passphrase.
At least one portable recovery protector must be offered; a device-only Keychain
entry is not sufficient for a backup.

The BIP-39 words are an encoding of computer-generated entropy, not a sentence
chosen by the owner. GreenBubbles must display or write the words before the
long database conversion begins, validate their checksum when read, and require
a word-position confirmation in graphical creation flows. The recovery kit is
shown once by the GUI or created exclusively as a mode-`0600`, single-link file
inside an owner-only directory by the CLI. Standard output receives only a
content-free creation report, never the words or a base64 key.

The mnemonic derives a key-encryption key with HKDF-SHA-256 and a per-protector
random salt. The DEK is wrapped with XChaCha20-Poly1305. Authenticated data binds
the wrapper version, snapshot identity, protector identity, protector kind,
and KDF/cipher parameters so protector records cannot be silently transplanted.
All encoded salt, nonce, and ciphertext lengths and all future KDF work factors
are strictly bounded before any expensive operation.

The local auto-unlock protector wraps the same DEK under a distinct random key.
On macOS the History app stores that credential as an application-scoped generic
password item with `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`, then
materializes it only into an owner-only no-follow temporary file for the CLI
while the source is open. The implemented hidden-file fallback remains outside
the snapshot generation, current-user-owned, single-link, mode `0600`, beneath
an owner-only directory, and opened without following links. It contains
neither the DEK nor the recovery words. Such a file is a convenience credential,
not the portable backup: deleting it does not affect the 24-word recovery path,
while copying it with the snapshot must not be the only recovery plan.

Protector changes create a new immutable manifest generation and normally
rewrap the same DEK; they do not rewrite every SQLCipher database. Removing the
last portable protector is forbidden. Raw-key format-1 snapshots remain
read-compatible and can be migrated into the wrapped format, but new graphical
snapshots use the wrapped hierarchy.

The protector container has its own version inside snapshot format 2 and still
requires an external cryptographic review before public release. Standard,
maintained primitives are required; GreenBubbles does not invent encryption
algorithms.

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
place. The implemented retention gate moves only a whole, explicitly selected
generation into an owner-only same-filesystem quarantine after a newer linked
generation passes portable recovery verification. It re-verifies after the
atomic move and rolls back on failure. GreenBubbles performs no automatic purge;
permanent deletion remains a separate explicit owner decision after a cooling
period and another recovery drill.

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

The implemented direct connector uses a distinct source-identifier policy.
Live conversation IDs cannot be substituted for the replica's account-scoped
one-way hashes, so archive/replica policies are rejected rather than silently
reinterpreted. `connector-policy-direct` authenticates the selected source,
verifies every named conversation, and binds the policy to the source identity.
Ordinary connector reads then use the same short-statement adapter and return
the existing minimized result contract. Direct conversation labels and
authorized sender projections use the bounded contact display-name resolver. A
group label comes from the group contact, never from its last sender. Missing
names retain raw IDs and set `RawOnly` plus an explicit limitation. Requested
direction, relationship, or attachment fields that cannot yet be derived
without restoration are omitted with explicit limitation codes.

`listConversations` now takes optional `cursor` and `limit` fields. Its opaque
cursor binds the source, exact policy digest, destination, and last conversation
key. A changed policy or local/remote destination therefore cannot reuse an old
page token. One-conversation message time windows are pushed into SQLite or
native FTS predicates and are also applied inside fallback source queries.
Cross-conversation search queries only explicitly authorized conversations, in
deterministic conversation-ID order and backend-native order within each
conversation. A composite cursor resumes the exact conversation/backend
position, and each connector response examines at most 32 authorized
conversations; unauthorized conversations are never scanned or counted.

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

- Exact single-message retrieval and bounded optional contact/name enrichment
  are implemented across list, search, fallback, and exact hydration.
- Native FTS probing and bounded search plus the no-write decoded-source
  fallback are implemented. Retain the compact cache as an optional measured
  acceleration, not a prerequisite for functional search.
- Lazy exact-message image, voice, video, and document inspection and
  one-candidate materialization are implemented under the same bounded
  identity and publication contract.
- Route the connector's read operations through the same adapter and policies.
  Implemented for ordinary conversation/message list, search, and exact-get;
  replica-only change feeds, cached surfaces, enrichment, artifacts, and drafts
  remain explicitly unavailable on the direct backend.

### Phase 3: independent snapshots

- Stable filesystem capture plus logical SQLite backup into independently keyed
  SQLCipher destinations is implemented as the Swift snapshotter followed by
  `snapshot create-capture`; direct per-database online backup also remains.
- Portable recovery-word, optional Argon2id passphrase, macOS Keychain-backed
  local convenience, and owner-only hidden-file fallback protectors are
  implemented. The graphical creation flow displays the 24 words once and
  requires four random word-position confirmations before conversion.
- Verify, recovery testing, raw-key rekey, and atomic publication are
  implemented, as are immutable protector rewrap and verified retention
  quarantine/restore.
- Run the exact same query adapter against snapshot roots.

### Phase 4: optional index and incremental behavior

- The measured fallback does not currently justify a compact encrypted
  reference/text FTS cache; retain the reproducible benchmark and reconsider
  only if field latency materially regresses.
- Track source identities and high-water keys per shard.
- Update proportionally to changed shards and invalidate on incompatible schema
  changes.

### Phase 5: retire mandatory restoration

- Live or snapshot query is now the default History app path; the AI
  connector's ordinary reads now use the same adapter through one-shot or
  socket entry points.
- The History app now labels the JSONL bundle as an explicit exported-history
  workflow; carry that distinction through remaining connector interfaces.
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

### 15.1 Final acceptance evidence

All ten criteria passed on 2026-08-29. Test names below are exact so the evidence
can be rerun without interpreting a prose claim.

| Criterion | Result | Primary implementation and test evidence |
|---:|:---:|---|
| 1 | Pass | `encrypted_cli_reads_directly_and_wrong_key_fails_without_disclosure` and `decrypted_cli_returns_versioned_cursor_pages_without_creating_an_archive` exercise bounded conversation and message pages directly against source SQLite without an archive or replica. |
| 2 | Pass | `LiveQuerySource::open_database` uses the read-only adapter, enables `PRAGMA query_only = ON`, verifies it, and installs a statement deadline; `source_connections_reject_writes` proves mutation fails. |
| 3 | Pass | `conversation_cursor_pages_duplicate_timestamps_once` and `message_cursor_is_total_across_shards_and_duplicate_server_ids` prove total keyset traversal without skips or duplicates. |
| 4 | Pass | `unbounded_and_ambiguous_access_options_fail_closed`, `projection_truncates_large_content_on_a_utf8_boundary`, `unsafe_roots_schema_drift_and_damaged_shards_are_explicit`, cursor-binding tests, and the encrypted wrong-key test cover limits, malformed authority, unsafe roots, schema drift, damaged shards, and nondisclosing key failure. |
| 5 | Pass | `fallback_search_latency_evidence_for_the_fixed_500_message_window` produced the p95 table in section 9.1 and reported `persistentWritesObserved: false` for every case; `missing_native_fts_uses_bounded_source_fallback_without_writes` independently compares the source inventory. |
| 6 | Pass | `bip39_recovery_kit_wraps_a_distinct_database_key_and_survives_source_loss` deletes the WeChat source, withholds its key, verifies the snapshot using only the 24-word kit, and queries its messages. |
| 7 | Pass | `stable_filesystem_capture_converts_without_plaintext_staging_or_live_source` inspects every published database and sidecar; `copy_database_logically` keys each destination before SQLite's backup API writes any source pages. |
| 8 | Pass | `wrapped_snapshot_rejects_tampered_envelopes_and_nonportable_manifests`, `tampered_snapshot_database_fails_manifest_verification`, `protector_rewrap_keeps_encrypted_database_bytes_and_source_generation_unchanged`, `snapshot_rekey_atomically_publishes_a_separately_recoverable_generation`, and `retention_quarantines_only_after_portable_replacement_proof_and_can_restore` cover authenticated protectors, database hashes and integrity, immutable generations, failed unpublished output, rekey, quarantine rollback gates, and restore. Owner-only permission assertions accompany recovery-kit, credential, and snapshot creation. |
| 9 | Pass | Native and decoded-fallback search tests prove fixed windows and cursor binding; all seven `live_attachment_cli` tests prove exact-message binding, bounded traversal, inspection without writes, one-artifact materialization, and no source/output path disclosure. |
| 10 | Pass | `policy_scoped_connector_reads_sqlite_directly_with_paging_search_and_audit` proves conversation, field, time, destination, result-size, and audit authorization on direct SQLite. `HistoryDirectQueryClientTests` and the History app direct views/model prove ordinary browsing, search, exact hydration, and snapshot access do not require JSON restoration. |

The final validation run completed:

- `cargo fmt --all` and `cargo check --all-targets`;
- `cargo test --all-targets`: 188 passed, zero failed, with the timing-sensitive
  benchmark intentionally excluded from the routine run;
- the release-mode benchmark command in section 9.1: one passed, zero failed;
- `swift test`: 115 passed in 24 suites, zero failed;
- `git diff --check`.

Criterion 10 applies to ordinary browsing and retrieval. The legacy
encrypted-replica connector remains intentionally available for change feeds,
cached Moments, restored enrichment, verified artifact paths, and draft
workflows; its continued availability does not make it a prerequisite for
ordinary reads.

## 16. Consequences

The main benefit is proportional work: asking for 100 messages reads and decodes
a bounded neighborhood instead of transforming 1.8 million rows. Disk usage for
ordinary access becomes effectively zero beyond small logs or an optional index.

The tradeoff is that live pages do not represent one global instant across all
WeChat databases, and schema adapters must evolve with WeChat releases. The
response makes that consistency limit explicit, while recoverable snapshots
provide the stable alternative. This is a better separation of concerns than
paying the cost of a forensic export for every interactive read.
