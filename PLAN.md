# GreenBubbles product and engineering plan

Last updated: 2026-08-28

## Strategic decision summary

GreenBubbles will be a focused, standalone WeChat connector and local service,
not a universal personal-context engine and not a monorepo of unrelated
connectors.

> GreenBubbles continuously synchronizes a user's own WeChat history into a
> private, searchable local replica that any authorized AI agent can query and,
> in a separately approved second product phase, use to reply or send files to
> the user's real WeChat contacts.

The connector must be independently useful from a one-shot CLI, JSON/JSONL, a
repository agent skill, and a local API. OpenClaw, Codex, Claude, a
Karpathy-style LLM-maintained wiki, or another memory system may consume it, but
GreenBubbles will not depend on any one agent host, protocol, or memory engine.

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

Decision: provide stable, machine-readable, agent-neutral surfaces. A one-shot
CLI and JSON/JSONL are the primary agent interface; a local API may support
long-running synchronization independently. A repository skill teaches modern agent hosts to
use the same CLI without creating another protocol or trust boundary. OpenClaw
is an integration target, not the owner of the data model or process lifecycle.
Codex, Claude, scripts, search interfaces, and personal-memory projects should
be equally able to consume the connector.

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
- **Version-aware:** fingerprint the official client and storage schema; accept
  the signed WeChat 4.1+ family while retaining explicit schema/type drift and
  failing closed only for incompatible identity, malformed evidence, or
  unsupported observed structures.
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
is technically and legally supportable. By explicit owner decision on
2026-08-27, one bounded exception to this ordering now exists: the
`greenbubbles-acquire` owner-authorized passphrase-capture helper
(`docs/PASSPHRASE_ACQUISITION.md`) for the owner's own account and device,
private-development only. It does not pass any part of this gate, and the
legal/distribution and action items below remain fully in force.

### Technical acquisition gate

- [ ] Confirm that useful conversation and attachment data exists on a pinned,
  current WeChat macOS version and document its locally available coverage.
- [ ] Determine whether a lawful owner-authorized acquisition route exists —
  including, as a separately gated fallback, re-signing WeChat and attaching to
  its process under explicit owner authorization; memory scanning and reusable
  session-credential export remain prohibited.
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
fingerprints without emitting schema SQL. Passive restoration accepts signed,
Hardened-Runtime official WeChat versions 4.1 and later; build, executable-hash,
CodeDirectory-hash, and architecture drift are recorded as compatibility/audit
evidence rather than blocking a restore. Incompatible version/signing identity
or malformed evidence still fails closed. Unhandled message candidates and
unknown logical types remain raw-retained, machine-readable completion gaps;
incremental merges recompute the schema profile. This satisfies the
fingerprint/fail-closed item but not the remaining real-corpus and disposable-
account requirements in this gate.

CI also extracts and compares every exact reference-profile field from the
Swift acquisition/static-inspection boundary and Rust restoration boundary. A
one-sided reference version, identifier, architecture, signing, executable-
hash, CodeDirectory-hash, Hardened Runtime, signature-validity, or profile-ID
change fails before merge. This keeps the debugger acquisition helper's exact
pin and cross-language reference evidence atomic without narrowing the passive
4.1+ compatibility family.

The bounded acquisition assessment statically confirms that the pinned client
contains user-mediated backup/restore, chat-history migration, device-transfer,
and file-export workflows. It does not prove a portable plaintext export,
official-backup compatibility, or complete conversation/media coverage. The
preferred acquisition order is official portable export/backup, then
owner-supplied plaintext or passphrase through standard input, then the gated
owner-authorized `greenbubbles-acquire` capture; if none is available or
authorized, acquisition for that account stops. The passive pipeline itself
still performs no automated key acquisition. See
`docs/ACQUISITION_FEASIBILITY.md` and `docs/PASSPHRASE_ACQUISITION.md`.

Owner-authorized local validation now adds real passive acquisition evidence:
two redacted account roots produced consistent 25-set and 15-set bootstrap
snapshots, every copied database was independently digest-verified and
classified as the pinned encrypted WCDB family, the idle account produced a
true no-op incremental, and fresh active-account evidence captured exactly 8
content-changed sets as 24 descriptor-based atomic APFS clones. The fresh
25-set bootstrap completed in 3,362 ms and its immediate incremental in 1,996
ms, but those capture timings omit source persistence, restoration,
publication, replica application, and disposable-scenario labels. Those
acquisition measurements themselves did not decrypt a database. A subsequent
GreenBubbles-only diagnostic restoration used an already exported owner-only
per-database key set: 25 of 26 snapshot databases authenticate, while one
auxiliary icon database has no available exported key. The workflow therefore
preserves explicit missing-key evidence and unconditionally marks
the archive `diagnosticSubset`; it does not convert the earlier timings into
real synchronization evidence. A separate content-free latest metadata pass
observed 136,786 attachment candidates (38,874,071,097 bytes) in the larger
attachment root and 87 (1,469,805 bytes) in the smaller root. One clean repeat
completed without traversal issues, symbolic links, or a cap hit; it did not
prove message/media linkage. See
`docs/LOCAL_ACQUISITION_VALIDATION.md`.

A refreshed public-project survey found no ordinary user-visible or officially
documented non-invasive source for the current macOS database passphrase.
Available projects either instrument or scan a live client, weaken platform
protections, re-sign the application, require an already known key, target
older mobile backups, or use a separate bot relationship. That finding remains
factually true. By explicit owner decision on 2026-08-27, the previous blanket
prohibition on debugger-based acquisition was lifted: the LLDB
`CCKeyDerivationPBKDF` capture mechanism was validated live on the owner's own
machine and account (26/26 databases HMAC-verified on the pinned 4.1.12 build)
and is embedded as the explicitly gated third acquisition path. Memory
scanning, injection, reusable session-credential export, automated re-signing,
and anti-detection work remain prohibited. See
`docs/ACQUISITION_FEASIBILITY.md` and `docs/PASSPHRASE_ACQUISITION.md`.

A fresh owner-authorized bootstrap/incremental pair independently reproduces
the incremental's 8 changed sets from the two complete 25-set content
inventories, with 24 copied entries versus 75 for bootstrap and exact baseline,
build, deletion, and reconciliation continuity. This strengthens real
change-proportional acquisition evidence but still does not establish decoded
message latency or semantic synchronization. See
`docs/ACQUISITION_CHAIN_AUDIT.md`.

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

The current factual dependency audit separates source, binary, schema/format
documentation, sanitized-fixture, real-data, hosted-repository, and research
publication categories. The Swift package has no external package dependency.
The Rust build pins `wx-cli` commit
`2abe708f55bfe135539a385df856fdc58f97fc74`; its repository carries an MIT
license while five selected package records omit inherited license metadata.
The native build also bundles SQLCipher and SILK C sources with their own
source/binary notice conditions. A deterministic CI check fails closed on
reviewed direct-dependency, revision, license-digest, native-package, or
publication-state drift. See `docs/DISTRIBUTION_INVENTORY.md`. This evidence
does not select GreenBubbles' license or complete any legal/publication item.

A private-development operational response draft now defines immediate release
holds, capability/build fail-closure, evidence quarantine, update revocation,
private-data exposure, security-report, complaint/takedown, notification, and
recovery procedures. It deliberately contains no invented contact or approval:
named owners, secure intake, counsel/security review, jurisdictional decisions,
and repository-host procedures remain required before the response-plan item can
be checked. See `docs/OPERATIONAL_RESPONSE_PLAN.md`.

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
- [x] Bind each new snapshot to exactly one account directory, include that
  evidence in the source fingerprint, and reject mixed account/database/WAL/SHM
  roots or incremental baselines without the same integrity-bound
  selected-account evidence.
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

The independent `audit-archive` pass now reopens the completed archive, derives
its per-table row/type/gap/entity/reference counts from the ledgers, verifies schema
profiles, bidirectional relationships, and every relationship-resolution state,
validates ordinary-message and cached-surface row provenance per source table,
re-derives account-scoped entity and sender identities from preserved sources,
requires exactly one preferred artifact for each known media-bearing message,
validates the complete availability/decode evidence state of every artifact,
resolves full `MessageResourceInfo`/`VoiceInfo` row provenance against schema
coverage, and descriptor-verifies every recorded downloaded or connector-derived
file against its identity, timestamps, size, and SHA-256. Its content-free
summary can make the eventual real-corpus evidence reproducible; it does not
replace the still-required observation of all real tables, types, and media
states. See `docs/ARCHIVE_AUDIT.md`.

Archive-audit format 2 now emits independently derived completion components
for row accounting, observed types, direction, entities, relationships, media
verification/decoding, archive scope, media phase, and client compatibility. It
also distinguishes non-empty message/media evidence from vacuous structural
success and permanently marks authorization, disposable-scenario provenance,
and observed-universe scope as external attestations. This makes the eventual
single-corpus Phase 1 decision machine-readable without claiming that synthetic
or encrypted evidence has passed it.

Merged-history and Finder/channel app messages now retain raw XML plus a
bounded, ordered, versioned structural projection, including recursively
embedded forwarded-message documents and namespace evidence. The independent
archive audit regenerates the projection from raw XML and rejects altered or
unjustifiably complete records while remaining compatible with legacy partial
archives. Synthetic fixtures close the previously known decoder-design gap;
they do not prove that every variant in a real compatible corpus has been observed,
so the logical-type completion item remains unchecked.

Real-corpus diagnostics are now incremental rather than an opaque decrypt-only
wait. Progress-event format 3 exposes snapshot bytes, existing-key
authentication, available/unavailable counts, per-database decrypt and WAL
work, table/row planning, per-table restoration records, cached-surface work,
ledger finalization, and independent audit. Human output shows workflow,
phase, and current-item percentages plus database/file ordinals, byte sizes,
record counts, gaps, and elapsed time. It coalesces repetitive tiny-table
chatter while the create-new owner-only NDJSON stream retains every event;
private JSON summary files support a UI or supervisor without exposing row
values.

Restoration now completes row planning with a fail-fast disk budget before any
archive file is created. It reports selected source, estimated archive,
compressed staging, peak, available, and required bytes; tracks actual staging,
compression, free-space, and published-archive bytes during the run; and stores
aggregate storage evidence in the final report. Canonical NDJSON remains
uncompressed and streamable. Only the private ordering spool uses per-record
Zstandard level-1 compression, with tested byte-for-byte round trips and a guard
that removes only its ephemeral directory on completion or propagated error.
The independent archive audit remeasures the complete archive and rejects
inconsistent storage equations or a retained ordering spool.

The completed privacy-safe aggregate establishes that the selected corpus
contains legitimate structured data rather than empty or nonsensical schemas.
GreenBubbles authenticated 25 of 26 databases, classified all 6,542 tables and
9,529,301 observed table rows as 6,291 message tables or 251 known auxiliary
tables, and found zero generic `other` tables or unhandled message candidates.
It restored 1,854,110/1,854,110 messages plus 23,589 cached Moments and 45,601
interactions (1,923,300 restored source records) with zero rejected rows,
duplicate canonical identities, unknown payloads, or cached-surface semantic
gaps. The independent seven-ledger audit reproduced every count. The archive
contains 4,581 conversations, 42,596 participants, and 235,108 explicitly
deferred artifact references.

The only message semantic gaps are two subtype `49:19` values that begin with
closing XML fragments and contain no opening `<msg>` or `<appmsg>` structure;
GreenBubbles retains the raw values and does not invent missing XML. Support
for adjacent `voipinvitemsg`, optional `voipextinfo`, and `voiplocalinfo` roots
closed 207 previously observed type-50 gaps. The aggregate also exposed a
quote-link adapter error: it searched compressed source columns after typed
decoding. Across all 193,503 relationship references, only 1 identifier was
already present; the privacy-safe profiler proved that 192,991 are recoverable
from source-preserving decoded XML, 511 are genuinely absent from that XML, and
0 lack decoded-XML evidence. The importer now extracts identifiers from that
already decoded XML while keeping original-column provenance unchanged.

This evidence still does not check the three Phase 1 real-source items. The
aggregate is a diagnostic subset, the run is not disposable-scenario
synchronization evidence, and media was intentionally deferred. The observed
signed 4.1.13 build belongs to the supported passive-restoration family, and
one unavailable database no longer blocks healthy restoration or replica
synchronization. Those improvements permit partial publication but do not turn
the diagnostic archive into production input or establish full-restoration,
active-read, or action evidence.

Snapshot format 4 now integrity-binds the selected account during export, and
restoration format 6 consumes that binding without an `--account-root`
argument. A corrected account-bound real-corpus rerun and its independent audit
remain pending; no account-bound direction counts are treated as evidence until
that run emits its create-new privacy-safe summary successfully. Direct peers,
contact names, message frequency, conversation shape, and group ownership are
never self heuristics.

### 1E. Incoming event reconciler

- [x] Observe filesystem changes as a wake-up hint.
- [x] Evaluate macOS notification accessibility only as an optional low-latency
  hint; do not depend on it for completeness.
- [x] Reconcile against normalized message identifiers and periodically recover
  missed/duplicate events.

Exit gate: on compatible signed 4.1+ versions, the bridge can reproduce selected
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

`audit-acquisition-chain` now independently verifies any retained bootstrap to
incremental/integrity-scan transition before restoration: it digest-verifies
copied entries, compares complete source inventories, accepts compatible 4.1+
client updates, and fails closed when the baseline, client compatibility,
deletion set, changed-set classification, or
reconciliation-only classification disagrees.

The continuous operator path now accepts atomic, generation-monotonic handoffs
from the isolated restoration process. A low-idle-overhead follower validates
each replica-eligible authoritative or partial-database archive, bootstraps or
synchronizes the encrypted replica,
and records crash-safe state bound to the random replica ID and committed
checkpoint. It cannot acquire a WeChat passphrase, inspect live stores, or
accept an unmerged incremental fragment. Synthetic restart, rollback,
equivocation, replacement, idempotency, bootstrap, and synchronization behavior
is covered. A bounded aggregate-only supervisor status reports published and
applied generations, generation lag, checkpoint age, and state-recovery needs
without exposing account/source identities or rescanning the archive; real
disposable-account latency evidence remains required. See
`docs/REPLICA_FOLLOW.md`.

An offline `restore-publish` transaction now removes manual sequencing between
an already acquired snapshot and that handoff. It requires a signed compatible
4.1+ build, independently proves every non-bootstrap snapshot chain against
both the retained prior snapshot and replica-eligible archive, restores and
audits a bounded fragment, preserves prior records when a selected database is
temporarily unavailable, merges and audits the new authoritative or explicit
partial-database archive, and allocates the next generation only if the
supplied predecessor is still the exact current sealed handoff, rejecting
concurrent stale branches. It has no live-store or replica-key access and does
not replace real-corpus evidence. See
`docs/OFFLINE_PIPELINE.md`.

Monotonic stage durations now cover snapshot planning/acquisition, offline input
validation, catalog preparation, restoration/merge, audit/publication, and
total runtime. The snapshot report remains owner-private because it includes
manifest/path material; the later reports are aggregate-only. Format-3 handoffs
add a private publication time so follower reports and status can expose
relative publication age, application runtime, and publication-to-checkpoint
latency without absolute timestamps. This makes the future 60-second
measurement reproducible; it does not manufacture the required
disposable-account samples.

A privacy-safe latency composer now binds the private snapshot report to the
current handoff source and publication generation, verifies the exact offline
transition and actual follower application, checks row/completion/timing
consistency, and emits only stage durations and aggregate coverage. Its sample
and nearest-rank p50/p95 summary schemas always preserve explicit missing
source-persistence, inter-command-delay, and disposable-scenario limitations;
they cannot claim the end-to-end objective from partial timing evidence. See
`docs/LATENCY_EVIDENCE.md`.

Publication now also retains an owner-only sealed generation history. A
recoverable retention operator verifies and protects at least the current and
immediately preceding publications (including shared physical archive paths),
then atomically moves only older archives into an owner-only same-filesystem
quarantine without deletion. Restore returns an archive to its exact original
path and re-audits it; interrupted moves reconcile from deterministic locations
and seals. This bounds active archive clutter without weakening rollback
recovery or exposing private paths. See `docs/ARCHIVE_RETENTION.md`.

Replica opening now independently verifies its singleton format/schema state
and the exact contiguous identity digest and timestamp record for every claimed
migration before creating a new pre-migration backup or changing data, then
verifies the complete ledger again after migration. Tampered, incomplete, or
unexpected history fails closed and requires restoration of a known-good
encrypted backup or explicit rebootstrap rather than silently legitimizing the
state.

A separate key-gated `audit-replica` transaction now independently verifies
SQLCipher/SQLite integrity, foreign keys, migration identities, every canonical
record hash and indexed projection, exact membership/relationship/artifact
links, the one-to-one FTS projection, checkpoint/coverage/completion identity,
sync/change history, and empty reconciliation staging. Its output is
aggregate-only and it never repairs state or replaces authoritative archive and
real-corpus audits. See `docs/REPLICA_AUDIT.md`.

Pre-migration backup creation now converts each encrypted recovery database to
a self-contained rollback-journal file, closes it, and runs a schema-aware
read-only content audit before migration begins. The same aggregate-only
`audit-replica-backup` command lets an operator recheck retained schemas 1–4;
wrong keys, unsafe file identities, current schemas, migration drift, record or
projection corruption, link/coverage inconsistency, and available
checkpoint/FTS/change-stream damage fail closed without rewriting the backup.
See `docs/REPLICA_BACKUP_AUDIT.md`.

A non-destructive `prepare-replica-recovery` transaction now turns a passing
schema-1 through schema-4 backup into a separate current-schema candidate. It
refuses existing or SQLite-namespace-overlapping output, makes a consistent
same-key encrypted copy, audits before and after migration, and preserves the
source backup and serving replica. Schema-4 cached Moments/interactions remain
fully audited and count-preserved; schema-1 migration backfills FTS,
checkpoint, synchronization, and initial change-stream state so the recovered
candidate passes the deep serving audit. Automated active cutover remains
intentionally out of scope. See `docs/REPLICA_RECOVERY.md`.

## Phase 3 — agent-neutral connector service and drafts

Status: **complete for replica-backed reads and non-executing drafts**

### 3A. Stable connector surface

Provide equivalent typed behavior through CLI/JSON and a local API where
appropriate. Agent workflows use the CLI through a repository skill:

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
get_artifact()
refresh()
```

- [x] Keep JSON/JSONL schemas stable and versioned so scripts and downstream
  memory systems do not require a particular agent host.
- [x] Make `capabilities()` report read, draft, text-send, reply, and file-send
  support independently, including a machine-readable reason when unavailable.
- [x] Build a local authorization service with account, conversation, field,
  time-range, and operation scopes.
- [x] Minimize context before any remote-model request and support a fully local
  query/model path.
- [x] Prevent message content and prompt injection from expanding deterministic
  scopes or enabling unavailable tools.
- [x] Add a one-shot, read-only `ai-query` CLI that accepts owner-only JSON,
  applies the same replica policy/audit boundary without a daemon, and attaches
  checkpoint, database freshness, and limitation evidence to every response.
- [x] Add an atomic, checkpoint-consistent `ai-export` projection containing
  normalized conversation, contact, message, relationship, and safe attachment
  JSONL plus a hashed manifest; expose record/file/percentage progress and never
  publish a mixed-generation bundle.
- [x] Bind AI-context v2 and replica schema 5 to the opaque account-holder
  participant, distinguish `groupOwnerParticipantId`, label self as `You`, and
  verify sender-versus-account direction while retaining v1 bundle readers.
- [x] Add an aggregate-only `audit-ai-context` verifier for permissions, exact
  inventory, schemas, hashes/counts, identities, references, freshness, and
  bundle/checkpoint/policy binding.
- [x] Package a discoverable `greenbubbles-context` skill that calls only the
  GreenBubbles CLI, treats source text as untrusted, requires coverage-aware
  conclusions, and neither acquires keys nor uses raw SQL.
- [x] Provide a native macOS SwiftUI history browser for verified static bundles
  with visible byte/record/percentage import, private atomic SQLite/FTS indexing,
  coverage-aware conversation/contact/search/timeline views, relationship
  navigation, and policy-reverified Quick Look for multimodal artifacts.

`get_artifact` resolves a message's opaque artifact reference only when the
request repeats an enabled conversation, `readRecentMessages` and the
`attachments` field are allowed, and a referencing message falls inside the
policy time range. It is local-only regardless of remote message permission and
descriptor/digest-verifies the source and decoded files immediately before
returning their exact paths. The one-shot CLI and optional local service share
this same operation and policy boundary.

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

New connector audit events are hash-chained under the append lock and the
service verifies the journal before startup. An independent privacy-safe audit
reports event/stage/outcome counts, chain integrity, and any unlinked legacy
prefix without emitting identities or bodies. This supplies tamper-detection
for current request/draft/review evidence; it is not a signature and does not
make the gated approval/attempt/reconciliation stages producible. See
`docs/CONNECTOR_AUDIT.md`.

A key-gated connector-state audit also recomputes every stored draft identity,
checks bounded owner-only files, separates current from stale/expired drafts,
requires one completed request event per draft, resolves completed reviews, and
rejects any gated lifecycle stage. Its report is aggregate-only and the command
is deliberately absent from agent-facing connector operations.

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

An adapter-independent offline safety contract now models the exact gate,
build, adapter, account, conversation, capability, immutable approval,
idempotency, kill-switch, rate-window, and lifecycle evidence a future action
boundary must check. It cannot mint or consume approvals, reserve an attempt,
invoke a client/network adapter, or expose an action through the CLI or Unix
API. Its synthetic fail-closed tests therefore strengthen the design without
completing any Phase 4 checkbox or substituting for adapter-bound concurrency,
restart, disposable-account, live-result, and legal/supportability evidence.
See `docs/ACTION_SAFETY_CONTRACT.md`.

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

`get_cached_moments` is available through the encrypted replica, one-shot CLI,
and optional connector JSON/Unix API. Its cursor is checkpoint/filter bound;
policy has independent fields, time retention, and destination scope; page and
text sizes are capped; and the service applies a 60-request rolling minute
limit. Raw columns/XML are never released by the AI-facing view.
`load_more_moments` is unavailable because authenticated active-read
feasibility has not passed Phase 0.5.

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

Status: **implemented for the CLI skill and one resumable downstream workflow**

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

The current proof is intentionally concrete: the repository
`greenbubbles-context` skill routes agents to the one-shot `ai-query` and
checkpoint-consistent `ai-export` commands; integration tests exercise policy-
scoped search, static normalized conversations/contacts/messages/artifacts,
coverage metadata, private file modes, path/raw-field suppression, write
rejection, and progress completion. The runnable change consumer separately
bootstraps, persists an owner-only cursor/state, builds an escaped Markdown
memory projection, resumes, and fails closed across replica replacement until
explicit rebootstrap. See
`docs/AI_CONTEXT_CLI.md`, `docs/DOWNSTREAM_CONSUMER.md`, and
`docs/SOURCE_CONNECTOR_CONTRACT.md`.

## Technical architecture

```text
Official WeChat local stores
          |
          v
version-aware passive-read adapter
          |
consistent database/WAL/SHM snapshots
          |
lossless restoration + coverage evidence
          |
encrypted canonical WeChat replica
          |
incremental reconciliation + exact/FTS indexes
          |
CLI / JSONL / skill / local API / get_changes(cursor)
          +-----------------------------+
          |                             |
          v                             v
agents, scripts, memory engines       native SwiftUI history browser

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
- `GreenBubblesCLI` and `greenbubbles-context`: human- and agent-facing surfaces
  over the same policy-enforcing local data boundary. The skill invokes the CLI
  and does not require a protocol server.
- `GreenBubblesHistory` and `GreenBubblesHistoryApp`: independent private-bundle
  verification, scalable derived indexing, coverage-aware navigation, and native
  multimodal preview through the existing read-only artifact operation.

Do not expose raw SQL, encryption material, arbitrary internal calls, or a
private application schema as the public connector interface.

## Repository boundary

This repository includes:

- WeChat-specific discovery, acquisition, restoration, and version support;
- the canonical encrypted WeChat replica and incremental synchronizer;
- provenance, freshness, semantic/source coverage, and authorization metadata;
- exact full-text and structured retrieval;
- CLI, versioned JSON/JSONL, a repository agent skill, a native history browser,
  and local API surfaces;
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
   official portable export/backup, owner-supplied plaintext, passphrase
   through standard input, or the gated owner-authorized `greenbubbles-acquire`
   capture, in that preference order.
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

The AI-facing normalization and retrieval milestone is now implemented: the
encrypted replica remains source-faithful, while `ai-query`, `ai-export`, and
the repository skill expose policy-minimized human labels, chat history,
participant/contact context, relationships, attachment metadata, freshness,
and partial-database coverage. Remaining items above require additional
machines, disposable test-account activity, legal/product decisions, or a
future client event; they are not safely inferable from the existing real
archive alone.

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
- Supporting pre-4.1 WeChat storage families or pretending every future schema
  is understood merely because its signed client version is in the 4.1+ range.

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
