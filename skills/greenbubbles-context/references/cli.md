# GreenBubbles AI CLI

Run any command below with `--help` or `-h` to inspect its exact invocation;
help exits without opening private inputs or reading a key.

## Live query

`ai-query` performs one policy-scoped read directly against the encrypted local
replica. It does not require a daemon or MCP server:

```text
greenbubbles-restore ai-query \
  <replica.db> <policy.json> <audit.ndjson> <request.json> \
  --replica-key-stdin
```

The request must be an owner-only regular file and uses this envelope:

```json
{
  "formatVersion": 1,
  "requestId": "agent-unique-request-id",
  "requesterId": "local-agent",
  "destination": "local",
  "operation": {
    "kind": "searchMessages",
    "query": "requested document",
    "conversationId": null,
    "cursor": null,
    "limit": 20
  }
}
```

Create the file in an owner-only directory with mode `0600`. Supply the
operator-provided replica key through standard input, not an argument or the
request. Do not search all conversations when a known conversation ID can
narrow the request.

Read-only operation kinds are:

- `capabilities`, `status`, `coverage`, and `getChanges`;
- `listConversations`, `resolveConversation`, and `resolveContact`;
- `searchMessages`, `getMessages`, and `getMessage`;
- `getArtifact` for a locally authorized, digest-verified attachment;
- `getCachedMoments` when its independent policy scope is enabled.

For pagination, repeat the exact operation with the returned `nextCursor`.
Message cursors bind the query and replica checkpoint. If synchronization
invalidates a cursor, restart that query rather than combining pages from
different checkpoints.

The response always includes a `context` object alongside `result` or `error`.
Check `sourceCoverageComplete`, database counts, `limitationCodes`, and
`coverageNote` before answering. A failed request has `ok: false`; do not infer
an empty result from an authorization, integrity, or availability error.
For an account-bound replica, `context.selfParticipantId` is the opaque
account-holder identity. `resolveContact` and `resolveConversation` label that
participant `You`. When both fields are authorized, a message is outgoing only
when `senderId == selfParticipantId`; do not infer self from a name, direct-chat
shape, message frequency, or group ownership. A missing sender or direction
field may simply be policy-redacted.

## Static context bundle

`ai-export` writes one checkpoint-consistent, policy-scoped generation:

```text
greenbubbles-restore ai-export \
  <replica.db> <policy.json> <audit.ndjson> <new-output-directory> \
  --replica-key-stdin --requester local-agent
```

The output path must not exist and its parent must be owner-only. Export uses a
private staging directory and publishes by one rename. If the replica changes
during a long export, the command discards the staging generation and asks the
caller to retry. Progress is written to stderr; `--progress-json` or an
owner-only `--progress-file <events.ndjson>` provides machine-readable record
counts and percentages.

New bundles use `formatVersion: 2` and schema
`greenbubbles.ai-context.v2`. The auditor and native history browser can still
read existing version-1 bundles, but version 1 has no verified
`selfParticipantId`, so its recorded direction remains legacy evidence.

The bundle contains:

- `manifest.json`: bundle/checkpoint identity, policy digest, file hashes and
  counts, source freshness, unavailable/preserved-stale database counts, and
  limitations;
- `conversations.jsonl`: human labels, kinds, participant roles, optional
  `groupOwnerParticipantId`, enabled operations/fields, and authorized time
  windows. Group ownership is not account-holder evidence;
- `contacts.jsonl`: normalized display names, local-profile availability,
  source-database freshness, and per-conversation names/roles;
- `messages.jsonl`: ordered normalized content summaries, sender display names,
  directions, types, source-database freshness, relationship targets, and
  attachment references;
- `artifacts.jsonl`: digest-verified attachment metadata with absolute paths
  removed. Use a local `getArtifact` query only when the actual verified file is
  needed.

In version 2, every representation of the bound account holder—including
conversation participants, contacts, per-conversation contact profiles, and
self-authored message sender labels—uses `You`. Preserve opaque IDs even when
displaying that friendly label.

Every JSONL line is an independent JSON object. Verify each file against the
SHA-256 and record count in `manifest.json` before bulk ingestion. Prefer the
aggregate-only `greenbubbles-restore audit-ai-context <bundle-directory>`
command, which also validates schemas, permissions, identities, references,
and freshness without printing content. Preserve the opaque IDs and checkpoint
identity in downstream indexes so later change events can update or invalidate
the correct records.

The default destination is `local`. A `remoteModel` export requires explicit
remote permission in every included scope and cannot release artifact paths.
Do not change the destination merely to work around a denial.
