# CLI contract

## Access

Prefer a private named profile so secrets never share standard input with other data:

```sh
greenbubbles profile list
greenbubbles source status --profile NAME
```

An explicit live source is also supported with `--passphrase-stdin`, but redirect the owner-only credential through standard input. Never interpolate or print it.

## Contacts

```sh
greenbubbles contacts list [SOURCE] [--profile NAME] \
  [--kind account-holder|person|group|official|service|unknown] \
  [--limit 1..500] [--cursor OPAQUE] [--details] [ACCESS]
```

Follow `page.nextCursor` until `page.hasMore` is false only when the task actually needs the full contact list. Preserve the filter and `--details` setting across pages; cursors are bound to them.

## Prepare the corpus once

Create the output parent with mode `0700`. The output itself must not exist.

```sh
greenbubbles memory prepare NEW_CORPUS \
  --selection-policy POLICY.json --profile NAME

greenbubbles memory prepare SOURCE NEW_CORPUS \
  --selection-policy POLICY.json --passphrase-stdin < PRIVATE_KEY_FILE
```

`prepare` scans reversible message-table metadata in bounded SQL pages, identifies self-authored messages by authenticated account-ID equality, selects account-holder-active calendar-month sessions, and hydrates only merged context windows. It atomically creates:

`deliveryOrder` accepts `accountHolderRelevance` or `chronological`. The former
is recommended for a new whole-corpus wiki. It changes only the immutable unit
schedule; selection counts, source evidence and eventual full traversal are
unchanged.

```text
NEW_CORPUS/
  manifest.json
  coverage.json
  contacts.jsonl
  activity.jsonl
  evidence.jsonl
  batches/index.json
  batches/U######.json
```

Do not feed these sidecars wholesale to the model. `memory next` is the token-bounded interface.

## Process one durable batch

Create the wiki and state parent as owner-only directories. A practical Gemini
3.7 Flash starting bound is 524288 text bytes; it must be at least
`manifest.json.largestUnitTextBytes`. Page output remains independently capped,
so increasing the batch bound does not make a page unreadable by Pi.

```sh
greenbubbles memory next NEW_CORPUS \
  --state RUN_STATE.json --wiki WIKI --max-text-bytes 524288
```

Keep `RUN_STATE.json` and `WIKI` outside the project and source control. `next`
returns only a small envelope. Inspect `delivery` for the page count, progress,
fixed output ceiling, and `deliveryOrder`. The persisted state already
identifies this uniquely current batch, so ordinary agent calls do not need to
copy its opaque `batchId`. The envelope contains no chat messages.
`accountHolderRelevance` is a deterministic weighted schedule:
it favors self-message volume, active-month breadth, recency and direct context,
interleaves strong months within a relationship, and still includes every
prepared unit. It does not claim that a high-ranked message is true or durable.

Fetch exactly the next unacknowledged page directly through the shell tool:

```sh
greenbubbles memory page NEW_CORPUS \
  --state RUN_STATE.json
```

Each compact page, including its JSON envelope and newline, is at most 49152
bytes, below Pi's 51200-byte built-in tool-output limit. A retry before
acknowledgement is byte-identical. Never acknowledge output marked as truncated.
The page contains:

- `pageToken` and `pageSHA256`: deterministic bindings retained for audit and
  explicit replay; ordinary acknowledgement resolves the uniquely current
  delivered page from state.
- `page.number`, `page.pageCount`, `messageCount`, and `textByteCount`.
- `targetPages`: the only Markdown paths this page can inform.
- `people`: stable `P######` aliases to display names for this page.
- `episodes`: chronological prepared-unit fragments. `u` is the unit alias,
  `o` is the fragment's zero-based message offset, and `n` is the unit's total
  message count; the same unit may continue on the next page.
- In each episode, `c` is the stable conversation alias and `m` is its message array.
- In each message, `e` is the evidence alias, `a` is `self`, `other`, or
  `unknown`, `p` is an optional person alias, `t` is Unix time, `k` is payload
  kind, `x` is concise text, and `tr=true` means only that this message's text
  reached the configured per-message bound. It is not a page/tool truncation
  signal; only an explicit shell-tool truncation notice makes the page unsafe
  to acknowledge.

Keep the wiki private:

```text
WIKI/
  index.md
  me.md
  people/P######.md
```

Use stable headings and comprehensive but concise factual bullets. Put exact
citations on every factual prose line, for example:

```markdown
- Prefers tea when working late. [E000000123] [E000000141]
```

When evidence conflicts, retain the dated conflict or qualify confidence. Do not silently replace a cited historical fact with the newest statement.
Prefer a representative citation set over attaching every repetition of the
same claim. Reconcile and consolidate existing prose on every page; do not grow
the wiki as an append-only transcript. A factual prose line may contain at most
eight exact aliases; ordinary commit enforces this token-bloat boundary.
Check that a target page exists before reading it; a missing page is normal and
should be created only when the current evidence supports useful prose. A prior
alias may remain on or be consolidated within the same page that already cited
it. Do not introduce that old alias on a different page unless it is also
present in and retained from the current batch; this prevents cross-page
citation laundering.
Every factual line on `me.md` must also contain at least one self-authored
alias. Incoming-only facts belong on the relevant person page; commit resolves
actor provenance from the immutable local evidence sidecar and enforces this.

Acknowledge only after reading every message in the page and durably writing its
useful memory into the wiki. List exactly the page aliases now cited by durable
wiki prose as one comma-separated argument:

```sh
greenbubbles memory acknowledge NEW_CORPUS \
  --state RUN_STATE.json \
  --retain-evidence E000000123,E000000141
```

If and only if the fully reviewed page has no durable evidence, keep its aliases
out of the wiki and record the explicit page disposition:

```sh
greenbubbles memory acknowledge NEW_CORPUS \
  --state RUN_STATE.json \
  --reviewed-no-durable-memory
```

Repeat `page` then `acknowledge` until `reviewComplete` is true. Acknowledgement
is sequential, requires prior delivery, validates retained aliases against that
exact page, and cannot later be reclassified. Retained aliases are not a scratch
candidate list: each must remain cited in the wiki through commit. `page`,
`acknowledge`, and `commit` default to the uniquely current persisted batch;
`acknowledge` likewise defaults to its current unreviewed delivered page. Use
`--batch ID` and `--page-token TOKEN` only when an operator needs to bind an
explicit audit/replay invocation. The literal selector `current` is also
accepted.

Commit only after edits are durably written with file mode `0600` and directories
mode `0700`. Do not touch all `targetPages` mechanically: create or edit only
useful pages. Every page must have been fetched and acknowledged. At least one
changed non-index page must contain prose and cite retained evidence; empty,
heading-only, uncited, retained-but-uncited, stale-evidence-only,
unretained-evidence, over-cited prose, and accidental no-op commits are
rejected. Changed `me.md` prose without self-authored support is also rejected:

```sh
greenbubbles memory commit NEW_CORPUS \
  --state RUN_STATE.json --wiki WIKI
```

If, and only if, every page was fully reviewed and acknowledged without retained
evidence, do not invent a claim or placeholder. Leave the wiki byte-for-byte
unchanged and record the deliberate batch disposition:

```sh
greenbubbles memory commit NEW_CORPUS \
  --state RUN_STATE.json --wiki WIKI \
  --reviewed-no-durable-memory
```

This mode rejects any wiki change. It is not a shortcut for incomplete review,
uncertainty that should instead be expressed in cited prose, or tool failure.

Commit checks immutable unit and deterministic page hashes, complete delivery and
review accounting, safe owner-only wiki paths, changed-page scope, retained
evidence aliases, citations on every factual prose line, and the explicit
unchanged-wiki disposition above. It does not merge Markdown. A rejected commit
leaves the same batch outstanding; correct the wiki and retry. Repeating a
successful commit is idempotent.

## Status and recovery

```sh
greenbubbles memory status NEW_CORPUS --state RUN_STATE.json
```

- After interruption, call `next` with the same state and wiki.
- Status reports `scannedMessageCount`, `selectedMessageCount`, cumulative
  `committedMessageCount`, source/content coverage flags, unmatched-table count,
  and aggregate `limitationCodes`. Use these fields for the final handoff; do
  not open private corpus sidecars from Pi.
- `reviewComplete` and the outstanding review counters are `null`/zero when no
  batch is outstanding. After commit, use the `lastCommitted` object for the
  preceding batch's reviewed page, message and retained-evidence counts.
- During page review, call `page` again; the same unacknowledged page repeats
  byte-for-byte. Do not manually advance state; acknowledgement resolves the
  same delivered current page, or an operator can pass its exact token.
- Never delete or manually advance the state cursor.
- Never edit the wiki after a successful commit and before the next `next`; its
  hashes are bound to state, and out-of-protocol drift is rejected even at
  completion.
- If `sourceCoverageComplete` or `contentComplete` is false, carry the status
  limitations into the handoff and never infer what missing messages said.
- Batch commit requires `acknowledgedPageCount == outstandingPageCount` and
  `reviewComplete=true`. Corpus completion additionally requires no outstanding
  batch and `nextUnitIndex == unitCount`.
