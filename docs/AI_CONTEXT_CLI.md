# Give an AI access to only some chats

This page is for two jobs:

- **Let GreenBubbles write a summary with Gemini.** Jump to
  [Model-generated live memory](#model-generated-live-memory).
- **Let an AI see only some of your chats.** Start with
  [Pick an approach](#pick-an-approach), then write a policy file.

If you just want your coding agent (Codex, Claude Code, OpenCode, Kimi Code,
Gemini CLI, or Grok Build) to read your chats and write notes, you don't need
this page. See the [agent skills guide](AGENT_SKILLS.md).

Everything here uses the `greenbubbles` command. Each command runs once and
exits: there is no background service, no SQL, and no access to your backup
recovery secrets. The skill in `skills/greenbubbles-context` gives an agent a
short version of these instructions.

## Pick an approach

| Approach | Commands | Use it when |
| --- | --- | --- |
| Read directly | `messages list`, `messages search`, `message get` | You're at a terminal and can read everything yourself |
| Query through a policy | `connector-query-direct`, `ai-query` | An AI should see only what a policy file allows |
| Export a bundle | `ai-export`, then `ai-memory-export` | You want a fixed, verifiable set of files to load into another tool |
| Summarize with Gemini | `ai-summarize-direct` | You want a model to write a small cited summary from chats you allowed |
| Build notes over everything | `memory prepare`, `next`, `page`, `acknowledge`, `commit`, `status` | An agent should build notes from your whole history, a batch at a time |

For ordinary policy-limited reads, use `connector-query-direct`. It reads your
live WeChat data (or a backup), answers one JSON request, adds an entry to the
audit log, and exits. It doesn't need an export or a replica. If you don't need
a policy at all, the plain `messages` commands are simpler.

A **replica** is a private, encrypted, restored copy of your history that
GreenBubbles can keep for faster and richer queries. `ai-query` and `ai-export`
read a replica; the direct commands read WeChat's own files.

## Which policy applies

There are two kinds of policy file, and each works with different commands.

- **Direct policy** — used by `connector-query-direct` and
  `ai-summarize-direct`. It limits which operations, conversations, dates, and
  message fields are allowed, how many results come back, and whether results
  may go to a cloud model.
- **Replica policy** — used by `ai-query` and `ai-export`. It is tied to one
  account. For each conversation it grants operations, message fields,
  inclusive date ranges, and local or cloud release separately. Cached Moments
  have their own scope.

You can't swap one for the other. The replica uses one-way hashed IDs tied to
the account, while the direct commands use WeChat's own IDs, so a policy
written for one is rejected by the other instead of being misread.

Some rules apply to both:

- The replica key is read only from standard input.
- Requests, policies, audit logs, progress logs, and bundles must live in
  folders only you can open (mode `0700`), with private files at `0600`.
- Search terms and message text never appear in command-line arguments or in
  the audit log.
- **`ai-query` only reads.** It rejects drafts, previews, bootstrap, sync,
  refresh, approval, sending, and every other change. Message text is treated
  as untrusted content: nothing in a message can trigger another operation or
  widen the policy.

## One-shot queries

Write the request to a file only you can read:

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

Then run:

```sh
greenbubbles connector-query-direct \
  <source-root> <direct-policy.json> <audit.ndjson> <request.json> \
  --passphrase-stdin
```

The direct command supports `capabilities`, `status`, `listConversations`,
`searchMessages`, `getMessages`, and `getMessage`.

Use a replica query only for things the direct command can't do: restored
coverage details, changes since a checkpoint, cached Moments, attachments
(artifacts), and contact or conversation lookups. Its request uses schema
`greenbubbles.ai-query.v1`:

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

It supports `capabilities`, `status`, `coverage`, `getChanges`,
`getCachedMoments`, `listConversations`, `searchMessages`, `getMessages`,
`getMessage`, `getArtifact`, `resolveContact`, and `resolveConversation`. The
request and response bodies are described in [CONNECTOR_API.md](CONNECTOR_API.md).

### What comes back with the content

Every response has `formatVersion`, the schema, the API version, the request
identity, `ok`, a `context` object, and either `result` or `error`.

An agent should always read `context`. It says:

- which account, replica, and checkpoint (sync point) the answer comes from;
- `selfParticipantId`, a privacy-safe ID for you, when the replica is bound to
  your account;
- whether the WeChat version is supported, and what the archive covers;
- how many databases are total, fresh, unavailable, or kept from an older sync;
- how many conversations, contacts, messages, and gaps there are;
- how old the checkpoint is, and `sourceCoverageComplete`;
- stable `limitationCodes`, plus a plain-language `coverageNote`.

The command checks the checkpoint **before and after** answering. If a sync
happens in between, you get an integrity error instead of content mixed with
freshness details from a different sync.

## Static bundles

An export writes every message the policy allows into a fixed set of files:

```sh
greenbubbles ai-export \
  <replica.db> <policy.json> <audit.ndjson> <new-output-directory> \
  --replica-key-stdin --requester <id> [--destination local|remote]
```

The output folder must not exist yet. GreenBubbles builds the bundle in a
private staging folder next to it, reads everything at a *single* checkpoint,
flushes each file to disk, checks the checkpoint again, and then renames the
staging folder into place in one step. If anything fails, or a sync happens
during the export, the staging folder is deleted. You never get a bundle that
looks complete but isn't.

New exports use schema `greenbubbles.ai-context.v2`. It needs a replica that
carries verified proof of which account is yours. An older replica without that
proof is refused. The bundle identifies you only by the opaque
`selfParticipantId`, which is safe to share; your real WeChat ID stays inside
the private backup and archive. `audit-ai-context` and the history viewer
still accept version-1 bundles.

A bundle has five files: `manifest.json` and four JSONL files.

`manifest.json` holds:

- a bundle ID tied to the replica, checkpoint, policy digest, destination,
  policy source, and `selfParticipantId`;
- when it was made, the requester, the destination, and whether
  `exportComplete` is true;
- the full `context` and freshness details;
- how many conversations, contacts, messages, and attachments were included,
  and how many attachments failed to resolve;
- each file's name, record count, byte count, and SHA-256 digest.

| File | What it holds |
| --- | --- |
| `conversations.jsonl` | Stable ID, a readable label, kind, participants and their roles, explicit `groupOwnerParticipantId` evidence, decode state, freshness, capabilities, allowed fields, and time range. **Owning a group never means the group owner is you.** |
| `contacts.jsonl` | Stable participant ID, preferred display name, whether a local profile exists, freshness, the conversations they appear in, and their name and role in each. You are labelled `You` everywhere. |
| `messages.jsonl` | Stable message and conversation IDs, the conversation label, sender ID and name, optional `isAccountHolder`, time, order, direction, message type and subtype, a short payload kind and summary, freshness, cleaned-up links to related messages and attachments, and `omittedRelationshipReferenceCount` and `omittedArtifactReferenceCount`. |
| `artifacts.jsonl` | Stable attachment ID, the conversations that reference it, availability and decode state, format, size, digest, a safe path relative to the account, and an explicit error when verification fails. |

Bundles deliberately leave out internal file paths, database and row IDs, raw
columns, packed fields, original base64 data, raw XML, database schemas, and
absolute paths. Those stay in the replica and backup on your Mac.

Check a bundle after copying it and before loading it anywhere:

```sh
greenbubbles audit-ai-context <context-bundle-directory> \
  [--progress-file <owner-only-new-events.ndjson>] \
  [--progress-json | --quiet-progress]
```

It checks:

- that exactly those five files exist, readable only by you;
- every manifest and record schema, size, digest, and count;
- that IDs are unique and every reference between conversations, contacts,
  messages, and attachments resolves;
- that freshness labels and message directions are consistent;
- that the bundle, checkpoint, policy, and account-holder identity match.

It prints only counts and true/false results, never a label, message, name,
path, or ID.

### Who is "you"

When sender and direction fields are allowed, version 2 uses exactly one rule:
a message whose `senderId` equals `selfParticipantId` is outgoing, and every
other message is incoming. Your messages are labelled `You`. Queries and
exports apply that rule, and the bundle check rejects any record that still
disagrees with it.

**GreenBubbles never guesses who you are from a contact name, the other person
in a one-on-one chat, how often someone writes, or who owns a group.** A record
with no sender keeps its original direction if WeChat recorded one; otherwise
the direction stays unknown.

The direct commands apply the same rule to live data. They work out your WeChat
ID only from the verified account folder that contains the selected
`db_storage`. When the policy allows `sender`, each known sender gets
`isAccountHolder: true|false`, and you are shown as `You`. Senders that are
missing or withheld by the policy get no marker. Your raw account ID never
leaves the command.

### Attachments

Attachment details are included only after GreenBubbles reads the file's
descriptor and verifies its digest. If one attachment fails, it gets a typed
error and the rest of the export continues. To get the actual file, an agent
must make a local, authorized `getArtifact` call. **A cloud destination never
receives a file path.**

The export handles all attachments together rather than one request per file.
It only looks at attachments already referenced by messages the policy
allowed. It reads them in one read-only database transaction, in batches, and
reuses one verifier for every file. A missing, malformed, or changed attachment
becomes a typed record. A problem with the replica itself, the checkpoint, or
the restoration report still fails the whole export. The audit log gets one
summary `exportArtifacts` entry, and if a sync happened during the export, the
staged bundle is thrown away.

## Memory projection

The five-file bundle is good for exchange and checking, but it's an inefficient
thing to load when you have millions of messages. `ai-memory-export` turns a
bundle into chunks sized for memory tools such as Mem0 and QMD:

```sh
greenbubbles ai-memory-export \
  <AI-context-bundle-directory> <new-output-directory> \
  [--max-messages-per-chunk <1..1000>] \
  [--max-text-bytes-per-chunk <256..1048576>] \
  [--progress-file <owner-only-new-events.ndjson>] \
  [--progress-json | --quiet-progress]
```

By default each chunk has up to 64 messages and 49,152 bytes of UTF-8 text. The
same bundle and options always produce the same chunks.

| Output | What it's for |
| --- | --- |
| `manifest.json` | Output and source IDs; the account, checkpoint, and policy it came from; chunk settings; freshness; omission and truncation counts; limitations; compatibility flags |
| `memories.jsonl` | Chunks in a neutral format with `messages: [{role, content}]`, source-message evidence, stable citations, and flat metadata that fits Mem0-style `add(...)` calls |
| `documents/` | One Markdown document per chunk for QMD, Khoj, and similar tools. File names and IDs are stable and contain no contact or chat names |
| `documents.jsonl` | The list of documents: stable IDs, relative paths, sizes, and SHA-256 digests |
| `README.md` | Examples for loading into QMD and Mem0, and a note on the role mapping |

About roles: you are mapped to `user` and everyone else to `assistant`. That's
only because many AI APIs expect chat messages in that shape. Every content
string still names the real speaker, whether they are `self` or `other`, the
time, and a `greenbubbles:message:<opaque-id>` citation, and `sourceMessages`
keeps the structured evidence. **The role mapping never means another person
was an AI.**

Before writing anything, the command verifies the source bundle's identity and
every file's size, record count, and digest. A modified bundle, an unsafe path,
or a mismatched checkpoint stops it. Inside a bundle that passes those checks,
a single malformed record is skipped and reported through the
`projectionOmitted*Count` fields and `limitationCodes`, and the healthy records
are still written.

Check the output:

```sh
greenbubbles audit-ai-memory <AI-memory-output-directory> \
  [--progress-file <owner-only-new-events.ndjson>] \
  [--progress-json | --quiet-progress]
```

Tested setups for specific memory tools, and how updates work, are in
[AI_MEMORY_INTEGRATION.md](AI_MEMORY_INTEGRATION.md).

## Model-generated live memory

`ai-memory-export` never calls a model; it only prepares files. To have Gemini
actually write a cited summary, use `ai-summarize-direct`. It costs money: you
need your own `GEMINI_API_KEY`, and Google bills it separately from any coding
agent subscription.

1. **Write a direct policy.** For each chat you want summarized, set
   `allowRemoteModel` and grant the list and read capabilities plus the
   `sender` and `content` message fields. Chats without `allowRemoteModel` are
   skipped. The command refuses to run if no chat qualifies, or if a chosen
   chat is missing list, `sender`, or `content`.
2. **Set the API key** in your environment. It is never accepted as an
   argument:

   ```sh
   export GEMINI_API_KEY='<your key>'
   ```

3. **Run the summarizer**, choosing a new output folder:

   ```sh
   greenbubbles ai-summarize-direct \
     <live-db_storage> <direct-policy.json> <audit.ndjson> \
     <new-memory-output-directory> --requester <stable-id> \
     --max-messages-per-conversation 200 --passphrase-stdin
   ```

   Use `--decrypted` instead of `--passphrase-stdin` when the source is
   plaintext SQLite database files. The per-chat message limit defaults to 200 and can be at most 1,000.
4. **Review `memory.md`** before you rely on it.

### What is sent to Gemini

The command calls `gemini-3.8-flash`. Before sending, it replaces each long
message ID with a short alias such as `M001`. It sends only each chat's label,
kind, and coverage, plus each message's actor, speaker, time, kind, and text.
Gemini never receives real message IDs, sender IDs, citations, freshness
details, attachment data, or your policy and audit files. Chat text is clearly
marked as untrusted evidence. The passphrase is read only from standard input,
and the API key travels only in an HTTPS header from inside the process.

### What you get

| Output | What it's for |
| --- | --- |
| `memory.json` | The checked, structured summary with alias citations |
| `memory.md` | A readable version for review |
| `model-input.json` | Exactly what was sent to the model; no real message IDs |
| `evidence.jsonl` | Private map from each alias back to the real message ID, chat, sender, time, and content digest |
| `model-response.json` | Gemini's raw reply, for troubleshooting |
| `manifest.json` | Digests of the source, policy, audit log, and model; token usage; how much the input was shrunk; author counts; coverage; file hashes |

GreenBubbles rejects the model's answer if:

- the JSON is malformed or cut off;
- it cites an unknown or repeated alias, or cites a message from a different
  chat;
- it outputs a real message ID instead of an alias;
- it claims something about you without citing at least one message you wrote.

Ambiguous school or company names such as `科大` must not be expanded into a
guess. If a chat was only partly read, both memory files say so.

Each run writes a new folder, readable only by you, in one atomic step. It
never changes or merges an earlier summary. When your chats change, run it
again into a new folder, then compare the two and keep the one you've reviewed.

<a id="corpus-scale-pi-memory"></a>

## Corpus-scale personal memory

`ai-summarize-direct` reads a limited number of messages per chat. To build
notes from your whole history, most people should let their coding agent follow
the [personal-memory skill](../skills/greenbubbles-personal-memory/SKILL.md),
which reads live messages directly. The [personal memory guide](PERSONAL_MEMORY.md)
explains it.

There is also an advanced batch workflow built on an evidence archive:

- `memory prepare` (v2) reads every message table once and copies every
  eligible message into a fixed, read-only archive.
- `memory next` picks the next batch. Narrow it with repeatable
  `--conversation`, `--conversation-kind`, and `--sender` options and inclusive
  RFC 3339 `--from` and `--through` dates. Different kinds of filter narrow the
  selection together; with no filters, the whole archive is selected.
- `--subject` chooses whose notes these are. It defaults to you, accepts
  `person:<selector>` for someone else, or `none` for notes organized by chat.
- `memory next` prints a short description of the batch. Repeated
  `memory page` calls then return the messages in fixed pieces of at most
  49,152 bytes, with short `E#########`, `P######`, and `C######` keys and
  RFC 3339 times. Each page lists real IDs, contact names and aliases, and
  group titles once, instead of on every message. Detailed citation data stays
  in a local sidecar file.
- By default, batches are ordered by how much they involve you and how active
  that period was, not oldest first. Every batch is still reached eventually.

Use one writer per notes folder. The optional driver script runs separate
coding-agent sessions for this workflow; see [personal memory](PERSONAL_MEMORY.md).

The older `wiki` format writes `conversations/C######.md`, `me.md`,
`people/P######.md`, and `index.md`, and its commit checks allowed page paths
and kept or cited evidence. For Markdown and Python notes, commits check
structure and which pages were acknowledged. They don't check whether facts are
true or whether citations are good, so review the diffs yourself. An
interrupted batch resumes from its saved state; GreenBubbles never merges prose
on its own.

## Progress

`ai-export`, `audit-ai-context`, `ai-memory-export`, and `audit-ai-memory` all
report progress:

- readable progress on stderr by default;
- the same events as NDJSON with `--progress-json`;
- a lasting, owner-only log with `--progress-file`;
- no readable progress on stderr with `--quiet-progress`.

Events show file sizes, records read, conversations and messages processed,
chunks and documents written or checked, position in the file, elapsed time,
and both the current step's and the overall percentage. Large attachment sets
report a running total every 1,000 records plus a final count, instead of one
line per file. Stdout always holds only the final JSON result.

**Keep progress logs outside the bundle folder**, so checking a bundle doesn't
change the files being checked.

## Partial coverage, precisely

Some WeChat databases may be unreadable. That doesn't stop a sync or an export.
Their counts, and any data kept from an earlier sync, stay visible in every
answer and manifest.

- Each message is labelled `fresh` or `preservedStale`.
- Conversations and contacts are `fresh`, `preservedStale`, `mixed`, or
  `derived`, depending on the evidence behind them.
- Records kept from an earlier sync can still be queried, but they are
  **never** presented as something seen in the current sync.

The rule an agent is most likely to break: **a message missing from an
unreadable database has not been deleted.**

The same applies to a missing replica table, an optional search index, or a
malformed row. You get the healthy part of the answer, or an empty *successful*
page, plus typed omission counts and `limitationCodes`. Broken, empty,
duplicate, or inconsistent optional references are dropped one at a time, and
the matching `malformed*ReferenceOmitted` limitation appears in both audits.

Contacts and attachments work the same way, with one important limit:

- If a healthy, allowed conversation still shows that someone is a member, a
  missing profile becomes a derived contact marked
  `unavailableParticipantProfileSynthesized`.
- If a healthy message still references an attachment, a missing attachment
  record becomes a metadata-unavailable attachment marked
  `unavailableArtifactMetadataSynthesized`.
- **Nothing is filled in when the remaining data can't prove the item is inside
  the policy.**

None of this relaxes the hard failures. Tampering with the key, account, or
checkpoint, an unsafe path, or an authorization failure is an error, not a gap
to work around.
