# Measurements

Every performance number in this repository appears here, with the machine it
came from, the date it was taken, how many samples it represents, and what it
does not establish. If a claim is made anywhere else in this project and is not
backed by a row on this page, treat it as unsupported.

Two things are true of all of it:

- **No number here was measured against a live WeChat account under load.**
  Everything is either a static property of one real corpus, or a synthetic
  benchmark on generated data.
- **The project's own latency objective has never been met or even measured.**
  That gate is described at the bottom.

Reference machine for every timing below: Apple M2 Max, macOS 26.6.2, arm64,
release build.

## The corpus these decisions were made against

Static sizes from the author's own owner-authorized account, measured
2026-08-29. This is one corpus on one machine; another account will differ.

| Item | Observed |
| --- | ---: |
| Source database groups | 26 |
| Total source set | 2.98 GB (2.78 GiB) |
| Database files | 2.92 GB |
| WAL files | 60.4 MB |
| Messages | 1,855,548 |
| Message tables | 6,292 |
| Media tree beside the databases | ~59 GB |

The media figure is a filesystem footprint, not a completeness claim. A
separate metadata-only inventory found 136,873 attachment candidates totalling
38,875,540,902 bytes across the two inventoried roots; it did not prove that
every file was referenced, decodable, or included in a snapshot.

These numbers are the reason the architecture changed. Restoring that corpus
into the canonical JSONL form costs:

| Restored artefact | Size |
| --- | ---: |
| Text-only canonical archive | ~13.50 GB |
| `messages.ndjson` alone | ~12.71 GB |
| Temporary staging SQLite peak | ~7.42 GB |
| Zstandard-compressed staged payload | ~4.74 GB |
| Eager media derivatives, one run | ~30 GB |
| Replica bootstrap WAL, one run | ~18 GB |

A 2.98 GB source becoming a 13.5 GB archive before returning a single page is
the whole argument for bounded live queries. SQLite stores typed integers and
BLOBs; the restored form re-encodes them with JSON field names and base64 for
provenance, audit and indexing. That is a fair price for a forensic export and
a bad one for reading one conversation. See
[ARCHITECTURE.md](ARCHITECTURE.md).

## Search latency, and why there is no text cache

Recorded 2026-08-29. Twenty end-to-end CLI samples after three warmups per
case. Every case forced native FTS to be absent and searched for a term that
does not match, so the entire bounded window had to be decoded. The complete
source file inventory was compared before and after each run.

| Source and window | Payload | Initial p95 | Optimized p95 | Verification p95 |
| --- | ---: | ---: | ---: | ---: |
| Plaintext, 1 conversation, 500 messages | 256 B | 8.486 ms | 6.261 ms | 8.023 ms |
| SQLCipher, 1 conversation, 500 messages | 256 B | 345.292 ms | 245.626 ms | 240.373 ms |
| SQLCipher, 1 conversation, 500 messages | 8 KiB | 356.039 ms | 247.861 ms | 245.720 ms |
| SQLCipher, 16 conversations, 500 messages | 1 KiB | 4,387.747 ms | 351.648 ms | 352.490 ms |

The 4.4-second initial figure was not text matching. It was reopening the same
SQLCipher shards and repeating key setup once per conversation. The optimized
fallback opens each shard once per request, reuses those read-only connections
across the window, and enriches contact names only for hits that are actually
returned.

**The decision this drove:** at a worst verified p95 of ~352 ms, a persistent
encrypted text cache is not worth building. It would be a second copy of your
messages on disk to save a third of a second. GreenBubbles keeps native FTS
first and this zero-write decoded fallback second. No persistent writes were
observed before or after either implementation.

Reproduce it:

```sh
cargo test --release --test live_query_cli \
  fallback_search_latency_evidence_for_the_fixed_500_message_window -- \
  --ignored --nocapture --test-threads=1
```

It is an ignored test so that routine `cargo test` runs are not
timing-sensitive.

## Synchronization benchmark

Recorded 2026-08-27. Seven samples per case against generated canonical
archives. With seven samples the nearest-rank p95 equals the maximum, so the
right-hand column is a worst observed value, not a tail estimate.

```sh
greenbubbles synthetic-benchmark private/benchmark-work \
  --samples 7 --small-messages 100 --large-messages 5000 --burst-messages 100
```

| Case | Messages | Candidate changes | Archive bytes | p50 ms | max ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| Bootstrap, small | 100 | 100 | 99,737 | 13.781 | 18.276 |
| Bootstrap, large | 5,000 | 5,000 | 4,876,744 | 416.564 | 438.784 |
| Idle / no-op | 100 | 0 | 99,737 | 1.183 | 1.332 |
| One message | 101 | 1 | 100,709 | 5.427 | 5.768 |
| Burst | 200 | 100 | 196,733 | 21.061 | 22.365 |
| Edit | 100 | 1 | 99,720 | 5.627 | 5.736 |
| Recall | 101 | 1 | 100,854 | 5.549 | 5.845 |
| Deletion | 99 | 1 | 98,773 | 5.525 | 5.820 |
| Missed wake-up hint | 101 | 1 | 100,724 | 5.402 | 5.588 |
| Same-source decoder upgrade | 100 | 0 | 99,737 | 4.518 | 4.820 |
| Crash, reopen, retry | 101 | 1 | 100,703 | 10.630 | 10.740 |

Every case verified its expected added, changed and removed counts. Three are
worth reading as correctness tests rather than timings:

- **Missed wake-up hint** reached the change through an authoritative sweep
  with no filesystem hint at all, which is what makes hints an optimization
  rather than a dependency.
- **Same-source decoder upgrade** performed a non-idempotent reconciliation and
  changed the checkpoint revision while the source fingerprint stayed
  identical.
- **Crash, reopen, retry** first presented malformed NDJSON, proved the old
  checkpoint stayed authoritative after reopening, then committed the valid
  input.

The work directory must be owner-only. Per-sample archives and replicas live in
a temporary child directory that is removed automatically, and the command
emits aggregate JSON only — no bodies, identifiers, keys or paths.

**Boundary.** These cover canonical archive reads and encrypted replica
transactions. They exclude live snapshot acquisition, source SQLCipher
decryption, schema decoding, notification delay, full media-tree I/O, and OS
scheduling over any long observation window. They catch functional and
performance regressions. They do not describe what the product does on a real
corpus.

## Acquisition, measured once

On 2026-08-27 an owner-authorized bootstrap and a following incremental from a
pinned WeChat 4.1.12 build were independently audited: 25 source sets in both
inventories, 75 copied DB/WAL/SHM entries in the bootstrap, 9 changed sets and
27 copied entries in the incremental, with independent comparison reproducing
exactly 9 content-changed sets and finding nothing reconciliation-only or
deleted.

This is real evidence that acquisition is change-proportional and that the
manifest classifies correctly. It says nothing about which messages changed, or
about latency, because no database was decrypted during the run. Details in
[AUDITING.md](AUDITING.md).

## Composing latency evidence locally

The snapshot, publication and follower commands each measure a different
segment of the passive path. `compose-latency-evidence` joins their reports
into one aggregate-only sample, without copying the manifest, source
fingerprint, archive path, account identity, content or absolute timestamps
into its output:

```sh
greenbubbles compose-latency-evidence \
  <private-snapshot-report.json> <private-offline-report.json> \
  <private-follower-report.json> <private-current-handoff.json>
```

Capture each command's complete JSON into an owner-only file — set `umask 077`
first. The composer rejects group- or world-readable files, symlinks, hard
links, oversized files, and files that change while being read. It requires one
common positive publication generation across all three reports, the same
acquisition mode and source fingerprint as the current handoff, a fully
verified transition, and a real follower application rather than an
`alreadyApplied` status check.

`summarize-latency-evidence` accepts 1–10,000 reviewed samples in one array and
reports nearest-rank p50/p95, minimum and maximum for active processing and
publication-to-checkpoint.

**Every sample sets `fullEndToEndObjectiveProven` to `false`, and accumulating
samples cannot change that.** Each records at least
`sourcePersistenceStartNotObserved`, `interCommandDelayNotMeasured` and
`disposableScenarioNotAttributed`. `activeProcessingDurationMilliseconds` is
the sum of three commands' measured runtimes; it deliberately excludes the
delay *between* commands and is not wall-clock source-to-search latency.

## The gate nobody has met

The objective is that text newly persisted by WeChat becomes searchable through
GreenBubbles **within 60 seconds at p95, on a real account**.

Nothing on this page demonstrates that, and the tooling is built so that no
combination of the evidence above can be assembled into a claim that it does.
Closing it requires a controlled disposable-account protocol that:

- authoritatively observes when each scenario's text became locally persisted;
- measures all inter-command and supervisor delay through the committed
  searchable checkpoint;
- attributes idle, one-message, burst, edit, recall, deletion and fault cases
  separately; and
- demonstrates complete semantic and media coverage over that corpus.

Until that exists, a fast publication-to-checkpoint number is a useful
diagnostic and nothing more. See [KNOWN_LIMITATIONS.md](KNOWN_LIMITATIONS.md).
