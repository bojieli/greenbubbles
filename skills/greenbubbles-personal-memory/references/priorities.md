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
Do not keep a transcript, and do not stop at a dump of extracted lines.
A nested list of short sentences is how a complicated episode stays
readable. Do not keep feeding one paragraph until it becomes a wall.

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

Do the reading in batches that fit the current context. A batch is one
time slice across several chats, not one chat read down to the window
start. Revise the articles, record each cursor, then take the next slice.
Continue until every qualifying chat in the window has been paged to the
window start, or marked as having no durable fact.

Rank order and reading order are different. Rank decides who is in scope:
direct before group, then the account holder's own message count. Reading
follows time. The same decision is often told in a direct chat and a group
on the same days. Sorting those messages by `at` makes the episode one
story. Sorting them by chat leaves a fragment in each article and hides
the contradiction until much later.

A slice is the newest unread span that still fits in one pass, about a
month when the chats are dense. Inside the slice, open qualifying directs
first, in rank order, one or a few pages each. Then open qualifying groups
whose `last` falls in the slice, and keep only lines the account holder
sent or that name the episode. When the episode has a name, search it and
confirm the hits. Park a chat's older cursor until the slice moves back to
that date. Do not keep turning one person's pages through the previous year
while other qualifying chats from this month are still unopened.

Message time is the join key, not the date of the event inside the story.
Someone can describe March in a November message. The article states the
event date they named, and the reference states when they said it.

Order:

1. Ask for the time window and any existing project path. Record both.
2. `source status`, then `chats rank --minimum-self-messages 10 --limit 2000`.
   The default rank page is 100 and will hide most of a two-year set. If
   `qualifying` is larger than `returned`, pass `--limit 2000` or follow
   `nextCursor`. The rank limit goes to 2000.
3. Select every direct chat whose last self-sent message is inside the window
   and whose self-sent count is at least 10. Then select groups by the same
   self-sent threshold. Do not stop at the first page of names.
4. Read the current slice with one `messages list` that repeats
   `--conversation` for the chats in that slice, as in [cli.md](cli.md).
   Pass the same `--since` and `--until` for every chat. Follow each
   `nextCursor` on its own later command. One page of one chat is not the slice.
   On that page, `[voice]`, `[unknown]`, `[emoji]`, and `[revoked]` are
   labels, not prose. Do not invent a transcript, a withdrawn sentence, or
   the words inside an emoji-only body. Words kept beside an emoji name are
   the text. A meeting invitation stays text unless `--redact` was passed.
   Do not pass `--redact` for this project.
5. Revise the articles once for the slice. Follow
   [format-markdown.md](format-markdown.md): one episode, in `at` order,
   inside the life-area article. A person stays a short entry in `family`
   or `social` and is not given a second copy of the episode. Break a
   paragraph that is already long before you add to it. Write the new
   dates as short sentences or a nested list. Leave out any sentence that
   only denies an inference or announces an omission.
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
claims. Keep a phone number, email address, identity number, street
address, or link when the message states it. Do not copy affection or
routine logistics that is not a durable fact.

A few readings keep getting mis-filed:

- A short run of digits in front of a display name, such as a class year,
  is not a phone number. A real mobile in the name stays on the page. Do
  not invent an identity from the digits that remain.
- A display name that includes an employer, a desk, or "HR" is that
  contact's label. It does not confirm where the account holder is going,
  and it does not fill in a company he left unnamed. File the company only
  when his own message names it.
- Reaching the window start is not the same as having no durable fact.
  Mark no durable fact only after the page is read and it has none. A
  finished chat can still add sentences to the episode.
- "I saw the news that you joined X" is the other person's framing. File it
  as their sentence. It does not confirm a move, and it does not get its
  own episode beside the account holder's own statements that day.
- Terms of affection, a red envelope, or a shared travel note do not
  identify that contact with a spouse or with another person already in
  the articles. Mark the page as no durable fact when that is all it holds.
- The same decision told to two people on the same day is one episode.
  Add the second telling in `at` order. Do not open a second employer.
- Do not record a how-to for keeping an account or network in good
  standing: proxy hops, identity-check steps, or similar. A street address
  or identity number that is his own fact stays in the article. If the same
  page also has a durable opinion, keep the opinion and drop the procedure.
- A two-letter fragment, such as "sd", is not a city. Leave it unexpanded.
- A short reply answers the message in front of it. Do not treat it as
  retracting an earlier sentence on the same page unless its words do that.
- When his own message names the company and says a named person will push
  that process, file the link as his statement. A later denial stays beside
  it. The display name still does not fill a company he left unnamed.
- A similar romanization is not the same person. Keep both until he says
  they are one.
- A yes-or-no about whether he has already left, already joined, or already
  reached an offer stage answers that question on that date. File it beside
  the other dates. It does not close the episode.
- The reading page inside one chat is newest first. The article is oldest
  first inside the episode. Insert the new dates by `at`. Do not paste the
  page onto the end of the paragraph.
- A question is not an assertion. "Is your partner so-and-so?" stays a
  question until his own words answer it.
- Recommending someone else into a company, including saying that company's
  engineering is strong, is not the account holder applying or joining.
- "These two are both fine" does not name the company he left out. Do not
  invent the second one. A bare nickname with no role on the page stays
  unnamed.
- A nameplate or title he offers for one event is that day's wording. File
  it beside later bios. It does not close where he works. A joke about a
  sanctions list in a title is not a legal finding.
- An arrival time, a gate, a course start, or a meal is logistics. After
  the page is read, mark no durable fact when that is all it holds. A
  teacher's name that this page does not identify is not a school.
- The same course told as a date span in one chat and as "four afternoons"
  in another is one course. Add the span. Do not open a second term.
- A sentence that only answers a question stays unfiled when that question
  is not on the page.
- A hometown he does not name stays unnamed. "Back in this city in ten
  days" is where he was that week, not a new residence.
- An identity number on the page is the number he wrote, not a birthday.
  Do not derive a date of birth from it. Words around it, such as the unit
  on a form, can still be filed.
- `[image]`, `[quote]`, `[file]`, and `[unknown]` with no words beside
  them are not a document. Do not describe the picture, and do not open
  the file into the article. A `[quote]` line is not his statement unless
  the words beside it are his. Do not expand the placeholder.
- A scratch digest that cuts a line short is not the message. If the
  sentence you would file ends in an ellipsis, re-read that line in the
  coverage jsonl before writing it. The page header's `conversationId` is
  not the display name. The name is `from` on the message line.
- The same refusal, told to several people in one week and naming the same
  person, is one episode. A similar refusal that does not name that person
  stays a separate note. Do not merge them because the wording rhymes.
- "I will only consider this kind of company" is that day's filter. The
  other person's employer in the display name is not a company he named,
  and it is not his.
- "I had already left a startup" names that company only when he names it.
  Do not attach the exit to a later employer. Keep the dates side by side.
- Accepting an offer, including a start date printed on the letter, is that
  day's acceptance. A later pause or withdrawal stays beside it. Neither
  sentence means he joined, and neither means the other side rejected him.
  A reason he gives that day is his statement, not an outside finding. The
  mailbox he gave for that process stays when the message states it.
- A page that returns no messages was read and has no durable fact. It does
  not mean that person does not exist.
- Gossip about a third person's body or gender is not a fact about the
  account holder. Leave it out.
- An email address, a link, or a bare hostname on the page can be filed
  when it belongs to the fact. Do not pass `--redact` and then reconstruct
  what that flag removed.
