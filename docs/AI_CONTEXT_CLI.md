# Giving an AI context

The `greenbubbles` command line is the primary agent surface. It serves
source-faithful WeChat context without a long-running process, without SQL, and
without access to restoration secrets. The repository skill in
`skills/greenbubbles-context` gives a compatible agent the short version of
these instructions.

Three levels, in increasing order of deliberateness:

| Level | Command | Use when |
| --- | --- | --- |
| Direct bounded read | `messages list/search`, `message get` | You are at a terminal, with your own authority |
| Policy-scoped query | `connector-query-direct`, `ai-query` | An AI caller needs a boundary you control |
| Static export | `ai-export`, then `ai-memory-export` | You want a durable, auditable bundle to ingest |
| Generated memory | `ai-summarize-direct` | You want a real model to compile a small cited wiki from live authorized messages |
| Corpus-scale agent memory | `memory prepare/next/page/acknowledge/commit/status` | One Pi agent should iteratively refine a cited wiki over all messages or a composable command-line scope without traversing a million messages itself |

For ordinary conversation and message reads, `connector-query-direct` is the
preferred policy-scoped path: one typed request against live or snapshot
SQLite, JSON out, the normal chained audit appended, and exit — with no JSONL
archive and no encrypted replica involved. The plain resource commands remain
simplest when you do not need a separate AI policy boundary at all.

## Which policy applies

`connector-query-direct` uses a **source-bound** policy for ordinary list,
search and get operations, enforcing capability, conversation, time, field,
result/summary and destination bounds.

`ai-query` and `ai-export` reuse the **replica tool policy**, which binds one
account and grants operations, normalized fields, inclusive time ranges and
local-or-remote release independently per conversation, with cached Moments as
a separate scope.

The two are not interchangeable, and the reason is structural rather than
stylistic: replica identifiers are account-scoped one-way hashes, while direct
identifiers are the source's own. A policy for one is rejected by the other
rather than silently reinterpreted.

The replica key is consumed only from standard input. Requests, policies, audit
logs, progress logs and bundles all live under owner-controlled mode-`0700`
directories, with private files at `0600`. Search terms and message bodies
never appear in process arguments or in the body-free audit log.

**`ai-query` accepts only read operations.** It rejects drafts, previews,
bootstrap, synchronization, refresh, approval, send, or any other mutation.
Message text is returned as untrusted source content and cannot select another
operation or widen a policy.

## One-shot queries

Put a request in an owner-only file:

```json
{
  "apiVersion": "greenbubbles.connector.v1",
  "requestId": "unique-caller-request",
  "requesterId": "local-agent",
  "destination": "local",
  "operation": {
    "kind": "getMessages",
    "conversationId": "wxid-or-chatroom-id",
    "cursor": null,
    "limit": 50
  }
}
```

```sh
greenbubbles connector-query-direct \
  <source-root> <direct-policy.json> <audit.ndjson> <request.json> \
  --passphrase-stdin
```

The direct backend supports `capabilities`, `status`, `listConversations`,
`searchMessages`, `getMessages` and `getMessage`. Use the replica query below
only for restored coverage and enrichment, changes, cached Moments, artifacts,
or anything else that is genuinely replica-only.

The replica form uses schema `greenbubbles.ai-query.v1`:

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

```sh
greenbubbles ai-query \
  <replica.db> <policy.json> <audit.ndjson> <request.json> \
  --replica-key-stdin
```

Supported: `capabilities`, `status`, `coverage`, `getChanges`,
`getCachedMoments`, `listConversations`, `searchMessages`, `getMessages`,
`getMessage`, `getArtifact`, `resolveContact`, `resolveConversation`. Their
bodies are the same tagged JSON union documented in
[CONNECTOR_API.md](CONNECTOR_API.md).

### What comes back with the content

Every response carries `formatVersion`, schema, API version, request identity,
`ok`, a `context` object, and either `result` or `error`. The context object is
the part an agent must actually read: the account, replica and checkpoint
binding; the privacy-safe `selfParticipantId` when the replica is
account-bound; client compatibility; archive scope; total, fresh, unavailable
and preserved-stale database counts; canonical entity counts; gap counts;
checkpoint age; `sourceCoverageComplete`; stable `limitationCodes`; and a
plain-language `coverageNote`.

The command checks the checkpoint **before and after** the request. A
concurrent sync produces an integrity error rather than content paired with
freshness metadata from a different generation.

## Static bundles

```sh
greenbubbles ai-export \
  <replica.db> <policy.json> <audit.ndjson> <new-output-directory> \
  --replica-key-stdin --requester <id> [--destination local|remote]
```

The output path must not exist. GreenBubbles writes a private sibling staging
directory, pages through every authorized message at a *single* checkpoint,
flushes and fsyncs each file, verifies the final checkpoint, and publishes with
one rename. An error or a concurrent sync removes the staging directory, so
there is never an apparently complete but actually partial generation.

New exports write `greenbubbles.ai-context.v2`, which requires the replica to
carry account-holder identity evidence integrity-bound to the selected account.
An unbound legacy replica is refused. The opaque `selfParticipantId` is safe to
propagate; the source WeChat identifier stays confined to the private
snapshot and archive boundary. `audit-ai-context` and the history loader still
accept version-1 bundles.

`manifest.json` carries a deterministic bundle identity bound to replica,
checkpoint, policy digest, destination, policy source identity and
`selfParticipantId`; creation time, requester, destination and explicit
`exportComplete` state; the complete context and freshness object; enabled
conversation, contact, message and attachment counts; the
attachment-resolution error count; and relative file names, record counts, byte
counts and SHA-256 digests.

| File | What it holds |
| --- | --- |
| `conversations.jsonl` | Stable ID, human label, kind, participant names and roles, explicit `groupOwnerParticipantId` evidence, decode state, source freshness, capabilities, allowed fields, time range. **Group ownership is never read as account ownership.** |
| `contacts.jsonl` | Stable participant ID, preferred normalized display name, local-profile availability, source freshness, enabled conversations, per-conversation names and roles. Every representation of the bound account holder is labelled `You`. |
| `messages.jsonl` | Stable message and conversation IDs, conversation label, sender ID and name, optional `isAccountHolder`, creation time, ordinal, direction, logical type and subtype, normalized payload kind and summary, per-message freshness, sanitized relationships and attachment references, plus `omittedRelationshipReferenceCount` and `omittedArtifactReferenceCount`. |
| `artifacts.jsonl` | Stable artifact ID, referencing conversations, availability and decode state, format, byte count, digest, safe account-relative path, and an explicit resolution error when verification fails. |

Static files deliberately omit source logical paths, database/table/row
identities, raw columns, packed fields, original base64 payloads, raw XML,
schema SQL and absolute filesystem paths. The lossless replica and restoration
archive keep those inside the local trust boundary, where they belong.

Verify a bundle after copying it and before indexing it:

```sh
greenbubbles audit-ai-context <context-bundle-directory> \
  [--progress-file <owner-only-new-events.ndjson>] \
  [--progress-json | --quiet-progress]
```

It checks the exact five-file inventory, owner-only permissions, manifest and
record schemas, sizes, digests and counts, unique identities, every
conversation–contact–message–artifact reference, per-record freshness
consistency, sender-versus-account direction consistency, and the
bundle/checkpoint/policy/account-holder identity — emitting counts and booleans
only, never a label, a message, a name, a path or an identifier.

### Who is "you"

When sender and direction fields are authorized, version 2 applies exactly one
rule: `senderId == selfParticipantId` is outgoing; every other sender is
incoming. A self sender is labelled `You`. Query and export normalize an
authorized pair to that rule, and bundle audit rejects a pair that still
disagrees.

**No contact name, direct-chat peer, message frequency or group-owner field is
ever used to guess who you are.** Sender-less records may retain an explicit
source direction; otherwise they stay unknown.

The live direct connector applies the same source-level rule before replica
restoration. It derives the raw account identifier only from the validated
account directory containing the selected `db_storage`. When `sender` is
authorized, each known sender receives `isAccountHolder: true|false`, and self
is displayed as `You`; absent or policy-withheld senders omit the marker. The
raw bound account identifier stays inside the connector boundary.

### Attachments

Metadata is included only after a descriptor read and digest verification.
A verification failure becomes a typed artifact error and does not abort
unrelated messages or attachments. An agent that needs the actual file must
make an authorized local `getArtifact` call — **a remote destination never
receives a path.**

Export resolves attachment metadata as one internal batch rather than one
connector request per attachment, and authorization comes exclusively from the
attachment references already returned by policy-authorized message pages. A
single read-only SQLCipher transaction loads the canonical artifacts in stable
ID order through bounded batches, loading the bound restoration report once,
with one reusable descriptor and digest verifier for every available file.
Missing, malformed or changed individual artifacts become typed records; a
replica identity, checkpoint or report failure still fails the whole export.
The journal records one aggregate `exportArtifacts` event, and the
start-versus-end checkpoint comparison discards the staged bundle if a
synchronization raced the batch.

## Memory projection

The five-file bundle is an interchange and audit format. It is not an efficient
prompt or ingestion format when a corpus holds millions of message lines.

```sh
greenbubbles ai-memory-export \
  <AI-context-bundle-directory> <new-output-directory> \
  [--max-messages-per-chunk <1..1000>] \
  [--max-text-bytes-per-chunk <256..1048576>] \
  [--progress-file <owner-only-new-events.ndjson>] \
  [--progress-json | --quiet-progress]
```

Defaults are 64 messages and 49,152 UTF-8 bytes per chunk, and boundaries are
deterministic for a given source generation and option set.

| Output | Purpose |
| --- | --- |
| `manifest.json` | Projection and source IDs, account/checkpoint/policy binding, chunk parameters, freshness, omission and truncation counts, limitations, compatibility flags |
| `memories.jsonl` | Vendor-neutral chunks with `messages: [{role, content}]`, source-message evidence, stable citations, and flat metadata suited to Mem0-style `add(...)` calls |
| `documents/` | One bounded Markdown document per chunk for QMD, Khoj and similar. Paths and IDs are stable and contain no contact or conversation names |
| `documents.jsonl` | The document inventory: stable IDs, relative paths, byte counts, SHA-256 |
| `README.md` | Local QMD and Mem0 ingestion examples, and the role-mapping caveat |

The account holder maps to role `user` and other speakers to `assistant`. That
is a transport convention for APIs that accept chat messages, nothing more —
every content string repeats the actual speaker, the `self`/`other` actor, the
timestamp and a `greenbubbles:message:<opaque-id>` citation, and
`sourceMessages` keeps the structured evidence. **Never read the role mapping
as evidence that another participant was an AI.**

The projector verifies the source manifest identity and every source file's
byte count, record count and digest. A modified generation, unsafe path or
inconsistent checkpoint fails closed. Inside an otherwise integrity-bound
generation, a malformed record is skipped and reported through
`projectionOmitted*Count` and `limitationCodes` while healthy records publish.

```sh
greenbubbles audit-ai-memory <AI-memory-output-directory> \
  [--progress-file <owner-only-new-events.ndjson>] \
  [--progress-json | --quiet-progress]
```

Tested framework workflows and update semantics are in
[AI_MEMORY_INTEGRATION.md](AI_MEMORY_INTEGRATION.md).

## Model-generated live memory

`ai-memory-export` above is intentionally deterministic: it prepares Mem0/QMD
inputs but does not ask a model to infer a wiki. Use the explicit live summary
command when model inference is desired:

```sh
export GEMINI_API_KEY='<provided outside process arguments>'
greenbubbles ai-summarize-direct \
  <live-db_storage> <direct-policy.json> <audit.ndjson> \
  <new-memory-output-directory> --requester <stable-id> \
  --max-messages-per-conversation 200 --passphrase-stdin
```

The policy must grant `list`, `read`, `sender`, and `content`, and each selected
scope must explicitly set `allowRemoteModel`. The passphrase remains stdin-only;
the Gemini key is read only from `GEMINI_API_KEY` and is sent as an in-process
HTTPS header, never as a subprocess argument.

The command invokes `gemini-3.7-flash`. Before the request, it replaces every
potentially long canonical message ID with a short `M###` alias and sends only
conversation label/kind/coverage plus compact message actor, speaker, time,
kind and text fields. The model never receives canonical IDs, sender IDs,
connector citations, freshness objects, attachment structures or policy/audit
metadata. Chat text is explicitly delimited as untrusted evidence.

| Output | Purpose |
| --- | --- |
| `memory.json` | Validated structured personal memory and conversation wiki with alias citations |
| `memory.md` | Human-readable rendering for review |
| `model-input.json` | Exact compact JSON embedded in the model prompt; no canonical message IDs |
| `evidence.jsonl` | Private alias-to-canonical-ID, conversation, actor, timestamp and content-digest map |
| `model-response.json` | Raw Gemini response for local diagnosis |
| `manifest.json` | Source/policy/audit/model digests, token usage, byte reduction, author counts, coverage and file hashes |

GreenBubbles rejects malformed/truncated model JSON, unknown or repeated
aliases, cross-conversation citations, raw canonical citation output, and any
account-holder claim without at least one self-authored source. Ambiguous
institution names such as `科大` may not be silently expanded. Incomplete
bounded pages remain visibly incomplete in both memory files.

Every invocation publishes a new immutable owner-only generation atomically;
it does not mutate or silently merge an earlier model-generated wiki. Run it
again with a new output path after live data changes, then compare or promote
the reviewed generation explicitly.

## Corpus-scale Pi memory

`ai-summarize-direct` is deliberately bounded per selected conversation. For a
whole live account, use a v2 `memory prepare`: one local process inventories the
message tables and hydrates every eligible row into a canonical immutable
corpus. Reuse that corpus with repeatable `memory next --conversation`,
`--conversation-kind`, and `--sender` arguments plus inclusive RFC 3339
`--from`/`--through` bounds. Categories intersect; empty evidence filters select
the entire hydrated corpus. `--subject` is independent: it defaults to the
authenticated account holder, accepts `person:<selector>`, or can be `none` for
conversation-centric memory.
`memory next` returns a small batch envelope; repeated
`memory page` calls then return deterministic at-most-49,152-byte fragments
with short `E#########`, `P######` and `C######` join keys and RFC 3339 message
times. Page-level identity dictionaries preserve real source IDs, contact
names/aliases, and group titles without repeating them on every message.
Verbose canonical-message citation data stays in a local sidecar.
The default personal-memory policy orders immutable units by deterministic
account-holder relevance and active-period coverage, rather than exhausting
the oldest conversation slice first; the schedule still traverses every
prepared unit.

One ReAct agent updates `conversations/C######.md`, `me.md`,
`people/P######.md` and `index.md` directly according to the resolved subject.
GreenBubbles does not semantically merge prose. The agent updates useful target
pages before acknowledging each fully read evidence page. The crash-safe `commit`
step validates immutable input hashes, complete page review, allowed page paths,
and exact retained/cited evidence before it advances to the next batch. The
uncommitted page and batch are returned again after a restart.

The complete algorithm, policy and command contract are in
[PERSONAL_MEMORY.md](PERSONAL_MEMORY.md). Pi discovers the project skill at
`skills/greenbubbles-personal-memory` through `.pi/settings.json`, and other
harnesses receive the same skill text in their prompt; no custom agent runtime
or tool extension is present.

## Progress

All four commands emit human progress on stderr by default, the same events as
NDJSON with `--progress-json`, and a durable owner-only log with
`--progress-file`. Events expose source and current-file sizes, source records,
processed conversation and message counts, emitted or verified chunk and
document counts and bytes, file position, elapsed time, phase percentage and
end-to-end percentage. Large attachment sets emit exact cumulative milestones
every 1,000 records plus a final count, rather than hundreds of thousands of
flushed lines. Stdout stays machine-readable final JSON.

**Keep progress logs outside audited bundle directories**, so that auditing a
bundle does not change the inventory being audited.

## Partial coverage, precisely

Unavailable databases do not block synchronization or publication. Their counts
and preserved-stale state stay visible in every query and every manifest. Each
message is independently labelled `fresh` or `preservedStale`; conversations
and contacts use `fresh`, `preservedStale`, `mixed` or `derived` according to
their retained evidence. Records carried from an earlier generation stay
queryable but are **never** presented as observations from the current sync.

The rule that follows from that, and the one an agent is most likely to break:
**absence from an unavailable shard is not a deletion.**

The same isolation applies to a missing replica domain table, an optional
search index, or a malformed row: the operation returns the healthy subset — or
an empty *successful* page — with typed omission counts and `limitationCodes`.
Malformed, empty, duplicate or structurally inconsistent optional references
are removed individually, and the corresponding `malformed*ReferenceOmitted`
limitation is carried into both audits.

Participant and artifact lookups follow the same rule with one important limit.
If a healthy authorized conversation still proves participant membership, a
missing profile becomes a derived contact with
`unavailableParticipantProfileSynthesized`; if a healthy canonical message
proves an attachment reference, a missing artifact record becomes a
metadata-unavailable artifact with `unavailableArtifactMetadataSynthesized`.
**No placeholder is returned when the remaining data cannot prove the requested
identity is in policy scope.**

And none of this softens the hard failures: key, account or checkpoint
tampering, an unsafe path, or an authorization failure is an error, not a
recoverable gap.
