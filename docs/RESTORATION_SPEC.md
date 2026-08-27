# Conversation restoration specification

GreenBubbles restoration is lossless with respect to the authorized local
snapshot. A renderer may provide a simplified view, but the canonical restored
record must retain enough information to reproduce and audit every source row.

## Completeness contract

A restoration is complete only when all of the following are true:

1. Every message-bearing table in every supported message shard has been
   enumerated, including ordinary, group, business/public-account, and chatbot
   stores.
2. Every source row is represented exactly once in the canonical output, or is
   listed in a machine-readable rejection ledger with the database, table,
   stable row identity, and reason. Silent drops are forbidden.
3. Ordering uses the strongest source ordering available (`sort_seq`, server
   sequence, creation time, local ID, and source-row tie breakers) and remains
   deterministic when timestamps collide.
4. The original type value, low-16-bit logical type, compression markers,
   source fields, packed metadata, and undecoded payload bytes are retained.
5. Known message types receive a typed interpretation. Unknown types remain
   lossless opaque records and count as coverage gaps until documented.
6. Sender, conversation, quote/reply, recall/edit, reaction, and attachment
   relationships are resolved when the corresponding local source exists.
7. Each downloaded image, animation, audio item, video, document, thumbnail, or
   auxiliary artifact is linked to a canonical local artifact record containing
   an opaque path reference, size, digest, detected format, and verification
   state.
8. A remote-only, expired, deleted, corrupt, or not-downloaded attachment is
   represented explicitly. It is never reported as restored media.
9. Restoration never alters the source database or media tree and never places
   account secrets, plaintext histories, or stable identifiers in Git or normal
   logs.

`restore --defer-media` is the explicit text-first mode. It still emits a
reference for every media-bearing message, but that reference is labeled as
deferred and carries no guessed path or digest. The report sets
`mediaPhase: deferred` and cannot claim full restoration. Re-running from the
identical immutable snapshot without the flag produces a `resolved` archive
with the same source fingerprint and verified artifact paths/digests; the
encrypted replica treats that as a new restoration revision.

## Canonical message envelope

Every message includes these source-preserving fields even if its typed decoder
also exposes friendlier data:

```text
account / conversation / sender opaque IDs
source database-set ID / table ID / row identity
local ID / server ID / sort sequence / timestamp
raw type / logical type / direction / status
content bytes / packed-info bytes / compression metadata
typed payload or explicit unknown-payload reason
zero or more relationship and artifact references
```

Raw payloads are held inside the local trust boundary. AI-facing tools receive
only policy-approved normalized fields, never the lossless archive by default.

## Passive cached surfaces

When a supported `sns/sns.db` is present, restoration also writes the
owner-only triplet `cached-moments.ndjson`,
`cached-moment-interactions.ndjson`, and `cached-surfaces.json`. Only the exact
`SnsTimeLine` and `SnsMessage_tmp3` table/column signatures observed on the
pinned client are normalized. Every other SNS table remains a schema-coverage
record and is never guessed into a Moment or interaction.

Cached records retain database/table/row provenance, opaque canonical
identities, raw SQLite columns, original XML/blob bytes, best-effort typed
fields, semantic decode state, and snapshot observation time. Their
completeness is always `partialLocalCache`: absence is not evidence that a
server-side Moment, like, or comment does not exist. This passive path does not
load more content, contact WeChat servers, or imply authenticated active-read
capability.

## Integrity report

Each run produces counts that can be checked without printing message content:

- every discovered table and column set, classified as a supported message
  table, known auxiliary table, other table, or unhandled message candidate;
- source tables and rows discovered;
- rows restored, rejected, duplicated, and unknown by logical message type;
- relationship references resolved and unresolved;
- attachment references resolved, missing, remote-only, and corrupt;
- source and output fingerprints;
- decoder and supported-client versions.

`report.json` also carries signed-client compatibility evidence and a
component-by-component completion verdict. For current format-2 snapshots, the
top-level `fullRestorationAchieved` flag is true only when the client exactly
matches the pinned supported profile and row accounting, canonical identity
uniqueness, semantic decoding, directions, entities, relationships, artifact
verification, and artifact decoding all pass. Retaining raw bytes is necessary
for losslessness but does not by itself satisfy production compatibility,
semantic completeness, or playable-media completeness.

`coverage.json` format 3 contains the complete schema ledger in `allTables`.
Each table carries a SHA-256 fingerprint derived from its ordered
`table_xinfo` evidence and related table/index/trigger definitions. The
top-level schema-profile fingerprint hashes the ordered logical database/table
identities and those table fingerprints. SQL definitions are not emitted.
Content-row changes therefore leave the schema profile stable, while a column,
constraint, index, trigger, or table-set change produces explicit drift.
Incremental merges recompute the profile from the merged authoritative ledger;
legacy archives without table fingerprints retain a missing profile rather
than receiving guessed evidence.

Any message-like name or column signature that does not match a supported safe
adapter is labeled `unhandledMessageCandidate`, increments
`messageCandidateGapCount`, and keeps semantic completion false. This makes a
new or version-drifted message shard fail closed instead of disappearing from
row accounting.

The completion invariant is:

```text
source rows = restored rows + rejected rows
restored rows = uniquely identified canonical messages
```

For a production-complete decoder, rejected rows must be zero. Unknown typed
payloads may be retained losslessly during development but prevent a claim of
full semantic restoration until each observed type is understood.

## Independent post-restoration audit

`audit-archive` does not accept the writer's completion report on trust. It
streams every canonical ledger again, reproduces row/type/gap/reference/entity
and cached-surface counts, validates the table and schema-profile ledgers,
checks ordering and bidirectional relationships, and verifies every recorded
source or derivative file from a read-only no-follow descriptor. External media
must still match device, inode, size, modification time, and SHA-256; connector
derivatives must remain owner-only beneath the archive and match size and
SHA-256. The successful result exposes aggregate evidence only.

This audit is required before a real archive can serve as Phase 1 completion
evidence or enter a replica. Its `fullRestorationVerified` field remains false
unless the archive is authoritative, media-resolved, pinned-build compatible,
and already satisfies every strict completion component. See
`docs/ARCHIVE_AUDIT.md`.

## Media path semantics

GreenBubbles records the verified location of the existing downloaded artifact;
it does not claim that a database reference proves a file exists. Paths are
redacted in default reports and can be revealed only by an explicitly
authorized local API. Symlinks escaping an authorized account root are rejected.

Encrypted `.dat` images and encoded voice data are separate from database
decryption. The resolver must retain the encrypted source, identify the decoder
version/key provenance, write any decoded derivative into connector-owned
storage, hash both, and avoid modifying the original.

The exact source path appears only in the explicitly authorized, owner-only
local restoration output. Candidate files are opened read-only with symlink
following disabled, bound to the account root recorded by the snapshot, and
fingerprinted before and after reading. A disappearing, changing, ambiguous,
or escaping path is never silently substituted.

## Entity reconstruction

The archive contains account-scoped `conversations.ndjson` and
`participants.ndjson` alongside messages. Session, contact, and group rows are
retained as raw SQLite values. Group ownership, membership, and per-group
display names are normalized when their local protobuf is present. A missing
contact row is represented as a participant with `missingLocalRecord`; an
unparseable group-member payload is an entity coverage gap.

## Cursor reads and scope

The lossless archive is not an implicit read grant. A separate mode-`0600`
policy lists the opaque conversation IDs that may be queried and caps page size.
The policy is bound to the account so it can survive periodic resnapshots.
Every cursor includes the source fingerprint, conversation ID, and last emitted
conversation ordinal. Cross-archive and cross-conversation cursor reuse fails
closed. Duplicate canonical identities encountered during paging abort the
read.

Archive reconciliation compares canonical identities and canonicalized record
digests under that same policy. It emits deterministic body-free events for
additions, changes, and removals; repeating the same comparison produces the
same event IDs. This is the recovery path for missed or duplicated wake-up
hints.

Snapshot acquisition mode and archive scope are retained in `report.json`.
Bootstrap and integrity-scan inputs are authoritative full source inventories.
An incremental input is a fragment even though its manifest fingerprints the
complete source tree, so its `fullRestorationAchieved` flag is forced false and
it cannot directly replace or reconcile a replica. `merge-incremental` binds it
to the prior authoritative fingerprint, replaces records by source-set
identity, and recalculates global integrity before producing a new
`authoritative` archive. This distinction prevents partial source selection
from weakening the row equation or silently deleting history.

Cached records and their table coverage follow the same source-set replacement
rule. Untouched SNS sets are retained, selected sets are replaced, deleted sets
are removed, and a partial cached-file triplet aborts the merge.
