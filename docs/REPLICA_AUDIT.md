# Independent encrypted-replica audit

`replica-status` is a bounded operational view. `audit-replica` is the slower,
key-gated integrity pass for a serving replica:

```sh
greenbubbles-restore audit-replica \
  <encrypted-replica.db> --replica-key-stdin
```

The random 32-byte replica key is distinct from the WeChat database
passphrase. Supply it locally through standard input; never put it in an
argument, report, issue, commit, chat, or model prompt.

The audit opens the existing owner-only SQLCipher database read-only and takes
one deferred read transaction. It does not create or migrate a replica,
advance a checkpoint, repair an index, or open a restoration archive or live
WeChat store. The database, WAL, and SHM entries must be owner-only regular
files without symlinks or hard links.

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
- the single account identity, source checkpoint counts/timestamp, latest sync
  run, authoritative report, restoration coverage, completion state, schema
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
