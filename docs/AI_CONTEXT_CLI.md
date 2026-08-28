# AI context CLI and static bundle

GreenBubbles' primary agent surface is the `greenbubbles-restore` command-line
program. It serves source-faithful WeChat context without requiring a
long-running connector process, direct SQL, or access to restoration secrets.
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

The request schema remains `greenbubbles.ai-query.v1`:

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
contains the account/replica/checkpoint binding, the privacy-safe
`selfParticipantId` when the replica is account-bound, client compatibility, archive
scope, total/fresh/unavailable/preserved-stale database counts, canonical
entity counts, gap counts, checkpoint age, `sourceCoverageComplete`, stable
`limitationCodes`, and a plain-language `coverageNote`.

The command checks the checkpoint before and after the request. A concurrent
sync produces an integrity error rather than returning content paired with
freshness metadata from another generation.

## Static bundle format

New `ai-export` generations write schema `greenbubbles.ai-context.v2` and
`formatVersion: 2`:

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

Version 2 requires the replica to carry account-holder identity evidence
integrity-bound to the selected account by snapshot acquisition or legacy
account-root restoration. Export refuses an
unbound legacy replica. The opaque `selfParticipantId` is safe to propagate; the
source WeChat identifier remains confined to the private snapshot/archive
boundary. `audit-ai-context` and the Swift history loader continue to accept
existing version-1 bundles, but all new exports use version 2.

`manifest.json` contains:

- a deterministic bundle identity bound to replica, checkpoint, policy digest,
  destination, policy source identity, and `selfParticipantId`;
- creation time, requester, destination, and explicit `exportComplete` state;
- the complete context/freshness object returned by live queries;
- enabled conversation, contact, message, and attachment counts;
- attachment-resolution error count;
- relative file names, record counts, byte counts, and SHA-256 digests.

After copying or before indexing a generation, run the aggregate-only bundled
verifier:

```text
greenbubbles-restore audit-ai-context <context-bundle-directory> \
  [--progress-file <owner-only-new-events.ndjson>] \
  [--progress-json | --quiet-progress]
```

It verifies the exact five-file inventory, owner-only files, manifest and
record schemas, file sizes/digests/counts, unique identities, conversation-
contact-message-artifact references, per-record freshness consistency,
sender-versus-account direction consistency, and the
bundle/checkpoint/policy/account-holder identity. It emits only counts and boolean evidence;
it never prints labels, message text, contact names, paths, or identifiers.

The JSONL files are:

| File | AI-oriented content |
| --- | --- |
| `conversations.jsonl` | Stable conversation ID, human label, kind, participant names and roles, explicit `groupOwnerParticipantId` evidence, decode state, source-database freshness, capabilities, allowed fields, and time range. Group ownership is never interpreted as account ownership. |
| `contacts.jsonl` | Stable participant ID, preferred normalized display name, local-profile availability, source-database freshness, enabled conversations, and per-conversation display names and roles. Every representation of the bound account holder is labelled `You`. |
| `messages.jsonl` | Stable message/conversation IDs, conversation label, sender ID/name, creation time, ordinal, direction, logical type/subtype, normalized payload kind/summary, per-message source-database freshness, relationships, and attachment references. |
| `artifacts.jsonl` | Stable artifact ID, referencing conversations, availability/decode state, format, byte count, digest, safe account-relative path, and explicit resolution error when verification fails. |

Static files deliberately omit source logical paths, database/table/row
identities, raw columns, packed fields, original base64 payloads, raw XML,
schema SQL, and absolute filesystem paths. The lossless encrypted replica and
restoration archive retain those details inside the local trust boundary.

## Personal-memory projection

The canonical five-file bundle is an interchange and audit format. It is not
an efficient prompt or memory-ingestion format when a corpus contains millions
of individual message lines. Create a deterministic conversational projection
after export:

```text
greenbubbles-restore ai-memory-export \
  <AI-context-bundle-directory> <new-output-directory> \
  [--max-messages-per-chunk <1..1000>] \
  [--max-text-bytes-per-chunk <256..1048576>] \
  [--progress-file <owner-only-new-events.ndjson>] \
  [--progress-json | --quiet-progress]
```

Defaults are 64 messages and 49,152 UTF-8 text bytes per chunk. Boundaries are
deterministic for a source generation and option set. The output is owner-only
and published atomically:

| Output | Purpose |
| --- | --- |
| `manifest.json` | Projection/source IDs, account/checkpoint/policy binding, chunk parameters, source freshness, omission/truncation counts, limitations, and compatibility flags. |
| `memories.jsonl` | Vendor-neutral chunks with `messages: [{role, content}]`, source-message evidence, stable citations, and flat metadata suitable for current Mem0-style `add(messages, user_id=..., metadata=...)` calls. |
| `documents/` | One bounded Markdown document per chunk for QMD, Khoj, and Markdown-oriented retrieval systems. Paths and IDs are stable and contain no contact or conversation names. |
| `documents.jsonl` | The Markdown document inventory with stable IDs, relative paths, byte counts, and SHA-256 digests. |
| `README.md` | Local QMD and Mem0 ingestion examples and the role-mapping caveat. |

The account holder maps to framework role `user`; other chat speakers map to
`assistant`. This is only a transport convention for APIs that accept chat
messages. Every content string repeats the actual speaker, `self`/`other` actor,
timestamp, and `greenbubbles:message:<opaque-id>` citation, while
`sourceMessages` retains structured evidence. Never interpret the role mapping
as proof that another participant was an AI assistant.

The projector verifies the source manifest identity and every source file's
byte count, record count, and digest. A modified source generation, unsafe
path, or inconsistent checkpoint fails closed. Within an otherwise
integrity-bound generation, a malformed conversation/message record is skipped
and reported through `projectionOmitted*Count` and `limitationCodes`; healthy
records still publish. See [AI_MEMORY_INTEGRATION.md](AI_MEMORY_INTEGRATION.md)
for tested framework workflows and update semantics.

Before indexing or after copying a projection, run:

```text
greenbubbles-restore audit-ai-memory <AI-memory-output-directory> \
  [--progress-file <owner-only-new-events.ndjson>] \
  [--progress-json | --quiet-progress]
```

The privacy-safe report verifies the projection/source binding, exact
owner-only inventory, hashes, bounded chunk schemas, stable citations, and
every Markdown document without emitting conversation names or content.

When sender and direction fields are authorized, version 2 applies one rule:
`senderId == selfParticipantId` is outgoing, and every other sender is incoming.
A self sender is labelled `You`. AI query/export normalizes an authorized
sender/direction pair to that rule, and bundle audit rejects a pair that still
disagrees; no contact name, direct-chat peer, message frequency, or group-owner
field is used to guess self. Sender-less records may retain an explicit source
direction, otherwise they remain unknown.

An attachment's metadata is included only after the connector's descriptor-read
and digest verification. Verification failure is retained as a typed artifact
error and does not abort unrelated messages or attachments. An agent that needs
the actual local file must make an authorized local `getArtifact` query; remote
destinations never receive a path.

## Progress and partial coverage

`ai-export`, `audit-ai-context`, `ai-memory-export`, and `audit-ai-memory` emit
human progress on stderr by default. `--progress-json` emits the same events as
NDJSON, and `--progress-file` creates an owner-only durable event log. Events
expose source and current-file sizes, source records, processed
conversation/message counts, emitted or verified chunk/document counts and
bytes, file position, elapsed time, phase percentage, and end-to-end
percentage. Stdout remains machine-readable final JSON. Keep progress logs
outside audited bundle directories so the exact private inventory is not
changed by the act of auditing it.

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

## Native human browser

The `greenbubbles-history` SwiftUI application opens an audited static bundle
without a replica key, independently rechecks its permissions, schemas, hashes,
counts, identities, references, freshness, checkpoint, and policy binding, and
builds a private atomic SQLite/FTS index for scalable chat navigation and
multilingual search. It presents coverage limitations and per-record freshness
alongside conversations, contacts, messages, relationships, and typed media
metadata.

For an explicit startup bundle, run
`swift run greenbubbles-history --bundle /absolute/path/to/ai-context-bundle`.
The file panel, drag/drop, and macOS open-event paths use the same verification
gate.

Actual media remains outside the static bundle. An explicit preview calls the
same read-only `ai-query/getArtifact` operation, supplies the replica key only
over standard input, then rechecks descriptor identity, size, and SHA-256 while
creating a private session-only copy for Quick Look. See
[HISTORY_BROWSER.md](HISTORY_BROWSER.md) for the interaction, scaling, trust,
and release architecture.
