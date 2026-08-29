# Running the encrypted replica

The serving replica is deliberately kept away from everything privileged.
Restoration happens offline, with the WeChat passphrase, under an operator. The
replica follower is a long-running process that never opens a live WeChat
store, never receives a WeChat passphrase, never invokes WeChat, and cannot
merge an incremental fragment by itself. The only thing that crosses between
them is a small signed-off handoff file.

This page covers publishing a generation, following it, retiring old archives
safely, and preparing a recovery candidate. The format itself is in
[REPLICA_SPEC.md](REPLICA_SPEC.md); verifying a replica is in
[AUDITING.md](AUDITING.md).

## Publishing a generation

For ordinary offline operation, `restore-publish` does restoration, chain
verification, merge, archive audit and publication as one fail-closed sequence
— see [RESTORATION_SPEC.md](RESTORATION_SPEC.md). The standalone publisher is
for an archive prepared some other way:

```sh
greenbubbles replica-publish \
  <replica-eligible-archive> <private-handoff.json> --generation 1
```

The archive must be owner-only, with an authoritative or complete-inventory
partial-database restoration report, and production-format archives must pass
the full independent archive audit first. The publisher then
descriptor-hashes every archive-owned file into a deterministic seal, and
writes a mode-`0600` format-3 handoff binding the canonical absolute archive
location, source fingerprint, exact report digest, seal, counts, bytes,
generation and publication time. The replacement is atomic and holds an
owner-only lock. Equal or lower generations fail closed.

**The handoff is private.** It contains a local archive path and a source
fingerprint. It does not belong in Git, an issue, a log, or a model prompt.

Each publication also extends a sealed generation history under the same lock,
binding prior publication digests and archive locations so retention can still
protect the predecessor after the single current handoff has been replaced. A
deployment with no history starts tracking from the current handoff; older
generations are never inferred. A handoff written immediately before a process
died is reconciled into the history on the next locked operation.

## Following it

```sh
greenbubbles replica-follow \
  <private-handoff.json> <private-follow-state.json> <encrypted-replica.db> \
  --replica-key-stdin --poll-milliseconds 1000
```

The replica key is distinct from the WeChat passphrase and enters only through
standard input. While idle the process watches handoff file metadata and
nothing else. A new atomic handoff triggers full verification and then a
bootstrap or a transactional synchronization.

Before applying anything, the follower requires a canonical absolute archive
path; an unchanged report digest and source fingerprint; an unchanged
whole-archive seal both before and after application; an authoritative or
complete-inventory archive; a strictly monotonic generation with no
same-generation equivocation; the same replica generation as its own prior
state; and a committed checkpoint matching the published account and source.

Its mode-`0600` state binds the replica generation, applied handoff digest and
generation, source fingerprint and committed checkpoint revision, and is
written atomically *after* the replica commit. That ordering matters: if the
process dies between the database commit and the state rename, the retry adopts
the replica only after proving read-only that its account, source fingerprint
and completion revision match the published archive exactly. State that exists
without a matching checkpoint fails at startup rather than assuming success.

Publisher and follower both take stable owner-only locks, so concurrent
publishers cannot reuse a generation and concurrent followers cannot race a
checkpoint update. Atomic replacement plus descriptor checks stop partial
control JSON from ever being accepted.

`replica-follow-once` performs the same verified transition without polling.
`--maximum-polls` gives a bounded supervisor mode; omit it for a continuous
process.

### Partial coverage is not deletion

A format-5 `partialDatabaseCoverage` archive is eligible when it accounts for
the complete database inventory. Partial incremental publication carries prior
records forward for changed source sets that were unavailable, so a temporary
decryption failure degrades to stale data rather than being applied as a mass
deletion.

### Checking health

```sh
greenbubbles replica-follow-status \
  <private-handoff.json> <private-follow-state.json> <encrypted-replica.db> \
  --replica-key-stdin
```

Reports `uninitialized`, `pending`, `current` or `stateRecoveryRequired`,
having verified handoff/state monotonicity and the applied replica identity.
To stay bounded, it defers the whole-archive audit and seal verification until
an actual application — and says so in the result rather than implying it
checked. It reports the current publication's age and, when that generation
produced a later checkpoint, the publication-to-checkpoint latency.

A supervisor gets generation lag and checkpoint age with no account ID, no
source fingerprint, no archive path, no content, and no absolute timestamps.

## Retiring archives without losing them

Old authoritative archives are large, and deleting the wrong one destroys the
ability to rebuild. GreenBubbles therefore never deletes: it quarantines.

Create an owner-only mode-`0700` directory **on the same filesystem** as the
archives, then:

```sh
greenbubbles replica-archive-quarantine \
  <private-handoff.json> <private-quarantine-directory> \
  --retain-publications 2
```

Two is the floor; the current and immediately preceding publications are always
protected. If an older publication reuses an archive path a protected record
references, that physical archive is retained too. Eligible archives are
seal-verified, atomically renamed into a deterministic quarantine location,
re-sealed there, and only then recorded in the history.

Same-filesystem renaming is the point: it is atomic, so an interrupted move
leaves one of two recognizable states rather than a half-copied directory. A
retry finds either location, verifies the complete seal, and repairs the
history. Both locations existing, neither existing, a cross-filesystem move,
nested archive and quarantine roots, symlinks, changed files, or non-private
permissions all fail closed.

**A quarantined archive is not usable in place.** Canonical reports and
artifact evidence intentionally bind the original absolute path. Do not edit it
there. To bring one back:

```sh
greenbubbles replica-archive-restore \
  <private-handoff.json> <private-quarantine-directory> \
  --generation <positive-integer>
```

This verifies the quarantine seal, renames the archive back to its exact
original canonical path, re-runs authoritative verification there, and clears
the quarantine state for every publication sharing that archive. A stop between
rename and history update is recognized and completed on retry.

Permanent deletion is outside this workflow, and once done that generation
cannot be restored.

## Preparing a recovery candidate

A retained pre-migration backup is never swapped over a serving replica by an
automated command. What GreenBubbles will do is build a separate, fully
migrated, deep-audited candidate:

```sh
greenbubbles prepare-replica-recovery \
  <encrypted-pre-migration-backup.db> <new-candidate.db> --replica-key-stdin
```

The candidate's parent directory must already be owner-only and non-symlink,
the candidate's database/WAL/SHM/journal paths must all be absent, and its
SQLite filename namespace must not overlap the source backup. Nothing existing
is ever replaced.

The command runs seven fail-closed stages: audit the schema-1-to-4 source
read-only, descriptor-hashing its whole file namespace before and after so any
mutation or sidecar appearance fails; reserve a mode-`0600` no-follow
candidate; copy it with SQLite's online backup API including committed WAL
state; close and independently audit the copy, requiring its verdict to match
the source's; apply the exact compiled migrations; switch to the serving
WAL configuration and run the full current-schema audit; and require every
canonical, link and cached-surface count to be identical across the migration.

Schema-1 migration backfills FTS from canonical message text and creates a
checkpoint and initial change event from the already committed identity, so a
populated old database cannot acquire a current schema number while keeping
empty indexes. Schema-4 recovery preserves every cached Moment and interaction.
Migration to schema 5 leaves the account-holder field null, because an old
backup has no account-bound evidence and recovery does not guess an identity.

### Preparation is not cutover

GreenBubbles will not stop your service, rename an active database, discard a
WAL, or decide that an old recovery point beats rebootstrapping from the latest
authoritative archive. Keep the backup, the current serving database and the
authoritative archive until you have reviewed the candidate and chosen. Any
replacement must happen with all replica users stopped and must preserve the
displaced state for rollback. That coordination is yours, not the command's.

A passing candidate proves internal consistency at that recovery point. It
cannot repair a wrong key, recover data that was already absent, or substitute
for real-corpus evidence.

## What none of this proves

This closes the handoff between offline restoration and the serving replica.
It does not establish the 60-second real-WeChat objective. That still needs a
disposable-account corpus producing successive semantic archives, measured end
to end — see [MEASUREMENTS.md](MEASUREMENTS.md) and
[KNOWN_LIMITATIONS.md](KNOWN_LIMITATIONS.md). Upstream acquisition must keep
following the snapshot, merge, reconciliation-window and integrity-scan rules;
a filesystem wake-up hint is a latency optimization and never an authority.
