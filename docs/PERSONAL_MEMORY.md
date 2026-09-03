# Personal memory: your WeChat history as a living knowledge project

GreenBubbles turns a large live WeChat history into a structured, incrementally
maintained knowledge project following the UserAsCode methodology. Rather than
producing a wiki or a flat list of facts, the pipeline maintains a
**git-versioned software project** — typed Python dataclasses with executable
constraints, or structured Markdown with ontology headings — that agents
CRUD-patch from message evidence. The project evolves with each new extraction
pass; no full re-extraction is required when new messages arrive.

The two-phase pipeline is:

1. **Fact extraction (per message unit):** The agent reads message units from
   the GreenBubbles corpus and extracts every fact as a plain string — anything
   about people, events, preferences, dates, relationships, possessions, health,
   or plans.
2. **CRUD-patch (per domain):** Each fact is classified into a life domain
   (identity, travel, finance, health, vehicles, family, social, work,
   entertainment, or a new domain the agent creates). The agent reads existing
   domain state, diffs incoming facts against it, and patches in place: new
   facts are added, changed facts are updated, unchanged facts are skipped. The
   manifest is regenerated and the project is git-committed.

The project skill is
[`skills/greenbubbles-personal-memory`](../skills/greenbubbles-personal-memory/SKILL.md).

## Quick start

```sh
# 1. Prepare the corpus (one-time; see Corpus preparation below)
greenbubbles memory prepare /private/path/corpus \
  --selection-policy selection-policy.json --profile live-account

# 2. Run an incremental extraction pass
python3 scripts/personal-memory-parallel.py tick \
  --corpus /private/path/corpus \
  --user-project ~/memory/me \
  --format python
```

The first `tick` creates `~/memory/me/` as a git repo, processes all messages
in the corpus, and writes a Python or Markdown knowledge project. Subsequent
ticks process only new messages since the last run.

## Corpus preparation

### Initial preparation

```sh
cp skills/greenbubbles-personal-memory/references/selection-policy.json \
  /private/path/selection-policy.json
chmod 600 /private/path/selection-policy.json

greenbubbles memory prepare /private/path/corpus \
  --selection-policy /private/path/selection-policy.json \
  --profile live-account
```

With an explicit live source, redirect its owner-only key file to
`--passphrase-stdin`. The corpus is private and read-only once published. A
failure leaves no partial corpus — rerun with the same inputs after removing
the incomplete output path.

Progress on standard error contains only phase names and aggregate counts.
The corpus is a point-in-time generation: messages arriving after preparation
are not included until the next prepare or extend run.

### Incremental preparation (`--extend`)

When new messages have arrived and you want to update the corpus without a full
re-scan, use `memory prepare --extend`. It re-scans metadata in full to pick
up new messages, but hydrates only new rows and inherits the alias numbering
(`C######`, `P######`) from the base corpus:

```sh
greenbubbles memory prepare /private/path/new-corpus \
  --extend /private/path/corpus \
  --selection-policy /private/path/selection-policy.json \
  --profile live-account
```

The extended corpus carries a `extends` manifest field linking it to the base,
and new unit files are appended after the base units. Interrupting an extend
run is safe — re-run and it starts fresh from the new-corpus path.

Cost note: preparation itself is local and free; it is the corpus that agents
later read at a metered cost. Extending costs proportionally less than a full
re-prepare because fewer messages are hydrated.

## Extraction formats

Choose one format when you first run `tick --format`. The format is recorded in
the user project and is immutable for the life of that project (changing format
requires a new project directory).

| Capability | Python (`--format python`) | Markdown (`--format markdown`) |
|---|---|---|
| Executable constraints | Yes — `constraints/*.py` with `check()` functions | No — alerts are manual notes in manifest.md |
| `git diff` self-check | Yes — typed instances, minimal diffs | Yes — structured sections, minimal diffs |
| pytest integration | Yes — `tests/test_*.py` | No |
| Python dependency | Yes — Python 3.10+ to run runner.py and tests | No |
| Human-editable | Readable but structured | Directly editable in any text editor |
| Ad-hoc generation | Best — LLM writes executable checks naturally | Good — LLM writes structured Markdown naturally |
| Recommended for | Power users, proactive alerts, CI integration | Simpler setup, Python-free environments |

The UserAsCode paper (format ablation, Section 4.5) shows that Python and
Markdown are close in read-path performance but Python wins on ad-hoc
constraint generation (100% vs 92.5% alert rate when the LLM must generate
checks without pre-computed alerts).

## Domain ontology

Facts are classified into standard life domains. The agent creates new domains
as needed — the taxonomy is a starting point, not a closed set.

| Domain | What it covers |
|---|---|
| `identity` | Full name, nicknames, date of birth, nationality, passport and ID numbers, email, phone, home address |
| `travel` | Past and upcoming trips, flights, hotels, passports, visas, travel preferences, loyalty programs |
| `finance` | Bank accounts, credit cards, investments, expenses, transfers, debts, insurance, financial goals |
| `health` | Medical conditions, allergies, current medications, prescriptions, fitness habits, appointments |
| `vehicles` | Cars, bikes, registration, insurance, service history, upcoming maintenance |
| `family` | Household members, relatives, relationships, ages, schools, health concerns, life events |
| `social` | Close friends, social activities, clubs, recurring plans, communication preferences |
| `work` | Employer, role, projects, schedule, colleagues, career events, professional goals |
| `entertainment` | Media preferences, subscriptions, hobbies, memberships |

Create a new domain (e.g., `property`, `legal`, `education`) whenever facts
accumulate in a life area that none of the above captures.

## Deduplication and correct-in-place

The agent reads before writing. For every incoming fact, it checks whether the
fact already appears in the domain state:

- **Unchanged**: skip entirely — no write, no duplicate.
- **Changed**: update the value in place (Python: edit the assignment; Markdown:
  edit the `## State` line). The source comment is updated to the current
  session. A History entry is appended (Markdown format).
- **New**: append the fact with a source comment (`# source: session_N,
  YYYY-MM-DD` in Python; `*(source: session_N, YYYY-MM-DD)*` in Markdown).

The `## History` section in Markdown format is **append-only**. History lines
are never edited or deleted — they are the immutable audit trail of what changed
and when.

After patching, the agent runs `git diff HEAD` in the user project to self-check
that only expected changes appear. If unexpected rewrites or duplications are
visible in the diff, the agent corrects them before committing.

## Constraint lifecycle (Python format only)

Constraints are executable cross-domain checks that the agent generates
autonomously and promotes to persistent background monitors.

**Generate:** When the agent notices a cross-domain implication during
extraction — a passport expiry date alongside an upcoming international trip, an
allergy alongside a new medication, conflicting instructions from different
sources — it writes a Python check inline and executes it immediately.

**Verify:** The Python interpreter runs the check deterministically. Date
arithmetic, threshold comparisons, and set membership are computed exactly —
no LLM estimation involved.

**Review:** The agent reviews the result. If the check is generally useful (the
condition is time-dependent or the underlying state could change), the agent
promotes it to `constraints/<name>.py` as a `def check(project) -> list[Alert]`
function.

**Promote:** `runner.py` at the project root discovers all `constraints/*.py`
modules automatically, runs each `check()` function, and aggregates their
alerts. The output populates `manifest.py:ACTIVE_ALERTS`.

**Surface:** `ACTIVE_ALERTS` is always loaded into the agent's context at the
start of every session. An alert that appears there is visible before the user
asks any question, enabling proactive warnings.

**Prune:** During a revise pass, outdated constraints (for events that have
passed or conditions no longer relevant) are removed or updated.

To refresh alerts without running a full extraction pass:

```sh
python3 scripts/personal-memory-parallel.py manifest-refresh \
  --user-project ~/memory/me
```

## Git version control

The user project is a git repository from its first `tick` run. Every
successful extraction batch produces a commit:

```
memory update: 2026-01-20T14:30:00Z

- session: shard-000 scope-0
- messages committed: 12,453
- corpus: corpus-v2
```

The driver checks for public-looking remotes before and after each commit and
prints a prominent warning if any are found. The project contains personal
information — only push to a private remote, and only when you have made an
explicit decision to do so.

`git diff HEAD` is in the agent prompt as the deduplication self-check. After
patching state files, the agent reviews the diff to verify that only expected
changes appear.

A periodic revision pass can be committed separately:

```sh
python3 scripts/personal-memory-parallel.py revise \
  --user-project ~/memory/me --format python --agent claude
```

## Cadence and cost

The driver provides the primitives; cadence is entirely the user's choice and
depends on their token budget and how fresh they need the project to be.

| Batch frequency | Approximate cost (Gemini 3.8 Flash) | Typical use case |
|---|---|---|
| Hourly (24x/day) | ~$1–3/day for active users | Power users who want near-real-time |
| Daily | ~$0.05–0.20/day | Most users — good balance |
| Weekly | ~$0.35–1.40/week | Casual use, large corpora |

These are estimates based on measured corpus rates (~USD 1.15 per 1,000
messages). Incremental `tick` passes cost proportionally less because only new
messages are processed, not the full corpus.

Wire the driver into cron or launchd at whatever interval fits your budget:

```sh
# Example launchd plist (~/Library/LaunchAgents/me.greenbubbles.tick.plist):
# ProgramArguments:
#   python3
#   /path/to/scripts/personal-memory-parallel.py
#   tick
#   --corpus /private/path/corpus
#   --user-project ~/memory/me
#   --format python
#   --agent claude
# StartCalendarInterval: { Hour: 2; Minute: 0 }
```

## Driver commands

### `tick`

One incremental extraction pass. Processes message units that arrived since the
last tick into the user project.

```sh
python3 scripts/personal-memory-parallel.py tick \
  --corpus /private/path/corpus \
  --user-project ~/memory/me \
  --format python \
  --agent claude
```

On the first run, creates the user project directory, initializes a git repo,
and writes `.gitignore`. Stores `lastTickTime` in
`<user_project>/.greenbubbles-tick-state.json`. On subsequent runs, processes
only messages since `lastTickTime`.

If no new activity is found, prints `tick: no new activity since <timestamp>`
and exits 0.

### `manifest-refresh`

Python format only. Re-runs all `constraints/*.py` check functions and updates
`manifest.py:ACTIVE_ALERTS` from their output. Commits the updated manifest.

```sh
python3 scripts/personal-memory-parallel.py manifest-refresh \
  --user-project ~/memory/me
```

Use this when you want to refresh alerts without processing new messages — for
example, after the current date has advanced past a constraint threshold.

### `revise`

Holistic revision pass. Launches one agent batch over the full user project
asking it to: evolve schemas, split or merge domains, archive stale state,
prune outdated constraints, and audit cross-domain references.

```sh
python3 scripts/personal-memory-parallel.py revise \
  --user-project ~/memory/me \
  --format python \
  --agent claude
```

Commits with a message summarizing the changes. Run periodically — monthly or
quarterly — rather than after every tick.

## Run-state and continuation

The `tick` command stores its internal run state in
`<user_project>/.greenbubbles-runs/tick-<timestamp>/`. Each shard tracks its
scope progress in `shards/NNN/progress.json`, so an interrupted tick can be
re-run and will continue from where it left off.

The format (`--format`) is immutable once a user project is created. To change
format, create a new user project directory and run `tick` with the new format.

For the underlying GreenBubbles corpus protocol (scopes, page protocol,
coverage reporting), see the detailed workflow earlier in this document and in
the [CLI reference](CLI_REFERENCE.md).

## Canonical corpus and scoped views

The v2 two-pass algorithm:

1. Inventory identifiers from the authorized session, contact, and chat-room
   tables and map them to hashed message tables.
2. Retain an unresolved conversation for each hashed table whose identifier
   cannot be reversed. This preserves row coverage without guessing identity;
   `unmatchedMessageTable` remains an explicit limitation.
3. Scan metadata — source row identity, order, sender, type, and time — in
   10,000-row SQL pages while numbered SQLCipher shards remain read-only.
4. Hydrate and decode every eligible row. Reject any row whose complete
   metadata identity changed between passes.
5. Split evidence into immutable, internally chronological units and assign
   stable `C######`, `P######`, and `E#########` aliases once.
6. Schedule those units deterministically before atomic publication.

`deliveryOrder: accountHolderRelevance` uses only structural signals — self
message volume, active-month breadth, recency, and conversation kind — to cover
a broad personal frontier early. It still schedules every canonical unit once.
`chronological` is an explicit alternative.

## Running shards in parallel

For large corpora, the parallel driver can shard the work across multiple agents
running concurrently:

```sh
python3 scripts/personal-memory-parallel.py tick \
  --corpus /private/path/corpus \
  --user-project ~/memory/me \
  --format python \
  --shards 4 --parallel 4 \
  --agent claude
```

Each shard writes to the same user project directory. Domain-level writes are
generally disjoint across shards. The driver's git commit after each shard
serializes any overlapping writes.

## Coverage means what the agent actually saw

`complete: true` describes the current state scope. It proves review of every
hydrated corpus message only when the corpus is canonical and
`scope.allMessages: true`.

`rowCoverageComplete: true` means every inventoried hashed message table
contributed readable metadata, including unresolved tables.
`sourceCoverageComplete: false` can still report lost conversation identity.
`contentComplete: false` means at least one eligible body could not be hydrated
or decoded.

None of these states permits inference about missing content. A completed
unfiltered scope proves that the agent processed every hydrated corpus message;
it does not prove that the knowledge project captured every nuance.
