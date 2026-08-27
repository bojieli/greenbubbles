# Local AI tool boundary

The original tool kernel is a deterministic authorization boundary over a
private restoration archive. The replica-backed versioned service now uses the
same policy principles through the owner-only Unix socket and MCP surfaces
documented in `CONNECTOR_API.md` and `MCP.md`.

## Policy dimensions

Tool policy format 2 binds authorization to one opaque account ID and grants
each conversation an independent set of:

- operations: list, read recent messages, exact-text search, and create draft;
- message fields: sender, creation time, direction, type, normalized content,
  attachment references, and relationship references;
- an optional inclusive Unix-time range;
- local-model access, with remote-model release disabled unless explicitly
  enabled for that conversation.

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

The current draft record binds the body to an account and conversation and is
immutable by connector convention (`create_new`). Phase 3B still must add
recipient display evidence, reply targets, attachment digests, connector
version, expiry, policy-decision identity, and a separate preview operation
before the plan's draft gate can be considered complete.
