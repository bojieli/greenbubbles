# Command-line reference

`greenbubbles` is the command-line tool behind the Mac app. It reads your
WeChat data and also handles backups, restoration, the replica (a local
encrypted copy kept for faster queries), the AI connector, and audits.

If you're new, start with the [user guide](USER_GUIDE.md). This page is a map
of every command family and the exact rules for reading output. People and
agents both rely on it.

## Getting help

The built-in help is the authority on syntax:

```sh
greenbubbles              # everyday commands
greenbubbles help --all   # every command
greenbubbles help ai-query
greenbubbles send --help
```

Help topics exist for `profile`, `source`, `conversations`, `contacts`,
`messages`, `message`, `memory`, `attachment`, `snapshot`, `send`,
`connector-policy-direct`, `connector-query-direct`, `connector-serve-direct`,
`ai-summarize-direct`, `audit-replica`, `audit-replica-backup`, `ai-query`,
`ai-export`, `ai-memory-export`, `audit-ai-context`, and `audit-ai-memory`. An
unknown topic prints the full usage list instead of an error.

A separate helper, `greenbubbles-discover`, finds WeChat installations and
accounts *before* you have a key. It never opens database contents.

To build from source:

```sh
cargo build --locked --release --manifest-path Native/GreenBubbles/Cargo.toml
Native/GreenBubbles/target/release/greenbubbles help
```

## Everyday commands

Once your key is set up, these need no extra arguments:

| Command | What it does |
| --- | --- |
| `greenbubbles chats` | List chats. Same as `conversations list`. |
| `greenbubbles chats find "Alice"` | Find identities by partial nickname, remark, alias, or ID, without reading messages. |
| `greenbubbles messages recent` | Read the newest messages across all identifiable chats. |
| `greenbubbles chats rank` | Rank chats by how many messages you sent. No message text. |
| `greenbubbles messages list --conversation <id>` | Read one chat by ID or unambiguous name, newest first. |
| `greenbubbles messages search --query-stdin` | Search messages. The search text comes from stdin. |
| `greenbubbles message get --conversation <id> --message <id>` | Fetch one message by its opaque id. |
| `greenbubbles contacts list` | List contacts. |

With no settings file, no profile, no source path, and no access flag, a
command opens the newest installed WeChat `db_storage` folder and reads the key
from `~/.greenbubbles-acquire/passphrase.txt`. To use a different path, set it
in `~/.greenbubbles/config.toml`. Use a [query profile](QUERY_PROFILES.md) only
for a second account or a backup.

## Find chats and read recent activity

```sh
greenbubbles chats
greenbubbles chats find "Alice"
greenbubbles messages list --conversation "Alice"
greenbubbles messages search --conversation "Alice" --query-stdin
greenbubbles messages recent --limit 50
greenbubbles messages recent --since 1790812800 --json
greenbubbles --version
```

`chats find` returns a JSON array with IDs, display names, nicknames, remarks,
aliased usernames, and contact kinds. It searches known contact and group
identities case-insensitively, including partial names, without reading
messages. It defaults to at most 100 matches. If there are more, refine the
name or raise `--limit` (maximum 500); results are never silently truncated.
An empty array means no matching identity was found.

`--conversation` on `messages list`, `messages search`, and `message get`
accepts an exact conversation ID or a name. Exact IDs take precedence, then
exact nickname/remark/alias matches, then partial name matches. Name matching
ignores case. A unique match resolves to its ID. Ambiguous names fail and list
up to 20 matching names and IDs on stderr; choose an exact ID. Names are
ordinary command arguments, so use an ID if you prefer to keep a name out of
shell history. Missing names fail with a suggestion to list chats.

`messages recent` reads message tables directly, across all chats whose
identity can be recovered from contacts or sessions. It includes chats with
message tables even when they are absent from the session list. It needs no
native search index. Messages are ordered by creation time descending, with
a deterministic tie order of sort sequence, server ID, shard, row, and chat ID.
The default is 100 messages, and `--limit` accepts 1..500. Each compact line
includes `chat` when available and `conversationId`, alongside the usual sender,
local timestamp, text, and attachment path. `--json` includes full message IDs,
coverage information, and warnings. Unidentifiable tables are omitted and
reported through incomplete coverage; this command does not claim an atomic
snapshot across databases.

`--since` and `--until` are inclusive Unix seconds. Follow `nextCursor` with
`--cursor`, repeating the same time window and source. The recent cursor is
bound to that window, source, and command. To debug new arrivals or synchronize
recent activity, poll **without a cursor**, optionally using `--since` with an
overlap, and deduplicate the full JSON message IDs. A cursor pages older
messages; it does not watch for arrivals. There is no durable synchronization
state or background watcher in this command.

## All command families

| Family | Commands | Use |
| --- | --- | --- |
| Query profiles | `profile path/template/list/show/validate/set-default` | Optional named sources for a second account or a snapshot. Live queries need none. |
| Direct reads | `source status`, `conversations list/find`, `contacts list`, `messages list/recent/search`, `message get` | Read one bounded page from live data or a snapshot, without creating a restoration. |
| Attachments | `attachment inspect/materialize` | Inspect one message, or copy one chosen attachment to a new private path. |
| Backups (snapshots) | `snapshot recovery-kit/local-credential/create/create-capture/verify/rewrap/rekey/retention` | Create, reopen, rotate keys for, verify, and retire encrypted backups. See [recoverable snapshots](RECOVERABLE_SNAPSHOTS.md). |
| Offline restoration | `preflight`, `probe`, `restore`, `restore-publish` | Check and restore a saved, owner-authorized copy of WeChat's files. |
| Diagnostics | `diagnose-batch`, `diagnose-available`, `diagnose-archive-schema`, `diagnose-archive-payloads` | Produce structural reports, with private content removed, for data GreenBubbles can't fully read. |
| Archive audit and merge | `audit-archive`, `audit-acquisition-chain`, `reconcile`, `merge-incremental` | Check archive integrity and combine incremental captures. |
| Replica upkeep | `replica-bootstrap/status/sync/publish/follow*`, `audit-replica*`, `prepare-replica-recovery` | Maintain and recover the replica. |
| Replica reads | `replica-conversations/search/message/coverage/changes/cached-moments` | Query data only the replica has. |
| Direct AI connector | `connector-policy-direct`, `connector-query-direct`, `connector-serve-direct` | Apply a policy and audit log directly to live or snapshot queries. |
| Replica AI connector | `tool-policy/list/recent/search/draft`, `connector-serve/call` | Serve policy-limited replica reads and drafts that are never sent. |
| AI context | `ai-query`, `ai-export`, `audit-ai-context`, `ai-memory-export`, `audit-ai-memory`, `ai-summarize-direct` | Create, verify, and summarize minimized AI context that keeps citations. See [Give an AI access to only some chats](AI_CONTEXT_CLI.md). |
| Personal memory | `memory prepare [--extend]`, `memory next`, `memory page/acknowledge/commit/status` | Build an evidence archive (or extend it with `--extend`), choose what to review, and feed an agent fixed-size pages. See [below](#personal-memory-commands). |
| Personal memory driver | `tick`, `manifest-refresh`, `revise` | Subcommands of `scripts/personal-memory-parallel.py`, which runs agents over the archive. `--format python\|markdown` on `tick` picks the output format. |
| Operational evidence | `synthetic-benchmark`, `compose-latency-evidence`, `summarize-latency-evidence`, `audit-connector-log/state` | Generate or check aggregate release and service measurements. |
| Sending | `send …` | Inspect the separate experimental sending adapter. Public builds keep it closed. |

Most of these are advanced. Listing a command here doesn't mean you should run
it first.

## Passing keys and passwords

Every direct read takes exactly one access mode. Daily live use needs none of
these flags; they're for snapshots, other accounts, and test data.

| Source | Option | What goes on stdin |
| --- | --- | --- |
| Live encrypted WeChat | `--passphrase-stdin` | the key, as line 1 |
| Snapshot, recovery phrase | `--snapshot-recovery-kit <file>` | nothing (the phrase is read from the owner-only file) |
| Snapshot, this Mac's unlock file | `--snapshot-local-credential <file>` | nothing |
| Snapshot, passphrase | `--snapshot-passphrase-stdin` | the passphrase, as line 1 |
| Older snapshot, raw key | `--snapshot-key-stdin` | the key, as line 1 |
| Test data or plaintext | `--decrypted` | nothing |

When a search also reads from stdin, the key or passphrase is line 1 and the
search text is the rest. In file-based and plaintext modes, stdin holds only
the search text.

**Never put a key, passphrase, recovery phrase, replica key, or private search
text in a command-line argument.** Arguments are visible to every process on
the Mac, and typed values end up in shell history. Redirect an owner-only file
into stdin instead.

## Reading `messages list`, `messages recent`, and `messages search`

All three print JSON Lines by default: one header line, then one line per message.

**Header fields:**

- `returned`: how many messages are on this page.
- `hasMore`: `true` if there is another page.
- `nextCursor`: present when there is another page.
- `order`: `newest`.
- `timezone`: the local UTC offset used for every `at`.
- `searchFreshness` (search only): present when the native search index wasn't
  checked.
- `accountHolderKnown`: present, as `false`, only when the account holder
  couldn't be identified.
- `conversationId`: present on each chat's page when you read several chats at
  once.

**Message fields:**

- `from`: the sender's name. Never empty. It uses the first available of:
  remark, nickname, alias, in-group display name, wxid.
- `self`: `true` if the account holder sent it, otherwise `false`.
- `at`: local time as `YYYY-MM-DD HH:MM`, in the header's timezone.
- `text`: the message text.
- `file` (list and recent): a local path for an image, video, or document.
  Encrypted images are decoded into `~/.greenbubbles/cache/media`. The path is
  left out when the file can't be opened. Don't copy a `file` path into notes.
- `chat` (search and recent): the chat's display name. A chat with no display name
  gets `conversationId` instead. Recent lines always include `conversationId`.
  Search lines have no `file`; open the chat
  with `messages list` to get one.

Identifiers, type codes, and source metadata are left out. `--json` prints the
full response instead, including the opaque message id that `message get`
needs. You can change the default with `[output] format` in
`~/.greenbubbles/config.toml`.

**Placeholders in `text`:**

| Placeholder | Meaning |
| --- | --- |
| `[voice]` | A voice message with no transcript. It has no `file`. |
| `[unknown]` | A body made only of identifiers and `true`/`false`, or a body that `--redact` emptied. |
| `[emoji]` | A body made only of bracketed emoji names, such as `[Grin]` or `[偷笑]`. Words next to the names are kept. |
| `[revoked]` | A short recall notice, including a line that only says who recalled a message. The withdrawn words are not recovered. A longer sentence that mentions a recall stays as text. |
| `[meeting]` | A video-meeting invitation with a meeting id or join link, only when `--redact` is set. |

**Personal details and `--redact`:** phone numbers, email addresses, identity
numbers, links, and meeting invitations stay in the text by default. With
`--redact`:

- A resident identity number, a mainland mobile number, an email address, and
  an `http` or `https` link are removed. This includes those inside a quote, a
  link title, a file name, a transfer note, and `from`.
- A video-meeting invitation with a meeting id or join link becomes
  `[meeting]`.
- Surrounding words stay. A body that was only one of those values becomes
  `[unknown]`.
- A bare hostname stays either way.

**Paging and time windows:**

- To get the next page, repeat the command with `--cursor <nextCursor>`.
- `--since` and `--until` are inclusive Unix timestamps in seconds. Repeat them
  with every cursor.
- Repeat `--conversation` up to 24 times to read the same time window from
  several chats with one database open. Each chat is its own page, and its
  header includes `conversationId`.
- To continue one of those chats, use `--cursor` with a single
  `--conversation`. A cursor with several conversations is rejected.

## Reading `chats rank`

`greenbubbles chats rank` counts how many messages the account holder sent in
each chat. One-on-one chats come before groups. It returns no message text.

- **Options:** `--minimum-self-messages <n>` keeps only chats where you sent at
  least `n` messages (default 10). `--limit` is 1..2000 (default 100).
- **Header fields:** `returned`, `qualifying`, `hasMore`, `order`
  (`direct-then-self-count`), `timezone`, `accountHolderKnown`,
  `coverageComplete`, `conversationCount`, and `nextCursor` when there is
  another page.
- **Chat fields:** `from` (never empty), `id`, `kind`, `selfCount`, and `last`
  (local `YYYY-MM-DD HH:MM`).
- **Paging:** follow `nextCursor` with `--cursor`, and repeat
  `--minimum-self-messages`.
- **`--json`** prints the full report, including each chat's total message
  count.

## Limits and reading rules for all reads

- Conversation and message lists default to 100 items, with a hard maximum of
  500.
- Contact pages use the same 1..500 limit.
- Search returns at most 200 results. When search falls back to scanning the
  source, it scans a bounded window at a time, so you may need to follow the
  cursor through *empty* pages. An empty page is not the end of a search;
  keep going while `hasMore` is `true`.
- There is no `--all` option for direct reads, and there won't be one.
- Cursors and message ids are tied to their source, command, conversation, and
  filters. Reusing one elsewhere is rejected, not reinterpreted.
- In `--json` output, check `ok`, `consistency`, `warnings`, `coverage`, and
  `page` before trusting a result.
- **Missing data under stale, unavailable, or partial coverage is not evidence
  that something was deleted.** This is the most important reading rule, and
  the one automated callers most often get wrong.

The full response format and paging design are in
[ARCHITECTURE.md](ARCHITECTURE.md). The AI request format is in
[AI_CONTEXT_CLI.md](AI_CONTEXT_CLI.md).

## Progress output

Most restoration, audit, snapshot, and export commands accept
`--progress-json`, `--quiet-progress`, or `--progress-file` (a new owner-only
file). Progress events contain no message text, credentials, or source paths.
A report can still reveal private totals, so treat it as private until you've
read it.

## Personal memory commands

These commands build an evidence archive and feed it to an agent in pages. Most
people should use the [agent skill](AGENT_SKILLS.md) instead, which reads live
data directly. Background is in [PERSONAL_MEMORY.md](PERSONAL_MEMORY.md).

**`memory prepare`** builds one fixed `allMessages` archive. It prints
content-free counts on stderr as it goes (phases, conversations, rows, and
messages whose text was loaded).

**`memory next`** chooses the next batch and prints only a small description of
it.

- Filters: repeatable `--conversation`, `--conversation-kind`, and `--sender`,
  plus inclusive RFC 3339 `--from` and `--through`. With no filters, every
  archived message with loaded text is in scope.
- `--subject` defaults to `account-holder`. It also accepts
  `person:<selector>`, or `none` for notes centered on conversations.
- A batch is 16 KiB..2 MiB and at most 5,000 messages.
- `--max-text-bytes` limits stored chat text per batch.
- `--max-messages` also limits the message count, which text size doesn't
  predict. Use it to fit a batch into a fixed agent context window. It's a soft
  limit: it stops a batch from taking another unit, but never splits or
  refuses the one unit a batch must deliver whole.
- New archives can use the `accountHolderRelevance` order, which covers active
  relationships and months first. It still schedules every unit.

**`memory page`** splits the current batch into JSON pages of at most 49,152
bytes each. **`memory acknowledge`** marks one delivered page as read.

- An agent can leave out the batch and page selectors; the commands use the
  one current batch and delivered page.
- `--batch` and `--page-token` remain available for audit and replay.
- Every timestamp an agent sees is RFC 3339 in the archive's timezone.
- Sticker, location, and system messages arrive as their human-readable text,
  not raw XML. The account holder is labeled `Me` beside their source id.

**`memory commit`** saves the agent's changes to the notes. It succeeds only
after every page was delivered and acknowledged.

- Changed factual lines need citations. A commit is rejected if it cites
  evidence that wasn't retained, retains evidence without citing it, or puts
  more than eight representative citations on one changed factual line.
- In `me.md`, each changed factual line also needs at least one citation of a
  message the account holder wrote.
- A rejection lists every problem at once, with one-based line numbers and the
  offending aliases and paths, so a single retry can fix them all.
- After fully reviewing a batch with nothing worth keeping, use
  `--reviewed-no-durable-memory`. The notes must then be byte-for-byte
  unchanged.

**`memory status`** reports archive totals separately from what the current
scope has selected and committed. It also shows the resolved subject and
summary counts of coverage limits, so an agent never has to read the archive's
internal files.

- `complete: true` means the current scope is complete. It proves every
  archived message was reviewed only when the archive uses the current format
  and `scope.allMessages` is `true`.

## Where to go next

| Task | Document |
| --- | --- |
| Named sources for another account or a backup | [Query profiles](QUERY_PROFILES.md) |
| Backups and recovery | [Recoverable snapshots](RECOVERABLE_SNAPSHOTS.md) |
| Offline restoration and publication | [Restoration specification](RESTORATION_SPEC.md) |
| The replica | [Replica specification](REPLICA_SPEC.md) · [operations](REPLICA_OPERATIONS.md) |
| The local request format | [Connector API](CONNECTOR_API.md) |
| Giving an AI access | [Give an AI access to only some chats](AI_CONTEXT_CLI.md) |
| Private knowledge base from live history | [Personal memory](PERSONAL_MEMORY.md) |
| Verifying any of the above | [Auditing](AUDITING.md) |
| The closed send path | [Send adapter](SEND_ADAPTER.md) |
