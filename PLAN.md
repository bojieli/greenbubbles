# GreenBubbles product and engineering plan

Last updated: 2026-08-27

## Strategic decision summary

GreenBubbles will be a focused, standalone WeChat connector and local service,
not a universal personal-context engine and not a monorepo of unrelated
connectors.

> GreenBubbles continuously synchronizes a user's own WeChat history into a
> private, searchable local replica that any authorized AI agent can query and,
> in a separately approved second product phase, use to reply or send files to
> the user's real WeChat contacts.

The connector must be independently useful from a CLI, JSON/JSONL, a local API,
and MCP. OpenClaw, Codex, Claude, a Karpathy-style LLM-maintained wiki, or another
memory system may consume it, but GreenBubbles will not depend on any one agent
host or memory engine.

The product is read-first but not read-only in its intended end state. Reliable
restoration, identity resolution, synchronization, search, authorization, and
draft preview come before externally visible actions. Feasibility of writing to
ordinary contacts must nevertheless be investigated now, because the ability
to reply to a friend or group and send a document is a central differentiator,
not a minor extension. Tencent's official bot channel is useful for messages in
the bot relationship, but it does not replace acting in the user's existing
ordinary conversations.

The bridge retains three non-transitive privilege tiers:

1. **Local passive read** — inspect authorized local stores without changing
   WeChat state or contacting its servers.
2. **Authenticated active read** — ask the existing client to fetch narrowly
   scoped read-only dynamic content, if a safe and supportable route exists.
3. **Write actions** — perform an externally visible action through the user's
   account only after deterministic policy checks and explicit approval.

Passing one tier never authorizes the next. Each capability can be unavailable
or disabled independently.

## First-principles rationale

A personal assistant is useful only to the extent that it can observe the
user's real context, reason over it, and act in the place where the work occurs.
Much of a user's social history, commitments, documents, and daily coordination
is trapped in private applications. Generic agent frameworks can orchestrate
tools, and personal-memory projects can synthesize sources that they receive,
but neither category creates access to data and actions that the source
application does not expose.

For WeChat, the missing source and action bridge is therefore the product:

```text
observe real conversations -> retrieve relevant context -> prepare an action
       -> show the exact recipient and payload -> user approves -> act
```

Conversation history, contacts, groups, files, replies, recalls, and current
commitments are generally higher-signal context than passive entertainment-feed
history. Deliberate saves, searches, purchases, and shares in services such as
Douyin, Xiaohongshu, or Zhihu may be useful, but ordinary iOS and Android
applications cannot continuously inspect those apps' private stores.
GreenBubbles will not broaden its scope to compensate for an acquisition path
that current mobile sandboxing does not provide.

## Alternatives considered and decisions

### Product boundary

Three product shapes were considered:

1. **A universal personal-context engine** that ingests many applications,
   reconciles people and activities, summarizes the user's life, and serves
   agents.
2. **A collection or monorepo of connectors** for WeChat, Telegram, iMessage,
   and other applications, with or without a thin common interface.
3. **One focused WeChat connector** with a stable downstream change stream and
   agent-neutral interfaces.

Decision: choose the focused WeChat connector.

The memory/context-engine category is already crowded, while private-source
acquisition remains the scarce capability. Each closed application also has a
different release cadence, schema, security model, legal exposure, and support
burden. A single-source project has a clearer promise, a smaller trust boundary,
and a more demonstrable result. A future context hub can consume GreenBubbles,
but GreenBubbles must never require that hub.

Sources with supported APIs, such as Slack or email providers, can already be
connected directly by OpenClaw and other agent frameworks; reimplementing all of
them would not create the same differentiated value. Telegram and iMessage may
still merit better live connectors, but their acquisition and release lifecycles
do not belong inside a WeChat implementation.

If a second connector is later justified, it should begin in a separate
repository. Only after two real implementations reveal stable common concepts
should a shared connector protocol or SDK be extracted. No universal ontology
should be designed from one source in anticipation of hypothetical connectors.

### Direct access, export, or synchronized replica

Three query models were considered:

- **Live database access for every agent request:** freshest in principle, but
  fragile under locks and schema drift, poorly indexed for agent queries, and
  unsafe to expose as raw SQL or internal application calls.
- **Periodic full export:** simple and source-faithful, but too slow and stale
  for a continuously useful assistant. Telegram's slow export workflow is an
  example of why export alone is not a live connector.
- **Bootstrap plus incremental local replica:** acquire consistent source
  snapshots, restore them losslessly, and update an agent-oriented replica with
  checkpoints and reconciliation.

Decision: use the source database only as an acquisition surface and make an
encrypted, canonical local replica the serving surface. The replica provides
stable identities, structured filters, full-text indexes, provenance, coverage,
and cursors without coupling every consumer to a private application schema.

Synchronization must be change-proportional rather than repeatedly decoding
the full history. Filesystem and notification observations are wake-up hints,
not authorities. The authoritative process compares source state, advances a
checkpoint transactionally, uses a rolling reconciliation window to recover
late edits or recalls, and runs occasional integrity scans. Media work must not
delay text freshness.

Initial service objectives are:

- newly persisted local text becomes searchable within 60 seconds at p95;
- idle work and I/O are negligible;
- no periodic full-history decode is required;
- a crash cannot advance a checkpoint past committed replica records;
- edits, recalls, deletions, and missed wake-up hints are eventually reconciled;
- media processing occurs independently from the text synchronization path.

These are objectives to benchmark, not assumptions that one-minute sync is
already proven.

### Device and synchronization topology

Options considered were a desktop-local observer, a mobile companion, hosted
cloud ingestion, and multi-device/multi-master replication.

Decision: begin with one local macOS machine and the user's already installed
official desktop client. A traditional Windows client may later provide another
feasible observation point, but it is a separate adapter and support decision.
Do not introduce cloud storage, cross-client multi-master state, or a mobile
database reader in the initial product.

Modern iOS and non-rooted Android sandboxing prevents a normal companion app
from reading WeChat, Douyin, Xiaohongshu, or another app's private database.
Mobile software can later accept explicit shares, use selected official system
APIs, or—on Android where appropriate—observe user-authorized notification
metadata. Those are partial, user-mediated inputs, not a universal extraction
mechanism. Keeping the authoritative replica on the Mac also minimizes the
privacy, security, consistency, and key-management surface. Remote access can
later be provided through a user-controlled secure channel without making a
hosted copy the system of record.

### Source priority

Decision: prioritize WeChat conversations, people and groups, attachments,
quotes, replies, edits/recalls, and action results. These directly encode
relationships, obligations, coordination, and document exchange.

Cached Moments and public-account material are optional later sources. Passive
recommendation histories from Douyin and Xiaohongshu are lower priority because
they are often lower-signal, much harder to acquire lawfully and continuously,
and outside the WeChat-specific repository boundary. Deliberate saved/shared
items could become inputs to a separate connector if a supportable acquisition
route appears.

### Read-only versus action-capable

The alternatives were a permanently read-only archive, write-first automation,
and a read foundation followed by separately gated actions.

Decision: build the read foundation first, investigate read and write
feasibility immediately, and productize writes second. A read-only connector is
useful for recall and research, but the highest-value assistant loop often ends
with replying to an existing person or group or sending a requested document.
Conversely, write-first automation lacks the reliable identity, context,
deduplication, and audit state needed to avoid irreversible mistakes.

The first action experience is draft-and-confirm, not silent autonomous send.
GreenBubbles must show the exact account, contact/conversation, reply target,
text, and attachments; require an approval bound to that immutable draft; use
idempotency keys; record an append-only audit trail; and reconcile the official
client's resulting state. Allow-listed autonomous rules may be considered only
after confirmed actions are reliable and require a new policy decision.

### Source-faithful model versus universal personal ontology

Decision: normalize WeChat storage into a lossless, source-faithful WeChat
model, not a universal model of a person's life. The canonical records include
accounts, conversations, participants, messages, quotes/replies, attachments,
edits/recalls, source identifiers, provenance, freshness, coverage, and
authorization metadata. Unknown payloads remain recoverable instead of being
forced into a guessed semantic category.

Cross-application person matching, inferred interests or commitments,
embeddings, autonomous summaries, and long-term Markdown synthesis belong in a
downstream context or memory system. `get_changes(cursor)` is the important
boundary: it lets those systems incrementally consume GreenBubbles without
turning this repository into one of them.

### Agent integration

Decision: provide stable, machine-readable, agent-neutral surfaces. A CLI and
JSON/JSONL are the lowest common denominator; a local API supports long-running
synchronization; MCP provides convenient typed tools. OpenClaw is an integration
target, not the owner of the data model or process lifecycle. Codex, Claude,
scripts, search interfaces, and personal-memory projects should be equally able
to consume the connector.

## Research evidence informing the decisions

As of 2026-08-27:

- [Karpathy's LLM Wiki](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f)
  is a compelling pattern for compounding synthesis: immutable curated sources
  feed an LLM-maintained Markdown wiki and an instruction/schema document. It
  assumes the raw sources already exist. GreenBubbles should supply a private
  source to systems following that pattern rather than reproduce the wiki
  layer.
- Personal-memory projects including
  [claude-mem](https://github.com/thedotmack/claude-mem),
  [Mem0](https://github.com/mem0ai/mem0),
  [Khoj](https://github.com/khoj-ai/khoj),
  [Supermemory](https://github.com/supermemoryai/supermemory),
  [qmd](https://github.com/tobi/qmd),
  [GBrain](https://github.com/garrytan/gbrain), and
  [MemOS](https://github.com/MemTensor/MemOS) demonstrate both demand and a
  crowded category. Their existence strengthens the case for interoperable
  acquisition rather than another all-purpose memory engine.
- OpenClaw's [wacli](https://github.com/openclaw/wacli) demonstrates the useful
  focused-connector pattern: continuous synchronization, a local searchable
  replica, coverage reporting, read-only operation, and structured output for
  humans, scripts, and agents. GreenBubbles can be understood as “wacli for
  WeChat” at the product level, without assuming the same protocol technique.
- Projects such as [WeChatMsg](https://github.com/LC044/WeChatMsg),
  [PyWxDump](https://github.com/xaoyaoo/PyWxDump),
  [chatlog](https://github.com/sjzar/chatlog), and
  [WechatExporter](https://github.com/BlueMatthew/WechatExporter) prove demand
  for access to a user's own archive. They also show that a one-time exporter,
  fragile live reader, or unsupported decoder is not enough. GreenBubbles'
  differentiation must be current-version support, lossless restoration,
  continuous reconciliation, coverage/freshness evidence, safe agent surfaces,
  and—if the gate can be passed—ordinary-contact actions.
- Tencent's [official OpenClaw Weixin channel](https://github.com/Tencent/openclaw-weixin)
  offers a sanctioned bot relationship for receiving and replying inside that
  channel. Its documented surface does not provide general historical access
  to the user's existing ordinary chats or the ability to act in those chats,
  so it is complementary rather than a replacement.

Research popularity is evidence of demand, not proof that GreenBubbles can be
distributed safely. The acquisition and action gates below remain controlling.

## Design principles

- **Local first:** raw databases, encryption material, and full histories stay
  on the user's Mac.
- **Connector independence:** GreenBubbles remains useful without a context hub,
  agent host, remote model, or another connector.
- **Least privilege:** expose typed operations over allow-listed conversations,
  never raw SQL or arbitrary internal calls to an AI.
- **Passive-read isolation:** parse consistent copies, not WeChat's live files.
  Preserve database, WAL, and SHM files as one snapshot unit, and never let the
  passive path acquire a write handle or action capability.
- **Source-faithful replica:** retain lossless source identities and explicit
  unknowns while adding stable indexes, provenance, freshness, and coverage for
  consumers.
- **Version pinned:** fingerprint the official client and storage schema; unknown
  versions fail closed.
- **Evidence driven:** distinguish cached, observed, and server-fetched data and
  retain provenance for every normalized record.
- **No stealth:** do not attempt to evade anti-tamper, environment, or account
  security controls.
- **Action accountability:** drafts, approvals, attempts, and observed results
  are immutable, auditable state transitions; approval never transfers between
  accounts, recipients, payloads, or versions.
- **Safe AI boundary:** connector policy, consent, rate limits, and audit logic
  remain deterministic and outside the model.

## Phase 0 — repository and safety baseline

Status: **complete for private development; public-release prerequisite remains**

- [x] Create a private personal GitHub repository.
- [x] Document scope, authorization requirements, and sensitive-data policy.
- [x] Ignore database, key, capture, and private-fixture artifacts.
- [x] Establish separate passive-read, active-read, and write privilege tiers.
- [ ] Select an open-source license before making the repository public.

Exit gate: secrets and real user artifacts are excluded by default, and the
project can evolve without conflating read access with permission to write.

## Phase 0.5 — acquisition, action, and public-distribution gate

Status: **required; not yet passed**

This gate applies before adding automated key acquisition, private-client
modification, active reads, write automation, or public source/binary
distribution. Existing restoration work that accepts an owner-supplied
passphrase does not by itself answer whether a complete public acquisition path
is technically and legally supportable.

### Technical acquisition gate

- [ ] Confirm that useful conversation and attachment data exists on a pinned,
  current WeChat macOS version and document its locally available coverage.
- [ ] Determine whether restoration can work without modifying or re-signing
  WeChat, attaching to its process, scanning process memory, or exporting
  reusable session credentials.
- [x] Evaluate user-created official backups, exports, and owner-supplied
  plaintext/passphrase workflows before any more invasive alternative.
- [ ] Prove bootstrap and incremental synchronization on disposable test data;
  measure idle, one-message, burst, edit, recall, deletion, and crash-recovery
  cases.
- [x] Fingerprint the client and schema precisely and prove that unknown or
  partially understood versions fail closed rather than silently losing data.

The signed client fingerprint binds bundle/build, executable, signing team,
CodeDirectory, architectures, Hardened Runtime, and signature validity. Archive
coverage format 3 now adds deterministic whole-profile and per-table schema
fingerprints without emitting schema SQL. Build drift is incompatible;
unhandled message candidates and unknown logical types remain raw-retained,
machine-readable completion gaps; incremental merges recompute the schema
profile. This satisfies the fingerprint/fail-closed item but not the remaining
real-corpus and disposable-account requirements in this gate.

The bounded acquisition assessment statically confirms that the pinned client
contains user-mediated backup/restore, chat-history migration, device-transfer,
and file-export workflows. It does not prove a portable plaintext export,
official-backup compatibility, or complete conversation/media coverage.
GreenBubbles accepts only owner-supplied plaintext snapshots or a passphrase
through standard input and has no automated key acquisition. The preferred
order is official portable export/backup, then owner-supplied plaintext or
passphrase, then stop; there is no invasive fallback. See
`docs/ACQUISITION_FEASIBILITY.md`.

### Ordinary-contact action feasibility gate

- [ ] On a disposable test account and pinned client, determine whether the
  existing official client can support text, reply, and file actions to an
  ordinary contact or group through a sanctioned or otherwise supportable
  user-authorized mechanism.
- [ ] Require a visible, user-mediated experiment; do not defeat anti-tamper,
  environment, account, or integrity controls.
- [ ] Prove exact recipient/conversation resolution, idempotency, and observable
  sent/failed state without treating an internal call's return value as
  delivery.
- [ ] Determine the client-version fragility, account-risk, maintenance cost,
  and automatic fail-closed signal before choosing an action adapter.

Researching this feasibility belongs in Phase 0.5 even though shipping write
actions belongs after the read, replica, and draft foundations. A negative
result materially changes the product promise and must be learned early.

### Legal and distribution gate

The current
[Weixin software agreement](https://weixin.qq.com/cgi-bin/readtemplate?lang=zh_CN&t=weixin_agreement&s=default)
restricts reverse engineering, unauthorized access to internal components or
data, process-memory copying, and unauthorized automation. Several WeChat
extraction projects report removing code after legal complaints, and GitHub has
processed WeChat-related DMCA notices, including notices dated
[2026-07-13](https://github.com/github/dmca/blob/master/2026/07/2026-07-13-wechat-3.md)
and
[2026-08-24](https://github.com/github/dmca/blob/master/2026/08/2026-08-24-wechat-4.md).
These are allegations and enforcement evidence, not a legal conclusion, and
local ownership of data does not by itself resolve contract or
anti-circumvention questions.

Before a public release, obtain qualified legal review and:

- [ ] assess source distribution, binary distribution, schema documentation,
  sanitized fixtures, and hosted repository exposure separately;
- [ ] determine whether applicable interoperability, portability, research, or
  other exceptions cover the exact planned mechanisms and jurisdictions;
- [ ] explore explicit Tencent permission or a sanctioned portability/action
  route;
- [ ] establish a response plan for client updates, account complaints,
  takedowns, security reports, and maintainer/host exposure;
- [ ] document which components may be published and which experiments, if any,
  must remain private.

This plan is not legal advice.

Kill criterion: if the only viable public implementation requires distributing
key-extraction or circumvention tooling and qualified counsel considers that
untenable, do not disguise the mechanism by rebranding it as a context engine.
Keep the research private, accept only lawfully supplied plaintext/exports,
seek an official route, or stop.

## Phase 1 — passive local data foundation

Status: **in progress**

### 1A. Discovery and inventory

- [x] Detect known WeChat application bundles and sandbox roots on macOS.
- [x] Record client version and bundle identifier when available.
- [x] Classify database, SQLite sidecar, index, configuration, and media
  candidates without opening their contents.
- [x] Redact paths by default and cap traversal depth and artifact count.
- [x] Test discovery and classification using synthetic filesystem fixtures.
- [ ] Verify known root candidates across Intel/Apple Silicon and at least two
  WeChat desktop versions without collecting personal contents.

### 1B. Consistent read-only snapshots

- [x] Define a snapshot manifest containing source fingerprint, file identity,
  size, modification time, and cryptographic digest.
- [x] Copy database/WAL/SHM sets into an owner-only temporary directory.
- [x] Detect mutation during copying and retry or reject inconsistent snapshots.
- [x] Prove with tests that the source tree is never opened for writing.
- [x] Securely clean up connector-created snapshots on expiration.

### 1C. Storage format research

- [x] Create generated databases and sanitized binary fixtures covering known
  structural patterns.
- [x] Identify database families, page format, schema/version markers, and
  serialization formats from authorized test data.
- [x] Keep any key-handling experiment in a small local component with no
  network access or model/log exposure.
- [x] Document uncertainty; never label guessed fields as verified.

### 1D. Normalized conversation reader

- [x] Define the lossless restoration and integrity contract in
  `docs/RESTORATION_SPEC.md`.
- [x] Define stable models for accounts, conversations, participants, messages,
  attachments, quotes, recalls, and provenance.
- [x] Implement cursor-based incremental reads and deterministic deduplication.
- [x] Reconcile contact/group identifiers without leaking identifiers into logs.
- [x] Expose only explicitly enabled conversations.
- [ ] Enumerate every message-bearing shard/table and prove
  `source rows = restored rows + rejected rows` with zero silent drops.
- [ ] Decode every observed logical message type while retaining unknown raw
  payloads and treating unknown types as an incomplete semantic restoration.
- [ ] Resolve every locally downloaded multimodal/file artifact to a verified
  local path and represent missing/remote-only media explicitly.

### 1E. Incoming event reconciler

- [x] Observe filesystem changes as a wake-up hint.
- [x] Evaluate macOS notification accessibility only as an optional low-latency
  hint; do not depend on it for completeness.
- [x] Reconcile against normalized message identifiers and periodically recover
  missed/duplicate events.

Exit gate: on supported, pinned versions, the bridge can reproduce selected
cached conversations from a consistent snapshot without modifying the source
or exposing raw keys/data outside the local boundary.

## Phase 2 — encrypted local replica and continuous synchronization

Status: **in progress; canonical replica and transactional synchronization are implemented**

### 2A. Canonical replica

- [x] Store canonical accounts, conversations, participants, messages,
  attachments, replies/quotes, edits/recalls, and unknown payload references in
  an owner-only encrypted local replica.
- [x] Keep source identities and provenance sufficient to reproduce every
  normalized record and diagnose decoder or schema changes.
- [x] Isolate accounts cryptographically and logically; never let a policy,
  cursor, contact identity, or action from one account resolve in another.
- [x] Retain freshness, coverage, rejection, and schema/version state alongside
  content so an agent can distinguish “not present” from “not synchronized” or
  “not understood.”
- [x] Make migrations transactional and retain a recoverable pre-migration
  state.

### 2B. Change-proportional synchronization

- [x] Bootstrap once from a consistent snapshot, then process only plausible
  changed shards/ranges plus a bounded reconciliation window.
- [x] Treat filesystem and optional notification events only as wake-up hints;
  make source comparison and restoration authoritative.
- [x] Advance source checkpoints and replica mutations in one transaction.
- [x] Reconcile additions, edits, recalls, deletions, attachment availability,
  and late-arriving rows without emitting duplicate logical events.
- [x] Merge selected changed-shard fragments into the prior authoritative
  archive by source identity before replica mutation; recompute global ordering,
  relationships, coverage, and verified connector-owned media paths.
- [x] Run occasional bounded integrity scans to recover from missed hints,
  timestamp anomalies, decoder upgrades, and checkpoint damage.
- [x] Keep attachment extraction, media decoding, thumbnails, and indexing off
  the path that makes new text searchable.
- [x] Publish an ordered, resumable `get_changes(cursor)` stream for downstream
  consumers.

### 2C. Retrieval and operational evidence

- [x] Add exact full-text search and structured filters for conversation,
  participant, direction, message type, time, reply target, and attachment.
- [x] Report synchronization health, last authoritative checkpoint, lag,
  supported version, enabled scope, known coverage gaps, and semantic decoder
  coverage.
- [x] Benchmark bootstrap and steady state on small and large authorized test
  archives.
- [x] Measure idle, one-message, burst, edit, recall, deletion, missed-hint,
  decoder-upgrade, and crash/restart cases against the service objectives.
- [x] Never let embeddings or LLM summaries become required for exact retrieval
  or lossless synchronization.

Exit gate: a supported account can remain synchronized without repeated full
exports, new locally persisted text is searchable within 60 seconds at p95,
idle overhead is negligible, checkpoints survive crashes without silent loss,
and coverage/freshness limitations are machine-readable.

Acquisition evidence format 2 carries the last full integrity-scan anchor. A
snapshot planned from a prior manifest automatically becomes an integrity scan
when the configured maximum age is reached (seven days by default), selects
every current database/WAL/SHM set, and records a new anchor. Normal
incrementals carry the anchor forward, while `--integrity-scan` remains an
explicit immediate override. Replica status separately reports the last
committed integrity-scan time and age.

## Phase 3 — agent-neutral connector service and drafts

Status: **complete for replica-backed reads and non-executing drafts**

### 3A. Stable connector surface

Provide equivalent typed behavior through CLI/JSON, a local API, and MCP where
appropriate:

```text
capabilities()
status()
coverage()
bootstrap()
synchronize()
get_changes(cursor)
list_conversations()
search_messages()
get_messages()
get_message()
refresh()
```

- [x] Keep JSON/JSONL schemas stable and versioned so scripts and downstream
  memory systems do not require MCP or a particular agent host.
- [x] Make `capabilities()` report read, draft, text-send, reply, and file-send
  support independently, including a machine-readable reason when unavailable.
- [x] Build a local authorization service with account, conversation, field,
  time-range, and operation scopes.
- [x] Minimize context before any remote-model request and support a fully local
  query/model path.
- [x] Prevent message content and prompt injection from expanding deterministic
  scopes or enabling unavailable tools.

### 3B. Draft-only action layer

Add non-executing operations before any send capability:

```text
resolve_contact()
resolve_conversation()
create_message_draft()
create_reply_draft()
create_attachment_draft()
preview_action()
```

- [x] Bind every draft to the account, immutable recipient/conversation
  identity, optional reply target, exact rendered text, attachment digests,
  connector version, expiration, and policy decision.
- [x] Show human-readable recipient and group details alongside stable internal
  identities to catch ambiguous or stale contact resolution.
- [x] Add an append-only, owner-only audit log that can omit message bodies but
  records who/what requested, reviewed, approved, attempted, and reconciled an
  action. Draft request/review stages are live; approval/attempt/reconciliation
  stages remain unproducible until the separately gated Phase 4 lifecycle.
- [x] Ensure creating or previewing a draft cannot mutate WeChat state.

Exit gate: GreenBubbles is independently useful to people, scripts, OpenClaw,
Codex, Claude, and downstream memory engines, and hostile source content cannot
access another conversation or turn a draft into an action.

## Phase 4 — confirmed actions to ordinary contacts

Status: **planned; implementation requires Phase 0.5 and Phases 1–3 gates**

This is the second core product capability, not an official-bot integration.
The goal is to act in the user's existing conversation with a real friend,
colleague, client, or group through the already-running official client, if a
supportable mechanism exists.

### 4A. Minimal confirmed action

- [ ] Re-evaluate the selected mechanism, WeChat version, platform rules,
  counsel guidance, and account safety immediately before implementation.
- [ ] Start with confirmed text sending to one allow-listed disposable test
  conversation.
- [ ] Require an approval token bound to the exact immutable draft; any edit,
  recipient change, attachment change, expiration, or connector upgrade
  invalidates approval.
- [ ] Enforce idempotency keys, rate limits, a global kill switch, and
  unknown-version fail closure outside the AI/model process.
- [ ] Model the action lifecycle as drafted, approved, attempted, observed-sent,
  observed-failed, or unknown; never call an internal acknowledgement
  “delivered.”
- [ ] Reconcile the resulting official-client message back into the canonical
  replica and link it to the action/audit record.

### 4B. Replies and documents

- [ ] Add reply-to-message only when the target identity and resulting quoted
  state can be verified.
- [ ] Add file sending with an immutable digest, exact filename/type/size
  preview, local allow-list, and revalidation immediately before the attempt.
- [ ] Treat images, reactions, contact cards, group membership/mentions,
  payments, calls, deletions, and Moments mutations as separate capabilities
  requiring their own risk review and exit gate.
- [ ] Consider narrowly defined allow-listed autonomous actions only after
  confirmed text/reply/file actions have strong operational evidence; never
  inherit autonomy from a read or draft permission.

Tencent's official bot channel may be offered as a separate sanctioned adapter
for workflows addressed to that bot. It must be labeled with its actual
coverage and must not be presented as access to ordinary personal chats.

Exit gate: retries, prompt injection, ambiguous contacts, client upgrades,
partial failures, crashes, and model misbehavior cannot produce a duplicate or
unauthorized external action, and every attempted action has a reconciled or
explicitly unknown outcome.

## Phase 5 — optional cached and authenticated read surfaces

Status: **passive cached reads implemented; authenticated active reads remain gated**

### 5A. Cached dynamic content

- [x] Locate and normalize Moments and public-account data that the official
  client has already cached only when it supports a proven user workflow.
- [x] Label cache completeness, observation time, and source explicitly.
- [ ] Parse public web articles only when normal URL access permits it; preserve
  authentication, robots, copyright, and paywall boundaries.

The pinned client has an observed `sns/sns.db`. GreenBubbles now recognizes
only the exact supported `SnsTimeLine` and `SnsMessage_tmp3` signatures,
retains raw provenance for every normalized row, records every other SNS table
as unsupported schema coverage, and labels all output `partialLocalCache`.
Cached public-account messages in supported business-message shards already
flow through the conversation model. Fetching an external article body is not
inferred from a cached link. A separate, explicit
`greenbubbles-public-article` process can fetch one ordinary public
`https://mp.weixin.qq.com/s...` URL. It has no replica/restoration dependency,
cookies, credentials, client session, crawler, or subresource loading; it
checks robots first, rejects authentication and visible paywall boundaries,
bounds redirects and bytes, and labels the result `singlePublicPage`. The
result remains untrusted source content and is not inserted into the canonical
replica. See `docs/PUBLIC_ARTICLE_FETCH.md`.

The helper is implemented and synthetically tested, but this item remains
unchecked operationally. The official `https://mp.weixin.qq.com/robots.txt`
observed on 2026-08-27 returns HTTP 200 with `Disallow: /` for all agents and no
allow rule covering `/s`. GreenBubbles consequently returns `robotsDenied`
before requesting an article. No article was fetched in the live validation;
the helper may become available only if normal public access and the published
robots policy permit the exact path in the future.

### 5B. Active read feasibility gate

- [x] Map official local IPC boundaries before considering in-process or network
  approaches, using a static signed-bundle inventory only.
- [ ] Determine whether a high-level, read-only operation can reuse the existing
  logged-in client without extracting reusable session credentials.
- [ ] Prototype only on a disposable test account and pinned client version,
  subject to Phase 0.5.
- [ ] Stop if the approach requires defeating security controls, weakens the
  account, or cannot fail closed after version drift.

### 5C. Narrow optional API

- [x] Add typed operations such as `get_cached_moments` and, only after the
  feasibility gate, `load_more_moments(cursor)`.
- [x] Bound pagination, rate, retention, and per-source authorization.
- [x] Keep active-read health, scopes, and failure independent from local
  conversation synchronization and write capabilities.

`get_cached_moments` is available through the encrypted replica, connector
JSON/Unix API, CLI, and MCP. Its cursor is checkpoint/filter bound; policy has
independent fields, time retention, and destination scope; page and text sizes
are capped; and the service applies a 60-request rolling minute limit. Raw
columns/XML are never released by the AI-facing view. `load_more_moments` is
unavailable because authenticated active-read feasibility has not passed Phase
0.5.

For the exact pinned build, the static inventory now records URL schemes,
helper apps, Share and File Provider extension points, the bundled XPC service,
app-group and Mach-lookup entitlements, named data-access allow-lists, and
bundled frameworks. Each item is classified by what its signed metadata proves.
Inbound handoff, system-managed storage, an internal service name, or a private
framework does not prove a third-party authenticated read contract. No such
contract is currently proven, so active message/Moments reads remain
unavailable. See `docs/ACTIVE_READ_FEASIBILITY.md`.

Exit gate: optional authenticated reads are isolated, version-gated, auditable,
and cannot weaken the reliable local conversation path or authorize a write.

## Phase 6 — ecosystem validation before abstraction

Status: **implemented for one MCP host and one resumable downstream workflow**

- [x] Integrate the stable connector with at least one existing agent host and
  one downstream memory/synthesis workflow.
- [x] Prove that downstream consumers can bootstrap and then remain current via
  `get_changes(cursor)` without scraping CLI prose or accessing the replica
  database directly.
- [x] Document a minimal source connector contract based on GreenBubbles'
  production behavior.
- [ ] If a second source is worth pursuing, implement it in a separate
  repository and only then compare contracts and extract shared code.

A universal context hub should be reconsidered only after concrete cross-source
workflows require at least several of the following: shared governance,
cross-application identity reconciliation, durable derived assertions,
agent-independent memory, or transactional queries across sources. Popularity
of the “personal memory” label alone is not a reason to add that layer.

The evidence is intentionally concrete: the real stdio MCP adapter completes
initialize/list/status-call against the real Unix service on synthetic data;
Claude Code 2.1.247 reports the temporarily registered server connected; and
the runnable change consumer bootstraps, persists an owner-only cursor/state,
builds an escaped Markdown memory projection, resumes, and fails closed across
replica replacement until explicit rebootstrap. See
`docs/ECOSYSTEM_VALIDATION.md`, `docs/DOWNSTREAM_CONSUMER.md`, and
`docs/SOURCE_CONNECTOR_CONTRACT.md`.

## Technical architecture

```text
Official WeChat local stores
          |
          v
version-pinned passive-read adapter
          |
consistent database/WAL/SHM snapshots
          |
lossless restoration + coverage evidence
          |
encrypted canonical WeChat replica
          |
incremental reconciliation + exact/FTS indexes
          |
CLI / JSONL / local API / MCP / get_changes(cursor)
          |
agents, scripts, search UIs, and downstream memory engines

agent request
     |
draft + deterministic policy + exact human preview
     |
approval-bound action adapter (separately gated)
     |
already-running official WeChat client
     |
observed result -> audit log + replica reconciliation
```

Optional authenticated reads and official-bot workflows attach as separately
scoped adapters. They do not bypass or inherit passive-read, ordinary-contact
write, or conversation authorization.

The current implementation combines a Swift package targeting macOS 14 or
later with a native Rust restoration engine. As the implementation grows, use
these logical boundaries; they need not become separate packages before the
interfaces stabilize:

- `GreenBubblesCore`: authorization-neutral identifiers, provenance, coverage,
  capabilities, and shared models.
- `GreenBubblesWeChatAdapter`: discovery, version fingerprints, consistent
  source acquisition, and WeChat-specific decoding/restoration.
- `GreenBubblesReplica`: encrypted canonical storage, migrations, FTS, and
  structured retrieval.
- `GreenBubblesSync`: bootstrap, checkpoints, reconciliation, change cursors,
  and health/lag reporting.
- `GreenBubblesActions`: drafts, approvals, outbox state, action adapter, audit,
  and result reconciliation.
- `GreenBubblesCLI` and `GreenBubblesMCP`: human- and agent-facing surfaces over
  the same policy-enforcing local service.

Do not expose raw SQL, encryption material, arbitrary internal calls, or a
private application schema as the public connector interface.

## Repository boundary

This repository includes:

- WeChat-specific discovery, acquisition, restoration, and version support;
- the canonical encrypted WeChat replica and incremental synchronizer;
- provenance, freshness, semantic/source coverage, and authorization metadata;
- exact full-text and structured retrieval;
- CLI, versioned JSON/JSONL, local API, and MCP surfaces;
- draft and, only after its gates, ordinary-contact action capabilities;
- thin examples or adapters for existing agent and memory systems.

It does not initially include:

- Telegram, iMessage, WhatsApp, Slack, email, Douyin, Xiaohongshu, or other
  connector implementations;
- a universal person/activity ontology or cross-application identity graph;
- a general memory engine, Markdown wiki maintainer, agent runtime, embeddings,
  inferred commitments/interests, or autonomous summaries;
- hosted raw-context storage, cloud synchronization, or multi-master
  cross-device replication;
- an iOS/Android mechanism that claims to read other apps' private databases.

## Near-term work queue

1. Keep the repository private. Obtain an authorized disposable/test corpus by
   official portable export/backup, owner-supplied plaintext, or passphrase
   through standard input; do not add an invasive fallback.
2. Verify discovery on Intel and Apple Silicon across two explicitly
   fingerprinted client versions using redacted metadata only.
3. On one immutable pinned-current-version corpus, close row accounting,
   observed logical-type coverage, relationships, and every downloaded/missing
   media state together; retain sanitized regressions only.
4. Prove real-client bootstrap and change-proportional synchronization for idle,
   one-message, burst, edit, recall, deletion, missed-hint, integrity-scan, and
   crash/restart cases against the 60-second p95 objective.
5. Obtain the legal, supportability, Tencent-route, and public-distribution
   decisions required by Phase 0.5. Keep active reads and actions unavailable
   in their absence.
6. Only if those gates approve it, run one visible, allow-listed disposable
   active-read or ordinary-contact feasibility experiment and accept a negative
   result without switching to stealth, credential export, or protocol spoofing.
7. Re-check the published robots policy before an explicitly requested public
   article fetch; remain fail-closed while `/s` is disallowed.
8. Treat a license, public release, or separate second connector as explicit
   repository-owner product decisions, not inferred implementation tasks.

The exact evidence needed to resume each unchecked item is mapped in
`docs/GATE_READINESS.md`.

## Product proof and positioning

The connector must demonstrate more than a historical dump. A representative
end-to-end proof is:

> Find the document Alice requested in prior WeChat context, prepare an
> appropriate reply, show the user the exact Alice/conversation, message, and
> file, and—after explicit confirmation—send it and reconcile the result.

Preferred public framing includes “user-controlled interoperability,” “local
WeChat context,” “continuous personal archive,” “read-only agent bridge” for the
first release, and “approved actions to your real contacts” when that capability
is supportable. Do not describe the work as WeChat “exploitation,” and do not
hide technical or legal limitations behind the broader “personal context
engine” label.

## Non-goals for the initial milestones

- A replacement WeChat client or server, or a second simultaneous login.
- Anti-detection, anti-tamper bypass, credential/session export, or protocol
  spoofing.
- Silent autonomous sending, account management, payments, or broad group
  administration.
- Uploading complete chat histories or the authoritative replica to a hosted AI
  service.
- A universal personal-memory/context product or multiple connector
  implementations in this repository.
- Exhaustive recommendation-feed capture or a claim to observe most mobile-app
  activity despite platform sandboxing.
- Supporting every WeChat version before one pinned current-version path is
  reliable and its coverage is measurable.

## Decision review triggers

Revisit—not silently expand—these decisions if:

- Tencent offers a sanctioned history, portability, ordinary-contact action, or
  local-agent interface;
- current macOS acquisition or action paths fail the Phase 0.5 gate;
- a Windows adapter provides materially better lawful coverage;
- a second connector implemented outside this repository proves a reusable
  contract;
- several high-value workflows require cross-source identity, governance, or
  durable derived context that cannot live cleanly in a downstream system;
- users demonstrate that deliberate feed saves/searches are more valuable than
  the present conversation-first priority.
