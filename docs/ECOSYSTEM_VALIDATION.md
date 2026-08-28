# Ecosystem validation evidence

Evidence recorded on 2026-08-27 and extended on 2026-08-28 uses only synthetic
connector data and tool metadata.

## Primary CLI skill workflow

The `greenbubbles-context` repository skill routes an agent to the one-shot
`ai-query` command and checkpoint-consistent `ai-export` bundle. Integration
tests prove policy-scoped search without a daemon, normalized static
conversations/contacts/messages/artifacts, source coverage on every response,
per-record fresh versus preserved-stale database state, owner-only and atomic
outputs, digest/count manifests, absolute-path/raw-field suppression, read-only
operation enforcement, and monotonic progress completion. This is the primary
agent integration for new development.

## Historical MCP compatibility evidence

Before the CLI/skill decision, the integration suite launched the real
owner-only Unix connector service and
the real `connector-mcp` stdio process. A host probe performs MCP `initialize`,
`tools/list`, and `tools/call`, verifies the GreenBubbles tool list, and calls
`greenbubbles_status` through the socket.

Claude Code 2.1.247 was also tested as an existing MCP host. A temporary
project-local stdio registration pointing at `connector-mcp` reported
`Status: Connected`; the registration was removed immediately after the check.
No repository or persistent GreenBubbles configuration was created by that
validation.

Codex CLI 0.150.1 accepted the same server as an enabled transient stdio MCP
configuration. A model-driven discovery turn was not counted as successful
because that separately installed CLI had an invalid API credential. This does
not affect the in-repository initialize/list/call proof or the successful
Claude host connection.

## Downstream change consumer

The runnable change consumer and deterministic Markdown projection are tested
end to end as described in `DOWNSTREAM_CONSUMER.md`. The proof covers bootstrap,
idle resume, durable cursor advancement through body-free changes, minimized
record refresh, owner-only outputs, and replacement-replica fail closure.

The compatibility adapter remains isolated for existing users and is not a
dependency of the skill, CLI, static bundle, or downstream change consumer.

These results validate the implemented GreenBubbles interfaces. They do not
prove interoperability with every agent host, real-client acquisition latency,
or any Phase 4 action mechanism.
