# The AI tool boundary

What an AI caller can ask GreenBubbles for, what it can never reach, and how
each of those is enforced. The commands themselves are in
[AI_CONTEXT_CLI.md](AI_CONTEXT_CLI.md); the request and response contract is in
[CONNECTOR_API.md](CONNECTOR_API.md).

The boundary is a deterministic authorization layer, not a prompt. Nothing
about it depends on a model behaving well.

## Where the boundary sits

Two surfaces enforce the same policy principles: a one-shot CLI, and an
owner-only Unix socket. Both check every dimension of policy *before* reading a
message body.

Private inputs — policies, requests, audit logs, drafts — must be regular files
or directories belonging to the current effective user, with no symlinks, no
group or world permissions, and (for files) exactly one hard link. A
restrictive mode on an object owned by a *different* account is explicitly not
accepted as owner authorization.

## What a policy grants

Tool policy format 3 binds authorization to one opaque account ID. Within that
account, each conversation independently grants:

- **operations** — list, read recent messages, exact-text search, create draft;
- **fields** — sender, creation time, direction, type, normalized content,
  attachment references, relationship references;
- **a time range** — optional, inclusive, in Unix time;
- **a destination** — local model access, with remote release *disabled* unless
  explicitly enabled for that conversation.

A policy may also grant one independent passive cached-Moments scope, with its
own fields, its own observation-time range and its own destination decision. It
grants no conversation read, no active read and no write. Format-2 policies
remain readable and default this scope to absent.

Consequences worth stating plainly: a policy for one account is rejected for
another rather than reinterpreted. Search requires content permission — you
cannot search a conversation whose bodies you are not allowed to read. A
message with no source timestamp is excluded from a time-bounded scope rather
than being included by default.

## What comes back

The minimized view omits raw database columns, source paths, original content
bytes, packed metadata and raw XML. Only explicitly enabled fields are
serialized at all. Result counts and per-message summaries are bounded by
policy on top of the global response caps.

This view is not a replacement for the lossless archive, and is not meant to
be. It is what a model gets.

## Prompt injection

Message text is never parsed as a tool request or as policy. The caller selects
a typed operation; the connector checks that operation against policy before
returning anything or creating a draft. A message asking an agent to open
another conversation, enable a remote model, or send a reply remains inert
message content.

This holds structurally rather than by convention, and the structure is worth
spelling out:

- There is **no send capability, no approval operation, no private-client call
  and no network client** in this module or in the connector.
- Draft creation writes a new mode-`0600` record into an owner-only directory
  and cannot mutate WeChat state.
- Sending lives in a separate command (`greenbubbles send`) reachable only from
  a local shell, never as a tool.
- That path requires approval evidence a human produces with
  `send approve --confirm`; it runs in a different process from the one that
  parses message content; and the process that actually drives the client holds
  no key, no replica handle and no policy.

So an injected instruction fails at every layer independently: the agent cannot
reach the adapter, and the adapter would refuse a recipient the owner never
approved.

Draft and search text enter the CLI on standard input, so neither appears in
process arguments.

## The audit journal

Every completed request *and* every deterministic denial appends a mode-`0600`
JSONL event under an exclusive file lock. An event records the opaque account
and conversation, the caller-supplied requester ID, the operation, the
local/remote destination, the outcome, and result and byte counts. It records
no query, no message and no draft body. Symlinked, multiply-linked, or
group/world-accessible audit files are rejected.

Format-2 events additionally hash each canonical event and bind the
predecessor's digest, making the journal a hash chain. The full journal is
verified under a shared lock before the service starts; each append validates
the tail under an exclusive lock.

**This is tamper-evident hashing, not signed attestation.** It detects editing,
reordering, insertion, and removal when a retained successor still names the
missing record. Without an independently retained anchor it cannot detect a
clean truncation of the final suffix, and it cannot stop an owner who rewrites
the journal and recomputes every unkeyed hash. Verify one with
`audit-connector-log`, and see [AUDITING.md](AUDITING.md).

## Drafts

A draft binds its body to the account and conversation, human-readable
recipient evidence, an optional reply target, attachment digests, the connector
and API version, an expiry, the requester, the policy decision, and the
authoritative checkpoint. Drafts are immutable — created with `create_new` —
and have a separate preview operation.

No approval or execution operation is exposed over the connector. A draft
becomes sendable only when the owner approves it out of band, and any edit,
recipient change, attachment change, expiry or connector upgrade invalidates
that approval.

`audit-connector-state` verifies drafts against the replica, the current policy
and the completed request/review events. It is a local maintenance command, not
a connector operation an AI caller can invoke, and it creates nothing — so it
cannot be used to introduce an approval indirectly.

## The pure contract underneath

The Rust library contains the validation types documented in
[ACTION_SAFETY_CONTRACT.md](ACTION_SAFETY_CONTRACT.md), which enforce the gate,
adapter, approval-binding, idempotency, rate, kill-switch and lifecycle
invariants for the send adapter. They are deliberately unregistered as
connector or Unix operations: the contract can be reviewed and tested without
any adapter existing, and no tool call can reach it.
