# Scope and reading workflow

## Ask first

Before measuring or reading, ask the user two things unless the request
already answers them:

1. **Time scope.** A 7-day snapshot, a month, a year, two years, and a
   lifetime are different knowledge bases:
   - **7 days**: the last week.
   - **30 days**: about one month.
   - **1 year** or **2 years**: the recent durable record.
   - **lifetime**: every qualifying chat, paged from the newest message
     backward.

   Record the start and end dates in `manifest.md`.
2. **Existing project.** If a summary or this knowledge base already exists,
   ask for its path. That directory is the project to revise. Read
   `index.md`, `manifest.md`, and the articles before changing them.

The output is a knowledge base of articles, linked from `index.md`. Keep
durable facts about the account holder and about the people, projects, and
decisions in the reviewed chats. Do not keep a transcript.

## Choose the chats

Measure with `source status`, then rank with
`chats rank --minimum-self-messages 10 --limit 2000`. The output fields are
explained in [cli.md](cli.md#reading-chats-rank).

### The default importance metric

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
4. Apply the requested time window before paging. A chat whose last
   self-sent message is older than the window still qualifies when the user
   asked for lifetime coverage, and is skipped when the user asked for a
   recent window. Start at the newest self-authored messages inside the
   window and page backward to the window start. Do not start at the oldest
   message.
5. Skip official accounts, service accounts, and file-transfer or system
   chats unless the user names them. Open every other selected chat. A page
   that is only logistics is marked as no durable fact after it is read. It
   is not a reason to leave the chat unread.

This is a selection metric, not a claim that unselected chats contain
nothing important.

## Read in time order

A year, two-year, or lifetime request is a knowledge base, not a snapshot
and not a few memories. The failure mode is reading the newest page of the
dozen loudest chats, running a handful of searches, and filing scattered
lines. Token cost is not a reason to stop, to lower the threshold, or to
sample. Volume is not a reason to stop the project. Read widely, then
organize.

Rank order and reading order are different. Rank decides who is in scope:
direct before group, then the account holder's own message count. Reading
follows time. The same decision is often told in a direct chat and a group
on the same days. Sorting those messages by `at` makes the episode one
story. Sorting them by chat leaves a fragment in each article and hides
the contradiction until much later. Message time is the join key, not the
date of the event inside the story.

Read in batches that fit the current context. A batch is one time slice
across several chats, not one chat read down to the window start. A slice
is the newest unread span that still fits in one pass, about a month when
the chats are dense. Inside the slice, open qualifying directs first, in
rank order, one or a few pages each. Then open qualifying groups whose
`last` falls in the slice, and keep only lines the account holder sent or
that name the episode. When the episode has a name, use `messages search`
to find the other chats in that slice, then confirm each hit in
`messages list`. Park a chat's older cursor until the slice moves back to
that date. Do not keep turning one person's pages through the previous year
while other qualifying chats from this month are still unopened.

Order:

1. Ask for the time window and any existing project path. Record both.
2. Run `source status`, then `chats rank`. Do not stop at the first page of
   names.
3. Select every direct chat whose last self-sent message is inside the
   window and whose self-sent count is at least 10. Then select groups by
   the same self-sent threshold.
4. Read the current slice with one `messages list` that repeats
   `--conversation` for the chats in that slice, as in
   [cli.md](cli.md#time-slices). Pass the same `--since` and `--until` for
   every chat. Follow each `nextCursor` on its own later command. One page
   of one chat is not the slice.
5. Revise the articles once for the slice, following
   [format-markdown.md](format-markdown.md). Record each cursor.
6. Take the next slice. Continue until the window is covered or the user
   sets a smaller bound. Record chats not yet paged as remaining scope in
   `manifest.md`. Do not describe a partial pass as the two-year or
   lifetime record.

Enough means every qualifying chat in the window has been paged to the
window start, far enough that its durable facts are in the articles, or the
chat was marked as having no durable fact. A count of recent pages is not
enough.

- Reaching the window start is not the same as having no durable fact.
  Mark no durable fact only after the page is read and it has none. A
  finished chat can still add sentences to the episode.
- `returned` 0 and `hasMore` false means that window page is empty. Rank
  `selfCount` can still be higher: those messages sit outside `--since`,
  or they did not decode. Mark the page read. It is not unread, it has no
  durable fact, and it does not mean that person does not exist.

## Incremental pass

Open the existing project with the same commands. Do not start a second
project. Read the articles. Search and page for
what they already discuss, plus chats whose last self-sent message is newer
than the last coverage date. Revise sentences that the new messages change.
Leave sentences that still hold. Record the new window in the manifest.

A later pass can miss messages that were imported with old timestamps.
When the user asks for reconciliation, page the selected chats again across
the original window. Absence from one search is not a deletion.

Before writing, read [priorities.md](priorities.md) for evidence selection and
[format-markdown.md](format-markdown.md) for article structure.
