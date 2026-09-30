# Scope, reading order, and what to file

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

## What becomes an article sentence

Keep a fact when it is still useful after the conversation is closed: who
someone is to the account holder, a durable role or project, a decision, or
a correction of an earlier fact. Record other people's claims as their
claims. Keep a phone number, email address, identity number, street
address, or link when the message states it; a street address or identity
number that is his own fact stays in the article. An email address, a link,
or a bare hostname can be filed when it belongs to the fact. The exceptions
are under [Secrets, personal details, and
procedures](#secrets-personal-details-and-procedures). Do not copy affection
or routine logistics that is not a durable fact.

A few readings keep getting mis-filed.

### Names, people, and employers

- A short run of digits in front of a display name, such as a class year,
  is not a phone number. A real mobile in the name stays on the page. Do
  not invent an identity from the digits that remain.
- A display name that includes an employer, a desk, or "HR" is that
  contact's label. It does not confirm where the account holder is going,
  and it does not fill in a company he left unnamed. File the company only
  when his own message names it. When that person's own message names a
  different employer from the label, file the sentence they typed. The
  label does not replace it.
- When a person's own greeting states a personal name that differs from
  `from`, file the name they typed and keep `from` as the display. A WeChat
  system line that names someone else is not a second contact.
- A similar romanization is not the same person. Keep both until he says
  they are one.
- Terms of affection, a red envelope, or a shared travel note do not
  identify that contact with a spouse or with another person already in
  the articles. Mark the page as no durable fact when that is all it holds.
- "I saw the news that you joined X" is the other person's framing. File it
  as their sentence. It does not confirm a move, and it does not get its
  own episode beside the account holder's own statements that day.
- When his own message names the company and says a named person will push
  that process, file the link as his statement. A later denial stays beside
  it.
- A one-word agreement with the other person's assumption about his
  employer stays in the person note. Do not open a work sentence that names
  their company as his.
- "I will only consider this kind of company" is that day's filter. The
  other person's employer in the display name is not a company he named,
  and it is not his.
- Recommending someone else into a company, including saying that company's
  engineering is strong, is not the account holder applying or joining.
- "These two are both fine" does not name the company he left out. Do not
  invent the second one. A bare nickname with no role on the page stays
  unnamed.
- "I had already left a startup" names that company only when he names it.
  Do not attach the exit to a later employer. Keep the dates side by side.
  When he says the old company was not renamed into the new one, file both
  names.
- Accepting an offer, including a start date printed on the letter, is that
  day's acceptance. A later pause or withdrawal stays beside it. Neither
  sentence means he joined, and neither means the other side rejected him.
  A reason he gives that day is his statement, not an outside finding. The
  mailbox he gave for that process stays when the message states it.
- Gossip about a third person's body or gender is not a fact about the
  account holder. Leave it out.

### Who said what

- A question is not an assertion. "Is your partner so-and-so?" stays a
  question until his own words answer it.
- A short reply answers the message in front of it. Do not treat it as
  retracting an earlier sentence on the same page unless its words do that.
- A sentence that only answers a question stays unfiled when that question
  is not on the page.
- A `[quote]` line is not his statement unless the words beside it are his.
  Do not expand the placeholder. When the line before "他" is `[quote]`,
  that person is not on the page. Do not attach the visa, job, or status in
  his reply to the account holder. Leave the sentence unfiled.
- A line that is only a title is the title he sent that minute. File it
  beside the other titles. When the turn before it is `[unknown]` or a call
  with no text, do not invent the question. A draft that arrives later,
  with no reply from him on the page, was not approved on this page.

### Placeholders and scratch notes

The labels themselves are defined in [cli.md](cli.md#placeholders).

- `[image]`, `[quote]`, `[file]`, `[attachment]`, and `[unknown]` with no
  words beside them are not a document. Do not describe the picture, and do
  not open the file into the article.
- "这两个", or "pick either", after two `[image]` lines means the pictures.
  Do not treat the phrase as two documents.
- A picture of a book, when the words only say he translated it, is that
  confirmation. Do not guess the title from a picture the reading page
  did not transcribe.
- A scratch note or digest is not the page. If the sentence you would file
  ends in an ellipsis, or a digest dropped the `[image]` lines around it,
  re-read that line in the coverage jsonl before writing it.
- A self line of one or two words is still the answer. Do not drop it
  because a digest keeps only longer lines. A city name, a "no", or a
  tool name sent as its own message is the fact. Read it against the
  line before it.

### Corrections and wording

- The next message can correct the one before it. File the corrected
  wording. A mistype he immediately replaces is not a second claim, and it
  does not need its own sentence.
- The same chat can name an organization correctly a few messages later.
  File the name he settled on. The earlier near-miss is not a second
  organization, and it does not need a sentence of its own.
- A word he typed and did not correct stays as he typed it. Do not silently
  replace a near-miss technical term, and do not add a sentence that only
  flags the slip.
- A nameplate or title he offers for one event is that day's wording. File
  it beside later bios. It does not close where he works. A joke about a
  sanctions list in a title is not a legal finding.
- A joke that his own homepage or a title is casually written is that
  day's tone. It does not retract the page or the title.

### Episodes, dates, and places

- The same decision told to two people on the same day is one episode.
  Add the second telling in `at` order. Do not open a second employer.
- The same refusal, told to several people in one week and naming the same
  person, is one episode. A similar refusal that does not name that person
  stays a separate note. Do not merge them because the wording rhymes.
- A yes-or-no about whether he has already left, already joined, or already
  reached an offer stage answers that question on that date. File it beside
  the other dates. It does not close the episode.
- The same course told as a date span in one chat and as "four afternoons"
  in another is one course. Add the span. Do not open a second term.
- An arrival time, a gate, a course start, or a meal is logistics. After
  the page is read, mark no durable fact when that is all it holds. A
  teacher's name that this page does not identify is not a school.
- A hometown he does not name stays unnamed. "Back in this city in ten
  days" is where he was that week, not a new residence.
- A two-letter fragment, such as "sd", is not a city. Leave it unexpanded.

### Numbers and money

A figure that stays out stays out silently. Do not write a sentence whose
only job is to say it was left out.

- Two counts he gives in the same minute both stay, including a headcount
  and a breakdown that do not add up. Do not correct the arithmetic.
- A valuation, a revenue, or a cloud-credit amount stays out, whether or
  not the sentence also has a round count. Keep the round count, the
  companies, the comparison, and the date.
- A performance table the other person pastes stays out. Keep his
  conclusion and the date.
- A figure inside a rumor the other person calls false stays out. Keep
  the denial, who said it, and the date.

### Secrets, personal details, and procedures

- An identity number on the page is the number he wrote, not a birthday.
  Do not derive a date of birth from it, and do not invent a birthday the
  message did not state. Words around it, such as the unit on a form, can
  still be filed.
- A line whose text is a credential, such as an API key, a token, or a
  password, is not a sentence to copy. If he sent a temporary key so
  someone could run a demo, file the occasion and leave the secret out.
- A class key he says was leaked is the occasion. File that he replaced it,
  and which model he told them to use. Leave the new key out.
- A stipend or payroll form is one line that mixes a credential with a job
  title. File the employer and the title he wrote. Leave out the identity
  number, the bank card, the branch, and the phone.
- A reservation card and a dump of internal rows are the occasion. File
  the place name he used, where he went, and what he decided. Leave the
  street, the phone, the room number, and the rows out of the article.
- A tracking number, a resume file, and a poster image are the occasion.
  File where the parcel was, that he sent the resume, and the bio he typed
  that minute. Leave the number, the file body, and the picture out.
- Do not record a how-to for keeping an account or network in good
  standing: proxy hops, identity-check steps, or similar. If the same page
  also has a durable opinion, keep the opinion and drop the procedure.
- An ssh command, a proxy export, or a VPN setup he types for someone else
  is the occasion. File that a machine was opened, or that the network was
  the problem. Leave the address, the key path, and the proxy lines out.
- A config dump, an overlay line, or a seller's answer about how two parts
  fit is the occasion. File that the device was being set up and what he
  concluded. Leave the dump and the wiring out.
- A checkout workaround he types, and a filing deadline the other person
  states, are the occasion. File that the page would not log in, or that
  he signed and sent the file back. Leave the steps and the other person's
  filing note out.

### Pasted or forwarded material

- A self-sent talk script, a pasted blog section, or a meeting-invite card
  is the occasion and the claim. Do not paste the script into the article,
  and do not copy the meeting id.
- The same pasted greeting, including a New Year code poem, sent again in
  another chat is still that script. File the occasion once. Do not paste
  the script.
- A coding prompt he pastes for an interview is the occasion. File that
  he set the exercise and what he said about the result. Do not paste
  the prompt.
- A summary he forwards from a pendant or a model, such as Limitless or
  GPT-5, is not a sentence he typed. File the occasion. File a claim from
  inside it only when he adopts that claim, or when he says the summary is
  right overall, and say the summary is the one he forwarded. Keep the
  errors he named. A figure, including a financing figure, that appears
  only inside that summary stays out.
