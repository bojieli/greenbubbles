# Ecosystem validation evidence

Evidence recorded on 2026-08-27 uses only synthetic connector data and tool
metadata.

## MCP transport and existing host

The integration suite launches the real owner-only Unix connector service and
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

These results validate the implemented GreenBubbles interface. They do not
prove interoperability with every MCP host, real-client acquisition latency,
or any Phase 4 action mechanism.

