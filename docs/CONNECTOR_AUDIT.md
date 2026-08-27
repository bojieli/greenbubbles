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
  --bin greenbubbles-restore -- \
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
