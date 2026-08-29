# Continuous replica follower

GreenBubbles separates privileged offline acquisition/restoration from the
long-running encrypted replica process. A restoration operator publishes only
an already completed replica-eligible archive; the follower never opens live
WeChat stores, receives a WeChat database passphrase, invokes WeChat, or merges
an incremental fragment by itself.

## Publish a verified generation

After bootstrap restoration, full restoration, or `merge-incremental` has
completed successfully, publish its canonical directory with a strictly
increasing owner-controlled generation:

```sh
greenbubbles replica-publish \
  <replica-eligible-archive> <private-handoff.json> --generation 1
```

For normal offline operation, `restore-publish` performs restoration, chain
verification, merge, archive audits, and lock-derived next-generation
publication as one fail-closed sequence. The explicit `replica-publish` command
remains useful when publishing an independently prepared replica-eligible archive.
See `OFFLINE_PIPELINE.md`.

The publisher requires an owner-only canonical archive directory and an
authoritative or complete-inventory partial-database restoration report.
Production-format archives pass the complete
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

Each publication also extends a mode-`0600` sealed-generation history under the
same handoff lock. It binds prior publication digests and archive locations so
retention can protect the current and immediate predecessor even after the
single current handoff is replaced. A missing history on an older deployment
is initialized from the exact current handoff; older unknown generations are
not inferred. A handoff committed immediately before process failure is safely
reconciled into history on the next locked operation.

## Run the follower

The replica key remains distinct from the WeChat passphrase and enters only
through standard input:

```sh
greenbubbles replica-follow \
  <private-handoff.json> <private-follow-state.json> <encrypted-replica.db> \
  --replica-key-stdin --poll-milliseconds 1000
```

The process watches only handoff file metadata while idle. A new atomic
handoff triggers full verification, then bootstrap or transactional replica
synchronization. Successful applications are emitted as aggregate NDJSON.
Both authoritative archives and format-5 `partialDatabaseCoverage` archives
are eligible when the latter account for the complete database inventory.
Partial incremental publication carries prior records for unavailable changed
source sets, preventing a temporary key/decryption failure from becoming a
replica deletion.
`--maximum-polls` provides a bounded supervisor/diagnostic mode; omit it for a
continuous process. `replica-follow-once` performs the same verified transition
without polling.

Supervisors can inspect exact generation lag and checkpoint age without
receiving an account ID, source fingerprint, archive path, or message content:

```sh
greenbubbles replica-follow-status \
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

The private handoff can also bind a snapshot/offline/follower report set for
`compose-latency-evidence`. The composer verifies one source and generation but
labels publication-to-checkpoint as only a partial timing because source
persistence and inter-command delay are not measured. See
`LATENCY_EVIDENCE.md`.

The follower requires:

- a canonical absolute archive path;
- an unchanged report digest and source fingerprint;
- an unchanged whole-archive seal before and after replica application;
- an authoritative or complete-inventory `partialDatabaseCoverage` archive,
  with the full independent audit for production formats;
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

`replica-archive-quarantine` can atomically move only seal-verified retired
archives into an owner-only same-filesystem quarantine. It enforces a minimum
of two protected publications, protects physical paths shared by an older and
protected generation, records each successful move, and never deletes data.
`replica-archive-restore` returns a quarantined archive to its exact original
path and verifies it there. Both operations recover a stop between rename and
history update and emit aggregate-only reports. See `ARCHIVE_RETENTION.md`.

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
