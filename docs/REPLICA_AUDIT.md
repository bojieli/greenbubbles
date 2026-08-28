# Independent encrypted-replica audit

`replica-status` is a bounded operational view. `audit-replica` is the slower,
key-gated integrity pass for a serving replica:

```sh
greenbubbles-restore audit-replica \
  <encrypted-replica.db> --replica-key-stdin \
  --progress-file <owner-only-new-progress.ndjson>
```

The random 32-byte replica key is distinct from the WeChat database
passphrase. Supply it locally through standard input; never put it in an
argument, report, issue, commit, chat, or model prompt.

The audit opens the existing owner-only SQLCipher database read-only and takes
one deferred read transaction. It does not create or migrate a replica,
advance a checkpoint, repair an index, or open a restoration archive or live
WeChat store. The database, WAL, and SHM entries must be owner-only regular
files without symlinks or hard links.

Human-readable progress is emitted to standard error by default. It reports
eight monotonic stages and an overall percentage: opening/planning, SQLite and
foreign-key integrity, canonical records, canonical links, checkpoint and
coverage, FTS, change/synchronization history, and finalization. Events include
the encrypted replica namespace size, canonical/link/change totals, exact row
counts for the row-addressable stages, elapsed time, and periodic heartbeats
while SQLite integrity and FTS queries are running. Those SQLite operations do
not expose a trustworthy internal row cursor, so their heartbeat remains at
the stage's starting percentage until the operation completes rather than
inventing progress. `--progress-json` emits the privacy-safe events as NDJSON;
`--quiet-progress` suppresses console output. `--progress-file` creates a new
mode-`0600` file in an owner-only directory and flushes every event. It cannot
overlap the database, WAL, SHM, or journal path.

Within the consistent transaction it verifies:

- SQLCipher access, SQLite `integrity_check`, foreign keys, current schema,
  replica identity, and the complete compiled migration identity ledger;
- the SHA-256 and stable canonical JSON encoding of every conversation,
  participant, message, artifact, cached Moment, and cached interaction;
- every indexed serving column against its canonical record, including message
  search text and account/conversation/type/direction projections;
- exact conversation memberships, message relationships, and message-artifact
  ordinals/fields against canonical messages;
- one exact FTS row for every message and no missing, extra, duplicate, stale,
  cross-account, or differently rendered FTS row;
- the single account identity, opaque account-holder participant and binding
  provenance, source checkpoint counts/timestamp, latest sync run,
  authoritative report, restoration coverage, completion state, schema
  profiles, and optional cached-surface coverage as one committed revision;
- a contiguous append-only change sequence with bounded known kinds, valid
  digests/timestamps, valid synchronization history, and empty reconciliation
  staging.

An uninitialized current-schema replica is valid only when every serving,
checkpoint, coverage, sync, change, FTS, link, and cached table is empty.

Success returns format-1 aggregate counts and boolean verdicts only. It omits
the replica ID, account ID, source fingerprint, paths, content, search text,
absolute timestamps, and key. Any mismatch fails rather than returning a
partially green report. Use the current authoritative archive and follower
state to diagnose/rebuild a damaged serving replica; the audit itself never
repairs or blesses unexplained state.

This audit proves internal encrypted-replica consistency. The authoritative
`audit-archive` remains responsible for source restoration ledgers and exact
recorded media files, and real-corpus completeness/latency gates remain
separate.

Older retained recovery points use the schema-aware
`audit-replica-backup`. A passing backup can be copied and migrated into a
separate deep-audited candidate with `prepare-replica-recovery`; neither command
performs active cutover. See `REPLICA_BACKUP_AUDIT.md` and
`REPLICA_RECOVERY.md`.
