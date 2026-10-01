# Live queries and the reading page

Use these commands in the current agent's shell. No driver and no separate
API key. Examples use `greenbubbles`. Substitute the verified absolute Rust
CLI path when it is not on `PATH`. Quote paths. Never put a passphrase in an argument. Search text uses
`--query "text"`, or `--query-stdin` for interactive/piped input.

## Commands

```sh
greenbubbles source status
greenbubbles chats rank --minimum-self-messages 10 --limit 2000
greenbubbles chats find "Alice"
greenbubbles messages list --conversation ID --since <unix> --until <unix> --limit 80
greenbubbles messages search --query "keyword" --since <unix> --limit 25
```

`source status` reports database count and storage bytes. It prints no
paths and no message text.

`greenbubbles chats` is the recent-activity list. It has no self-message
count, so it is the wrong first query for importance.

## Names and latest activity

`chats find "name"` searches known identities by partial nickname, remark,
alias, or ID without reading messages. `--conversation` on list/search/get
accepts an exact ID or an unambiguous name, ignoring case. Exact names precede
partial matches. If a name is ambiguous, choose an exact ID from the error;
do not guess which person the user meant.

For an explicitly requested view of recent messages across chats:

```sh
greenbubbles messages recent --limit 50
greenbubbles messages recent --since <unix> --json
```

This reads across all identifiable chats. Use per-chat `messages list` for
incremental work scoped to chosen chats. Recent is a discovery/debugging query,
not a substitute for reading a selected chat in context or for ranking its
importance. Compact lines include `chat` when known and `conversationId`;
`--json` supplies full IDs for citations and deduplication. Messages are globally
ordered by creation time, newest first, with deterministic ties. The default
is 100 and the maximum is 500. Check full-envelope warnings and coverage:
unidentifiable message tables may be omitted and databases are not read as one
atomic snapshot.

A recent cursor pages older messages in the same source and time window. To
check new activity, run again without a cursor, optionally using an overlapping
`--since` window and deduplicating full message IDs. This command stores no
synchronization state and starts no watcher. Do not count a recent page as
reviewing the history of every chat it mentions.

## Reading `chats rank`

`chats rank` is one read-only scan and returns no message text. Use it
instead of opening every conversation to count messages. Its default page
is JSON Lines. The header fields that matter:

- `conversationCount`: chats that have a message table.
- `qualifying`: chats at or above `--minimum-self-messages`.
- `accountHolderKnown`: whether self-message counts are meaningful. If this
  is false, stop and resolve the account binding. Do not guess self from a
  display name.
- `coverageComplete`: if false, treat the ranking as partial and say so.
- `hasMore` and `nextCursor`: the default limit is 100, which hides most of
  a two-year set. When `qualifying` is larger than `returned`, pass
  `--limit 2000` (the maximum), or continue with `--cursor` and repeat the
  same `--minimum-self-messages`.

Each chat line has `from` (never empty), `id`, `kind` (`direct` or
`group`), `selfCount`, and `last` (local time of the account holder's
newest message in that chat). `--json` prints the full report, including
each chat's total message count. Use `from` for the person's name and `id`
when you open the chat. How to rank is in [workflow.md](workflow.md).

## Reading a message page

`messages list` and `messages search` print JSON Lines. The first line is a
header. Each later line is one message:

```
{"returned":80,"hasMore":true,"order":"newest","timezone":"+08:00","nextCursor":"..."}
{"from":"Account Holder","self":true,"at":"2026-08-31 21:04","text":"..."}
{"from":"Friend","self":false,"at":"2026-08-31 21:05","text":"[image]","file":"/path/to/image.jpg"}
```

`from` is never empty. It is the remark, then the nickname, then the alias,
then the name that person uses inside that group, then the wxid. `self` is
always present: `true` when the account holder sent the line, `false`
otherwise. Do not infer either fact from the display name. `at` is local
time, `YYYY-MM-DD HH:MM`, in the header's `timezone`. Compare dates with
`at`; do not convert a unix timestamp. Markup and raw internal identifiers
are omitted.

An image, video, or document adds `file`, a local path. Open that path when
the picture or document matters to the article. Do not copy the path, or the
bytes' cache location, into the knowledge base.

### Time slices

A time slice is one `messages list` with the same `--since` and `--until`
repeated on several `--conversation` flags, at most 24. The database opens
once. Each chat is its own page, and that header includes `conversationId`.
That id is not the person's name. The name is `from` on each message line.
Line the chats up on `at`. Follow one chat's `nextCursor` with a single
`--conversation` and `--cursor`, and repeat the same `--since` and
`--until`. Do not pass `--cursor` together with several conversations.

### Placeholders

Some lines are labels, not prose. Leave each one as a placeholder, and do
not invent the words behind it:

- `[voice]` is voice with no transcript. It has no `file`. Do not invent a
  transcript.
- `[attachment]` is an app card with no readable title. It has no `file`
  path.
- A call that leaves only a duration is the same. File that they talked.
  The words are not on the page.
- `[unknown]` is a text body that is only identifiers and `true` or `false`.
- `[emoji]` is a text body that is only bracketed emoji names, in English
  such as `[Grin]` or in Chinese such as `[偷笑]`. Do not invent the words
  inside an emoji-only body. Words kept beside those names are the text.
- `[revoked]` is a short recall notice, including `You recalled a message`,
  a Chinese notice, and a line that only says someone recalled a message.
  Do not recover the withdrawn words, and do not treat the English word
  "You" or a name inside that notice as proof of `self`. A longer sentence
  that merely mentions a recall stays text.

How to file around `[image]`, `[quote]`, `[file]`, and the other
placeholders is in [priorities.md](priorities.md#placeholders-and-scratch-notes).

### Personal details on the page

Phone numbers, email addresses, identity numbers, street addresses, links,
and meeting invitations stay in the text, including a quote, a link title,
a file name, a transfer note, and `from`. Do not pass `--redact` for this
private knowledge base. That flag is only for a page that must leave those
values out. Do not pass `--redact` and then reconstruct what it removed.
Which of these values may go into an article is in
[priorities.md](priorities.md#secrets-personal-details-and-procedures).

## Paging

Follow `nextCursor` with `--cursor`, and repeat `--since` and `--until`,
until `hasMore` is false or the oldest line is before the window.
`returned` can equal `--limit` while `hasMore` is false. That header is the
end of the window. Do not invent another cursor, and do not open another
page because the count matches the limit. `--limit` is 1..500 for list and
1..200 for search.

A large chat is many pages. Read a batch, revise the articles, record the
cursor, then read the next page. Keep only lines that belong in an article.

## Search

`messages search` accepts `--query "text"` or `--query-stdin`, but not both. Search lines add `chat` when the conversation has
a display name, and `conversationId` when it does not. A search line has no
`file`. Open the conversation with `messages list` to read an image or file
found by search.

Hits can be older than the window and can be forwards inside groups. Keep a
hit only when the sender and the date support the sentence you write. A
search header with `searchFreshness` of `unverified` means the native index
was not checked against the message shards. Confirm an important claim in
`messages list` when the hit is ambiguous.

## Output settings

`--json` prints the full envelope. Use it only when a later command needs a
message id. The default is the reading page. `[output] format` in
`~/.greenbubbles/config.toml` can set `brief` or `json`; `--json` and
`--brief` override that file for one command. `[source] root` and
`passphrase_file` in the same file are the database directory and the
passphrase file.
