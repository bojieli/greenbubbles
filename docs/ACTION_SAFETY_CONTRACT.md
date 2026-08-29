# Action safety contract

Before GreenBubbles will let anything become visible to another person, an
intent has to survive a pure, deterministic checker. That checker exists
separately from any adapter so its invariants can be reviewed and tested on
their own — and it is now also the gate the shipped send adapter actually
passes through.

The checker (`action.rs`) has no connector operation, no Unix action tool, no
client or network call, and holds no secret. Connector capability responses
continue to report text, reply and file sending as **unavailable**, because
sending is deliberately not a connector operation: it is an owner-approved,
out-of-band command. See [SEND_ADAPTER.md](SEND_ADAPTER.md) and
[AI_TOOL_BOUNDARY.md](AI_TOOL_BOUNDARY.md).

## What an intent must bind

An evaluated intent carries all of:

- the dated gate-decision digest;
- the independently reviewed text, reply or file capability;
- the immutable draft identity;
- the exact account and an allow-listed disposable conversation;
- the selected adapter identity and version;
- the exact pinned client-build profile;
- a one-use idempotency key.

External approval evidence must repeat the SHA-256 binding of those fields,
name a local approver, and carry a bounded validity interval. On top of that,
the checker requires authoritative inputs proving that the approval ID has not
already been consumed, the idempotency key has not been reserved, the global
kill switch is off, and the attempt window still has capacity.

Any mismatch produces one or more machine-readable denials. There is no partial
pass.

## Where approval comes from

`greenbubbles send approve` is the only issuer. It is a local, explicit,
owner-run command: it prints the resolved recipient, the body length and the
body digest, and refuses to write anything without `--confirm`. It mints
nothing on behalf of an AI caller and is reachable from no connector operation.
The evidence it writes is an owner-only file whose validity interval is capped
at one hour.

Approval identities and idempotency keys are persisted and consumed by the
adapter's own outbox (`send_outbox.rs`), which is single-flight and durable
across restarts. The idempotency key is deterministic in the gate decision, the
draft and the approval — so retrying an approved draft reuses the key and is
refused. **A second attempt requires a second approval.**

## Lifecycle

The modelled sequence is deliberately narrower than delivery semantics:

```text
drafted → approved → attempted → observedSent
                              ├→ observedFailed
                              └→ unknown → observedSent | observedFailed
```

Two properties matter more than the diagram:

**An adapter acknowledgement cannot create `observedSent`.** That state comes
only from later reconciliation against the official client's own record. A
helper's own capture, however confident it looks, parks the attempt instead of
completing it.

**There is no `delivered` state at all.** GreenBubbles can observe that a
message exists in the client's data. It cannot observe that a human received
it, so it does not model a state that would imply so.

Every event in a valid sequence keeps the same action, draft, approval and
idempotency identities, with strictly increasing observation times.

## What is met, and what is not

The contract's prerequisites are met in code. The path still ships closed, and
opening it is an operator decision rather than a code change.

**Met:**

- an adapter-owned atomic reservation transaction covering approval,
  idempotency, rate state and outbox state, including concurrency and restart —
  proven by fault-injection tests;
- every denial occurring before any client invocation: the precheck runs to
  completion and the dispatcher is provably not called on a denial;
- reconciliation evidence for sent, failed and ambiguous outcomes, redacted to
  digests and match decisions.

**Outstanding:**

- a qualified mechanism, legal and account-safety decision for an exact client
  build. Four gate-evidence flags in the send configuration stand for exactly
  this, and the guard denies while any of them is false.
- a provisioned release signing key. Without one, no release calibration
  profile verifies and no rollout stage above `dryRun` can open.

Neither of those is an engineering task, which is why neither has a date. See
[ROADMAP.md](ROADMAP.md).

## The record

The body-free connector journal provides the chained substrate described in
[AUDITING.md](AUDITING.md), and the send adapter extends it with approval,
attempt and reconciliation stages. Its unkeyed hashes are tamper-evident, not
an independently protected attestation, and are not a substitute for one if the
threat model ever demands it.

`audit-connector-state` independently proves that immutable draft inputs still
match their files, the current policy and checkpoint, and the chained
request/review history. Since the adapter exists, an action stage in the
journal is no longer an integrity failure by itself — instead it is validated:
each must name a real draft, carry that draft's policy decision, use one of the
adapter's own operation names, and respect `approval → attempt →
reconciliation` ordering. An attempt without its approval, or a reconciliation
without its attempt, fails the audit.

The verifier creates nothing, so it cannot be used to introduce an approval or
an attempt indirectly.
