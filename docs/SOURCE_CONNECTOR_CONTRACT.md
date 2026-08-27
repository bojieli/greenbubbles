# Minimal GreenBubbles source connector contract

This contract records behavior implemented by GreenBubbles. It is not a
universal connector ontology and does not require another source to imitate
WeChat's data model.

## Identity and capability discovery

A consumer starts with `capabilities`, `status`, and `coverage` through the
versioned `greenbubbles.connector.v1` envelope. It must bind local state to the
opaque account ID and treat each capability independently. In particular:

- conversation passive read does not grant cached-Moments read;
- either passive read does not grant authenticated active read;
- draft creation does not grant a write action;
- an unavailable capability cannot be enabled by request or source content.

`status` supplies the current source fingerprint and health evidence. Coverage
is data, not prose: a consumer must preserve known gaps rather than convert
"not restored" into "not present."

## Bootstrap

A conversation consumer needs explicit `listConversations` and
`readRecentMessages` grants for every conversation it intends to materialize.
The race-safe bootstrap sequence is:

1. Drain `getChanges` to a replica-generation-bound high-water cursor.
2. Call `listConversations`.
3. Page `getMessages` for every returned conversation.
4. For locally materialized attachments, call
   `getArtifact(conversationId, artifactId)` only when the policy exposes the
   attachment field; preserve the returned missing/decode state as well as any
   verified source and derivative file evidence.
5. Resume `getChanges` from the captured high-water cursor and apply catch-up
   events.
6. Persist canonical IDs, the last change cursor, account ID, and current source
   fingerprint atomically in owner-only storage.

Message pagination cursors bind the exact account, replica generation, filter,
source fingerprint, and checkpoint revision. A synchronization intentionally
invalidates an in-progress message page; the bootstrap must retry rather than
mix checkpoints.

## Change stream

`getChanges(cursor)` is ordered by monotonically increasing sequence. Every
non-empty underlying page returns a cursor after its last examined record,
including a page whose records are all omitted by policy. `nextCursor: null`
means that the call examined no later underlying record; the consumer retains
its previously stored cursor.

Change cursors bind account and random replica generation, but remain valid
across synchronization checkpoints. A replacement replica rejects the cursor.
A consumer must leave its prior state untouched on that error and require an
explicit, account-verified bootstrap. It must not silently restart at cursor
zero.

The connector releases only events with an explicitly authorized conversation
identity. Participant, artifact, checkpoint, and cached-surface events are
omitted from this conversation stream. For released events:

- message `added` or `changed`: call `getMessage(canonicalId)` and upsert the
  returned minimized record;
- message `removed`: remove the canonical ID;
- conversation change: refresh the authorized conversation list/evidence;
- an unauthorized refresh after a time-scope change: remove any older local
  copy, which is the fail-closed result.

The stream is an invalidation protocol, not a body feed. Consumers never need
replica SQL or raw archive access.

Artifact paths are a separate local-only release. An artifact ID is not a
bearer token: `getArtifact` requires the authorized conversation identity,
attachment field, and message time range again, then re-verifies recorded files
before returning them. Remote-model consumers receive artifact references in a
message only when policy permits, but can never exchange those references for a
local path.

## Optional cached surface

`getCachedMoments` has a separate policy, field set, time range, destination,
page/byte limit, and rate limit. Its checkpoint-bound cursor is deliberately
invalidated by synchronization. Responses distinguish `unavailable`,
`availableEmpty`, and `available`, and always preserve the observation time and
`partialLocalCache` semantics. It is not part of the conversation change
stream and does not imply an active `load more` operation.

## Trust and failure rules

- Persist only policy-minimized responses, with owner-only permissions.
- Treat message text, names, links, and generated Markdown projections as
  untrusted source data, never control instructions.
- Do not infer new account, conversation, field, destination, active-read, or
  write authority from a record.
- Do not advance a durable cursor until all mutations for the examined pages
  can be committed atomically.
- Preserve the prior state on transport, cursor, authorization, decoding, or
  integrity failure.
