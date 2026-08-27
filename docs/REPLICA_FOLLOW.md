# Continuous replica follower

GreenBubbles separates privileged offline acquisition/restoration from the
long-running encrypted replica process. A restoration operator publishes only
an already completed authoritative archive; the follower never opens live
WeChat stores, receives a WeChat database passphrase, invokes WeChat, or merges
an incremental fragment by itself.

## Publish an authoritative generation

After bootstrap restoration, full restoration, or `merge-incremental` has
completed successfully, publish its canonical directory with a strictly
increasing owner-controlled generation:

```sh
greenbubbles-restore replica-publish \
  <authoritative-archive> <private-handoff.json> --generation 1
```

For normal offline operation, `restore-publish` performs restoration, chain
verification, merge, archive audits, and lock-derived next-generation
publication as one fail-closed sequence. The explicit `replica-publish` command
remains useful when publishing an independently prepared authoritative archive.
See `OFFLINE_PIPELINE.md`.

The publisher requires an owner-only canonical archive directory and an
authoritative restoration report. Production-format archives pass the complete
independent archive audit before publication. The publisher also
descriptor-hashes every archive-owned regular file into a deterministic seal.
The mode-`0600` format-3 handoff binds the canonical absolute archive location,
source fingerprint, exact report digest, seal/count/bytes, generation, and
publication time, then replaces the prior handoff atomically under an owner-only
lock. Equal or lower generations fail closed. Legacy format-2 handoffs remain
readable but have no timing evidence. Handoff, state, and replica files must
live outside the sealed archive.

The command returns only the generation and aggregate verdicts. The handoff
itself is private because it contains the local archive path and source
fingerprint.

## Run the follower

The replica key remains distinct from the WeChat passphrase and enters only
through standard input:

```sh
greenbubbles-restore replica-follow \
  <private-handoff.json> <private-follow-state.json> <encrypted-replica.db> \
  --replica-key-stdin --poll-milliseconds 1000
```

The process watches only handoff file metadata while idle. A new atomic
handoff triggers full verification, then bootstrap or transactional replica
synchronization. Successful applications are emitted as aggregate NDJSON.
`--maximum-polls` provides a bounded supervisor/diagnostic mode; omit it for a
continuous process. `replica-follow-once` performs the same verified transition
without polling.

Supervisors can inspect exact generation lag and checkpoint age without
receiving an account ID, source fingerprint, archive path, or message content:

```sh
greenbubbles-restore replica-follow-status \
  <private-handoff.json> <private-follow-state.json> <encrypted-replica.db> \
  --replica-key-stdin
```

The status is `uninitialized`, `pending`, `current`, or
`stateRecoveryRequired`. It verifies handoff/state monotonicity and the applied
replica identity/checkpoint binding. To keep health checks bounded, it defers
the potentially large whole-archive audit and seal verification until actual
application and says so explicitly in the result. It reports the current
publication age and, when that generation produced a later checkpoint, the
publication-to-checkpoint latency. Each follower application report also
includes its monotonic local runtime. No absolute timestamp is emitted by these
aggregate reports.

The follower requires:

- a canonical absolute archive path;
- an unchanged report digest and source fingerprint;
- an unchanged whole-archive seal before and after replica application;
- an authoritative archive, with the full independent audit for production
  formats;
- a strictly monotonic generation with no same-generation equivocation;
- the same encrypted replica generation as its prior state; and
- a committed replica checkpoint matching the published account and source.

Its mode-`0600` state binds the random replica generation, applied handoff
digest/generation, source fingerprint, and committed checkpoint revision. A
repeated generation is an idempotent no-op only when the encrypted replica
still matches that state. State is written atomically after the replica commit.
If the process crashes after the database commit but before the state rename,
the retry adopts the replica only after a read-only comparison proves that its
account, source fingerprint, and complete restoration revision exactly match
the published archive, then completes the state write.
If state exists without the matching replica checkpoint, startup fails instead
of assuming success.

Publisher and follower state transitions use stable owner-only lock files, so
concurrent publishers cannot reuse a generation and concurrent followers
cannot race a checkpoint/state update. Atomic file replacement plus descriptor
checks prevent partial control JSON from being accepted.

## Remaining real-data boundary

This closes the missing long-running handoff between offline restoration and
the encrypted serving replica. It does not prove the 60-second real-WeChat
objective by itself. That measurement still requires a disposable-account
corpus and an owner-supplied plaintext/passphrase workflow that can produce
successive semantic archives. The upstream acquisition process must continue
to follow the snapshot, incremental merge, reconciliation-window, and integrity
scan rules; wake-up hints never become authority. Format-3 timing deltas make
that future publication-to-searchable measurement reproducible but are not a
substitute for the missing real run.
