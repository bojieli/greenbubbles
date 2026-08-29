# The history browser

A native, read-only macOS app for reading your own history. It never acquires a
database key, never runs an agent server, and cannot send anything. For setup
and daily use start with the [user guide](USER_GUIDE.md); this document is the
implementation and its trust boundaries.

## Two source modes

**Direct** — bounded queries against live WeChat databases or a GreenBubbles
snapshot. The app shells out to the local `greenbubbles` helper, which opens
the files read-only and returns bounded JSON pages. Nothing is restored,
indexed or copied.

**Exported bundle** — an audited AI context bundle produced by `ai-export`,
loaded into a private derived SQLite/FTS index. This is the mode for reading a
large history quickly, and it never requires a replica key to open.

## Where secrets go

Direct mode passes live keys and snapshot passphrases **only** through standard
input. Recovery-kit and local-unlock modes pass a private file *path*, never
its contents and never the unwrapped SQLCipher key. Search text also goes
through standard input.

A live key or passphrase is retained only while that source is open and cleared
when switching or closing. Keychain mode retrieves only a random
snapshot-local wrapper credential and materializes it into an owner-only
session-directory file that is removed on close.

```text
live WeChat SQLite/WCDB, or recoverable snapshot
      │ read-only resource commands
      ▼
bounded conversation / message / search JSON pages
      ▼
SwiftUI overview · chats · search · exact hydration
```

```text
encrypted replica + owner policy
      │ ai-export
      ▼
private five-file context bundle
      │ independent permission / schema / hash / reference audit
      ▼
private atomic SQLite + FTS derived index
      ▼
SwiftUI chats · contacts · search · timeline
      │ optional: ai-query getArtifact, replica key on stdin
      ▼
policy + descriptor + digest revalidation → session-only copy → Quick Look
```

## Direct mode

Opening a source runs `source status`, then requests the first 100-conversation
keyset page. Overview reports database, WAL, SHM, journal and total SQLite
bytes. Chats request 100 messages at a time and follow the opaque `nextCursor`;
equal timestamps and server IDs stay totally ordered because the ordering also
binds shard and row identity. Search returns at most 100 hits, prefers
compatible native WeChat FTS read-only, and otherwise advances through the
fixed no-write source window. Selecting a hit hydrates that exact source-bound
message rather than loading its neighbours.

Every response is checked for the expected schema, operation, source mode,
source identity, response-size cap and bounded process lifetime. Consistency
and shard warnings are surfaced in the UI, and **incomplete coverage is never
rendered as proof that a message is absent.** Live pages are
statement-consistent per database, not globally atomic; when cross-page
repeatability matters, a snapshot generation is the stable target.

The snapshot wizard creates the mandatory owner-only 24-word kit before
conversion, displays it once, challenges four random positions, and requires
confirmation of an independent copy. It can add a Keychain or hidden-file
convenience credential and an optional Argon2id passphrase. Conversion writes
logical pages directly into already-keyed SQLCipher destinations and publishes
only after recovery verification succeeds *without* the WeChat key. Full
contract in [RECOVERABLE_SNAPSHOTS.md](RECOVERABLE_SNAPSHOTS.md).

## Bundle mode

Before any history is shown, the browser independently verifies the exact
five-file inventory (`manifest.json`, `conversations.jsonl`, `contacts.jsonl`,
`messages.jsonl`, `artifacts.jsonl`); current-user ownership, single-link
regular files, owner-only permissions and no followed symlinks; the manifest's
completion evidence and bundle identity bound to replica, checkpoint, policy,
policy source, destination and — for version 2 — `selfParticipantId`; each
file's manifest byte count, record count and SHA-256; every record's schema,
version, unique identity, source freshness and allowed references; exact
conversation-participant and contact coverage; every message-to-conversation,
sender-to-contact, relationship and artifact reference, including sender-versus
account direction consistency; and exact correspondence between message
artifact references and artifact records.

### Loading a large corpus

Messages are never loaded as one giant Swift array. The loader scans JSONL in
bounded chunks off the main actor and writes only searchable metadata plus byte
offsets into an owner-only SQLite index, populating FTS incrementally inside
the same bounded transactions rather than doing one opaque corpus-wide rebuild
at the end. The index is built in an unpublished sibling file, validated with
cross-record reference checks and `PRAGMA integrity_check`, flushed, and
published with a single rename. Cancellation rolls back and removes it.

The finished index is bound to the bundle ID, checkpoint, policy digest, source
file digests and record counts. Reopening the same generation still revalidates
every source byte and record before reusing the index — and the import UI
labels that as revalidation rather than pretending no work is happening.

The browser holds descriptors for the message and artifact JSONL files and
checks device, inode, size and modification identity before and after every
offset read. Those descriptors are retained from validation through browsing,
which closes the path-replacement window between load and store creation.
Post-validation or post-open mutation fails closed.

Chat timelines use keyset pagination in pages of 100. Global search uses FTS5's
trigram tokenizer, which is what makes Chinese, other non-whitespace languages
and substring lookup work; one- and two-character searches fall back to a
bounded escaped `LIKE`.

### Version 1 and version 2 bundles

Version 2 requires an opaque `selfParticipantId`, uses
`groupOwnerParticipantId` only for a group's creator, rejects the legacy owner
field in new records, and verifies every released sender/direction pair. The UI
derives self-authorship from `senderId == selfParticipantId`, displays the
account holder as **You**, and never uses group ownership, contact names or
conversation shape as a heuristic for who you are. A version-1 bundle keeps its
recorded legacy direction, because it carries no integrity-bound account-holder
identity to check against.

### Media preview

A preview is a separate, explicit action, because bundle records deliberately
do not expose absolute file paths. The sheet calls only the read-only
`getArtifact` operation: replica key over standard input, an owner-only request
file, a 60-second timeout, bounded stdout and stderr, and a requirement that
the response's request, API, account, replica, source and artifact identities
all match. It streams a fresh size and SHA-256 check into a mode-`0600` preview
copy under a new mode-`0700` per-process temporary directory, removed on normal
exit. The key field is cleared before the request and is never written to
arguments, requests, settings, audit output or preview metadata.

The UI never joins an account-relative path to a guessed source root, and never
treats artifact metadata as permission to open a file.

## The interface

A native three-column layout:

- **Direct Overview** — source identity, access mode, real SQLite/WAL storage,
  consistency scope, and warnings, without restoring anything.
- **Exported Overview** — verified bundle and checkpoint evidence, counts,
  database freshness, and source-coverage limitations.
- **Chats** — human-label and participant filtering, latest-message dates,
  group/direct labels, keyset-paged timelines; exported mode adds message
  counts, business and system affordances, and stale badges.
- **Contacts** (exported mode) — normalized display names, whether a local
  profile existed, per-conversation names and roles, and navigation into shared
  chats.
- **Search** — bounded native or fallback source search in direct mode; in
  exported mode, normalized authorized text, sender names and conversation
  labels, opening a result with nearby context.
- **Timeline** — incoming, outgoing and unknown directions rendered
  distinctly, with timestamps, logical payload kinds, retained policy-truncation
  warnings, navigable reply and quote relationships, and older pages on demand.
- **Media cards** — image, animated image, voice, video, document, rich-media
  and unknown references, each showing availability, format, size, verification,
  decode and typed error states, offering preview only for an artefact reported
  as downloaded or database-materialized.

**Partial coverage is never reduced to a small status icon.** It appears in the
overview, the chat header, per-record freshness badges, and a details sheet
explaining why an absent record is not evidence of deletion.

There is no composer, draft, approval, synchronization or send control anywhere
in this app. Message text is displayed as untrusted source material and cannot
select a command or alter policy.

## Build and run

```sh
cargo build --release --manifest-path Native/GreenBubbles/Cargo.toml
swift build --product greenbubbles-history
swift run greenbubbles-history
swift run greenbubbles-history --bundle /absolute/path/to/ai-context-bundle
```

For direct browsing or snapshot creation, select
`Native/GreenBubbles/target/release/greenbubbles` as the local CLI, then
**Browse Live or Snapshot…** or **Create Recoverable Snapshot…**.

For exported mode, open a directory produced by `greenbubbles ai-export`, or
drag it into the empty window. The `--bundle` option, the file panel,
drag-and-drop, and macOS directory/`manifest.json` open events all converge on
the same verifier. Everything must keep its owner-only permissions.

The derived index lives under
`Application Support/GreenBubbles/HistoryIndexes`. It contains normalized
search text — no keys and no raw source columns — and must stay private
regardless.

Media preview additionally needs the encrypted replica, the tool policy, the
connector audit log, and a one-time replica key. A source build asks for the
local `greenbubbles` executable; the signed release application discovers its
matching bundled executable automatically.

## Why native, and how the release app is packaged

Swift 6 and SwiftUI, Observation for main-actor state, descriptor-based POSIX
I/O for bounded JSONL streaming, system SQLite 3 with FTS5 trigram indexing,
CryptoKit SHA-256 for independent verification, and Quick Look for previews.

Electron would add a browser runtime, a web-origin boundary and a JavaScript
dependency chain to a private local archive, for no benefit here. Tauri would
reuse more Rust in the shell and is a reasonable cross-platform answer, but
GreenBubbles targets macOS and gains materially from native Quick Look, file
panels, accessibility, media handling, window conventions and security-scoped
bookmarks. The Rust engine stays the source of truth; the Swift app consumes
its stable bounded-JSON and explicit JSONL contracts.

The release uses the same Swift Package executables exercised by the tests.
`scripts/package-send-helper.sh` assembles them into `GreenBubbles.app`, embeds
the matching Rust `greenbubbles` CLI and the privilege-separated input helper,
adds the icon, notices, SBOM and build provenance, then signs every executable
inside-out with Developer ID and Hardened Runtime. The release workflow submits
the app, the complete CLI archive and the final disk image to Apple's notary
service; it staples the accepted tickets to the app and DMG and verifies both
with Gatekeeper before publishing them.

This is a direct Developer ID distribution, not a Mac App Store build. The app
is intentionally not sandboxed because the separately signed input helper's
optional cross-application control cannot operate inside App Sandbox. The main
history application itself has empty entitlements and no Accessibility or
Screen Recording grant; those grants belong only to the helper. Source paths
selected in the UI are still re-opened and validated through GreenBubbles'
descriptor, ownership, permission, identity and digest checks. Keys are not
stored in preferences, restoration state, crash reports or analytics.

## Tests

`GreenBubblesHistoryTests` builds complete owner-only synthetic v1 and v2
bundles and exercises the real loader, index and store: bundle identity,
schemas, account-holder direction validation, group-owner separation, hashes,
permissions, record and reference checks, stale coverage, FTS and short Chinese
search, keyset pagination, exact artifact lookup, index reuse, source tampering,
validation-to-store path replacement, post-open mutation, large-corpus
transactions, and cancellation without index publication.

The direct-query and snapshot suites prove stdin-only secret and search
delivery, protector-file permission checks, bounded output and deadlines,
operation and source-mode binding, keyset paging, exact hydration, recovery-kit
integrity, mandatory portable protection, Keychain materialization,
cancellation and post-publication handling. The live-media suite uses a
synthetic one-shot CLI process to prove stdin-only key delivery, policy
response parsing, bounded process capture, identity and digest checks,
request-envelope binding, cancellation, byte-progress reporting, private-copy
permissions and malformed-key rejection.

No test contains a real message, identity, path, key or media file.
