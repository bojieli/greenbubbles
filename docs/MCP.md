# MCP adapter

This is a retained compatibility adapter, not the primary GreenBubbles agent
interface and not a target for new development. New agent workflows should use
the repository `greenbubbles-context` skill with `ai-query` and `ai-export` as
documented in `AI_CONTEXT_CLI.md`; those commands require no MCP host or daemon.

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
conversation resolution, passive cached Moments, immutable draft creation, and
preview. `greenbubbles_get_cached_moments` maps to the same separately scoped,
minimized connector operation; listing the MCP tool does not mean the local
policy enables it.

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

Synthetic end-to-end and existing-host validation evidence is recorded in
`ECOSYSTEM_VALIDATION.md`. The test suite exercises initialize, list, and a
real status call through the Unix service; an installed Claude Code host was
also able to launch the adapter and report it connected.
