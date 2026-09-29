# Article format

The knowledge base is a private wiki. `index.md` is the front page. Each
file in `domains/` is one Wikipedia-style article. Write prose. A page that
is only a bullet list of facts is not finished.

## Layout

```
user_project/
├── .gitignore
├── index.md
├── manifest.md
└── domains/
    ├── identity.md
    ├── education.md
    ├── work.md
    ├── family.md
    ├── social.md
    ├── health.md
    ├── finance.md
    ├── travel.md
    ├── home.md
    ├── vehicles.md
    ├── entertainment.md
    ├── legal.md
    └── <domain>.md
```

Create a domain file when the reviewed messages support an article. Do not
create an empty page for a life area that has not come up.

## Language

Write the whole project in the language the account holder usually uses in
their own messages. Read several substantive self-sent messages before
choosing. Chat with family, editors, and colleagues counts. A few English
product names do not make the project English.

Apply that language to `index.md`, `manifest.md`, article titles, headings,
prose, see-also lines, references, revision notes, and alerts. Keep personal
names, book titles, and product names as they are written. Translate the
surrounding sentences.

If an existing project is in another language, rewrite it into the account
holder's language on the next pass. Do not leave an English scaffold beside
Chinese evidence. Ask which language to use only when their own messages do
not settle it.

The sample later in this file shows structure. Do not copy its English into
the user's project.

## `.gitignore`

```
__pycache__/
*.pyc
*.pyo
.DS_Store
.greenbubbles-runs/
.greenbubbles-tick.lock
.greenbubbles-revise.log
```

Add `coverage/` when the project keeps a private list of chat ids. Do not commit that list.

## `index.md`

The front page stands alone. Open with the person's name as the title and a
lead of one or two paragraphs: who they are, in the period the knowledge
base covers. Then link every article, and add sections a reader would scan:
people, organizations, works, places. Link to the heading inside the
article, for example `[图解大模型](domains/identity.md#books)`.

Update the index when an article, person, organization, or work is added.
Do not paste article bodies into the index.

## `manifest.md`

The manifest is the agent's ledger, not the public front page.

```markdown
# Personal Memory Manifest

**Last updated:** 2026-01-20T14:30:00+08:00

## Scope

- Project path, continued-from path, and the requested window.

## Domains

| Domain | Summary | Updated |
|---|---|---|
| [identity](domains/identity.md) | One-line scope of that article | 2026-01-20 |

## Active Alerts

- [WARNING] coverage: which qualifying chats are still unread

## Coverage

Window, source identity, chats ranked, chats read, searches run, and the
unread remainder. Name the commands used. Do not claim a complete window
while chats remain unread.
```

After revising articles, update the domain row, the alerts, and the
timestamp. Severity is `CRITICAL`, `WARNING`, or `INFO`. Remove an alert
when the underlying issue is gone.

## Canonical domain names

Use one writer per project. Prefer these names:

| Name | Covers |
|---|---|
| `identity` | Name, public role, biography that is not its own article |
| `work` | Employment, companies, products, colleagues, offers |
| `family` | Relatives and the closest household relationships. Not purchases or logistics. |
| `social` | Friends, collaborators, editors, communities |
| `health` | Medical events, medications, fitness |
| `finance` | Income, payments, investments, taxes |
| `travel` | Trips, flights, visas |
| `home` | Housing and household |
| `vehicles` | Cars and registration |
| `education` | Degrees, schools, teaching, research training |
| `entertainment` | Media, hobbies, memberships |
| `legal` | Companies' legal affairs, contracts, compliance, disputes |

Add a domain only when the facts belong to a life area this list does not
cover. Never use a synonym (`career` stays `work`).

Books and a public role usually belong in `identity`, with the publishing
work linked from `work`. A person belongs in `social` or `family`, and is
named again inside the article their messages inform.

## Article shape

```markdown
# Education

Lead paragraph. The first sentence states what this article is about. The
rest of the lead summarizes the article without depending on other pages.

## School

Prose. Dates and names appear in sentences. A later correction replaces the
sentence and leaves the old dated claim visible when the two disagree.

## Teaching

More prose.

## See also

- [Identity](identity.md)
- [Work](work.md)

## References

Reviewed directly from the live database. Native search freshness is unverified.

- 刘美英, direct chat, 2026-09-14. `messages list`, newest pages.
- Search "图解大模型", hits kept from 2026-04 through 2026-05.

## Revision notes

- 2026-09-29: Article rewritten from the two-year reading pass.
```

Rules:

- Title the page with the life area. Use stable `##` headings so the index
  can link to them.
- The lead comes before any section. A reader who stops there should know
  the shape of the subject.
- Prefer paragraphs. Use a list only for a real enumeration, such as a
  series of books or the references.
- One topic has one section. Merge a new fact into that section. Do not
  append a second paragraph that repeats the first.
- Say who is speaking. "He told Tracy" is his statement. "Tracy said" is hers.
- When one episode is told in several chats, including a group and a direct
  chat, write it once, in order of `at`. Name each chat. Do not retell it
  under every person.
- Message time joins the chats. When someone later describes an earlier
  event, the sentence keeps the event's date, and the reference keeps the
  date they said it.
- When evidence conflicts, keep both dates in the prose. Do not silently
  replace the older claim.
- References name the chat display name, the message date, and whether the
  line came from `messages list` or `messages search`. Do not cite a corpus
  alias. A phone number, email address, identity number, street address,
  or link stays in the article when the message states it. Do not put a
  real one into a public repository or a skill example.
- Revision notes are append-only and short. They record that the article
  changed. The prose above them is the current account. Git holds the
  older wording.
- Do not copy sample facts from this file into the user's project.

## Revise pass

Read `index.md`, the manifest, and every article. Fold duplicated facts
into the section that owns them. Split an article that covers two life
areas. Delete an index link that points nowhere. Leave a dated
contradiction in the prose. Append one revision note per article you
change, then git-commit the project.
