# Offline action-safety contract

GreenBubbles contains a pure contract for evaluating the safety evidence an
ordinary-contact adapter must supply. It exists so fail-closed invariants can
be reviewed and tested independently of any adapter, and it is now also the
gate the shipped send adapter actually passes through.

The pure checker (`action.rs`) still has no connector operation, no Unix action
tool, no client or network call, and no secret. What changed on 2026-08-29 is
that a *selected adapter boundary* now exists around it: the deterministic
UI-automation send adapter documented in `SEND_ADAPTER.md`. It supplies the
approval issuer, the durable outbox, and the reconciliation the contract always
described as prerequisites. Connector capability responses continue to report
text, reply, and file sending as unavailable, because sending is deliberately
**not** a connector operation — it is an owner-approved, out-of-band command.

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

The approval issuer is `greenbubbles-restore send approve`. It is a local,
explicit, owner-run command: it prints the resolved recipient, the body length,
and the body digest, and refuses to write anything without `--confirm`. It
mints nothing on behalf of an AI caller and is reachable from no connector
operation. The evidence it writes is an owner-only file whose validity interval
is bounded to at most an hour.

Approval identities and idempotency keys are persisted and consumed by the
adapter-owned outbox (`send_outbox.rs`), which is single-flight and durable
across restarts. The idempotency key is deterministic in the gate decision, the
draft, and the approval, so retrying an approved draft reuses the key and is
refused; a second attempt needs a second approval.

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

The contract's prerequisites are now met in code, but the send path still ships
closed and opening it is an operator decision, not a code change:

- **met** — an adapter-owned atomic reservation transaction covering approval,
  idempotency, rate state, and outbox state, including concurrency and restart
  (`send_outbox.rs`, proven by fault-injection tests);
- **met** — every denial occurs before any client invocation: PRECHECK runs to
  completion and the dispatcher is provably not called on any denial;
- **met** — reconciliation evidence for sent, failed, and ambiguous outcomes,
  redacted to digests and match decisions;
- **outstanding** — a qualified mechanism/legal/account-safety decision for an
  exact build. The four gate-evidence flags in the send configuration stand for
  exactly this, and the guard denies while any of them is false;
- **outstanding** — a provisioned release signing key. Without one, no release
  calibration profile verifies and no rollout stage above `dryRun` can open.

`observedSent` is created only by replica reconciliation. A helper's own
capture, however confident, parks the attempt instead of completing it.

The body-free connector audit journal provides the chained, independently
verifiable substrate described in `CONNECTOR_AUDIT.md`, and the send adapter
extends it with the approval, attempt, and reconciliation stages. Its unkeyed
hashes are tamper-evident, not an independently protected attestation, and are
still not a substitute for one if the threat model ever requires that.

`audit-connector-state` independently proves that the immutable draft inputs
still match their files, current policy/checkpoint, and chained request/review
history. Since the send adapter exists, it no longer treats an action stage as
an integrity failure by itself; instead it validates one. An approval, attempt,
or reconciliation event must name a real draft, carry that draft's policy
decision, use one of the adapter's own operation names, and respect the
ordering approval -> attempt -> reconciliation per draft. A journal that
records an attempt without its approval, or a reconciliation without its
attempt, fails the audit. `audit-connector-state` still creates nothing: it is
a verifier, so it cannot be used to introduce an approval or attempt
indirectly.
