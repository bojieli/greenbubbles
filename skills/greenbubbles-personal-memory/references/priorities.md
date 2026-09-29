# Which conversations to index first

Ask for the time scope and any existing summary before measuring or reading.
A 7-day snapshot, a 30-day snapshot, a year, and a lifetime are different
tasks. If the user points at an existing summary, that directory is the
project to update. Do not create a parallel one.

The output is a personal memory and a knowledge base. Keep durable facts
about the account holder, and durable facts about the people, projects, and
decisions in the reviewed chats. Attribute other people's claims to them.
Do not keep a transcript.

## Measure the database first

```sh
greenbubbles source status
greenbubbles chats rank --minimum-self-messages 10 --limit 2000
```

`source status` reports database count and storage bytes. It does not print
paths or message text. `chats rank` is one read-only scan. It returns no
message text. Use it instead of opening every conversation to count messages.

The report fields that matter:

- `conversationCount`: chats that have a message table.
- `qualifyingConversationCount`: chats at or above `--minimum-self-messages`.
- `accountHolderKnown`: whether self-message counts are meaningful. If this
  is false, stop and resolve the account binding. Do not guess self from a
  display name.
- `coverageComplete`: if false, treat the ranking as partial and say so.
- Each item has `kind` (`direct` or `group`), `selfMessageCount`,
  `messageCount`, `lastSelfMessageUnix`, and `displayName`.

`greenbubbles chats` is the recent-activity list. It has no self-message
count, so it is the wrong first query for importance.

## The default importance metric

When the user does not name conversations, rank by the account holder's own
participation, then by recency:

1. Keep a chat only when the account holder sent at least 10 messages there.
   Raise the threshold when the qualifying set is too large for the requested
   pass. A chat the user mostly received is not evidence about the user.
2. Review direct chats before group chats. In a group, most messages are
   other people. A group qualifies only when the account holder sent at least
   10 messages there, and it still comes after direct chats with the same
   self-message count.
3. Within a kind, a larger `selfMessageCount` comes first. Recency breaks
   ties: `lastSelfMessageUnix`, newest first.
4. Apply the requested time window before paging. `chats rank` shows
   `lastSelfMessageUnix`; a chat whose last self-sent message is older than
   the window still qualifies when the user asked for lifetime coverage, and
   is skipped when the user asked for a recent window. Start at the newest
   self-authored messages inside the window and page backward to the window
   start. Do not start at the oldest message.
5. Skip official accounts, service accounts, and file-transfer or system
   chats unless the user names them. Skip a direct chat whose recent pages
   are only logistics with no durable fact.

This is a selection metric, not a claim that unselected chats contain nothing
important. Report the threshold, the qualifying count, how many chats were
actually read, and that the rest was not reviewed.

## Long passes: a year, two years, or a lifetime

A long pass is a knowledge-base build, not a snapshot. The failure mode is
reading the newest page of a few chats and writing one paragraph per person.
That is not this workflow.

Order:

1. Ask for the time window and any existing project path. Record both.
2. `source status`, then `chats rank --minimum-self-messages 10 --limit 2000`.
   The default rank page is 100 and will hide most of a two-year set. If
   `qualifyingConversationCount` is larger than the returned page, raise the
   limit. The rank limit goes to 2000.
3. Select every direct chat whose last self-sent message is inside the window
   and whose self-sent count is at least 10. Then select groups by the same
   self-sent threshold. Do not stop at the first page of names.
4. Prepare a corpus with the Markdown selection policy. Page it with
   `memory next` and `memory page`. Read each delivered page completely.
5. Organize into domain files. Follow
   [format-markdown.md](format-markdown.md): one domain per life area, a
   schema, one state entry per fact, and an append-only history. Update the
   manifest after each committed batch. A person is a relationship in
   `family` or `social`, not a substitute for the domains their messages
   inform.
6. Continue until the window is covered or the user sets a smaller bound.
   Record chats not yet paged as remaining scope. Do not describe a partial
   pass as the two-year or lifetime record.

Enough means every qualifying chat in the window has had its delivered pages
read, acknowledged, and either filed into a domain or explicitly marked as
having no durable fact. A count of recent pages is not enough.

## What becomes memory

Keep a fact only when it is still useful after the conversation is closed:
who someone is to the account holder, a durable role or project, a decision,
or a correction of an earlier fact. Record other people's claims as their
claims. Do not copy affection, routine logistics, prices, or a passing
denial into memory just because it appeared in a recent page.

Cite the evidence alias and date when the memory workflow provides one. A
bounded `messages list` page has opaque message IDs; keep those IDs in the
private project, not in a summary you send back unprompted.
