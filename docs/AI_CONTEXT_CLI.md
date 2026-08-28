# AI context CLI and static bundle

GreenBubbles' primary agent surface is the `greenbubbles-restore` command-line
program. It serves source-faithful WeChat context without requiring an MCP host,
a long-running connector process, direct SQL, or access to restoration secrets.
The repository skill in `skills/greenbubbles-context` gives an agent the concise
operating instructions for this surface.

## Trust and authorization boundary

`ai-query` and `ai-export` reuse the existing owner-created tool policy. The
policy binds one account and grants operations, normalized fields, inclusive
time ranges, and local or remote-model release independently per conversation.
Cached Moments remain an independent scope. Both commands use the encrypted
canonical replica rather than live WeChat files or private source schemas.

The replica key is consumed only from standard input. Request documents,
policies, audit logs, progress logs, and static bundles must live under
owner-controlled mode-`0700` directories; private files are mode `0600`. Search
terms and message bodies never appear in process arguments or the body-free
audit log.

`ai-query` accepts only read operations. It rejects drafts, previews, bootstrap,
synchronization, refresh, approval, send, or any other mutation. Message text is
returned as untrusted source content and cannot select another operation or
expand policy.

## One-shot query format

The request schema is `greenbubbles.ai-query.v1`:

```json
{
  "formatVersion": 1,
  "requestId": "unique-caller-request",
  "requesterId": "local-agent",
  "destination": "local",
  "operation": {
    "kind": "getMessages",
    "conversationId": "opaque-conversation-id",
    "cursor": null,
    "limit": 50
  }
}
```

Invoke it with:

```text
greenbubbles-restore ai-query \
  <replica.db> <policy.json> <audit.ndjson> <request.json> \
  --replica-key-stdin
```

Supported operations are `capabilities`, `status`, `coverage`, `getChanges`,
`getCachedMoments`, `listConversations`, `searchMessages`, `getMessages`,
`getMessage`, `getArtifact`, `resolveContact`, and `resolveConversation`. Their
operation bodies are the same stable tagged JSON union documented in
`CONNECTOR_API.md`.

Every response has `formatVersion`, schema, API version, request identity,
`ok`, a `context` object, and either `result` or `error`. The context object
contains the account/replica/checkpoint binding, client compatibility, archive
scope, total/fresh/unavailable/preserved-stale database counts, canonical
entity counts, gap counts, checkpoint age, `sourceCoverageComplete`, stable
`limitationCodes`, and a plain-language `coverageNote`.

The command checks the checkpoint before and after the request. A concurrent
sync produces an integrity error rather than returning content paired with
freshness metadata from another generation.

## Static bundle format

`ai-export` writes schema `greenbubbles.ai-context.v1`:

```text
greenbubbles-restore ai-export \
  <replica.db> <policy.json> <audit.ndjson> <new-output-directory> \
  --replica-key-stdin --requester <id> [--destination local|remote]
```

The output path must not exist. GreenBubbles writes a private sibling staging
directory, pages through every authorized message at a single checkpoint,
flushes and synchronizes each file, verifies the final checkpoint, and then
publishes with one rename. An error or concurrent sync removes the staging
directory and leaves no apparently complete output generation.

`manifest.json` contains:

- a deterministic bundle identity bound to replica, checkpoint, policy digest,
  destination, and policy source identity;
- creation time, requester, destination, and explicit `exportComplete` state;
- the complete context/freshness object returned by live queries;
- enabled conversation, contact, message, and attachment counts;
- attachment-resolution error count;
- relative file names, record counts, byte counts, and SHA-256 digests.

After copying or before indexing a generation, run the aggregate-only bundled
verifier:

```text
greenbubbles-restore audit-ai-context <context-bundle-directory>
```

It verifies the exact five-file inventory, owner-only files, manifest and
record schemas, file sizes/digests/counts, unique identities, conversation-
contact-message-artifact references, per-record freshness consistency, and the
bundle/checkpoint/policy identity. It emits only counts and boolean evidence;
it never prints labels, message text, contact names, paths, or identifiers.

The JSONL files are:

| File | AI-oriented content |
| --- | --- |
| `conversations.jsonl` | Stable conversation ID, human label, kind, participant names and roles, owner evidence, decode state, source-database freshness, capabilities, allowed fields, and time range. |
| `contacts.jsonl` | Stable participant ID, preferred normalized display name, local-profile availability, source-database freshness, enabled conversations, and per-conversation display names and roles. |
| `messages.jsonl` | Stable message/conversation IDs, conversation label, sender ID/name, creation time, ordinal, direction, logical type/subtype, normalized payload kind/summary, per-message source-database freshness, relationships, and attachment references. |
| `artifacts.jsonl` | Stable artifact ID, referencing conversations, availability/decode state, format, byte count, digest, safe account-relative path, and explicit resolution error when verification fails. |

Static files deliberately omit source logical paths, database/table/row
identities, raw columns, packed fields, original base64 payloads, raw XML,
schema SQL, and absolute filesystem paths. The lossless encrypted replica and
restoration archive retain those details inside the local trust boundary.

An attachment's metadata is included only after the connector's descriptor-read
and digest verification. Verification failure is retained as a typed artifact
error and does not abort unrelated messages or attachments. An agent that needs
the actual local file must make an authorized local `getArtifact` query; remote
destinations never receive a path.

## Progress and partial coverage

Default human progress is emitted on stderr. `--progress-json` emits the same
events as NDJSON, and `--progress-file` creates an owner-only durable event log.
Events expose bundle planning, conversation/contact/message/artifact phases,
current and total record counts, file index/count, elapsed time, phase
percentage, and end-to-end percentage. Stdout remains machine-readable final
JSON.

Unavailable databases do not block synchronization or context publication.
Their counts and preserved-stale state remain visible in every query and static
manifest. Each message is independently labeled `fresh` or `preservedStale`;
conversation and contact records use `fresh`, `preservedStale`, `mixed`, or
`derived` according to their retained source evidence. Records retained from an
earlier generation remain queryable, but are never presented as observations
from the current sync. Neither an agent nor a downstream indexer may interpret
absence from an unavailable shard as a deletion. Healthy databases,
conversations, messages, contacts, and attachments continue to be served.

## Downstream update model

Static consumers should run `audit-ai-context`, build a new index generation,
then atomically switch to it. Preserve the bundle ID,
replica ID, source fingerprint, checkpoint revision, stable entity IDs, and
coverage object beside derived data.

Live consumers can call `getChanges(cursor)` and refresh changed entities with
the other read operations. A message search cursor is checkpoint-bound and must
be restarted after synchronization; a change cursor is replica-generation-
bound and remains resumable across checkpoints. Generated summaries,
embeddings, inferred relationships, and commitments are downstream derivatives,
not GreenBubbles canonical facts, and should retain links to their source IDs
and checkpoint.
