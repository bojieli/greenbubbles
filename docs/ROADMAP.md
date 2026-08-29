# Roadmap

What is built, what is next, and — for each thing that is not built — the
specific evidence that would have to exist first. Several items below are
blocked on evidence rather than on code, and are deliberately not scheduled.

The full original plan is preserved in
[`archive/PLAN-2026-08.md`](archive/PLAN-2026-08.md). It predates the shift from
full restoration to bounded live queries, so read it as history.

## Built

- **Discovery and inventory.** Locating installations, accounts and candidate
  artefacts without opening any database contents.
- **Bounded read-only queries.** Conversation and message pagination, native
  FTS search with a zero-write decoded fallback, and exact message retrieval,
  against live encrypted, plaintext and snapshot sources.
- **Lazy attachments.** Exact-message image, voice, video and document
  inspection with single-candidate materialization and verified digests.
- **The native history browser.** Live and snapshot browsing on macOS.
- **Recoverable snapshots.** Random per-generation key, 24-word portable
  recovery, optional Keychain and hidden-file convenience protectors, atomic
  rotation, retention quarantine, and a recovery proof that uses no WeChat key.
- **Lossless restoration and the encrypted replica.** Archive format, offline
  publication, change-proportional synchronization, and a follower that never
  sees a WeChat passphrase.
- **The AI boundary.** Policy-scoped direct and replica connectors, one-shot
  `ai-query`, static `ai-export` bundles, memory projections, and a
  hash-chained body-free audit journal.
- **Independent verification** for every artefact above — see
  [AUDITING.md](AUDITING.md).
- **Signed distribution.** Developer ID signed, Apple notarized releases for
  Apple silicon, with SBOM, checksums and notarization logs.

## Next, and unblocked

These need work, not permission.

1. **Close the real-corpus latency gate.** Build the disposable-account
   protocol described in [MEASUREMENTS.md](MEASUREMENTS.md): observe when text
   became locally persisted, measure through the committed searchable
   checkpoint, and attribute idle, one-message, burst, edit, recall, deletion
   and fault cases separately. Until this exists the 60-second p95 objective is
   an aspiration.
2. **One complete real-corpus coverage run.** On a single immutable
   pinned-version corpus, close row accounting, observed logical-type coverage,
   relationships, and every downloaded or missing media state *together*. This
   is what would let `fullRestorationVerified` mean something general rather
   than per-archive.
3. **Verify discovery on Intel.** Currently unbuilt and unverified. Also across
   two explicitly fingerprinted client versions, using redacted metadata only.
4. **Reduce the fallback search cost further, or stop caring.** ~352 ms p95
   across 16 conversations is acceptable; the measurement harness exists to
   catch a regression that would change that judgment.

## Blocked on decisions, not code

**Sending to ordinary contacts.** The contract, adapter, outbox,
approval issuer and reconciliation are implemented and pass fault-injection
tests. Two things are outstanding and neither is an engineering task:

- a qualified mechanism, legal, platform-rules and account-safety decision for
  an exact client build; and
- a provisioned release signing key, without which no calibration profile
  verifies and no rollout stage above `dryRun` can open.

The guard denies while any gate-evidence flag is false, which is the current
state of every public build. If those decisions are ever made, the first step
is confirmed text sending to one allow-listed disposable conversation — not a
general capability. See [SEND_ADAPTER.md](SEND_ADAPTER.md) and
[ACTION_SAFETY_CONTRACT.md](ACTION_SAFETY_CONTRACT.md).

**Authenticated active reads.** Asking the running client to fetch narrowly
scoped dynamic content requires the same class of decision, and the feasibility
study is archived rather than acted on. Passing the passive-read tier never
authorizes this tier; the three privilege levels are non-transitive by design.

**Public article fetch.** Remains fail-closed while the published robots policy
disallows the relevant path. Re-checked before any explicitly requested fetch.
See [PUBLIC_ARTICLE_FETCH.md](PUBLIC_ARTICLE_FETCH.md).

## Not planned

- Windows, Linux, Android or iOS support.
- A cloud service, sync backend, or hosted component of any kind.
- A second messaging connector. GreenBubbles is a WeChat connector, not a
  universal personal-context engine; that was an explicit early decision and
  broadening it would be a new product, not a feature.
- Stealth, anti-detection, or any form of access-control bypass. A negative
  feasibility result gets accepted, not routed around.

## What would change this plan

- WeChat shipping a supported export or local API, which would make most of the
  acquisition machinery unnecessary and much of the compatibility work moot.
- A format change large enough that the current decoders cannot be repaired
  incrementally.
- External review finding a flaw in the snapshot protector construction.
- Legal developments affecting tools in this category — see
  [COMPARISON.md](COMPARISON.md).
