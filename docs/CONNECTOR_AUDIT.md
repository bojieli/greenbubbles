# Connector audit journal verification

The replica-backed connector writes body-free audit events to an owner-only
NDJSON journal. Event format 2 links every new event to its predecessor and
hashes the complete canonical event, including the predecessor digest.

The service verifies the complete journal under a shared file lock before it
starts. Every append takes an exclusive lock, validates the existing tail,
binds the new event to that tail, writes one record, and synchronizes it before
unlocking. Files that are symlinks, multiply linked, non-regular, or accessible
to group/other users fail closed.

An independent content-free audit is available:

```sh
cargo run --locked \
  --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles -- \
  audit-connector-log <owner-only-connector-audit.ndjson>
```

The report contains aggregate event/stage/outcome counts and chain verdicts.
It emits no account, requester, request, conversation, draft, policy, path, or
event identity and no message/search/draft body.

## Legacy boundary

Format-1 events created before chaining remain structurally validated but were
not linked to one another. When the first format-2 event is appended, it anchors
the exact bytes of the final legacy record. The verifier reports all earlier
events as `legacyUnchainedEventCount` and sets `fullyChained` false. A legacy
record after the chained suffix, a bad event digest, a broken predecessor, a
duplicate event ID, mixed account IDs, malformed identifiers, or an unknown
format is rejected.

## Security meaning

The chain detects accidental modification, truncation followed by an unrelated
append, reordered/inserted records, and ordinary post-hoc editing that does not
rebuild the suffix. It is not a digital signature and cannot defeat a malicious
owner who can rewrite the entire journal and recompute every unkeyed hash.
Future action accountability may require an independently protected signing or
anchoring mechanism after its threat model and adapter boundary are approved.

The current serving process can produce only request, draft-requested, and
draft-reviewed stages. The schema names future approval, attempt, and
reconciliation stages so the verifier can count them, but no operation can
produce those stages while Phase 4 is gated.

## Full connector-state audit

The key-gated state audit additionally opens the encrypted replica and current
policy and verifies the dedicated connector draft directory:

```sh
cargo run --locked \
  --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles -- \
  audit-connector-state <replica> <policy> <audit-log> <draft-directory> \
  --replica-key-stdin
```

Every directory entry must be a single-link, mode-`0600`, bounded JSON draft
whose filename equals its recomputed immutable identity. Unknown JSON fields,
duplicate attachments/participants, missing attachment sizes or digests,
invalid reply evidence, inconsistent recipient evidence, excessive expiry,
and any body or binding mutation fail closed.

Drafts are opened with no-follow descriptors and checked before and after each
bounded read. The verifier fingerprints both the journal and the complete draft
directory again before returning, so a concurrent creation, replacement, or
audit append cannot produce a mixed-state success report; the operator retries
after the connector becomes quiescent.

The audit then distinguishes structurally valid drafts from drafts that are
expired or stale under the current policy, connector/API version, or replica
checkpoint. Every draft must have exactly one matching completed
`draftRequested` journal event, and every completed `draftReviewed` event must
resolve back to the same draft, conversation, and policy decision. The command
rejects any approval, attempt, or reconciliation stage while Phase 4 remains
closed. Its output contains counts and booleans only; it emits no draft text,
recipient, account, conversation, requester, path, or stable identity.

## Send-adapter action stages

The `approvalRecorded`, `attemptRecorded`, and `reconciliationRecorded` stages
are written by the send adapter (`SEND_ADAPTER.md`), never by a connector
operation. Each event names the immutable draft and its policy decision, uses
one of the adapter's own operation names (`executeSend`, `executeSendDryRun`,
`reconcileSend`), and carries no message body: only counts, digests, and the
outcome.

One attempt produces exactly three events, in order and before the actions they
describe can have had an effect:

1. `approvalRecorded` — the PRECHECK decision. A denial ends here, and no
   effector call is ever made.
2. `attemptRecorded` — appended *before* the capability is dispatched, so an
   interrupted process still leaves a record that a dispatch was about to
   happen.
3. `reconciliationRecorded` — the settled outcome, and again later when a
   parked attempt is resolved against the replica.

`audit-connector-state` validates this ordering per draft: an attempt without a
completed approval, or a reconciliation without an attempt, is an integrity
failure.
