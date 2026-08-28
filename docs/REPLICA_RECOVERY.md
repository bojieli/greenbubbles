# Non-destructive replica recovery preparation

A retained pre-migration backup is intentionally not swapped over the serving
replica by an automated command. GreenBubbles can instead turn a verified older
backup into a separate, current-schema recovery candidate:

```sh
greenbubbles-restore prepare-replica-recovery \
  <encrypted-pre-migration-backup.db> <new-candidate.db> \
  --replica-key-stdin
```

The candidate's parent must already be an owner-only, non-symlink directory.
The candidate database, WAL, SHM, and rollback-journal paths must all be absent,
and their SQLite filename namespace must not overlap the source backup. The
command never replaces an existing entry. Supply the distinct 32-byte replica
key locally through standard input, preferably from an owner-controlled secret
manager.

## Preparation transaction

The command performs these fail-closed stages:

1. audit the schema-1 through schema-4 source backup read-only;
   descriptor-hash its complete database/WAL/SHM/journal namespace before and
   after audit and copying so mutation or sidecar appearance fails closed;
2. reserve a new mode-`0600`, no-follow candidate file;
3. use SQLite's online backup API to create a consistent, same-key encrypted
   rollback-journal copy, including any committed source WAL state;
4. close and independently audit the copied historical schema, requiring the
   complete aggregate verdict to match the source audit;
5. apply the exact compiled migrations and migration-identity records;
6. switch the candidate to the serving WAL/full-synchronous configuration and
   run the complete current-schema `audit-replica` transaction;
7. require initialized state and every canonical/link/cached-surface count to
   remain identical across the migration.

Schema-1 migration backfills FTS from canonical message text and creates a
checkpoint, matching reconciliation run, and initial checkpoint change event
from the already committed identity and canonical counts. This prevents a
populated older database from acquiring a current schema number while retaining
empty operational indexes or history.

Schema-4 recovery additionally audits and preserves every cached Moment,
interaction, and cached-surface coverage record. Migration to schema 5 leaves
the new account-holder participant field null because an older backup contains
no integrity-bound selected-account evidence; recovery never guesses that identity.

Success emits a format-1 aggregate report containing only schema versions,
verification verdicts, initialization state, and canonical counts. It omits
account/source/replica identities, paths, content, timestamps, and the key. On
failure, only the newly reserved candidate namespace is cleaned up. The source
backup and any serving replica are never opened for writing.

## Cutover remains explicit

Preparation is not cutover. GreenBubbles does not stop a running service,
rename an active database, discard its WAL, or decide whether an older recovery
point is preferable to rebootstrap from the latest authoritative archive. Keep
the source backup, current serving database, and authoritative archive until an
operator has reviewed the candidate and chosen a recovery procedure. Any later
replacement must happen with all replica users stopped and must preserve the
displaced state for rollback; that externally coordinated action is outside
this command.

The candidate proves internal consistency at the retained recovery point. It
cannot repair a wrong key or a backup that fails its historical audit, recover
source data absent from that point, or replace real-corpus completeness and
latency evidence.
