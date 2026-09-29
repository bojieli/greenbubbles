# Which conversations to index first

Do this before preparing a corpus or reading message pages. The goal is a
durable memory of the account holder, not a transcript and not a knowledge
base of other people's claims.

## Measure the database first

```sh
greenbubbles source status
greenbubbles chats rank --minimum-self-messages 10 --limit 40
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
4. Start at the newest self-authored messages in the selected chat and page
   backward. Stop at the user's requested bound. Do not start at the oldest
   message.
5. Skip official accounts, service accounts, and file-transfer or system
   chats unless the user names them. Skip a direct chat whose recent pages
   are only logistics with no durable fact.

This is a selection metric, not a claim that unselected chats contain nothing
important. Report the threshold, the qualifying count, how many chats were
actually read, and that the rest was not reviewed.

## What becomes memory

Keep a fact only when it is still useful after the conversation is closed:
who someone is to the account holder, a durable role or project, a decision,
or a correction of an earlier fact. Record other people's claims as their
claims. Do not copy affection, routine logistics, prices, or a passing
denial into memory just because it appeared in a recent page.

Cite the evidence alias and date when the memory workflow provides one. A
bounded `messages list` page has opaque message IDs; keep those IDs in the
private project, not in a summary you send back unprompted.
