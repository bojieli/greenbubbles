# Independent restoration-archive audit

`greenbubbles-restore audit-archive` independently reopens a completed local
archive and verifies that its ledgers, coverage verdict, relationships, and
recorded artifact files still agree. It does not trust `report.json` merely
because the restoration process wrote it successfully.

This command is the final offline check before replica bootstrap or
synchronization:

```sh
cargo run --locked \
  --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
  audit-archive <owner-only-restoration-archive>
```

It needs no database passphrase or replica key. It reads only the already
restored owner-private archive and the exact downloaded media paths recorded by
that archive. It does not open a WeChat database, contact WeChat, invoke the
official client, or alter any source file.

Restoration and merge writers record canonical absolute archive paths, so the
archive can be audited from any working directory. Older or externally created
reports whose recorded paths do not resolve to their own archive files fail
closed.

## What is independently verified

The auditor fails closed unless all of these checks pass:

- every required archive file is an owner-only, single-link, non-symlink
  regular file and every path in `report.json` resolves to the corresponding
  file;
- message, rejection, artifact, conversation, participant, cached-Moment, and
  cached-interaction NDJSON ledgers parse completely with no empty or duplicate
  identity;
- source-preserving base64 fields are valid and artifact digests are
  well-formed;
- the row equation is reproduced from the ledgers for every individual message
  table, restored and rejected row identities are disjoint, and the global
  equation is not accepted as a substitute for per-table accounting;
- message logical-type, subtype, semantic-gap, direction, ordering,
  relationship, and artifact-reference counts reproduce both `coverage.json`
  and `report.json`;
- every present merged-history or Finder/channel `normalized_xml` projection is
  regenerated from its retained `raw_xml` and must match exactly; complete
  records must carry the reproducible projection with no gap, while legacy
  partial records may omit it only with an explicit semantic-gap verdict;
- the complete table ledger, supported message-table subset, source-row
  counts, message-candidate gaps, per-table schema fingerprints, and aggregate
  schema-profile fingerprint agree; every message row's source table name and
  raw-column set agree with that ledger;
- conversations are deterministically grouped, per-conversation ordinals are
  contiguous, and one conversation does not silently change ordering basis;
- every message conversation and sender resolves to the account-scoped entity
  ledgers and has a canonical ID independently re-derived from its preserved
  source identifier; participant/membership links are bidirectional, entity
  source rows match the complete schema ledger without duplication, every resolved
  relationship target exists in the same conversation, every resolved,
  absent-target, missing-identifier, ambiguous, and pending relationship state
  reproduces its integrity counter, and every message artifact ID and role
  agrees with the artifact ledger; every known media-bearing logical type has
  artifact references and exactly one preferred variant;
- every artifact availability state has one coherent evidence shape:
  downloaded sources carry the complete external-file identity, database
  payloads carry the complete connector-owned source identity, unavailable
  states carry neither, and ambiguous/corrupt states identify exactly one local
  source without mixing the two shapes;
- resource-row provenance is either wholly absent or includes database set,
  logical path, table ID/name, and positive row ID; present provenance resolves
  to the exact `MessageResourceInfo` or `VoiceInfo` auxiliary table in complete
  schema coverage, and every database-materialized voice identifies
  `VoiceInfo`;
- every downloaded source path is still an absolute canonical regular file,
  ends with its safe account-relative path, and matches the recorded device,
  inode, size, modification time, and SHA-256 before and after a descriptor-based
  read;
- every database-materialized or decoded derivative stays beneath the archive,
  traverses no symlink, remains owner-only and single-link, and matches its
  recorded size and SHA-256;
- decoded artifacts carry a complete verified derivative; non-decoded states
  carry no derivative fields; key-unavailable, unsupported, and failed states
  are compatible with the media kind and with an actually present lossless
  source; encoded image and materialized voice sources cannot silently use
  `notRequired` as their decode outcome;
- cached-surface row equations hold independently for every timeline and
  interaction table; canonical row and participant identities are reproduced
  from source evidence; interaction kind agrees with raw type; and its complete
  table/schema ledger agrees with the restoration archive;
- the component and top-level completion verdicts are no stronger than the
  independently reproduced evidence permits.

Pending relationships cannot satisfy semantic completion. An unresolved
relationship is completion-compatible only when it is explicitly classified as
a target that is not present in the locally available history; missing target
identifiers and ambiguous matches remain completion gaps. Canonical group-member
and entity-decode-gap counts are likewise reproduced exactly from the entity
ledgers rather than compared only as upper or lower bounds.

A source file that is deleted, replaced, edited, or evicted after restoration
therefore makes the audit fail. This is intentional: a stale pathname is not a
faithfully restorable downloaded artifact. Run the audit promptly after the
full media pass and again before a long-lived replica consumes a newly restored
revision.

## Privacy-safe output

On success, the command emits only aggregate counts, format/scope states,
coverage-gap counts, database freshness/unavailable/stale counts, and booleans.
It emits no message body, identifier,
filesystem path, source fingerprint, file digest, table name, or failure-row
identity. Errors name the failed invariant but not the sensitive record.

Audit-report format 2 includes `completionEvidence`, an independently derived
component verdict rather than a copy of the writer's top-level flag. It reports
row accounting (including zero rejections and unique identities), observed
message-type semantics, direction, entity, relationship, artifact verification,
artifact decoding, authoritative source scope, resolved media phase, and signed
4.1+-client compatibility separately. It also states whether the archive
actually contains messages, media references, and at least one still-verified
local source or connector-owned media file. This prevents a structurally empty
or media-free synthetic run from being mistaken for the representative corpus
required by the plan.

`technicalRestorationComplete` means all machine-verifiable completion
components pass for this one audited archive. It is deliberately accompanied by
`externalAuthorizationAttestationRequired`,
`disposableScenarioAttestationRequired`, and `observedCorpusScopeOnly`, all true.
The auditor cannot prove owner authorization, disposable-account provenance, or
that a different undiscovered table/type exists outside the observed snapshot.
Those attestations must never be flipped by archive contents.

`fullRestorationVerified` is true only when all archive checks pass and the
archive itself claims full restoration from an authoritative, media-resolved,
production-compatible signed 4.1+ build. A structurally valid archive with an
unknown message type, missing media, unsupported relationship, schema gap,
unrecognized client, deferred media phase, partial database coverage, or
incremental-fragment scope is
audited successfully but still reports `fullRestorationVerified: false`.

## Boundary of the evidence

This audit proves internal archive consistency and the current identity of
every recorded local file. It does not prove that an undiscovered WeChat table
was absent, that a private field's semantics were interpreted correctly, or
that a synthetically exercised nested tag graph covers every real-world
merged-message or Finder variant, or that remote-only history exists locally.
Those claims still require one real compatible-version corpus with zero unhandled
message tables, zero observed logical-type gaps, and an explicit state for
every media reference. Likewise, the audit does not authorize public
distribution, authenticated active reads, or actions.
