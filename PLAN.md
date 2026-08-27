# GreenBubbles engineering plan

Last updated: 2026-08-27

## Outcome

Build a macOS-only, local-first bridge that lets a user authorize an AI assistant
to read narrowly selected WeChat information and, in a later separately gated
phase, request limited actions through the user's already-running official
client.

The bridge is intentionally split into three privilege tiers:

1. **Local passive read** — inspect authorized local stores without changing
   WeChat state or contacting its servers.
2. **Authenticated active read** — ask the existing client to fetch read-only
   dynamic content such as Moments pages or public-account metadata.
3. **Write actions** — request externally visible changes such as sending a text
   message.

Passing one tier never implicitly authorizes the next.

## Design principles

- **Local first:** raw databases, encryption material, and full histories stay
  on the user's Mac.
- **Least privilege:** expose typed operations over allow-listed conversations,
  never raw SQL or arbitrary internal calls to an AI.
- **Read-only by construction:** parse consistent copies, not WeChat's live
  files. Preserve database, WAL, and SHM files as one snapshot unit.
- **Version pinned:** fingerprint the official client and storage schema; unknown
  versions fail closed.
- **Evidence driven:** distinguish cached, observed, and server-fetched data and
  retain provenance for every normalized record.
- **No stealth:** do not attempt to evade anti-tamper, environment, or account
  security controls.
- **Safe AI boundary:** connector policy, consent, rate limits, and audit logic
  remain deterministic and outside the model.

## Phase 0 — repository and safety baseline

Status: **complete**

- [x] Create a private personal GitHub repository.
- [x] Document scope, authorization requirements, and sensitive-data policy.
- [x] Ignore database, key, capture, and private-fixture artifacts.
- [x] Establish separate passive-read, active-read, and write privilege tiers.
- [ ] Select an open-source license before making the repository public.

Exit gate: secrets and real user artifacts are excluded by default, and the
project can evolve without conflating read access with permission to write.

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

- [ ] Define a snapshot manifest containing source fingerprint, file identity,
  size, modification time, and cryptographic digest.
- [ ] Copy database/WAL/SHM sets into an owner-only temporary directory.
- [ ] Detect mutation during copying and retry or reject inconsistent snapshots.
- [ ] Prove with tests that the source tree is never opened for writing.
- [ ] Securely clean up connector-created snapshots on expiration.

### 1C. Storage format research

- [ ] Create generated databases and sanitized binary fixtures covering known
  structural patterns.
- [ ] Identify database families, page format, schema/version markers, and
  serialization formats from authorized test data.
- [ ] Keep any key-handling experiment in a small local component with no
  network access or model/log exposure.
- [ ] Document uncertainty; never label guessed fields as verified.

### 1D. Normalized conversation reader

- [ ] Define stable models for accounts, conversations, participants, messages,
  attachments, quotes, recalls, and provenance.
- [ ] Implement cursor-based incremental reads and deterministic deduplication.
- [ ] Reconcile contact/group identifiers without leaking identifiers into logs.
- [ ] Expose only explicitly enabled conversations.

### 1E. Incoming event reconciler

- [ ] Observe filesystem changes as a wake-up hint.
- [ ] Evaluate macOS notification accessibility only as an optional low-latency
  hint; do not depend on it for completeness.
- [ ] Reconcile against normalized message identifiers and periodically recover
  missed/duplicate events.

Exit gate: on supported, pinned versions, the bridge can reproduce selected
cached conversations from a consistent snapshot without modifying the source
or exposing raw keys/data outside the local boundary.

## Phase 2 — cached and authenticated read-only content

Status: **planned**

### 2A. Cached dynamic content

- [ ] Locate and normalize Moments and public-account data that the official
  client has already cached.
- [ ] Label cache completeness, observation time, and source explicitly.
- [ ] Parse public web articles only when normal URL access permits it; preserve
  authentication and paywall boundaries.

### 2B. Active read feasibility gate

- [ ] Map official local IPC boundaries before considering in-process or network
  approaches.
- [ ] Determine whether a high-level, read-only operation can reuse the existing
  logged-in client without extracting reusable session credentials.
- [ ] Prototype only on a disposable test account and a pinned client version.
- [ ] Stop if the approach requires defeating integrity/security controls,
  weakens the user's account, or cannot fail closed after version drift.

### 2C. Narrow read API

- [ ] Add typed operations such as `get_cached_moments` and, only after the
  feasibility gate, `load_more_moments(cursor)`.
- [ ] Bound pagination, rate, retention, and per-source authorization.
- [ ] Add compatibility health checks and automatic disablement on unknown
  builds.

Exit gate: authenticated reads are isolated from local parsing, version-gated,
auditable, and cannot perform write operations.

## Phase 3 — AI tool layer and drafts

Status: **planned**

- [ ] Build a local authorization service with per-conversation scopes.
- [ ] Provide narrow tools: list enabled conversations, read recent messages,
  search enabled messages, inspect cached feed items, and create drafts.
- [ ] Minimize context before any remote-model request; support a fully local
  model path.
- [ ] Defend against prompt injection with deterministic capability checks.
- [ ] Add an audit log that contains action metadata but can omit message bodies.
- [ ] Support draft-only workflows before any automatic sending work.

Exit gate: hostile message content cannot expand scopes, access other chats, or
turn a draft into a send action.

## Phase 4 — separately gated write actions

Status: **planned; no implementation authorized by earlier phases**

- [ ] Re-evaluate platform rules, account safety, and technical feasibility.
- [ ] Start with text-only sending to one allow-listed test conversation.
- [ ] Require explicit approval, then consider narrowly defined auto-send rules.
- [ ] Enforce idempotency, rate limits, kill switch, and unknown-version fail
  closure outside the AI.
- [ ] Confirm success from the official client's resulting state; never equate
  an internal return value with delivery.
- [ ] Treat replies, media, reactions, group operations, and Moments mutations as
  independent capabilities requiring new review.

Exit gate: no duplicate or unauthorized external action under retries, prompt
injection, client upgrades, partial failures, or model misbehavior.

## Initial technical architecture

```text
Official WeChat client and local stores
               |
       version-specific adapters
         |         |          |
   passive read  active read  write action
         |         |          |
         +---- normalized local service ----+
                         |
             policy / consent / audit
                         |
                  narrow AI tools
```

The initial implementation is a Swift package targeting macOS 14 or later:

- `GreenBubblesCore`: discovery, artifact models, classifiers, and later
  snapshot/decoder services.
- `greenbubbles`: a local CLI for inspection and development.
- `GreenBubblesCoreTests`: synthetic fixtures only.

No long-running service or model connection will be introduced until passive
reading and the authorization model can be tested independently.

## Near-term work queue

1. Run the discovery inventory on controlled systems and update known paths.
2. Add a snapshot manifest and immutable snapshot copier.
3. Build mutation-detection and source-read-only tests.
4. Research storage families using disposable accounts and sanitized fixtures.
5. Specify normalized message models before implementing any decoder.

## Non-goals for the initial milestones

- A replacement WeChat client or server.
- A second simultaneous login.
- Automated sending or account management.
- Anti-detection, anti-tamper bypass, credential export, or protocol spoofing.
- Uploading complete chat histories to a hosted AI service.
- Supporting every WeChat version before the pinned-version path is reliable.
