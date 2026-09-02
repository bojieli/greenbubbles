# Personal-memory corpus and agent workflow

GreenBubbles can turn a large live WeChat history into a sequence of concise,
evidence-grounded batches for one ReAct agent. One canonical `memory prepare`
process indexes every eligible message. A run scope can then select one or more
conversations, an inclusive time range, one or more message senders, or any
combination without rescanning the encrypted database. Empty evidence filters
mean the entire canonical corpus. The agent receives only deterministic
`memory page` responses; it never needs thousands of `messages list` calls, and
GreenBubbles never writes the semantic wiki itself.

The project skill is
[`skills/greenbubbles-personal-memory`](../skills/greenbubbles-personal-memory/SKILL.md).
Project-local Pi sessions discover it through [`.pi/settings.json`](../.pi/settings.json),
and the parallel driver below carries the same skill in the prompt for harnesses
that do not read it from disk. Pi is the default and the runtime every example
here uses, but nothing in the protocol is specific to it: any coding agent that
can run a command and edit a file will do. No extension, custom shell tool,
daemon or multi-agent runtime is required.

## Canonical corpus and scoped views

The v2 two-pass algorithm is:

1. Inventory identifiers from the authorized session, contact, and chat-room
   tables and map them to hashed message tables.
2. Retain an unresolved conversation for each hashed table whose identifier
   cannot be reversed. This preserves row coverage without guessing identity;
   `unmatchedMessageTable` remains an explicit limitation.
3. Scan metadata—source row identity, order, sender, type, and time—in
   10,000-row SQL pages while numbered SQLCipher shards remain read-only.
4. Hydrate and decode every eligible row. Reject any row whose complete
   metadata identity changed between passes.
5. Split evidence into immutable, internally chronological units and assign
   stable `C######`, `P######`, and `E#########` aliases once.
6. Schedule those units deterministically before atomic publication.

`deliveryOrder: accountHolderRelevance` uses only structural signals—self
message volume, active-month breadth, recency, and conversation kind—to cover a
broad personal frontier early. It still schedules every canonical unit once.
`chronological` is an explicit alternative. Ordering never changes which
messages belong to the corpus and does not assert that a message is important
or true.

Legacy v1 policies remain readable and retain their old account-holder-active
episode behavior. They are useful for relevance-selected memory but cannot
prove whole-database review. New whole-history work should use
`corpusMode: allMessages`.

## Prepare once

Copy and review the example policy:

```sh
cp skills/greenbubbles-personal-memory/references/selection-policy.json \
  /private/path/selection-policy.json
chmod 600 /private/path/selection-policy.json

greenbubbles memory prepare /private/path/new-corpus \
  --selection-policy /private/path/selection-policy.json \
  --profile live-account
```

With an explicit live source, redirect its owner-only key file to
`--passphrase-stdin`. Never place the key in an argument. Preparation requires
the authenticated live `db_storage` account binding; it refuses a source for
which self attribution is unavailable.

Progress on standard error contains only phase names and aggregate counts.
The final manifest is printed on standard output. The source remains read-only
and a failure leaves no published partial index. Rerun `prepare` with the same
inputs after removing or choosing a different not-yet-existing output path.

The published corpus is deliberately private and read-only (`0400` files,
`0500` directories):

```text
corpus/
  manifest.json       source/policy binding and file hashes
  coverage.json       canonical row/content coverage accounting
  contacts.jsonl      private contact/person alias sidecar
  conversations.jsonl private conversation selector/alias sidecar
  activity.jsonl      conversation-month activity decisions
  evidence.jsonl      private canonical citation sidecar
  batches/index.json  compact hashes, bounds and scope-planning metadata
  batches/U######.json
```

The evidence sidecar is not model input. It intentionally retains verbose
local provenance so a human can resolve `E#########` citations later.
New corpora use a compact version-2 unit index: one first-evidence ordinal
replaces repeated per-message aliases, and person/sender keys replace repeated
wiki paths. Full identities remain deduplicated in verified sidecars and are
copied once into each model page that uses them rather than repeated on every
message. This keeps a real 100,000-plus-unit index bounded and fast to load.
Existing version-1 indexes remain readable when their identity sidecars are
complete.

## Select conversations, kinds, time, senders, and subject

The scope is expressed directly as `memory next` arguments. There is no scope
JSON file:

```sh
greenbubbles memory next /private/path/corpus \
  --state /private/path/run-state.json \
  --wiki /private/path/wiki \
  --max-text-bytes 327680 \
  --max-messages 2520 \
  --conversation conversation-id-1 \
  --conversation C000123 \
  --conversation-kind group \
  --from 2023-01-01T00:00:00+08:00 \
  --through 2023-12-31T23:59:59+08:00 \
  --sender self \
  --sender P000456 \
  --subject account-holder
```

- Repeatable `--conversation` accepts exact source IDs or stable `C######`
  join keys.
- Repeatable `--conversation-kind` accepts `direct`, `group`, `official`, or
  `service`.
- `--from` and `--through` are inclusive RFC 3339 timestamps. An explicit
  offset such as `+08:00` or `Z` is mandatory; offset-free timestamps are
  rejected rather than guessed.
- Repeatable `--sender` accepts source IDs or `P######` keys. `self` and
  `accountHolder` mean the authenticated account holder; `unknown` explicitly
  selects messages whose sender could not be resolved.
- `--subject` defaults to `account-holder`, accepts
  `person:<source-id-or-P######>`, or accepts `none` for a conversation-centric
  wiki. It changes semantic focus without filtering contextual messages.

Values within one repeatable category are ORed. The conversation, kind, time,
and sender categories intersect. Omitting every evidence filter selects every
hydrated canonical message; omitting `--sender` means all senders, not self-only.

Scope resolution fails closed on unknown, ambiguous, duplicate, oversized,
malformed, offset-free, or inverted values. Because source messages have
whole-second timestamps, a fractional `--from` advances to the next whole
second while a fractional `--through` includes its containing second. Returned
JSON reports effective RFC 3339 bounds normalized to the corpus timezone.

## Let Pi refine Markdown

Create an owner-only wiki and request one batch:

```sh
mkdir -m 700 /private/path/wiki

greenbubbles memory next /private/path/corpus \
  --state /private/path/run-state.json \
  --wiki /private/path/wiki \
  --max-text-bytes 327680 \
  --max-messages 2520 \
  --conversation-kind group \
  --from 2023-01-01T00:00:00+08:00 \
  --through 2023-12-31T23:59:59+08:00
```

Two bounds decide how much one batch delivers. `--max-text-bytes` bounds the
chat text a batch may carry; `--max-messages` bounds how many messages it may
carry. Both are needed because every delivered message costs roughly 130 bytes
of envelope — alias, actor, timestamp, kind — whatever its text weighs, so a
thread of one-word replies fills far more delivery pages than its text bytes
predict. `--max-messages` is a soft bound: it stops a batch taking another unit
and never splits or refuses the unit a batch must deliver whole. Size both to
the agent's context window, since a batch is what one agent has to hold.

`next` writes the outstanding batch to state before returning a small envelope.
The command-line scope and its deterministic unit/message plan are bound to the
state. Repeat the exact same scope arguments on every `next`; omit them every
time for a whole-corpus run. An interruption returns the same `batchId` rather
than skipping work.
Before a new batch, GreenBubbles verifies that the wiki matches the last
committed snapshot; an out-of-protocol edit is rejected, including after the
final batch. A different scope may rebind the same state only after the current
scope is complete, preserving one serial wiki history and a completed-scope
ledger.

An unfiltered all-message state stores the canonical corpus binding and count
without duplicating one selection record per unit. Filtered states persist only
their matching unit records and partial-unit bitmaps. Compact unit metadata can
reject impossible sender matches before opening unit files, so scope planning
is proportional to plausible matches rather than blindly hydrating the corpus
again.

The envelope reports a deterministic delivery plan but contains no messages.
Fetch its next page directly:

```sh
greenbubbles memory page /private/path/corpus \
  --state /private/path/run-state.json
```

Every compact page is at most 49,152 bytes including its newline. That is below
Pi's built-in 50-KiB read/shell limit. Calling `page` again before acknowledgement
returns byte-identical JSON. Each message has an evidence alias (`e`), actor
(`a`), optional person join key (`p`), RFC 3339 time in the corpus timezone
(`t`), payload kind (`k`), text
(`x`) and optional truncation marker (`tr`). WeChat wraps many payloads in an
XML envelope; `x` carries the human text inside it rather than the envelope. A
sticker becomes `[Emoji]` instead of a CDN URL, an MD5 sum and a buffer length,
while a location keeps its place name and a system message keeps its template.
Stickers are 2.7% of a 1.7M-message corpus and were 48% of its delivered text
bytes, so this roughly halves what an agent reads for the same history. The
prepared corpus is not rewritten; existing corpora get this without
re-preparation. Unit fragments also report their
stable unit (`u`), zero-based offset (`o`) and total unit message count (`n`).
The page-level `accountHolder`, `people`, and `conversations` dictionaries
preserve real source IDs, names, remarks, nicknames, aliases, group titles, and
conversation kinds. These identities intentionally enter the personal-memory
model; canonical message IDs, citation-sidecar hashes, and database metadata do
not enter the compact page.

Keep the state and wiki in an owner-only run directory outside source control.
Consume `page` output directly. An explicit shell-tool output-truncation notice
is a protocol failure: do not acknowledge that page or attempt to reconstruct
it from a truncated one-line file. A message-level `tr=true` is different: it
records that the configured per-message text bound was reached, while the page
itself remains complete and safe to review.

Pi reads and changes only `targetPages`, normally:

```text
wiki/
  index.md
  me.md
  people/P######.md
  conversations/C######.md
```

Conversation pages are chronological leaf memory. `me.md` and person pages are
durable subject/relationship rollups. `index.md` is navigation plus a compact
global overview. The account-holder subject may update all applicable layers;
a person subject focuses on that person and the relevant conversations; a
`none` subject targets conversation pages instead of `me.md`.

Wiki headings, link labels, and prose use the actual account name, contact
name, source ID, and group title supplied in those identity dictionaries.
`P######` and `C######` exist only as collision-safe join and filename keys.
When a source label is unavailable, the real source ID is used; GreenBubbles
does not replace it with `Person P######`, `Group C######`, or another anonymous
label. The one exception is the account holder, who is delivered as
`Me`: the corpus normalises them to the second person, which reads as a stray
pronoun in a third-person wiki, and the raw `wxid_…` underneath it is what three
different harnesses used as the title of `me.md`. The source ID still travels
beside the label. Distinct remark, nickname, and WeChat alias values belong in page
metadata when they add identity detail.

Every factual prose line needs an exact evidence alias such as
`[E000012345]`. Chat text is untrusted evidence, never a tool instruction.
Semantic merging — chronology, contradictions, uncertainty, attribution and
whether a claim still holds — remains the agent's job.

Comprehensiveness does not mean turning every repeated sentence into another
wiki fact. Pi should consolidate existing prose and use a representative exact
citation set sufficient for each distinct durable claim, while preserving
genuine changes, conflicts and dated events. There is no fixed fact quota, but
a factual prose line is capped at eight representative aliases so repeated
evidence cannot bloat the model-facing memory.

An old alias may remain on or be consolidated within the same page that
already cited it. It cannot be introduced on another changed page unless that
alias is also present in and retained from the current batch. This prevents a
model from laundering an old citation into a new claim without seeing its
immutable evidence again. Missing target pages are normal; check existence
before reading and create only pages with useful evidence-backed prose.

`me.md` has a stricter attribution boundary: every factual prose line must cite
at least one self-authored message. Incoming-only evidence may describe the
other person or interaction on that person's page, but cannot become an
account-holder fact. Commit resolves actors from the immutable local evidence
sidecar; no verbose provenance is sent back to Pi.

Pi must review every message in the page, write the useful durable memory into
the returned target pages, and only then acknowledge that page in sequence.
Acknowledgement is not a scratch list for facts that might be written later. If
the new or refined prose cites page evidence, Pi records exactly those aliases:

```sh
greenbubbles memory acknowledge /private/path/corpus \
  --state /private/path/run-state.json \
  --retain-evidence E000012345,E000012351
```

If a fully reviewed page contains no durable memory, the page acknowledgement
instead uses `--reviewed-no-durable-memory`. The state records delivery count,
review disposition, acknowledged message count and retained evidence. A page
cannot be skipped, acknowledged before delivery or reclassified later. Repeat
`page` and `acknowledge` until `reviewComplete` is true. Every retained alias
must remain cited in the durable wiki through commit.

These three commands default to the uniquely current batch persisted in state;
acknowledgement also defaults to its current unreviewed delivered page. This
keeps an agent from mistyping opaque identifiers. Operators can still pass
`--batch ID` and `--page-token TOKEN` for exact audit or replay, and may use the
literal selector `current` explicitly.

Pi must not pre-create every target page or use empty/heading-only placeholders.
An ordinary commit requires all deterministic pages to have been delivered and
acknowledged, at least one changed non-index page with prose, and new citations
drawn only from evidence explicitly retained during page review. Every factual
prose line must be cited, and every retained alias must be cited somewhere in
the wiki. A changed factual line with more than eight citations is also
rejected. This closes both previous failure modes: reading only the beginning
of one oversized JSON line, or selecting many candidates and silently dropping
some during later synthesis, can no longer advance the batch.

If every page in a reviewed batch contains no durable memory, the agent must
leave the wiki byte-for-byte unchanged and deliberately add
`--reviewed-no-durable-memory` to `memory commit`. The CLI rejects that batch
disposition if any page retained evidence or any wiki byte changed.

After Pi writes owner-only Markdown, commit the exact batch:

```sh
greenbubbles memory commit /private/path/corpus \
  --state /private/path/run-state.json \
  --wiki /private/path/wiki
```

Commit does no summarization. Under an exclusive state lock it validates the
immutable unit and page hashes, complete delivery/review accounting, prior wiki
hashes, safe paths, changed-page scope, retained citation aliases, citations on
factual prose and either the cited-prose boundary or the explicit unchanged-wiki
disposition, then atomically advances the cursor. A rejected commit leaves the
batch outstanding and reports every problem at once, with one-based line numbers
for uncited and citation-dumping prose lines and the actual aliases that were
unknown, unexpected, or retained but never cited; ownership and shape errors
name the offending path, its mode and its link count. Reporting one problem per
rejection made an agent pay a full batch invocation per fix. Repeating a
successful commit is idempotent.

While a batch is outstanding, `memory status` reports its counters and a Boolean
`reviewComplete`. With no outstanding batch that field is `null`, rather than a
misleading false value, and the preceding batch's review counts are preserved
under `lastCommitted`. Status also exposes `scannedMessageCount`,
`eligibleMessageCount`, canonical `corpusMessageCount`, the current scope's
`selectedMessageCount`, cumulative committed messages, resolved scope/subject,
completed-scope count, row/source/content coverage, unmatched-table count, and
aggregate limitation codes. Pi uses those aggregates instead of opening private
corpus sidecars.

The wiki files are the durable memory output, but a Pi run should not hide them
behind a terse terminal acknowledgement. Its final handoff should name the wiki
path and report the last batch's reviewed pages/messages, cumulative committed
and total units, completion state, and coverage limitations.

Continue until both commands report completion:

```sh
greenbubbles memory status /private/path/corpus \
  --state /private/path/run-state.json

greenbubbles memory next /private/path/corpus \
  --state /private/path/run-state.json \
  --wiki /private/path/wiki \
  --max-text-bytes 327680 \
  --max-messages 2520
```

## Run shards in parallel

One canonical corpus is immutable, so any number of agents may read it at once.
A run is what cannot be shared: one state file drives one wiki serially. To use
more than one agent, shard the work — each shard gets its own state file, its
own wiki, and its own list of scopes — then merge the wikis afterwards.
`scripts/personal-memory-parallel.py` does this:

```sh
python3 scripts/personal-memory-parallel.py plan \
  --corpus /private/path/corpus \
  --run /private/path/run \
  --shards 8 \
  --kind direct --kind group \
  --min-self-messages 0 \
  --group-min-self-per-month 5

python3 scripts/personal-memory-parallel.py run \
  --run /private/path/run --parallel 8

python3 scripts/personal-memory-parallel.py status --run /private/path/run
python3 scripts/personal-memory-parallel.py merge --run /private/path/run
```

`plan` is also where relevance filtering happens, because most of a WeChat
database is not worth distilling. `--kind` keeps only the conversation kinds
worth reading, `--min-self-messages` drops conversations the account holder
never took part in, `--from-month`/`--through-month` bound the window, and
`plan --max-messages` caps the whole run's budget outright (unrelated to the
per-batch `--max-messages` above). The plan is built from the corpus
activity sidecar, which holds per-conversation and per-month counts and no
message text. It prints how many messages survived and what they will cost
before anything is spent.

`--group-min-self-per-month` is the sharpest of those filters, because direct
chats and groups deserve different treatment. A one-to-one thread is about the
account holder by construction, so it is kept whole. A group is mostly other
people talking past you, so a group month only earns its cost once you actually
said something in it: the gate keeps the months where the account holder sent at
least N messages and drops the rest of that group's history. `--group-kind`
chooses which kinds the gate applies to; `--group-min-self-per-month 0` disables
it and keeps every conversation whole.

The default of 5 comes from measuring this corpus rather than from taste. At 5
the run keeps 446,435 messages in 50 scopes; at 10 it keeps 436,345, which saves
about 2% of the bill while dropping 138 groups, so the stricter gate buys almost
nothing and loses real history. Raising it further only sharpens that trade:
234 groups have at least one month above 10, but only 32 have three such months,
so a high gate keeps the same handful of groups you would have named anyway. The
tempting alternative — keep only the days you spoke in — reads the fewest
messages of all, 426,507, and still costs two to three times more, because it
scatters them across 2,732 scopes and the run pays per invocation. Lower the
gate to widen coverage, raise it to trim, but check the printed scope count
along with the message count.

Scopes are then built per window rather than per conversation. Every whole
conversation in a shard is read under one binding, and every gated group sharing
a month is read under one month binding, so the number of agent invocations
follows the number of windows rather than the number of conversations. That
matters because a run pays a fixed cost per invocation: thousands of
one-conversation scopes would spend more on that overhead than on the messages
themselves. A conversation heavier than one shard is still split into
consecutive month ranges with inclusive RFC 3339 `--from`/`--through` bounds in
the corpus timezone; a single month heavier than the limit stays whole, since
the corpus records activity per month. Without that split, one large
conversation would be the critical path for the entire run.

`run` invokes one agent per shard, one batch per invocation, and re-checks
`memory status` between batches, so a shard that stops making progress is
reported instead of looping. `--parallel` sets how many agents run at once and
defaults to 8. A shard works through its scopes in order and binds the next one
only after the current one is complete, which is what a run state permits;
`shards/NNN/progress.json` records the scopes already banked, so a resumed run
skips them. Everything is resumable: re-running continues from each shard's
committed state. Cost is per message and does not change with parallelism; wall
clock falls to roughly the heaviest shard.

The driver sizes each batch from `--context-window` rather than a fixed number,
because a batch has to fit in one agent's context: it derives both
`--max-text-bytes` and `--max-messages` from that window and passes them to
`memory next`. `--max-text-bytes` and `--max-batch-messages` override the derived
values when a harness needs something else. `--language` fixes the language every
shard writes in; without it one shard writes English and the next writes Chinese,
and `merge`, which drops duplicate lines rather than translating them, would then
state each fact twice in two languages.

### Any coding agent, and any provider behind it

`run` shells out to a coding agent, and which one is mostly a cost decision.
The measured API price of a full run is in the hundreds of dollars, while the
coding-agent subscriptions many people already pay for include the same class of
model at a flat monthly rate. `--agent pi` (the default), `--agent claude`,
`--agent codex` and `--agent gemini` each drive that harness the way it expects:
non-interactive, approvals off, and with the shard directory made writable,
since the state file and the wiki live outside the repository on purpose.
`--agent command --agent-command '<template>'` runs anything else — `{prompt}`,
`{model}`, `{cwd}` and `{directory}` are substituted, and a template with no
`{prompt}` receives the prompt on standard input.

```sh
# A subscription instead of a metered key.
python3 scripts/personal-memory-parallel.py run \
  --run /private/path/run --agent claude

# A third-party router instead of the first-party API.
export OPENROUTER_API_KEY=...
python3 scripts/personal-memory-parallel.py run \
  --run /private/path/run \
  --base-url https://openrouter.ai/api/v1 \
  --model google/gemini-3.7-flash
```

A subscription only pays for the run if the harness actually uses it. Several
harnesses prefer an API key over the login when both are present — Claude Code
says so on stderr and then bills the key — so a bare `--env NAME` removes that
variable for the agent process alone (`--env ANTHROPIC_API_KEY`), leaving your
own shell untouched. It is worth checking the first batch's stderr in the shard
log before starting a run that will make thousands of calls.

Only Pi is configured to discover the project skill from disk, so every other
harness is given the same skill text inside its prompt. That is `--skill inline`,
the default everywhere except Pi; `--skill discover` leaves it to the harness.
An agent that carries the skill has no reason to sit in the source tree, so it
runs from its own shard directory and cannot leave stray files in the checkout.

A third-party router is the other way to pay less: OpenRouter, Krill AI and
similar gateways serve the same models well below first-party prices.
`--base-url` points the run at one. Claude Code, Codex and Gemini CLI each read
an endpoint from their own environment variable, so the driver sets that
variable; Pi has no such variable, so the driver writes a private `models.json`
into a run-local agent directory and points `PI_CODING_AGENT_DIR` at it, which
leaves `~/.pi/agent` untouched. `--api-key-env` names the environment variable
holding the key — only the name is ever written to disk, never the key —
`--api-type`, `--context-window` and `--max-output-tokens` describe the endpoint,
`--env NAME=VALUE` passes anything else to the agent process alone (and a bare
`--env NAME` removes a variable), and
`--agent-arg` passes a flag straight through to the harness.

Because the printed estimate is only true of a metered key,
`plan --usd-per-1k-messages` re-prices it: pass what your provider charges, or
`0` for a subscription harness, and the estimate says so instead of quoting a
number that does not apply.

One caveat is worth knowing. `memory page` releases up to 49,152 bytes at a
time, and some harnesses truncate long tool output below that; a truncated page
must not be acknowledged, so the shard stalls rather than recording something it
did not read. The driver raises Claude Code's `BASH_MAX_OUTPUT_LENGTH` for this
reason, and `--env` or `--agent-arg` can raise the equivalent limit elsewhere.

`merge` combines the shard wikis into a derived wiki: people pages and `me.md`
are merged with duplicate lines dropped, and `index.md` is regenerated. A gated
conversation's months can land in different shards, so a conversation page may
have several contributors; they are merged in month order and reported, keeping
the page a timeline. The merged tree is an output artifact, not a run state —
keep refining through the shard states and merge again.

## Coverage means what the agent actually saw

For a canonical corpus, `selectedMessageCount == eligibleMessageCount`: time,
conversation, and sender filtering happens later in state, never during
preparation. `rowCoverageComplete: true` means every inventoried hashed message
table contributed readable metadata, including unresolved tables.
`sourceCoverageComplete: false` can still report lost conversation identity or
another source limitation. `contentComplete: false` means at least one eligible
body could not be hydrated or decoded. None of these states permits inference
about missing content.

`complete: true` always describes the current state scope. It proves review of
every hydrated corpus message only when the corpus is canonical and
`scope.allMessages: true`. A conversation-, time-, or sender-filtered complete
run proves only that exact scope.

Contact kind is conservative. `person` means an ordinary address-book record,
not proof of a current reciprocal friendship; `official`, `service`, `group`
and `unknown` remain separate.
