# Auditing what GreenBubbles produced

Every artefact GreenBubbles writes — a restoration archive, a serving replica,
an acquisition chain, a connector journal — carries a report saying it
succeeded. None of those reports are evidence. The process that wrote the
artefact also wrote the report, and a bug that corrupted one very plausibly
corrupted the other.

So each artefact has a second command that reopens it cold, re-derives the
claims from the data itself, and refuses to agree merely because the first
report was optimistic. This page is how to run those commands and what each one
does and does not prove.

| Command | Verifies | Needs a key |
| --- | --- | --- |
| `audit-archive` | a completed restoration archive | no |
| `audit-acquisition-chain` | that one snapshot continues another | no |
| `audit-replica` | the encrypted serving replica | replica key |
| `audit-replica-backup` | a retained pre-migration backup | replica key |
| `audit-connector-log` | the connector's audit journal | no |
| `audit-connector-state` | journal, drafts, policy and replica together | replica key |

Every one of them emits counts, verdicts and booleans only — never a message
body, an identifier, a filesystem path, a digest, a table name, or the identity
of the row that failed. An error names the invariant that broke, not the record
that broke it. You can paste any of these reports into an issue.

## Restoration archives

```sh
greenbubbles audit-archive <owner-only-restoration-archive>
```

This needs no database passphrase and no replica key. It reads the restored
owner-private archive and the exact media files that archive recorded. It never
opens a WeChat database, contacts WeChat, or touches a source file.

What it re-derives, rather than trusts:

**Row accounting.** The message, rejection, artifact, conversation,
participant, cached-Moment and cached-interaction ledgers are parsed in full.
The row equation is reproduced for *every individual message table* —
a matching global total is explicitly not accepted as a substitute, because two
tables can be wrong in opposite directions. Restored and rejected identities
must be disjoint, and no identity may be empty or duplicated.

**Semantics.** Logical type, subtype, direction, ordering, relationships and
artifact references are recounted from the ledgers and must reproduce both
`coverage.json` and `report.json`. Direction is checked against the archive's
one bound account-holder participant: a message is outgoing exactly when its
sender is that participant. Every sender/flag conflict is counted individually
rather than absorbed into an aggregate.

**Reproducible projections.** Merged-history and Finder XML projections are
regenerated from the retained raw XML and must match byte for byte. A
sender recovered by fallback is re-extracted independently; if it disagrees
with the unambiguous XML identity, it is rejected.

**Files that still exist.** Every downloaded media source must still be an
absolute canonical regular file whose device, inode, size, modification time
and SHA-256 match what was recorded — checked before *and* after a
descriptor-based read. Derivatives must stay inside the archive, traverse no
symlink, and remain owner-only and single-link.

That last check is why a file deleted, edited or evicted after restoration
makes the audit fail. This is deliberate: a stale pathname is not a restorable
artifact. Run the audit promptly after the media pass, and again before a
long-lived replica consumes a new revision.

### Reading the verdict

`technicalRestorationComplete` means every machine-verifiable component passed
*for this one archive*. It always ships alongside three flags that are always
true — `externalAuthorizationAttestationRequired`,
`disposableScenarioAttestationRequired` and `observedCorpusScopeOnly` — because
the auditor cannot prove that you were authorized, that the account was
disposable, or that no undiscovered table exists outside what it saw. **No
archive content can flip those.**

`fullRestorationVerified` is stricter still: it requires the archive itself to
claim full restoration from an authoritative, media-resolved, production
compatible signed 4.1+ build. An archive with one unknown message type, one
missing media file, an unsupported relationship, a schema gap, a deferred media
phase or incremental scope audits *successfully* and still reports
`fullRestorationVerified: false`. Passing the audit and being complete are
different questions.

### What it cannot tell you

It proves internal consistency and current file identity. It cannot prove that
an undiscovered WeChat table was absent, that a private field's semantics were
read correctly, or that a synthetic nested-tag fixture covers every real
merged-message variant. Those need one real compatible-version corpus with zero
unhandled tables and an explicit state for every media reference — see
[known limitations](KNOWN_LIMITATIONS.md).

## Acquisition chains

```sh
greenbubbles audit-acquisition-chain <previous-snapshot> <current-snapshot>
```

Proves that an incremental or integrity-scan snapshot is an exact continuation
of the baseline you supply, not a fresh unrelated capture wearing a generation
number. Every copied DB/WAL/SHM entry is digest-verified before the two
inventories are compared, and then the current baseline fingerprint must equal
the previous snapshot's source fingerprint, the account binding must be exactly
unchanged, reported deletions must equal the sets actually absent, reported
changes must equal the sets whose identity/size/timestamp/digest evidence
actually changed, and every reconciliation-only set must be byte-identical to
its baseline.

An ordinary WeChat update does not break a chain: exact build equality is
reported, but only signed 4.1+ compatibility at both endpoints is required.

**Recorded run.** On 2026-08-27 an owner-authorized bootstrap and a following
incremental from a pinned WeChat 4.1.12 build were audited. Both inventories
held 25 source sets; the bootstrap copied 75 DB/WAL/SHM entries; the
incremental reported 9 changed sets and copied 27 entries; independent
comparison reproduced exactly 9 content-changed sets; nothing was
reconciliation-only or deleted; all 9 copied databases stayed in the encrypted
WCDB/SQLCipher family.

That is real evidence of change-proportional acquisition and exact manifest
classification. It says nothing about *which* messages changed, decode latency,
or edits and recalls, because no database was decrypted during it.

## The serving replica

`replica-status` is the cheap operational view. `audit-replica` is the slow,
key-gated integrity pass:

```sh
greenbubbles audit-replica <encrypted-replica.db> --replica-key-stdin \
  --progress-file <owner-only-new-progress.ndjson>
```

The 32-byte replica key is **not** the WeChat database passphrase. It arrives
on standard input and belongs nowhere else — not an argument, a report, an
issue, a commit, or a model prompt.

The audit opens the replica read-only in one deferred transaction. It never
creates or migrates a replica, advances a checkpoint, repairs an index, or
opens an archive or live store. Inside that transaction it verifies SQLCipher
access, SQLite `integrity_check`, foreign keys, schema and migration-identity
ledger; the SHA-256 and canonical JSON encoding of every conversation,
participant, message, artifact, cached Moment and cached interaction; every
indexed serving column against its canonical record; exact memberships,
relationships and artifact ordinals; exactly one FTS row per message with no
missing, extra, duplicate, stale or cross-account row; the single account
identity and its checkpoint as one committed revision; and a contiguous
append-only change sequence with valid digests and empty reconciliation
staging.

An uninitialized replica is valid only when *every* serving, checkpoint,
coverage, sync, change, FTS, link and cached table is empty.

Progress is reported through eight monotonic stages. During SQLite's own
integrity and FTS operations there is no trustworthy row cursor, so the
heartbeat holds at the stage's starting percentage rather than inventing
movement. `--progress-json` emits NDJSON; `--quiet-progress` silences it;
`--progress-file` writes a flushed mode-`0600` file that may not overlap the
database or any sidecar.

Any mismatch fails. There is no partially green report. The audit never repairs
anything: use the authoritative archive and follower state to rebuild a damaged
replica.

## Pre-migration backups

Every upgrade from an older non-empty replica schema first writes a same-key
encrypted recovery database beside the serving replica. GreenBubbles does not
consider that backup successful because SQLite's backup call returned — it
converts the copy to rollback-journal mode, closes it, reopens it read-only, and
runs the same schema-aware verification an operator can run. Migration begins
only after that passes; a failed candidate is removed and the older replica is
left untouched.

To verify a retained backup yourself:

```sh
greenbubbles audit-replica-backup <encrypted-pre-migration-backup.db> \
  --replica-key-stdin --progress-file <owner-only-new-progress.ndjson>
```

It accepts only a supported, non-empty *older* schema (1–4). An uninitialized
schema-0 file and the current or a future schema are both rejected, so an
ordinary serving replica cannot be mislabelled as a migration backup. Within
that schema it checks every invariant the schema can express: canonical
encodings and digests everywhere; checkpoints, FTS and change streams from
schema 2; replica-generation identity from schema 3; cached Moments and their
coverage from schema 4.

Cached Moments arrived in schema 4 and are absent by contract from earlier
backups. The account-holder participant column arrived in schema 5, so
migrating a historical backup leaves it null rather than inventing an identity;
a later synchronization from an account-bound archive may set it, and a non-null
binding can never be changed or downgraded back to null.

## Preparing a recovery candidate

A verified backup is never swapped over the serving replica automatically. See
[replica operations](REPLICA_OPERATIONS.md) for `prepare-replica-recovery`,
which builds a separate current-schema candidate and deep-audits it without
replacing anything.

## The connector journal

The replica-backed connector appends body-free events to an owner-only NDJSON
journal. Format-2 events hash the complete canonical event including the
predecessor's digest, so the file is a hash chain. The service verifies the
whole journal under a shared lock before starting; each append takes an
exclusive lock, revalidates the tail, binds to it, writes one record and
fsyncs. Symlinked, multiply-linked, non-regular or group/other-readable files
fail closed.

```sh
greenbubbles audit-connector-log <owner-only-connector-audit.ndjson>
```

Format-1 events predate chaining. They are structurally validated but not
linked; when the first format-2 event is appended it anchors the exact bytes of
the last legacy record. The verifier counts the unchained prefix as
`legacyUnchainedEventCount` and sets `fullyChained` false. A legacy record
appearing *after* the chained suffix, a bad digest, a broken predecessor, a
duplicate event ID, mixed accounts or an unknown format are all rejected.

**What the chain is worth.** It detects accidental modification, reordering,
insertion, and removal when a retained successor still binds the missing
record. It is not a signature. Without an independently retained anchor it
cannot detect a clean truncation of the final suffix, and it cannot stop an
owner who rewrites the whole journal and recomputes every unkeyed hash.
Accountability for outward actions would need independent signing or anchoring,
and that is not built — see the [action safety
contract](ACTION_SAFETY_CONTRACT.md).

## Journal, drafts, policy and replica together

```sh
greenbubbles audit-connector-state <replica> <policy> <audit-log> \
  <draft-directory> --replica-key-stdin
```

Every entry in the draft directory must be a single-link, mode-`0600`, bounded
JSON draft whose filename equals its own recomputed identity. Unknown fields,
duplicate attachments or participants, missing sizes or digests, invalid reply
evidence, inconsistent recipients, excessive expiry, and any mutation of a body
or binding all fail closed. Drafts are opened with no-follow descriptors and
re-checked before and after each read.

The verifier fingerprints the journal and the entire draft directory again
before returning, so a concurrent creation or append cannot produce a
mixed-state success. If that happens, retry once the connector is quiescent.

It then reconciles the three surfaces: every draft must have exactly one
completed `draftRequested` event, and every completed `draftReviewed` event
must resolve back to the same draft, conversation and policy decision. Drafts
that are structurally valid but expired or stale under the current policy,
connector version or checkpoint are reported separately from valid ones.

### Action stages

`approvalRecorded`, `attemptRecorded` and `reconciliationRecorded` are written
only by the send adapter, never by a connector read. One attempt produces
exactly three events, in order, each written before the thing it describes can
have taken effect:

1. `approvalRecorded` — the precheck decision. A denial ends here and no
   effector is ever called.
2. `attemptRecorded` — appended *before* dispatch, so a process killed
   mid-send still leaves proof that a dispatch was imminent.
3. `reconciliationRecorded` — the settled outcome, and again later if a parked
   attempt is resolved against the replica.

`audit-connector-state` enforces that ordering per draft: an attempt without a
completed approval, or a reconciliation without an attempt, is an integrity
failure. While the send path remains closed, the audit rejects any of these
stages outright.
