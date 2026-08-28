# Pre-migration replica backup audit

Every upgrade from an older non-empty replica schema first creates a same-key,
SQLCipher-encrypted recovery database beside the serving replica. Creation is
not considered successful merely because SQLite's online backup call returned:
GreenBubbles converts the backup to rollback-journal mode, closes it, reopens it
read-only, and completes the same schema-aware content verification available
to an operator. Migration starts only after that verification succeeds. A
failed candidate and its sidecars are removed while the older source replica is
left unchanged.

An operator can independently verify a retained backup before relying on it:

```sh
greenbubbles-restore audit-replica-backup \
  <encrypted-pre-migration-backup.db> --replica-key-stdin
```

The 32-byte replica key is distinct from the WeChat database passphrase. Pass
it locally through standard input, preferably from an owner-controlled secret
manager. Never place it in an argument, report, issue, commit, chat, or model
prompt.

## Verification contract

The command accepts only a supported, non-empty older schema (currently 1–4).
It rejects an uninitialized schema-0 file and the current or a future schema so
that an ordinary serving replica cannot be mislabeled as a migration backup.
The database and any existing WAL/SHM entries must be single-link, owner-only
regular files inside an owner-only, non-symlink directory.

In one deferred read transaction, the audit verifies every invariant available
in that historical schema:

- SQLCipher access, SQLite integrity, foreign keys, replica format, and the
  exact contiguous compiled migration-identity ledger;
- the stable JSON encoding, digest, and indexed projections of every canonical
  conversation, participant, message, and artifact;
- exact conversation memberships, message relationships, message-artifact
  links, account identity, authoritative coverage, and recomputed restoration
  completion;
- for schema 2 and later, checkpoint counts, a matching synchronization run,
  exact FTS projection, and a contiguous valid change stream;
- for schema 3 and later, replica-generation identity and empty reconciliation
  staging.
- for schema 4, every cached Moment/interaction record, its indexed
  projections, aggregate counts, and cached-surface coverage state.

Cached Moments were introduced in schema 4. They are absent by contract from
schema-1 through schema-3 backups and are fully verified and preserved when a
schema-4 backup is migrated to current schema 5. The opaque account-holder
participant column is new in schema 5, so migration of a historical backup
leaves it null rather than inventing an identity. A later synchronization may
bind that replica from an independently audited, account-bound archive, but a non-null
binding can never be changed or downgraded to null.

Success emits format-1 aggregate counts and boolean verdicts only. It omits the
account, source fingerprint, replica identity, paths, record contents, search
text, timestamps, and key. The audit does not migrate, checkpoint, repair, or
rewrite the backup and does not create WAL/SHM files for backups created by the
current implementation. A wrong key or any mismatch fails without a partially
green report.

The command proves internal consistency of the retained encrypted backup. It
does not prove that source restoration was complete, replace `audit-archive`,
or decide that restoring an older database is operationally preferable to
rebootstrapping from the current authoritative archive. Preserve the current
serving database and authoritative archive until recovery has been chosen and
validated.

`prepare-replica-recovery` can copy a passing backup into a new path, migrate
that copy, and deep-audit the current-schema candidate without replacing any
existing state. See `REPLICA_RECOVERY.md`.
