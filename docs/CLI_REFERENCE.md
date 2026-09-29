# Command-line reference

The native `greenbubbles` executable is the query, snapshot, restoration,
replica, connector and audit engine behind the history browser. Its built-in
help is the canonical syntax:

```sh
greenbubbles help
greenbubbles help ai-query
greenbubbles send --help
```

Help topics exist for `profile`, `source`, `conversations`, `contacts`,
`messages`, `message`, `memory`, `attachment`, `snapshot`, `send`, the three `connector-*-direct`
commands, `audit-replica`, `audit-replica-backup`, `ai-query`, `ai-export`,
`ai-memory-export`, `audit-ai-context` and `audit-ai-memory`. An unrecognized
topic falls back to the full usage listing rather than erroring.

This page maps command families to the workflow they belong to. It does not
turn an advanced forensic or action command into a recommended first run — for
ordinary use start with the [user guide](USER_GUIDE.md).

Do not confuse it with `greenbubbles-discover`, the small Swift helper that
locates installations, accounts and candidate artefacts *before* any key is
supplied. It opens no database contents.

## Build

```sh
cargo build --locked --release --manifest-path Native/GreenBubbles/Cargo.toml

GB_CLI="Native/GreenBubbles/target/release/greenbubbles"
"$GB_CLI" help
```

## Command map

| Family | Commands | Use |
| --- | --- | --- |
| Query profiles | `profile path/template/list/show/validate/set-default` | Optional named sources for a second account or a snapshot; live queries need none |
| Direct resources | `source status`, `conversations list`, `contacts list`, `messages list/search`, `message get` | Read one bounded live or snapshot page without creating a restoration |
| Exact attachments | `attachment inspect/materialize` | Inspect one message; copy one selected artefact to a new private path |
| Recoverable snapshots | `snapshot recovery-kit/local-credential/create/create-capture/verify/rewrap/rekey/retention` | Create, reopen, rotate, verify and retain independently encrypted snapshots |
| Offline restoration | `preflight`, `probe`, `restore`, `restore-publish` | Validate and restore an owner-authorized immutable capture |
| Diagnostics | `diagnose-batch`, `diagnose-available`, `diagnose-archive-schema`, `diagnose-archive-payloads` | Produce privacy-minimized structural evidence for incomplete or changing formats |
| Archive audit and merge | `audit-archive`, `audit-acquisition-chain`, `reconcile`, `merge-incremental` | Verify archive integrity and combine change-proportional generations |
| Replica lifecycle | `replica-bootstrap/status/sync/publish/follow*`, `audit-replica*`, `prepare-replica-recovery` | Maintain and recover the encrypted serving replica |
| Replica reads | `replica-conversations/search/message/coverage/changes/cached-moments` | Query replica-only enrichment and change surfaces |
| Direct AI connector | `connector-policy-direct`, `connector-query-direct`, `connector-serve-direct` | Apply source-bound policy and audit directly to live or snapshot queries |
| Replica AI connector | `tool-policy/list/recent/search/draft`, `connector-serve/call` | Serve policy-scoped replica reads and non-executing drafts |
| AI interchange | `ai-query`, `ai-export`, `audit-ai-context`, `ai-memory-export`, `audit-ai-memory`, `ai-summarize-direct` | Create, verify, and model-summarize minimized, citation-preserving AI context |
| Personal memory | `memory prepare [--extend]`, `memory next`, `memory page/acknowledge/commit/status` | Prepare one canonical corpus (or extend it incrementally with `--extend`), select composable run scopes, and feed deterministic crash-safe evidence pages to an agent |
| Personal memory driver | `tick`, `manifest-refresh`, `revise` | Python driver (`scripts/personal-memory-parallel.py`) that orchestrates agents over the corpus; `--format python\|markdown` on `tick` selects the UserAsCode output format |
| Operational evidence | `synthetic-benchmark`, `compose-latency-evidence`, `summarize-latency-evidence`, `audit-connector-log/state` | Generate or verify aggregate release and service evidence |
| Sending | `send …` | Inspect the separate experimental adapter; public builds stay closed |

## How secrets reach a process

Every direct source command takes exactly one access mode:

| Mode | Option | Secret input |
| --- | --- | --- |
| Live encrypted WeChat | `--passphrase-stdin` | key as the first stdin line |
| Snapshot, portable | `--snapshot-recovery-kit <file>` | owner-only file; nothing on stdin |
| Snapshot, local | `--snapshot-local-credential <file>` | owner-only file; nothing on stdin |
| Snapshot, passphrase | `--snapshot-passphrase-stdin` | passphrase as the first stdin line |
| Legacy snapshot raw key | `--snapshot-key-stdin` | key as the first stdin line |
| Synthetic / plaintext | `--decrypted` | none |

When a search also uses stdin, the key or passphrase is the first line and the
query is the remaining UTF-8 input. In file-backed and plaintext modes, stdin
contains only the query.

**Never put a key, passphrase, recovery phrase, replica key or private search
text in a shell argument.** Prefer an owner-only file redirected into stdin
over an interactive shell literal — the literal enters shell history, and
arguments are visible to every process on the machine.

Daily live use needs none of those flags. With no settings file, no profile
file, no source path, and no access flag, the command opens the newest
installed WeChat `db_storage` directory and reads
`~/.greenbubbles-acquire/passphrase.txt`. A different path goes in
`~/.greenbubbles/config.toml`. A [query profile](QUERY_PROFILES.md) is for a
second account or a snapshot.

`greenbubbles` and `greenbubbles help` show the everyday commands.
`greenbubbles help --all` shows the complete list. `greenbubbles chats` lists
conversations; `conversations list` remains the same command.
`greenbubbles chats rank` counts how many messages the account holder sent in
each chat, with direct chats before groups, and returns no message text. Its
default page is JSON Lines. The header has `returned`, `qualifying`, `hasMore`,
`order` (`direct-then-self-count`), `timezone`, `accountHolderKnown`,
`coverageComplete`, `conversationCount`, and `nextCursor` when another page
exists. Each chat has `from` (never empty), `id`, `kind`, `selfCount`, and
`last` as local `YYYY-MM-DD HH:MM`. `--limit` is 1..2000 and defaults to 100.
Follow `nextCursor` with `--cursor` and repeat `--minimum-self-messages`.
`--json` prints the full report, including each chat's total message count.

## Bounds and how to read a response

- `messages list` and `messages search` print JSON Lines by default. The header
  has `returned`, `hasMore`, `order` (`newest`), `timezone` (the local offset),
  and `nextCursor` when another page exists. Search adds `searchFreshness` when
  the native index was not checked, and `accountHolderKnown` only when the
  account holder could not be identified. Each message has `from`, `self`,
  `at`, and `text`. `from` is never empty: remark, then nickname, then alias,
  then the in-group display name, then the wxid. `self` is `true` when the
  account holder sent the message and `false` otherwise. `at` is local time,
  `YYYY-MM-DD HH:MM`, in the header's timezone. An image, video, or document
  adds `file`, a local path. Encrypted images are decoded into
  `~/.greenbubbles/cache/media`. A path is omitted when the bytes cannot be
  opened. Voice without a transcript stays `[voice]` and has no `file`. A text
  body that is only identifiers and `true` or `false` becomes `[unknown]`. A
  text body that is only bracketed emoji names, in English such as `[Grin]` or
  in Chinese such as `[偷笑]`, becomes `[emoji]`; words beside those names are
  kept. A short recall notice becomes `[revoked]`, including a line that only
  names who recalled a message. A longer sentence that mentions a recall stays
  text. The withdrawn words are not recovered. Phone numbers, email
  addresses, identity numbers, links, and meeting invitations stay in the
  text. `--redact` leaves out a resident identity number, a mainland mobile,
  an email address, and an `http` or `https` link, including inside a quote,
  a link title, a file name, a transfer note, and `from`. The same flag
  collapses a video-meeting invitation that includes a meeting id or a join
  link to `[meeting]`. Words around a removed value stay. A body that is only
  one of those becomes `[unknown]`. A bare hostname stays either way. Search
  lines name the chat with `chat`, or `conversationId` when it has no display
  name, and do not include `file`; open that conversation with `messages list`
  to read an image or file. Do not copy a `file` path into notes or a knowledge
  base. Identifiers, type codes, and source metadata are omitted. `--json`
  prints the full envelope, including the opaque id `message get` requires.
  The default can also be set with `[output] format` in
  `~/.greenbubbles/config.toml`. `--since` and `--until` are inclusive unix
  seconds and must be repeated with the cursor. Repeat `--conversation` up to
  24 times to read one time slice in a single database open. Each chat is its
  own page, and that header includes `conversationId`. Follow `nextCursor`
  with one `--conversation`. A cursor with several conversations is rejected.
- Conversation and message lists default to 100 items, hard maximum 500.
- Contact pages use the same 1..500 bound and source/filter-bound cursors.
- Search has a hard maximum of 200 results. Fallback source scanning examines a
  bounded window and may need a continuation followed through *empty* result
  pages — an empty page is not the end of the search.
- There is no `--all` direct-query option, and there will not be one.
- Opaque cursors and message IDs are bound to their source, operation,
  conversation and filters. Do not reuse one across those boundaries; it will
  be rejected rather than silently reinterpreted.
- Read `ok`, `consistency`, `warnings`, `coverage` and `page` before
  interpreting any result.
- **Missing data under stale, unavailable or partial coverage is not evidence
  of deletion.** This is the single most important reading rule, and the one an
  automated caller is most likely to get wrong.

Envelope and paging details are in [ARCHITECTURE.md](ARCHITECTURE.md); the AI
request contract is in [AI_CONTEXT_CLI.md](AI_CONTEXT_CLI.md).

## Progress output

Most restoration, audit, snapshot and export commands accept
`--progress-json`, `--quiet-progress`, or an owner-only create-new
`--progress-file`. Progress events omit message content, credentials and source
paths — but an operational report can still reveal private aggregate facts, so
treat one as private until you have read it.

`memory prepare` emits content-free phase, conversation, row and hydration
counts on standard error while building one immutable `allMessages` corpus.
Repeatable `--conversation`, `--conversation-kind`, and `--sender` filters plus
inclusive RFC 3339 `--from`/`--through` bounds are applied by `memory next`;
omitting every evidence filter means every hydrated corpus message. `--subject`
independently defaults to `account-holder`, accepts `person:<selector>`, or may
be `none` for conversation-centric memory. Every agent-visible memory timestamp
is RFC 3339 in the corpus timezone. `memory next` selects a
16 KiB..2 MiB, at-most-5,000-message batch but returns only its small delivery
envelope. `--max-text-bytes` bounds stored chat text; optional `--max-messages`
additionally bounds the per-message delivery envelope that text bytes do not
predict, which is what a caller sizing a batch against a fixed agent context
window needs. It is a soft bound: it stops a batch taking another unit, and
never splits or refuses the one unit a batch must deliver whole. Sticker,
location and system payloads reach the agent as the human text inside their
WeChat markup envelope rather than as verbatim XML, and the account holder is
labelled `Me` beside their source id. New corpora can use the deterministic `accountHolderRelevance` order
to cover self-active relationships and months early; it still schedules every
canonical unit.
`memory page`
deterministically fragments that immutable batch into at-most-49,152-byte JSON
responses, and `memory acknowledge` advances one delivered page at a time. An
agent may omit opaque batch/page selectors: these commands resolve only the
uniquely current persisted batch and delivered page. Explicit `--batch` and
`--page-token` bindings remain available for operator audit and replay. An
ordinary `memory commit` needs cited factual prose, rejects both unretained
citations, retained-but-uncited evidence, and more than eight representative
citations on one changed factual line. A rejection reports every problem it
found at once, with one-based line numbers and the offending aliases and paths,
so one more invocation can fix all of them. Changed `me.md` prose additionally
requires at least one self-authored citation per factual line. It advances only after every page was
delivered and acknowledged. After full review of a low-value batch,
`--reviewed-no-durable-memory` instead requires the wiki to be byte-for-byte
unchanged. `memory status` separates canonical corpus counts from current-scope
selected and committed counts, reports the resolved subject, and exposes
row/source/content limitation aggregates so an agent never needs to read corpus
sidecars. `complete: true` means the current scope is complete; it proves review
of every hydrated message only for a canonical run whose `scope.allMessages` is
true. See
[PERSONAL_MEMORY.md](PERSONAL_MEMORY.md).

## Where to go next

| Task | Document |
| --- | --- |
| Repeated queries without retyping | [Query profiles](QUERY_PROFILES.md) |
| Backups and recovery | [Recoverable snapshots](RECOVERABLE_SNAPSHOTS.md) |
| Offline restoration and publication | [Restoration specification](RESTORATION_SPEC.md) |
| The serving replica | [Replica specification](REPLICA_SPEC.md) · [operations](REPLICA_OPERATIONS.md) |
| The local request contract | [Connector API](CONNECTOR_API.md) |
| Giving an AI access | [AI context CLI](AI_CONTEXT_CLI.md) |
| Wikipedia-style knowledge base from live history | [Personal memory](PERSONAL_MEMORY.md) |
| Verifying any of the above | [Auditing](AUDITING.md) |
| The closed send path | [Send adapter](SEND_ADAPTER.md) |
