# Offline action-safety contract

GreenBubbles contains a pure, non-executing contract for evaluating the safety
evidence that a future ordinary-contact adapter would have to supply. It exists
so fail-closed invariants can be reviewed and tested before an adapter is
selected. It does not open Phase 4 and does not satisfy any live-action gate.

The contract has no connector operation, CLI/Unix action tool, client or network call,
approval issuer, secret, outbox, or function that reserves or attempts an
action. Current capability responses continue to report text, reply, and file
sending as unavailable.

## Bound evidence

An evaluated intent binds all of the following:

- the dated Phase 0.5/restoration gate-decision digest;
- the independently reviewed text, reply, or file capability;
- the immutable draft identity;
- exact account and allow-listed disposable conversation;
- selected adapter identity and version;
- exact pinned client-build profile; and
- a one-use idempotency key.

External approval evidence must repeat the SHA-256 binding of those fields,
name a local approver, and carry a bounded validity interval. The pure checker
also requires authoritative inputs showing that the approval ID has not been
consumed, the idempotency key has not been reserved, the global kill switch is
off, and the configured attempt window has capacity. Any mismatch produces
one or more machine-readable denials.

This approval structure is only a validation contract. GreenBubbles does not
currently mint, authenticate, persist, or consume approval evidence. Those
operations require a selected adapter boundary, durable transactional storage,
and the Phase 0.5 decision.

## Lifecycle semantics

The modeled sequence is deliberately narrower than delivery semantics:

```text
drafted -> approved -> attempted -> observedSent
                                |-> observedFailed
                                |-> unknown -> observedSent | observedFailed
```

An adapter acknowledgement cannot create `observedSent`; that state must come
from later official-client/local reconciliation. There is no `delivered` state.
All events in a valid sequence retain the same action, draft, approval, and
idempotency identities and have strictly increasing observation times.

## What remains gated

The contract is necessary test scaffolding, not operational proof. Phase 4
still requires:

- a qualified mechanism/legal/account-safety decision for an exact build;
- an allow-listed disposable account and test peer;
- an adapter-owned atomic reservation transaction covering approval,
  idempotency, rate state, and outbox state, including concurrency and restart;
- proof that every denial occurs before any client/network invocation; and
- redacted live reconciliation evidence for sent, failed, and ambiguous
  outcomes.

Until that evidence exists, approval, attempt, reconciliation, and send
operations remain absent from the serving process.

The current body-free connector audit journal provides the chained,
independently verifiable request/draft/review substrate described in
`CONNECTOR_AUDIT.md`. It deliberately does not create the future stages, and
its unkeyed hashes are not a substitute for any independently protected action
attestation required by the eventual threat model.

`audit-connector-state` independently proves that the immutable draft inputs to
this future contract still match their files, current policy/checkpoint, and
chained request/review history. It treats any already-present gated lifecycle
stage as an integrity failure; therefore it cannot be used to introduce an
approval or attempt indirectly.
