# Native macOS history browser

GreenBubbles includes a native, read-only macOS history browser with two
explicit source modes:

- bounded direct queries against owner-authorized live WeChat SQLite/WCDB or an
  independently encrypted GreenBubbles snapshot; and
- audited, exported AI context bundles with a private derived SQLite/FTS index.

For setup and ordinary operation, start with the
[GreenBubbles user guide](USER_GUIDE.md). This document describes the browser's
implementation and trust boundaries. The app never acquires a database key,
runs an agent server, or sends messages. In direct mode it invokes the local
`greenbubbles-restore` helper, which opens the selected databases read-only and
returns only bounded JSON pages.

## Technology decision

The recommended production stack is:

- Swift 6 and SwiftUI for the macOS application and navigation;
- Observation for main-actor UI state;
- Foundation and descriptor-based POSIX I/O for bounded JSONL streaming;
- system SQLite 3 with FTS5 trigram indexing for large-history search;
- CryptoKit SHA-256 for independent bundle and media verification;
- Quick Look for images, animated images, video, audio, PDF, Office documents,
  and other system-supported formats; and
- the existing `greenbubbles-restore` one-shot CLI for bounded live/snapshot
  queries, recoverable snapshot creation, and policy-scoped exported-bundle
  media resolution.

This is preferable to Electron for a private, local-first archive because it
does not add a browser runtime, web-origin boundary, or JavaScript dependency
chain. Tauri would reuse more Rust in the UI shell and is a reasonable
cross-platform choice, but GreenBubbles currently targets macOS and benefits
materially from native Quick Look, file panels, accessibility, media handling,
window conventions, and future security-scoped bookmarks. The Rust native
engine remains the source of truth; the Swift application consumes its stable
bounded-JSON and explicit exported-JSONL contracts.

## Data flow and trust boundary

```text
live WeChat SQLite/WCDB or recoverable snapshot
              |
              | greenbubbles-restore read-only resource commands
              v
bounded conversation/message/search JSON pages
              |
              v
SwiftUI overview / chats / search / exact hydration

or, for an explicit exported-history workflow:

encrypted replica + owner policy
              |
              | greenbubbles-restore ai-export
              v
private five-file AI context bundle
              |
              | independent permissions/schema/hash/reference audit
              v
private atomic SQLite/FTS derived index
              |
              v
SwiftUI chats / contacts / search / message timeline

optional media preview
              |
              | ai-query getArtifact + replica key on stdin
              v
policy + descriptor + digest revalidation
              |
              v
private session-only copy -> native Quick Look
```

Direct mode passes live keys and snapshot passphrases only through standard
input. Recovery-kit and local-unlock modes pass private file paths, never their
contents or the unwrapped SQLCipher key. Search text also uses standard input.
The app retains a live key or passphrase only while that source is open and
clears it when switching or closing. Keychain mode retrieves only a random
snapshot-local wrapper credential and materializes it in an owner-only
session-directory file that is removed on close.

## Direct live and snapshot behavior

Opening a direct source first runs `source status`, then requests the first
100-conversation keyset page. The Overview reports database, WAL, SHM, journal,
and total SQLite bytes. Chats request 100 messages at a time and use the opaque
`nextCursor`; equal timestamps and server IDs remain total because native
ordering also binds shard and row identity. Search returns at most 100 hits in
the UI, prefers compatible native WeChat FTS read-only, and otherwise advances
through a fixed no-write source window. Selecting a search result hydrates that
exact source-bound message rather than loading neighboring history.

Every direct response is checked for the expected schema, operation, source
mode, source identity, response-size cap, and bounded process lifetime. The UI
surfaces consistency and shard warnings and does not interpret incomplete
coverage as proof that a message is absent. Live pages are statement-consistent
per database, not globally atomic across WeChat's independent databases. A
recoverable snapshot provides a stable immutable generation when cross-page
repeatability matters.

The graphical snapshot wizard creates the mandatory owner-only 24-word kit
before conversion, displays it once, challenges four random positions, and
requires confirmation of an independent copy. It can add a Keychain or hidden-
file convenience credential and an optional Argon2id passphrase. Native
snapshot creation writes logical pages directly into already-keyed SQLCipher
destinations and publishes only after recovery verification without the WeChat
key. See [RECOVERABLE_SNAPSHOTS.md](RECOVERABLE_SNAPSHOTS.md) for the full
protector and retention contract.

Opening a static bundle never requires a replica key. A media preview is a
separate, explicit local action because static bundle records intentionally do
not expose absolute file paths. The preview sheet calls only GreenBubbles'
read-only `getArtifact` operation. It sends the replica key over standard input,
uses an owner-only request file, imposes a 60-second timeout and bounded
stdout/stderr, requires the response request, API, account, replica, source, and
artifact identities to match, and streams a fresh size/SHA-256 check into a
mode-`0600` preview copy. The key field is cleared before the request and is
never saved to arguments, requests, settings, audit output, or preview metadata.

The preview copy exists under a new mode-`0700` per-process temporary
directory and is removed when the browser exits normally. The UI never joins an
account-relative path to a guessed source root and never treats static artifact
metadata as permission to open a file.

## Bundle verification and large-corpus behavior

Before any history is displayed, the browser independently verifies:

- the exact `manifest.json`, `conversations.jsonl`, `contacts.jsonl`,
  `messages.jsonl`, and `artifacts.jsonl` inventory;
- current-user ownership, single-link regular files, owner-only permissions,
  and no followed symlinks;
- manifest format/completion evidence and the bundle identity bound to replica,
  checkpoint, policy, policy source, destination, and, for version 2,
  `selfParticipantId`;
- each JSONL file's manifest byte count, record count, and SHA-256;
- every record schema, format version, unique identity, source freshness, and
  allowed reference;
- exact conversation-participant/contact coverage;
- message-to-conversation, sender-to-contact, relationship, and artifact
  references, plus sender-versus-account direction consistency; and
- exact correspondence between message artifact references and artifact
  records, including the manifest's artifact error count.

Messages are never loaded as one giant Swift array. The loader scans JSONL in
bounded chunks off the main actor and writes only searchable metadata plus byte
offsets into an owner-only SQLite index. FTS entries are populated incrementally
inside the same bounded transactions, avoiding an opaque corpus-wide rebuild at
the end. The browser builds the index in an unpublished sibling file, validates
cross-record references and `PRAGMA integrity_check`, flushes it, and publishes
it with one rename. Cancellation rolls back and removes the unpublished index.
The final index is bound to the bundle ID, checkpoint, policy digest,
source-file digests, and record counts.

The loader reads both legacy `greenbubbles.ai-context.v1` bundles and current
`greenbubbles.ai-context.v2` bundles. Version 2 requires an opaque
`selfParticipantId`, uses `groupOwnerParticipantId` only for the creator/owner
of a group, rejects the legacy owner field in new records, and verifies every
released sender/direction pair. The UI derives self-authorship from
`senderId == selfParticipantId`, displays the account holder as `You`, and never
uses group ownership, contact names, or conversation shape as a self heuristic.
Opening a version-1 bundle preserves its recorded legacy direction because it
contains no integrity-bound account-holder identity.

Reopening the same generation still revalidates every source byte and record;
it may then reuse the bound index. The browser holds descriptors for message and
artifact JSONL and checks their device, inode, size, and modification identity
before and after every offset read. Those descriptors are retained directly
from validation through browsing, closing the path-replacement window between
load and store creation; post-validation or post-open mutation fails closed.
Chat timelines use keyset pagination in pages of 100 rather than growing an
offset query, and global search uses FTS5's trigram tokenizer for Chinese,
other non-whitespace languages, and substring lookup. One- and two-character
searches use a bounded escaped `LIKE` fallback.

Import UI reports the current phase, overall and phase percentages, current-
file bytes and records, and total bundle bytes and records for manifest
validation, conversations, contacts, messages, artifacts, index finalization,
and completion. A cached-index reopening is labeled as source revalidation
rather than pretending no work is occurring. Opening another bundle cancels
the old scan and prevents stale progress or results from replacing the new
selection.

## Interaction design

The application uses a native three-column navigation layout:

- **Direct Overview** shows source identity, access mode, actual SQLite/WAL
  storage, consistency scope, and prominent warnings without restoration.
- **Exported Overview** shows verified bundle/checkpoint evidence,
  conversation/contact/message/media counts, database freshness counts, and
  prominent source-coverage limitations.
- **Chats** provides human-label and participant filtering, latest-message
  dates, group/direct labels, and keyset-paged timelines. Exported mode also
  includes message counts, business/system affordances, and stale badges.
- **Contacts** is an exported-history surface showing normalized display names,
  whether a local profile was available, per-conversation names and roles, and
  navigation into shared chats.
- **Search** uses native/fallback bounded source search in direct mode. Exported
  mode queries normalized authorized text, sender names, and conversation
  labels, then opens a result with nearby message context.
- **Timeline** renders incoming/outgoing/unknown direction distinctly, shows
  timestamps and logical payload kinds, retains policy-truncation warnings,
  navigates resolved reply/quote relationships, and loads older pages on
  demand.
- **Media cards** distinguish image, animated image, voice, video, document,
  rich-media, and unknown references; show availability, format, size,
  verification, decode, and typed error states; and offer preview only for an
  artifact reported as downloaded or database-materialized.

Partial coverage is never reduced to a small status icon. It appears in the
overview, chat header, conversation/contact/message freshness badges, and a
details sheet that explains why an absent record is not deletion evidence.

The browser intentionally has no composer, draft, approval, synchronization,
or send controls. Message text is displayed as untrusted source material and
cannot select commands or alter policy.

## Build and run

Build or run the native SwiftUI executable from the repository root:

```sh
cargo build --release \
  --manifest-path Native/GreenBubblesRestore/Cargo.toml
swift build --product greenbubbles-history
swift run greenbubbles-history
swift run greenbubbles-history --bundle /absolute/path/to/ai-context-bundle
```

For direct browsing or snapshot creation, select
`Native/GreenBubblesRestore/target/release/greenbubbles-restore` as the local
CLI and choose **Browse Live or Snapshot…** or **Create Recoverable Snapshot…**.

For exported-history mode, open a new output directory produced by
`greenbubbles-restore ai-export`, or drag that directory into the empty browser
window. The explicit `--bundle`
launch option, file panel, drag/drop path, and macOS directory/`manifest.json`
open events all converge on the same verifier. The directory and every file
must retain their owner-only permissions. The private derived index is stored
under `Application Support/GreenBubbles/HistoryIndexes` and contains normalized
search text, so it must remain private even though it contains no keys or raw
source columns.

Media preview additionally asks for the local `greenbubbles-restore`
executable, encrypted replica, tool policy, connector audit log, and one-time
replica key. A release application should bundle the matching signed Rust
executable instead of asking for its path.

## Production packaging path

The Swift Package executable is the development and automated-test surface.
Before distributing a `.app`, add an Xcode archive target that reuses these
sources and:

- embeds the matching `greenbubbles-restore` binary as a signed helper;
- enables Hardened Runtime, code signing, notarization, and deterministic
  version binding between the UI and helper;
- adopts App Sandbox plus user-selected security-scoped bookmarks for bundle,
  replica, policy, and audit locations, after verifying the embedded helper can
  consume inherited extensions correctly;
- stores no replica or database key in preferences, restoration state, crash
  reports, or analytics;
- disables state restoration for private message/search fields or explicitly
  redacts them;
- adds VoiceOver labels, keyboard navigation, reduced-motion/high-contrast
  audits, localization, and UI automation for empty/loading/partial/error/large-
  text states; and
- applies a retention policy to old derived indexes and abnormal-termination
  preview directories without ever deleting a source bundle.

No public distribution claim follows from the current private development
target; the repository's licensing and distribution gates remain controlling.

## Automated evidence

`GreenBubblesHistoryTests` builds complete owner-only synthetic v1 and v2 bundles and
executes the real loader/index/store. It covers bundle identity, schemas,
account-holder direction validation, group-owner separation, hashes,
permissions, record/reference checks, stale coverage, FTS and short
Chinese search, keyset pagination, exact artifact lookup, index reuse, source
tampering, validation-to-store path replacement, post-open mutation, large-
corpus transactions, and cancellation without index publication.

The direct-query and snapshot-creation client suites prove stdin-only secret
and search delivery, protector-file permission checks, bounded output and
deadlines, operation/source-mode binding, keyset paging, exact message
hydration, recovery-kit integrity, mandatory portable protection, Keychain
materialization, cancellation, and post-publication handling. The live-media
suite uses a synthetic one-shot CLI process to prove stdin-only key delivery,
policy-response parsing, bounded process capture, identity and
digest checks, request-envelope binding, cancellation, byte-progress reporting,
private-copy permissions, and malformed-key rejection. Tests contain no real
messages, identities, paths, keys, or media.
