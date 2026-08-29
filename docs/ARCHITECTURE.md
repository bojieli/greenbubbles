# Architecture

GreenBubbles serves WeChat history through bounded, typed, read-only queries
against the original SQLite/WCDB databases. JSON is a response format here, not
a storage format.

This document explains why, and what that costs.

## The measurement that decided it

The project originally restored the whole corpus first and served queries from
the result. For every source row that meant preserving raw and typed
projections, base64-encoding binary values, serializing JSON, compressing and
staging it, resolving relationships, serializing again, importing into a second
database, updating indexes and populating FTS tables. Media restoration hashed,
decrypted, wrote, verified and synced derivatives on top.

Against one real corpus, that trade looked like this:

| Item | Observed |
| --- | ---: |
| Source databases (26 groups) | 2.98 GB |
| Messages | 1,855,548 |
| Message tables | 6,292 |
| Text-only canonical archive | ~13.50 GB |
| `messages.ndjson` alone | ~12.71 GB |
| Staging SQLite peak | ~7.42 GB |
| Eager media derivatives, one run | ~30 GB |
| Replica bootstrap WAL, one run | ~18 GB |

The source is compact because SQLite stores typed integers and BLOBs without
field names or base64 expansion. The restored form duplicates information for
provenance, audit, indexing and presentation — genuinely useful properties for
a forensic export, and an absurd price to pay before returning one page of one
conversation.

So the default path became:

```text
Live encrypted WeChat SQLite/WCDB
        ↓  read-only schema adapter and decoder
Bounded keyset pagination
        ↓
Small, versioned JSON response
```

The same query contract works unchanged against a snapshot generation.
Canonical JSONL restoration still exists as an explicit forensic and
interchange export — it is simply no longer the serving path, and is never
created as a side effect of an ordinary query.

## Goals

1. Return the first useful page without restoring the corpus.
2. Keep normal responses sized for a person, a script, or a model.
3. Make every source access demonstrably read-only.
4. Never hold a transaction that could block WeChat's WAL checkpointing.
5. Use keyset cursors, so later pages neither slow down nor shift.
6. Preserve typed decoding and tolerate individually unreadable shards.
7. Resolve attachments only when explicitly asked.
8. Support repeatable queries through immutable snapshots.
9. Make snapshots recoverable without WeChat or its key.
10. Keep an auditable export for interoperability without making it canonical.

## Non-goals

The AI-facing CLI will not accept arbitrary SQL, expose an unrestricted `--all`
operation, hold a source transaction open while a caller thinks, promise one
atomic instant across all WeChat databases during a live query, eagerly copy or
decode media, treat a copied encrypted WeChat file as a sufficient backup, or
put decrypted databases and keys in ordinary temporary directories.

## The resource surface

```text
source status <database-root>
conversations list <root> [--limit N] [--cursor TOKEN]
messages list <root> --conversation ID [--limit N] [--cursor TOKEN]
messages search <root> --query-stdin [--conversation ID] [--limit N] [--cursor TOKEN]
message get <root> --conversation ID --message ID
attachment inspect|materialize <account-or-root> --conversation ID
    --message OPAQUE_ID --kind image|voice|video|document …
connector-policy-direct  <source> <new-policy> <conversation-ID>…
connector-query-direct   <source> <policy> <audit> <request.json>
connector-serve-direct   <source> <policy> <audit> <private-socket>
snapshot create|create-capture|verify|rewrap|rekey …
restore <stable-snapshot> <new-owner-only-archive>
```

Every database command takes exactly one access mode: `--passphrase-stdin`,
`--snapshot-local-credential`, `--snapshot-recovery-kit`,
`--snapshot-passphrase-stdin`, `--snapshot-key-stdin` (legacy), or `--decrypted`
for an explicitly plaintext source.

Secrets never appear in arguments, responses, logs, errors, manifests or
cursors. Query text arrives on standard input so it stays out of shell history
and `ps`.

### Bounds

| Limit | Default | Hard maximum |
| --- | ---: | ---: |
| Conversations per response | 100 | 500 |
| Messages per response | 100 | 500 |
| Search hits per response | 50 | 200 |
| One projected text field | 16 KiB | 16 KiB |
| Serialized JSON response | 8 MiB | 8 MiB |
| Query text | 16 KiB | 16 KiB |
| Unique contact IDs per enrichment read | 500 | 500 |
| One lazy image source | 128 MiB | 128 MiB |
| One lazy voice source | 32 MiB | 32 MiB |
| Cumulative voice candidates inspected | 128 MiB | 128 MiB |
| One decoded audio output | 128 MiB | 128 MiB |
| One lazy video source | 2 GiB | 2 GiB |
| One lazy document source | 512 MiB | 512 MiB |
| Attachment candidates | 256 | 256 |

Attachment inspection additionally caps one conversation scan at 4,096 child
directories and 100,000 filesystem entries; voice lookup reads at most 256
exact-server-ID rows.

These are safety bounds, not page sizes — callers cannot raise the hard
maximums. Processing an entire corpus is what the explicit export is for.

## The response envelope

Every bounded query returns an envelope, never a bare array:

```json
{
  "schema": "greenbubbles.query.v1",
  "formatVersion": 1,
  "operation": "messages.list",
  "ok": true,
  "source": { "mode": "liveEncrypted", "identity": "sha256:opaque-prefix" },
  "consistency": {
    "guarantee": "perDatabaseReadStatement",
    "databaseCount": 4,
    "crossDatabaseAtomic": false,
    "coverageComplete": true,
    "observedAtUnixMilliseconds": 1787880000000
  },
  "page": { "limit": 100, "returned": 100, "hasMore": true, "nextCursor": "…" },
  "warnings": [],
  "items": []
}
```

Errors reuse the same schema and operation fields with `ok: false`, a stable
error code and a safe description. Content, source paths, SQL and key material
never appear in error details.

Compatibility rules: additive optional fields do not bump `formatVersion`;
removing or redefining a field requires a new version; cursors carry their own
version and are rejected by incompatible operations; unknown input options fail
closed rather than being ignored; source schema incompatibility is reported
explicitly.

## The adapter

The pinned `wx-db` dependency opens SQLCipher databases with
`SQLITE_OPEN_READ_ONLY`, applies `sqlite3_key()`, validates the key and sets
`PRAGMA query_only = ON`. GreenBubbles reuses that and its typed decoders.

Four layers:

1. **Source validation** — canonicalize the account root, require a
   current-user-owned directory, reject unsafe file types, derive an opaque
   source identity that is not the path.
2. **Schema routing** — resolve `contact/contact.db`, `session/session.db` and
   the numbered `message/message_N.db` shards. Table identifiers are computed
   from the conversation ID; a caller never supplies raw SQL.
3. **Bounded query** — push a compound keyset predicate and `LIMIT + 1` into
   each relevant statement, and merge only those bounded windows in memory.
4. **Projection** — decode typed values, drop implementation-only raw columns,
   truncate oversized fields on a UTF-8 boundary, serialize a bounded envelope.

Connections are command-scoped. Each statement completes before serialization
and before control returns to a caller. A future daemon may pool connections
but must reopen them after source changes and keep the short-statement rule.

### Consistency, stated honestly

SQLite gives one stable view per read statement. WeChat splits data across
databases, so a page touching several shards is not one global instant:

| Query | Guarantee |
| --- | --- |
| Conversation page | one statement in `session.db`, then one bounded `contact.db` statement if items exist |
| Message page, one shard | one statement in that shard, then bounded contact enrichment |
| Message page across shards | one statement per shard, deterministic merge, then enrichment |
| Native live search | one native FTS statement, then enrichment |
| Decoded fallback search | operation-specific bounded statements, reported explicitly |
| Snapshot query | a stable generation; statement-level within it |

Responses always report `databaseCount` and `crossDatabaseAtomic`. Enriched
results read both the primary database and `contact.db`, so they report
`crossDatabaseAtomic: false` deliberately: a friendly name and a message row
can race on a live source without either raw identity being wrong. Callers
needing stable cross-page results should query a snapshot generation.

### WAL behaviour

A read-only connection still participates in WAL visibility, and a long read
transaction can pin an old frame and stall checkpointing. Hence: no transaction
around caller work; select one bounded page and finalize immediately; close
command-scoped connections before serializing where practical; impose a busy
timeout and a wall-clock deadline; and never copy a `.db` file alone while it
has uncheckpointed WAL state.

Where policy requires zero interaction with live WAL/SHM state, the adapter can
query an APFS clone of just the needed database and sidecars. That is query
isolation, not restoration.

## Pagination

Cursors are URL-safe base64 encodings of a small versioned structure, opaque to
callers, binding the operation kind, opaque source identity, filters and
conversation identity, the last returned compound ordering key, and the cursor
format version.

```text
conversations:  (sort_timestamp DESC, username ASC)
messages:       (sort_seq DESC, create_time DESC, server_id DESC,
                 shard_id DESC, rowid DESC)
```

Including shard and row identity is what stops unsynchronized messages with a
zero or repeated server ID from being skipped. A page queries keys strictly
older than the cursor, fetches at most `limit + 1` per relevant shard, merges by
the same compound key, returns `limit`, and uses the spare row only to compute
`hasMore`.

**A cursor is not authorization.** The CLI validates every binding and applies
access policy independently. A future long-running service should authenticate
cursors with an installation-local MAC.

## Search

Search is optional infrastructure, not a reason to duplicate the corpus. In
preference order:

1. use compatible native WeChat FTS databases, read-only;
2. scan a fixed decoded source window with no writes, returning an opaque
   continuation — including when the window contained no match;
3. only if measurements ever justify it, maintain a compact local FTS cache.

The implemented fallback examines at most 500 messages and 16 conversations per
response, ordering conversations by identifier and messages newest-first within
each. It never claims an empty window is the end of the search while a
continuation remains, and its results are ordinary source identities that
hydrate exactly. Responses mark coverage incomplete and report the decoded
row count.

**Option 3 was measured and rejected.** At a worst verified p95 of ~352 ms, a
persistent encrypted text cache would be a second copy of your messages on disk
to save a third of a second. Full numbers, including the 4.4-second version
that existed before shard connections were reused, are in
[MEASUREMENTS.md](MEASUREMENTS.md). If bounded no-write latency ever regresses
materially, the cache becomes an option again — and it would still have to be
encrypted, incremental, rebuildable, versioned, keyed by source and row
identity, hold no canonical message JSON, and hydrate hits from the source in a
second bounded step.

## Attachments

Message pages return lightweight artifact references and availability metadata.
They decode nothing.

`attachment inspect` reads headers and metadata without creating a durable
derivative. `attachment materialize` decrypts or converts exactly one selected
artifact into a new owner-only path, verifies its digest, and reports the
result.

The lazy path takes an exact source-bound message identity plus conversation
and kind, hydrates only that row, rejects a kind mismatch, derives locators
from decoded content rather than process arguments, and binds every candidate
identity to the source message and current row and file evidence. A stale
identity cannot be reused for a different source, conversation, message, kind,
media row or changed file.

By media kind: images use the decoded 32-hex MD5 and a bounded conversation
scan, supporting legacy XOR plus the pinned V1/V2 decoders. Voice performs
bounded exact-server-ID queries against read-only `VoiceInfo` tables, attempts
SILK-to-Ogg-Opus conversion, and safely retains the raw SILK payload when
conversion is unavailable. Video and documents consult bounded read-only
`hardlink.db` metadata first, then a fixed-depth conversation-scoped filesystem
fallback using the decoded MD5 and, for documents, a title basename — the title
never appears in a command argument. Video and document bytes stream into the
output rather than being buffered whole.

Every filesystem candidate must be a bounded, current-user-owned regular file
beneath real non-symlink account directories, opened with no-follow semantics.
Materialization re-runs inventory, requires the same opaque identity, detects
source version changes, and atomically creates one mode-`0600` output in an
existing owner-only directory outside the source. It refuses to overwrite and
leaves no partial output after a failed read. Neither inspection nor any
success or error JSON returns a source or output path.

The original `--conversation` plus `--md5` image form remains compatible and
reads no database. A database-only snapshot can resolve database-resident voice
payloads but does not claim to contain external image, video or document files.

## Snapshots

A durable snapshot must not depend on WeChat's key: losing access to the
running application must not make an intact backup unreadable. Snapshot
creation reads decrypted logical pages through SQLite's backup API and writes
them into new databases under a random GreenBubbles key, so no plaintext SQLite
staging file is ever required.

The key hierarchy, the 24-word recovery material, protector rotation, retention
and the recovery proof are in
[RECOVERABLE_SNAPSHOTS.md](RECOVERABLE_SNAPSHOTS.md).

## Failure behaviour

The adapter distinguishes invalid arguments and cursor bindings, unsafe source
paths or permissions, incorrect keys, unsupported source schema or build,
busy/timeout conditions, individual shard failures, decode omissions, and
response-bound violations.

An unreadable *required* database fails its operation. An unreadable message
shard may yield a partial page **only** when the response names the skipped
shard by opaque ID and marks coverage incomplete. Silent omission is forbidden.

## Knowing when something changed

Notification delivery can cut latency, but it can never be the source of truth.

Apple's UserNotifications API lets an application manage its own notifications;
it provides no supported subscription to another application's notification
bodies. Accessibility automation can observe some Notification Center UI state
after the user grants permission, but Focus modes, grouping, dismissal,
previews and OS updates make that incomplete. Reading Notification Center's
private database would add an unsupported, TCC-sensitive dependency and is not
part of the passive adapter.

The hierarchy is therefore:

1. filesystem events on the database directory can wake the reconciler;
2. a future, explicitly user-enabled Accessibility observer may add another
   wake-up hint;
3. consistent database snapshots and canonical-ID reconciliation determine
   actual state;
4. periodic reconciliation recovers missed or duplicated hints.

`greenbubbles-discover notification-hints` reports only whether the current
process already has Accessibility trust. It does not prompt, inspect
Notification Center, or read any notification content, and its completeness
result is always false — a hint is an optimization, and the synthetic benchmark
includes a missed-hint case precisely to prove the system works without one.

## Security posture

Read-only SQL is necessary and not sufficient. The boundary also requires typed
operations with allowlisted filters; current-user ownership checks on source
and secret files; `O_NOFOLLOW`/`O_CLOEXEC` on direct descriptor opens; no raw
source paths in normal JSON; per-operation hard limits, deadlines and
response-size enforcement; conversation and field authorization before content
is returned; audit events carrying identities and counts but no bodies;
zeroization of keys after use; no network access in the local query path; and
an explicit destination policy before content reaches a remote model.

Direct CLI use inherits the invoking owner's filesystem authority. The
connector layer enforces requester, conversation, time, field and destination
policy for AI consumers — see [AI_TOOL_BOUNDARY.md](AI_TOOL_BOUNDARY.md).

The direct connector uses a distinct source-identifier policy. Live
conversation IDs cannot substitute for the replica's account-scoped one-way
hashes, so an archive or replica policy is *rejected* rather than silently
reinterpreted. `connector-policy-direct` authenticates the source, verifies
every named conversation and binds the policy to the source identity.
`listConversations` cursors bind the source, exact policy digest, destination
and last conversation key, so a changed policy or destination cannot reuse an
old page token. Cross-conversation search visits only explicitly authorized
conversations, in deterministic order, examining at most 32 per response;
unauthorized conversations are never scanned or counted.
