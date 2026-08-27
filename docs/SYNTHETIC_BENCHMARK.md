# Synthetic synchronization benchmark

GreenBubbles includes a bounded, reproducible benchmark and fault harness for
the encrypted canonical replica:

```text
cargo run --release --locked --manifest-path Native/GreenBubblesRestore/Cargo.toml -- \
  synthetic-benchmark private/benchmark-work \
  --samples 7 --small-messages 100 --large-messages 5000 --burst-messages 100
```

The work directory must be owner-only. Per-sample canonical archives and
SQLCipher replicas live in an automatically removed temporary child directory.
The command emits only aggregate JSON; it emits no message bodies, identifiers,
keys, or filesystem paths.

## Recorded baseline

This baseline was recorded on 2026-08-27 from an optimized build on macOS
26.6.2, arm64, Apple M2 Max. Seven samples used generated canonical archives.
The p95 is the nearest-rank percentile and therefore equals the maximum with
seven samples.

| Case | Messages | Candidate changes | Archive bytes | p50 ms | p95/max ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| Bootstrap, small | 100 | 100 | 99,737 | 13.781 | 18.276 |
| Bootstrap, large | 5,000 | 5,000 | 4,876,744 | 416.564 | 438.784 |
| Idle/no-op | 100 | 0 | 99,737 | 1.183 | 1.332 |
| One message | 101 | 1 | 100,709 | 5.427 | 5.768 |
| Burst | 200 | 100 | 196,733 | 21.061 | 22.365 |
| Edit | 100 | 1 | 99,720 | 5.627 | 5.736 |
| Recall | 101 | 1 | 100,854 | 5.549 | 5.845 |
| Deletion | 99 | 1 | 98,773 | 5.525 | 5.820 |
| Missed wake-up hint | 101 | 1 | 100,724 | 5.402 | 5.588 |
| Same-source decoder upgrade | 100 | 0 | 99,737 | 4.518 | 4.820 |
| Crash, reopen, retry | 101 | 1 | 100,703 | 10.630 | 10.740 |

Every case verified its expected added/changed/removed counts. The missed-hint
case reached the change through an authoritative sweep without a wake-up hint.
The decoder-upgrade case verified a non-idempotent reconciliation and checkpoint
revision change with an unchanged source fingerprint. The crash case first
presented malformed NDJSON, proved that the old checkpoint remained
authoritative after reopening, and then successfully committed the valid input.

## Evidence boundary

These numbers cover generated canonical archive reads and encrypted replica
transactions. They do not include live WeChat snapshot acquisition, source
SQLCipher decryption, schema decoding, notification delay, full media-tree I/O,
or OS scheduling over a long observation window. They establish functional and
synthetic performance regressions; they do **not** establish the product's
real-corpus “new locally persisted text within 60 seconds at p95” objective.
That claim requires owner-authorized measurements on the pinned client and a
stable database passphrase.
