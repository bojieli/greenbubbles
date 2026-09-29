# Which conversations to read first

Ask for the time scope and any existing project before measuring or reading.
A 7-day snapshot, a 30-day snapshot, a year, and a lifetime are different
tasks. If the user points at an existing summary, that directory is the
project to revise.

The output is a knowledge base of articles, linked from `index.md`, written
in the language the account holder usually writes. The language rule is in
[format-markdown.md](format-markdown.md). Keep
durable facts about the account holder and about the people, projects, and
decisions in the reviewed chats. Attribute other people's claims to them.
Do not keep a transcript, and do not stop at a bullet list of facts.

## Measure the database first

```sh
greenbubbles source status
greenbubbles chats rank --minimum-self-messages 10 --limit 2000
```

`source status` reports database count and storage bytes. It does not print
paths or message text. `chats rank` is one read-only scan. It returns no
message text. Use it instead of opening every conversation to count messages.

The default rank page is JSON Lines. The header fields that matter:

- `conversationCount`: chats that have a message table.
- `qualifying`: chats at or above `--minimum-self-messages`.
- `accountHolderKnown`: whether self-message counts are meaningful. If this
  is false, stop and resolve the account binding. Do not guess self from a
  display name.
- `coverageComplete`: if false, treat the ranking as partial and say so.
- `hasMore` and `nextCursor`: the default limit is 100. Pass `--limit 2000`,
  or continue with `--cursor` and the same `--minimum-self-messages`.

Each chat line has `from` (never empty), `id`, `kind` (`direct` or `group`),
`selfCount`, and `last` (local time of the account holder's newest message
in that chat). `--json` adds the total message count. Use `from` for the
person's name and `id` when you open the chat.

`greenbubbles chats` is the recent-activity list. It has no self-message
count, so it is the wrong first query for importance.

## The default importance metric

When the user does not name conversations, rank by the account holder's own
participation, then by recency:

1. Keep a chat when the account holder sent at least 10 messages there.
   Do not raise that threshold to shrink a year, two-year, or lifetime pass.
   A chat the user mostly received is not evidence about the user.
2. Review direct chats before group chats. In a group, most messages are
   other people. A group qualifies only when the account holder sent at least
   10 messages there, and it still comes after direct chats with the same
   self-message count.
3. Within a kind, a larger `selfCount` comes first. Recency breaks ties:
   `last`, newest first.
4. Apply the requested time window before paging. `chats rank` shows `last`
   as local time. A chat whose last self-sent message is older than the
   window still qualifies when the user asked for lifetime coverage, and is
   skipped when the user asked for a recent window. Start at the newest
   self-authored messages inside the window and page backward to the window
   start. Do not start at the oldest message.
5. Skip official accounts, service accounts, and file-transfer or system
   chats unless the user names them. Open every other selected chat. A page
   that is only logistics is marked as no durable fact after it is read. It
   is not a reason to leave the chat unread.

This is a selection metric, not a claim that unselected chats contain nothing
important. Report the threshold, the qualifying count, how many chats were
actually read, and that the rest was not reviewed.

## Long passes: a year, two years, or a lifetime

A long pass is a knowledge base, not a snapshot and not a few memories.
The failure mode is reading the newest page of the dozen loudest chats,
running a handful of searches, and filing scattered lines. Token cost is
not a reason to stop, to lower the threshold, or to sample. The user asked
for a knowledge base. Read widely, then organize.

Do the reading in batches that fit the current context. Read a few
qualifying chats, or the next pages of a long chat, revise the articles,
record the cursor and what remains, then read the next batch. A batch is
how the work fits in one pass. It is not a smaller knowledge base. Continue
until every qualifying chat in the window has been paged to the window
start, or marked as having no durable fact.

Order:

1. Ask for the time window and any existing project path. Record both.
2. `source status`, then `chats rank --minimum-self-messages 10 --limit 2000`.
   The default rank page is 100 and will hide most of a two-year set. If
   `qualifying` is larger than `returned`, pass `--limit 2000` or follow
   `nextCursor`. The rank limit goes to 2000.
3. Select every direct chat whose last self-sent message is inside the window
   and whose self-sent count is at least 10. Then select groups by the same
   self-sent threshold. Do not stop at the first page of names.
4. Read with `messages list` and `messages search`, as in
   [cli.md](cli.md). Those commands print a compact reading page. Page each
   selected chat from the newest message backward, passing `--since` and
   `--until` for the window and repeating them with `--cursor`. One page is
   not enough. A long chat continues on the next batch. Search for the
   projects, organizations, and decisions you have already seen so older
   mentions are not missed.
5. Revise the articles as you go. Follow
   [format-markdown.md](format-markdown.md): `index.md` plus one article per
   life area. A person is a relationship in `family` or `social`, and also
   appears inside the articles their messages inform. Write prose sections,
   not a list of extracted lines.
6. Continue until the window is covered or the user sets a smaller bound.
   Record chats not yet paged as remaining scope in `manifest.md`. Do not
   describe a partial pass as the two-year or lifetime record.

Enough means every qualifying chat in the window has been paged far enough
that its durable facts are in the articles, or the chat was marked as having
no durable fact. A count of recent pages is not enough.

## What becomes an article sentence

Keep a fact when it is still useful after the conversation is closed: who
someone is to the account holder, a durable role or project, a decision, or
a correction of an earlier fact. Record other people's claims as their
claims. Do not copy affection, routine logistics, prices, phone numbers,
addresses, or meeting and form links into the articles.
