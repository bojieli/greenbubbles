# Privacy-safe latency evidence

The snapshot, offline publication, and follower commands measure different
parts of the passive synchronization path. `compose-latency-evidence` validates
and joins those reports into one aggregate-only sample without copying the
snapshot manifest, source fingerprint, archive path, account identity, content,
or absolute timestamps into its output.

Capture each command's complete JSON in an owner-only regular file. The
snapshot report is always private because it includes the manifest and retained
snapshot path. The handoff is also private. A safe local shell should set
`umask 077` before redirection; the composer rejects group/world permissions,
symlinks, hard links, oversized files, and files that change while read.

```sh
greenbubbles-restore compose-latency-evidence \
  <private-snapshot-report.json> \
  <private-offline-report.json> \
  <private-follower-report.json> \
  <private-current-handoff.json>
```

The composer requires:

- snapshot report format 2 with a retained acquisition-aware format-3 or
  account-bound format-4 snapshot;
- the exact same acquisition mode and private source fingerprint as the current
  format-3 handoff; a format-4 snapshot must also contain a structurally valid
  private account binding;
- one common positive publication generation across offline, follower, and
  handoff reports;
- a fully verified bootstrap, incremental, or integrity-scan publication
  transition and authoritative archive;
- a real `bootstrapped` or `synchronized` follower application rather than an
  `alreadyApplied` status check;
- exact row accounting, consistent restoration-completion state, bounded and
  internally consistent stage durations, and an available
  publication-to-checkpoint measurement.

The result contains the individual monotonic stage durations, aggregate row and
coverage-gap counts, whether restoration was complete, whether the source
advanced, and whether publication-to-checkpoint was at most 60 seconds.
`activeProcessingDurationMilliseconds` is the sum of the three commands'
measured active runtimes. It deliberately excludes delays between commands and
is not wall-clock source-to-search latency.

Every format-1 sample sets `fullEndToEndObjectiveProven` to `false` and records
at least these limitations:

- `sourcePersistenceStartNotObserved`;
- `interCommandDelayNotMeasured`;
- `disposableScenarioNotAttributed`.

Incomplete restoration and a source that did not advance add their own explicit
limitations. Consequently a fast publication-to-checkpoint value is useful
diagnostic evidence but cannot by itself satisfy the plan's real-client
60-second p95 gate.

## Aggregate samples

Store reviewed format-1 samples in one owner-only JSON array, then run:

```sh
greenbubbles-restore summarize-latency-evidence \
  <private-latency-sample-array.json>
```

The summary validates every sample again and reports nearest-rank p50/p95 plus
minimum/maximum values for active processing and publication-to-checkpoint,
mode counts, completion/source-advance counts, and the union of limitations.
It accepts 1–10,000 samples and still sets `fullEndToEndObjectiveProven` to
`false`; accumulating partial samples cannot manufacture missing evidence.

To close the real gate later, a controlled disposable-account protocol must
add an authoritative observation of when each scenario's text became locally
persisted, measure all inter-command/supervisor delay through the committed
searchable checkpoint, attribute idle/one-message/burst/edit/recall/deletion
and fault cases, and show complete semantic/media coverage. This tool prepares
privacy-safe stage evidence but does not claim that external work has occurred.
