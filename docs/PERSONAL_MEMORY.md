# Personal memory: notes about your life from your chats

GreenBubbles can help your coding agent turn your WeChat history into a small,
private, Wikipedia-style set of notes about your life. To set that up, start
with the [agent skills guide](AGENT_SKILLS.md). It uses your agent's existing
setup and needs no extra API key.

This page explains what the notes look like and which commands the agent uses.
The second half covers an older, advanced batch workflow that most people don't
need.

## What the notes look like

The notes are a private folder tracked with Git:

- `index.md` is the front page, with links to the articles.
- `domains/*.md` are the articles, one per life area (work, family, travel, and
  so on), written as prose with a list of sources.
- `manifest.md` records the time range covered and which chats were actually
  read.

The agent writes everything in the language you usually use in WeChat. If you
mostly write Chinese, the notes are in Chinese. Keep the folder private: it
contains personal details about you and the people you talk to.

## How the agent builds them

1. It asks what time range to cover and whether notes already exist.
2. It ranks your chats with `chats rank` to decide which ones matter.
3. It reads them with `messages list` and `messages search`.
4. It writes or revises the articles, adding a source line for each fact.

A later update opens the same folder and revises the same articles. It doesn't
start a second set of notes. The full instructions the agent follows are in the
[personal-memory skill](../skills/greenbubbles-personal-memory/SKILL.md).

## Commands the agent uses

You can run these yourself too:

```sh
# Check which WeChat data GreenBubbles will read.
greenbubbles source status

# List chats where you sent at least 10 messages.
greenbubbles chats rank --minimum-self-messages 10 --limit 2000

# Read one chat between two times (Unix timestamps).
greenbubbles messages list --conversation ID --since <unix> --until <unix> --limit 80

# Search. The search text is read from standard input.
printf '%s\n' 'query' | greenbubbles messages search --query-stdin --limit 25
```

How to read the output:

- Each message line has `from` (the sender), `self` (whether it was you), and
  `at` (local time).
- A photo or file line may include `file`, its path on your Mac. The agent
  opens it there but doesn't copy the path into the notes.
- A voice message without a transcript shows as `[voice]`.
- Phone numbers, email addresses, ID numbers, and links are shown. Don't add
  `--redact` when building your private notes; it would hide details the notes
  need.
- `chats rank` shows each chat's `from`, `selfCount` (messages you sent), and
  `last` (latest message time).
- For more results, pass `--cursor` with the `nextCursor` value from the
  output, or use a larger `--limit`.
- `--json` prints every field. To make JSON the default, set `[output] format`
  in `~/.greenbubbles/config.toml`.

## Batch script and evidence archive (advanced)

Everything below describes an older workflow. **The agent-skill workflow above
does not use it**, for first builds or for updates. The commands are still in
GreenBubbles for people who want scripted, scheduled runs.

The older workflow has two parts:

- **An evidence archive** (called a *corpus* in the commands): a read-only,
  private copy of the chosen messages, made by `memory prepare`. It is a
  snapshot; messages that arrive later aren't in it until you extend it.
- **A batch script**, `scripts/personal-memory-parallel.py`, that launches a
  coding agent over the archive and writes a notes folder (called the *user
  project*).

### Create the evidence archive

1. Copy the example selection policy, which decides which chats are included,
   and make it private:

   ```sh
   cp skills/greenbubbles-personal-memory/references/selection-policy.json \
     /private/path/selection-policy.json
   chmod 600 /private/path/selection-policy.json
   ```

2. Build the archive:

   ```sh
   greenbubbles memory prepare /private/path/corpus \
     --selection-policy /private/path/selection-policy.json \
     --profile live-account
   ```

If you name the live source explicitly instead of using a profile, send its key
file on standard input with `--passphrase-stdin`.

- The archive is private and becomes read-only once finished.
- If preparation fails, it leaves no half-built archive. Delete the incomplete
  output folder and run the same command again.
- Progress messages show only step names and counts, never message text.

The archive holds a copy of possibly every included message, so protect it like
WeChat's own data.

### Add new messages to the archive (`--extend`)

To add new messages without rebuilding from scratch, create a new archive based
on the old one:

```sh
greenbubbles memory prepare /private/path/new-corpus \
  --extend /private/path/corpus \
  --selection-policy /private/path/selection-policy.json \
  --profile live-account
```

It checks every message's details again to find new ones, but only reads the
full text of new messages. It keeps the old archive's short stand-in names for
chats and people (`C######`, `P######`), records which archive it extends, and
adds the new messages after the old ones. If it's interrupted, run it again; it
starts fresh in the new folder.

Building an archive runs on your Mac and costs nothing. The cost comes later,
when an agent reads the archive. Extending costs less than rebuilding because
fewer messages are read in full.

### Choose a notes format

Pick a format the first time you run `tick --format`. You can't change it later
without starting a new notes folder. The default is `python`.

| | Python (`--format python`) | Markdown (`--format markdown`) |
|---|---|---|
| Automatic checks | Yes: `constraints/*.py` files with `check()` functions | No: alerts are written by hand in `manifest.md` |
| Small, reviewable Git diffs | Yes | Yes |
| Tests with pytest | Yes: `tests/test_*.py` | No |
| Needs Python | Yes, 3.10 or later | No |
| Edit by hand | Possible, but structured | Easy, in any text editor |
| Best for | Proactive alerts and automation | Simpler setup without Python |

In the UserAsCode paper's comparison of formats (Section 4.5), Python and
Markdown answered questions about equally well. Python did better when the AI
had to write its own checks without help (100% versus 92.5% of alerts raised).

### Life areas

Facts are sorted into these standard life areas. The agent can add a new one
when facts truly don't fit.

Use exactly these names. Several batches write to the same files, so a synonym
(`household` instead of `home`, `career` instead of `work`) would split facts
across two files.

| Area | What it covers |
|---|---|
| `identity` | Full name, nicknames, date of birth, nationality, passport and ID numbers, email, phone, home address |
| `work` | Employer, role, projects, schedule, colleagues, career events, job search, offers, professional goals |
| `family` | **People only**: spouse, parents, siblings, relatives — relationships, ages, schools, health concerns, milestones. Not household purchases, equipment, or home logistics. |
| `social` | Close friends, acquaintances, social activities, clubs, recurring plans, communication preferences |
| `health` | Medical conditions, allergies, current medications, prescriptions, fitness habits, appointments |
| `finance` | Bank accounts, income, expenses, investments, transfers, debts, insurance, financial goals, taxes |
| `travel` | Past and upcoming trips, flights, hotels, passports, visas, travel preferences, itineraries |
| `home` | Housing, household appliances and purchases, renovation, real estate, home logistics |
| `vehicles` | Cars, bikes, registration, insurance, service history, upcoming maintenance |
| `education` | Academic history, degrees, courses, research, academic service (for example, conference program committees) |
| `entertainment` | Media preferences, games, hobbies, subscriptions, memberships |
| `legal` | Contracts, agreements, disputes, compliance, intellectual property |

### How facts are added and corrected

The agent reads a file before writing to it. For each new fact:

- **Already there and unchanged:** nothing is written.
- **Changed:** the value is updated in place (Python: edit the assignment;
  Markdown: edit the `## State` line) and its source note points to the current
  session. In Markdown, a line is also added to `## History`.
- **New:** the fact is added with a source note (`# source: session_N,
  YYYY-MM-DD` in Python; `*(source: session_N, YYYY-MM-DD)*` in Markdown).

In Markdown, `## History` is only ever added to, never edited, so it is a
permanent record of what changed and when.

After editing, the agent runs `git diff HEAD` to check that only the expected
lines changed, and fixes any accidental rewrites or duplicates before
committing.

### Automatic checks (Python format only)

A *constraint* is a small Python check across life areas, such as "my passport
expires before my upcoming trip" or "a new medication conflicts with an
allergy."

1. **Write:** when the agent notices a link like that, it writes a check and
   runs it right away.
2. **Run:** Python computes the answer exactly (dates, thresholds, lists), with
   no guessing by the AI.
3. **Keep:** if the check will stay useful as things change, the agent saves it
   as `constraints/<name>.py`, with a `def check(project) -> list[Alert]`
   function.
4. **Collect:** `runner.py` runs every saved check and writes their alerts to
   `manifest.py:ACTIVE_ALERTS`.
5. **Show:** the agent reads `ACTIVE_ALERTS` at the start of every session, so
   you see warnings before you ask.
6. **Clean up:** during a revision pass, checks about past events are removed
   or updated.

To refresh alerts without processing new messages:

```sh
python3 scripts/personal-memory-parallel.py manifest-refresh \
  --user-project ~/memory/me
```

### Git history

The notes folder becomes a Git repository on the first `tick`. Every successful
batch makes a commit like this:

```
memory update: 2026-01-20T14:30:00Z

- session: shard-000 scope-0
- messages committed: 12,453
- corpus: corpus-v2
```

The script warns loudly if the repository has a remote that looks public,
before and after each commit. The notes contain personal information: push them
only to a private remote, and only if you have decided to.

A revision pass can be committed separately:

```sh
python3 scripts/personal-memory-parallel.py revise \
  --user-project ~/memory/me --format python --agent claude
```

### How often to run, and cost

How often you run it is up to you, depending on your budget and how current you
want the notes. Start with a small test run and see how much it uses; daily or
weekly is a reasonable start. Retries, model reasoning, prompt size, and message
volume all change the cost, and subscriptions and API billing work differently,
so there is no fixed daily price.

Extend the archive before each scheduled run; running again on the same archive
finds no new messages. If a run is unfinished, finish it on its original archive
before switching to a newer one.

To schedule it, use cron or launchd. launchd doesn't expand `~`, so use full
paths. This example shows only the `tick` step; schedule the archive extension
separately:

```sh
# Example launchd plist (~/Library/LaunchAgents/me.greenbubbles.tick.plist):
# ProgramArguments:
#   python3
#   /path/to/scripts/personal-memory-parallel.py
#   tick
#   --corpus /private/path/corpus
#   --user-project /Users/you/memory/me
#   --format python
#   --agent claude
# StartCalendarInterval: { Hour: 2; Minute: 0 }
```

### Script commands

#### `tick`

Processes messages that arrived since the last run and adds them to the notes.

```sh
python3 scripts/personal-memory-parallel.py tick \
  --corpus /private/path/corpus \
  --user-project ~/memory/me \
  --format python \
  --agent claude
```

- The first run creates the notes folder, starts a Git repository, and writes
  `.gitignore`.
- It saves the time of the last run as `lastTickTime` in
  `<user_project>/.greenbubbles-tick-state.json`, and later runs start from
  there.
- If there's nothing new, it prints `tick: no new activity since <timestamp>`
  and exits with code 0.
- `--shards N` splits the work into N smaller batches. See
  [Running shards](#running-shards).

#### `manifest-refresh`

Python format only. Runs every saved check again, updates
`manifest.py:ACTIVE_ALERTS`, and commits. Use it when time has passed and an
alert might now apply, without processing new messages.

```sh
python3 scripts/personal-memory-parallel.py manifest-refresh \
  --user-project ~/memory/me
```

#### `revise`

A big-picture cleanup. The agent reviews the whole notes folder: it reorganizes
structure, splits or merges life areas, archives out-of-date facts, removes old
checks, and checks links between areas.

```sh
python3 scripts/personal-memory-parallel.py revise \
  --user-project ~/memory/me \
  --format python \
  --agent claude
```

It commits with a summary of the changes. Run it every month or quarter, not
after every `tick`.

### If a run stops partway

`tick` keeps its progress in `<user_project>/.greenbubbles-runs/tick-<timestamp>/`,
with each shard's progress in `shards/NNN/progress.json`.

If a run doesn't finish, it exits with an error and leaves `lastTickTime`
unchanged, even if some messages were already committed. Run it again with the
same archive, format, and chat-selection options to continue where it stopped.
You may change model and retry settings, for example after a usage limit
resets. `lastTickTime` moves forward only when every batch in the run has
finished. Finish a pending run before extending to a new archive.

### Running shards

For large archives, `tick` can split the work into shards (smaller batches):

```sh
python3 scripts/personal-memory-parallel.py tick \
  --corpus /private/path/corpus \
  --user-project ~/memory/me \
  --format python \
  --shards 4 \
  --agent claude
```

All shards write to the same notes folder, so `tick` runs them **one at a
time**. `--parallel` above 1 is lowered to 1, and the script logs why. When
agents ran at the same time, they overwrote each other's edits and silently lost
facts. Git commits and the commit lock don't prevent this: they order the
commits, not the edits.

Shards still help: each batch is smaller, so an interrupted run loses less work
and each agent has less to keep in mind. They don't make the run faster.

The separate `run` command builds a different kind of output (a separate wiki
per shard, merged afterwards), so it does run agents at the same time.
`--parallel` defaults to 8 there.

### Testing the script

The script's logic has unit tests that need no archive, agent, or network:

```sh
python3 -m unittest discover -s scripts -p 'test_*.py' -v
```

CI runs the same command. A full run needs a real archive and agent, so it isn't
tested automatically. Before a release, test changes to `tick` by hand on a
small archive made with `--max-conversations`.

### How the archive is built

For those who want the details, `memory prepare` works in two passes:

1. List chats and contacts from WeChat's session, contact, and group tables, and
   match each to its message table (WeChat names these by hash).
2. Keep a chat even when its hashed table can't be matched to a name, so no
   messages are dropped. Those chats are reported as `unmatchedMessageTable`
   instead of being guessed.
3. Read each message's basic details (position, order, sender, type, time) in
   pages of 10,000 rows, read-only.
4. Read and decode the full text of every included message. If any message's
   details changed between the two passes, it is rejected.
5. Split messages into fixed batches, each in time order, and give chats,
   people, and messages short stand-in names (`C######`, `P######`,
   `E#########`) that never change.
6. Order the batches in a repeatable way, then publish the archive all at once.

With `deliveryOrder: accountHolderRelevance`, chats you're most active in come
first, judged only by how many messages you sent, over how many months, how
recently, and the kind of chat. Every batch is still delivered exactly once.
`chronological` delivers them in time order instead.

### What "complete" means

These flags describe what the agent actually saw:

- `complete: true` means the current selection was fully read. It means every
  message in the archive was read only if the archive uses the current (v2)
  format and the selection was `scope.allMessages: true`. Older v1 archives
  never prove that.
- `rowCoverageComplete: true` means every message table was read, including
  ones that couldn't be matched to a chat name.
- `sourceCoverageComplete: false` means some messages couldn't be tied to a
  chat.
- `contentComplete: false` means at least one message's text couldn't be read
  or decoded.

None of these lets you assume anything about content that wasn't read. Even a
fully read archive shows only that the agent saw every message, not that the
notes captured every detail.
