# MCP adapter

The MCP adapter is a thin stdio-to-Unix-socket process. It never opens the
encrypted replica and never receives either encryption key.

Start the connector service first, then configure an MCP-capable local agent
host to launch:

```text
greenbubbles-restore connector-mcp private/connector.sock \
  --requester my-local-agent --destination local
```

The adapter implements MCP `initialize`, `ping`, `tools/list`, and `tools/call`
over newline-delimited JSON-RPC stdio. Its typed tools cover capabilities,
status, coverage, scoped changes, conversation/message retrieval, contact and
conversation resolution, immutable draft creation, and preview.

Use `--destination remote` only when the agent host will release tool results
to a remote model. That fixed choice makes each server request use the policy's
remote-model grants; conversations without an explicit remote grant fail
closed. Retrieved WeChat text cannot change the adapter's destination or add a
tool. Draft and preview operations remain local-only even if an adapter is
started with the remote destination.

The MCP adapter intentionally exposes no send, internal-call, raw-SQL,
passphrase, key, source-path, or session-credential tool. Its tool descriptions
also state that drafts never execute. An MCP host can therefore use the same
connector contract as scripts without becoming a new privilege boundary.

