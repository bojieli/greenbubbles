# Local AI tool boundary

The original tool kernel is a deterministic authorization boundary over a
private restoration archive. The replica-backed versioned service now uses the
same policy principles through the one-shot CLI and owner-only Unix socket
documented in `AI_CONTEXT_CLI.md` and `CONNECTOR_API.md`.

## Policy dimensions

Tool policy format 3 binds authorization to one opaque account ID and grants
each conversation an independent set of:

- operations: list, read recent messages, exact-text search, and create draft;
- message fields: sender, creation time, direction, type, normalized content,
  attachment references, and relationship references;
- an optional inclusive Unix-time range;
- local-model access, with remote-model release disabled unless explicitly
  enabled for that conversation.

It may additionally grant one independent passive cached-Moments scope. That
scope has its own fields, inclusive observation-content time range, and
local/remote destination decision; it grants no conversation read, active read,
or write capability. Format-2 policies remain readable and default this new
scope to absent.

The service checks these dimensions before reading message bodies. A policy for
one account cannot be reused for another. Search requires content permission.
A message without a source timestamp is excluded from a time-bounded scope.

The returned message view omits raw database columns, source paths, original
content bytes, packed metadata, and raw XML. Only explicitly enabled fields are
serialized. Result counts and per-message summaries are bounded by policy.
This minimized view does not replace the lossless archive.

## Prompt-injection boundary

Message text is never parsed as a tool request or policy. The caller selects a
typed operation, and the connector checks that operation against policy before
returning data or creating a draft. A message that asks an agent to access a
different conversation, enable a remote model, or send a reply remains inert
message content.

There is deliberately no send capability, approval operation, private-client
call, or network client in this module. Draft creation writes a new mode-`0600`
local record into an owner-only directory and cannot mutate WeChat state.
Draft text and search queries enter the CLI through standard input so they do
not appear in process arguments.

## Audit semantics

Every completed tool request and deterministic authorization denial appends a
mode-`0600` JSONL event under an exclusive file lock. Events include the opaque
account/conversation, caller-supplied requester ID, operation, local/remote
destination, outcome, result count, and byte counts. They omit queries,
messages, and draft bodies. Symlinks, multiply linked files, and group- or
world-accessible audit files are rejected.

Connector event format 2 additionally hashes each canonical event and binds the
predecessor digest. The full journal is checked under a shared lock before
service startup; append validates the tail under an exclusive lock. The
aggregate-only `audit-connector-log` command reports chain integrity and any
unlinked format-1 prefix. This is tamper-evident hashing, not an independently
signed attestation; see `CONNECTOR_AUDIT.md`.

The key-gated `audit-connector-state` command further verifies all immutable
draft files against the encrypted replica, current policy, and completed
request/review events. It reports stale and expired counts without releasing
draft or recipient data, and fails if a gated action stage appears. It is a
local maintenance command, not a Unix connector operation available to an AI caller.

Connector drafts bind the body to account/conversation and human-readable
recipient evidence, optional reply target, attachment digests, connector/API
version, expiry, requester, policy decision, and authoritative checkpoint.
They are immutable (`create_new`) and have a separate preview operation, but no
approval or execution operation exists before the Phase 4 gate.

The Rust library also contains the pure validation types documented in
`ACTION_SAFETY_CONTRACT.md`. They let tests exercise future gate, adapter,
approval-binding, idempotency, rate, kill-switch, and lifecycle invariants, but
they are not registered as connector, CLI, or Unix operations. They have no
approval issuer or action adapter and do not change this tool boundary.
