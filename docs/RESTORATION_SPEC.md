# Restoration

Restoration is lossless with respect to the authorized local snapshot. A
renderer may show you a simplified view, but the canonical record must retain
enough to reproduce *and audit* every source row.

This is the format and the offline pipeline that publishes it. Verifying the
result is [AUDITING.md](AUDITING.md); the serving side is
[REPLICA_SPEC.md](REPLICA_SPEC.md).

## What "complete" means

A restoration is complete only when all of the following hold:

1. Every message-bearing table in every supported shard was enumerated —
   ordinary, group, business/public-account and chatbot stores alike.
2. Every source row appears exactly once in the canonical output, **or** is
   listed in a machine-readable rejection ledger with its database, table,
   stable row identity and reason. Silent drops are forbidden.
3. Ordering uses the strongest available source ordering (`sort_seq`, server
   sequence, creation time, local ID, and source-row tie breakers) and stays
   deterministic when timestamps collide.
4. The original type value, low-16-bit logical type, compression markers,
   source fields, packed metadata and undecoded payload bytes are all retained.
5. Known types get a typed interpretation; unknown types stay lossless opaque
   records and count as coverage gaps until documented.
6. Sender, conversation, quote/reply, recall/edit, reaction and attachment
   relationships are resolved wherever the local source exists.
7. Every downloaded image, animation, audio item, video, document, thumbnail or
   auxiliary artefact links to a canonical artifact record with an opaque path
   reference, size, digest, detected format, verification state, and complete
   auxiliary provenance when `MessageResourceInfo` or `VoiceInfo` supplied it.
8. A remote-only, expired, deleted, corrupt or not-downloaded attachment is
   represented **explicitly**, and is never reported as restored media.
9. Restoration never alters the source database or media tree, and never puts
   account secrets, plaintext history or stable identifiers into Git or an
   ordinary log.

The completion invariant:

```text
source rows = restored rows + rejected rows
restored rows = uniquely identified canonical messages
```

For a production-complete decoder, rejected rows must be **zero**. Unknown
typed payloads may be retained losslessly during development, but they prevent
any claim of full semantic restoration until each observed type is understood.

### Text first

`restore --defer-media` is the explicit text-first mode. It still emits a
reference for every media-bearing message, but that reference is labelled
deferred and carries no guessed path or digest. The report sets
`mediaPhase: deferred` and cannot claim full restoration. Re-running from the
*identical* immutable snapshot without the flag produces a `resolved` archive
with the same source fingerprint and verified paths, which the replica treats
as a new revision.

### Isolation in resolved mode

If an optional `MessageResourceInfo` or `VoiceInfo` table becomes unreadable,
healthy message tables still restore and media-bearing messages keep
`metadataMissing` evidence. If one candidate file or voice row cannot be read,
that candidate is `corrupt` while everything else continues. Session, contact
and group tables are enrichment surfaces under the same rule: message-derived
conversation and participant seeds stay publishable when an enrichment row is
unreadable.

Account-root binding and path containment are unchanged security boundaries
throughout. **Degraded media handling never releases an unverified path or
file.**

## The canonical message

Every message carries these source-preserving fields, even when a typed decoder
also exposes something friendlier:

```text
account / conversation / sender opaque IDs
source database-set ID / table ID / row identity
local ID / server ID / sort sequence / timestamp
raw type / logical type / direction / status
content bytes / packed-info bytes / compression metadata
typed payload, or an explicit unknown-payload reason
zero or more relationship and artifact references
```

Every known media-bearing type has one or more artifact references and exactly
one *preferred* variant — and preference does not erase alternates. Originals,
high-resolution images, thumbnails, video payloads and posters, and ambiguous
candidates all stay independently represented.

Availability and decode states are orthogonal but constrained: a downloaded or
database-materialized lossless source is verified separately from any decoded
derivative; an unavailable state cannot retain local-file evidence; a decoded
state cannot omit its derivative size, digest, format or archive-owned path.

## Who sent it

Direction is one of the easiest things to get subtly wrong, so it is derived
from bound identity rather than inference.

During **acquisition** — not later analysis — the format-4 exporter derives the
account holder from the single selected WeChat account directory, verifies that
every database, WAL and SHM source lives under that account's `db_storage`, and
writes private binding evidence into the manifest. The source identifier stays
base64-encoded inside that owner-only manifest; archives, replicas, AI bundles
and UI state receive only the account-scoped opaque participant ID. The binding
is part of the source fingerprint and cannot change across an incremental
chain.

```text
sender == bound account holder    → outgoing / self-authored
sender != bound account holder    → incoming / other-authored
no sender + explicit source flag  → the direction that flag states
no sender + no flag               → unknown
```

**A sender-bearing row is never classified from a contact display name, a
direct conversation peer, message frequency, conversation shape, or group
ownership.** The sender comparison outranks an `is_sender`-style source column;
a disagreement is retained as
`senderAccountConflictWithExplicitSourceColumn`, increments
`directionConflictCount`, and makes restoration incomplete. A group's owner is
conversation metadata, not evidence about who you are.

An empty sender is *absence*, and never becomes a synthetic participant. For
Pat messages (logical type 49, subtype 62) whose sender column is empty,
restoration reads `<fromusername>` from the retained typed XML: blank duplicates
are ignored, identical nonempty duplicates agree, and two differing nonempty
values, nested values, malformed XML, control characters or an oversized value
all fail closed and leave the sender unresolved. The independent audit
regenerates that XML evidence and rejects any disagreement with a retained row
sender. A system notice with neither sender evidence nor a flag stays honestly
`unknown`.

Account-bound restoration writes archive report format 6 with
`selfParticipantId` and binding provenance. Format-5 and older archives remain
readable as legacy evidence and **do not acquire an identity by heuristic.**

Raw payloads stay inside the local trust boundary. AI-facing tools receive only
policy-approved normalized fields — never the lossless archive by default.

## Nested XML

Merged-message and Finder/channel app messages retain their complete source XML
and, when bounded parsing succeeds, add a format-versioned ordered structural
projection preserving namespaces, attributes, text, comments, processing
instructions, and recursively parsed XML embedded in `recorditem`, `content` or
`recordxml` text.

It assigns no undocumented meaning to private tags. DTD processing is disabled,
and byte, node and embedded-depth limits turn a malformed or adversarial
structure into an explicit semantic gap while leaving the authoritative raw XML
recoverable. Details in [STORAGE_FORMAT.md](STORAGE_FORMAT.md).

## Cached Moments

When a supported `sns/sns.db` is present, restoration also writes the
owner-only triplet `cached-moments.ndjson`,
`cached-moment-interactions.ndjson` and `cached-surfaces.json`. **Only** the
exact `SnsTimeLine` and `SnsMessage_tmp3` signatures observed on the pinned
client are normalized; every other SNS table stays a schema-coverage record and
is never guessed into a Moment.

Cached records retain database/table/row provenance, opaque canonical
identities, raw SQLite columns, original XML and blob bytes, best-effort typed
fields, semantic decode state, and the snapshot observation time. Their
completeness is always `partialLocalCache`: **absence is not evidence that a
server-side Moment, like or comment does not exist.** This path loads no more
content, contacts no server, and implies no active-read capability.

## The integrity report

Counts that can be checked without printing any message content: every
discovered table and column set, classified as supported message table, known
auxiliary table, other table or unhandled message candidate; source tables and
rows discovered; rows restored, rejected, duplicated and unknown by logical
type; relationship references resolved and unresolved; attachment references
resolved, missing, remote-only and corrupt; source and output fingerprints;
decoder and supported-client versions.

Any message-like name or column signature that does not match a supported safe
adapter is labelled `unhandledMessageCandidate`, increments
`messageCandidateGapCount`, and keeps semantic completion false. That is what
makes a new or version-drifted shard fail closed instead of quietly vanishing
from row accounting.

`report.json` also carries signed-client compatibility evidence and a
component-by-component verdict. `fullRestorationAchieved` is true only when the
client is a signed compatible WeChat 4.1+ build **and** row accounting,
canonical identity uniqueness, semantic decoding, directions, entities,
relationships, artifact verification and artifact decoding all pass. Retaining
raw bytes is necessary for losslessness and does not by itself establish
production compatibility, semantic completeness, or playable media.

Format 5 added **database freshness**: every source set inside the boundary is
classified freshly restored or unavailable, and cumulative publication may
additionally mark unavailable sets whose prior records were preserved as stale.
An unavailable database prevents a full-restoration claim but does not abort
export, publication or synchronization. Replica mutation accepts
`partialDatabaseCoverage` only when the complete inventory is accounted for.

`coverage.json` format 4 holds the complete schema ledger in `allTables`, each
table fingerprinted by SHA-256 over its ordered `table_xinfo` evidence and
related table, index and trigger definitions, with a top-level profile
fingerprint over the ordered logical identities. **No SQL is emitted.**
Content-row changes leave the profile stable; a column, constraint, index,
trigger or table-set change produces explicit drift. Incremental merges
recompute the profile from the merged ledger, and legacy archives without table
fingerprints keep a *missing* profile rather than receiving guessed evidence.

### Space

Before archive creation, record planning produces a fail-fast budget: selected
source bytes, record counts, estimated archive, staging and peak bytes, current
free bytes, and required free bytes including an operating reserve.
Progress-event format 3 carries that budget throughout, plus measured
compressed and uncompressed spool bytes, on-disk staging bytes and published
archive bytes.

Canonical output stays ordinary NDJSON; only the private ephemeral ordering
spool uses per-record Zstandard compression. The spool is removed on completion
or propagated failure **without deleting partial archive evidence**. The final
report retains aggregate storage evidence and an exact byte count, and the
audit rejects an inconsistent estimate equation, missing spool measurements, a
retained `.staging-*` directory, an unsafe archive file, or a byte-count
mismatch.

## Media paths

GreenBubbles records the verified location of an artefact that exists. **A
database reference is not proof that a file exists.** Paths are redacted in
default reports and revealed only through an explicitly authorized local API.
Symlinks escaping an authorized account root are rejected.

Encrypted `.dat` images and encoded voice data are separate from database
decryption: the resolver retains the encrypted source, identifies the decoder
version and key provenance, writes any decoded derivative into connector-owned
storage, hashes both, and never modifies the original.

Candidate files are opened read-only with symlink following disabled, bound to
the account root the snapshot recorded, and fingerprinted before and after
reading. A disappearing, changing, ambiguous or escaping path is never silently
substituted.

## Entities

The archive holds account-scoped `conversations.ndjson` and
`participants.ndjson` alongside messages. Session, contact and group rows are
retained as raw SQLite values; group ownership, membership and per-group
display names are normalized when the local protobuf is present. A missing
contact row becomes a participant with `missingLocalRecord`; an unparseable
group-member payload is an entity coverage gap.

## Reading an archive is not a grant

The lossless archive is not an implicit read permission. A separate mode-`0600`
policy lists the opaque conversation IDs that may be queried and caps page
size, bound to the account so it survives periodic resnapshots. Every cursor
includes the source fingerprint, conversation ID and last emitted ordinal;
cross-archive and cross-conversation reuse fails closed, and a duplicate
canonical identity encountered during paging aborts the read.

Archive reconciliation compares canonical identities and canonicalized record
digests under that same policy, emitting deterministic body-free events for
additions, changes and removals. Repeating the comparison produces the same
event IDs — which is what makes it the recovery path for a missed or duplicated
wake-up hint.

## Bootstrap, incremental, integrity scan

Acquisition mode and archive scope are retained in `report.json`. Bootstrap and
integrity-scan inputs are authoritative full source inventories.

An **incremental input is a fragment**, even though its manifest fingerprints
the complete source tree. Its `fullRestorationAchieved` is forced false and it
cannot directly replace or reconcile a replica. `merge-incremental` binds it to
the prior replica-eligible fingerprint, replaces records only for source sets
that restored successfully, preserves prior records for selected but
unavailable sets, and recalculates global integrity before producing a new
`authoritative` or `partialDatabaseCoverage` archive.

That distinction is the whole point: it stops partial source selection or a
transient database failure from weakening the row equation or silently deleting
history. Cached records follow the same source-set replacement rule — untouched
SNS sets retained, selected sets replaced, deleted sets removed, and a partial
cached-file triplet aborts the merge.

## The offline pipeline

`restore-publish` is the fail-closed boundary between an acquired immutable
snapshot and the replica follower. It does not discover or open live WeChat
stores, acquire a passphrase, read process memory, invoke WeChat, or open the
encrypted replica.

### Bootstrap

```sh
greenbubbles restore-publish \
  <bootstrap-snapshot> <new-publication-archive> <private-handoff.json> \
  --account-root <authorized-account-root> --passphrase-stdin
```

Requires acquisition-aware format-3 or account-bound format-4 evidence and a
signed official WeChat 4.1-or-later client. Format-4 snapshots already carry
the exporter-derived account holder; an optional account root is independently
checked against that binding and stays useful for local media resolution.

The command prepares the snapshot, restores it, independently audits every
ledger and recorded artefact, and only then publishes as generation 1 under the
handoff lock. An existing handoff is rejected, and the output archive must not
already exist.

### Incremental and integrity scan

Every non-bootstrap acquisition must supply **both** retained sides of its
baseline:

```sh
greenbubbles restore-publish \
  <next-snapshot> <new-publication-archive> <private-handoff.json> \
  --previous-snapshot <previous-snapshot> \
  --previous-archive <previous-publication-archive> \
  --account-root <authorized-account-root> --passphrase-stdin
```

Before decrypting anything, the operator independently verifies the complete
acquisition transition, signed 4.1+ compatibility at both endpoints (reporting
exact fingerprint changes), the changed/reconciliation/deleted source-set
classifications, the previous archive, and its baseline fingerprint. Format-4
transitions additionally require identical integrity-bound account evidence at
both endpoints.

The snapshot is then restored into an owner-only temporary fragment,
independently audited, merged by source identity into a new atomic publication
archive, and audited again. If a changed database is unavailable, the merge
retains that set's prior records, marks them stale, and publishes
`partialDatabaseCoverage` rather than aborting or treating them as deletions.

**Operators do not choose a generation.** It is derived and incremented while
holding the handoff lock, and the same compare-and-swap verifies that the
supplied previous archive is still the exact current sealed handoff — so two
restorers working from one baseline cannot publish a stale branch. Failed
validation never changes the handoff, and incremental staging is removed
automatically. A failure *after* an output directory exists can leave an
unpublished partial output at that explicitly new path: inspect or remove that
directory before retrying with a new output path.

### What the result says, and does not

The result contains the acquisition mode, verification verdicts, generation,
media and completion state, and aggregate coverage counts — no account IDs, no
source fingerprints, no local paths, no table names, no content. The handoff
stays owner-private because it necessarily contains the archive path and source
fingerprint. Replica application and its distinct key stay isolated in
`replica-follow` — see [REPLICA_OPERATIONS.md](REPLICA_OPERATIONS.md).

Monotonic durations separately cover input validation, catalog preparation and
decryption, restoration and merge, final audit and publication, and the whole
command, enabling later latency evidence without disclosing absolute activity
times.

This workflow supplies deterministic sequencing and synthetic fault coverage.
**It does not satisfy the real disposable-account corpus, semantic and media
coverage, or 60-second p95 evidence gates** — see
[MEASUREMENTS.md](MEASUREMENTS.md).

## The audit is not optional

`audit-archive` does not accept the writer's report on trust. It streams every
ledger again, reproduces row, type, gap, reference, entity and cached-surface
counts, validates the table and schema-profile ledgers, checks ordering,
account-bound directions and bidirectional relationships, regenerates every
nested-XML projection from its retained source XML, and verifies every recorded
file from a read-only no-follow descriptor.

Row accounting is enforced independently for each `(source set, message table)`
identity, so a globally balanced total cannot conceal a missing row in one
table and an extra row in another, and one source row cannot appear in both the
restored and rejected ledgers. Every restored message must point back to its
exact covered source table — source set, logical path, table identity and name,
and the complete raw column-name set. Relationship completion additionally
requires that nothing remain `pending`: every unresolved relationship must be
accounted for as target-not-present-locally, missing-identifier or ambiguous,
and only the first of those is compatible with a complete restoration of the
locally available archive.

Full details, including what the audit deliberately cannot prove, are in
[AUDITING.md](AUDITING.md).
