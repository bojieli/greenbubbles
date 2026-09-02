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

## Prepare one canonical corpus

Create the output parent with mode `0700`. The output itself must not exist.

```sh
greenbubbles memory prepare NEW_CORPUS \
  --selection-policy POLICY.json --profile NAME

greenbubbles memory prepare SOURCE NEW_CORPUS \
  --selection-policy POLICY.json --passphrase-stdin < PRIVATE_KEY_FILE
```

Use the v2 `allMessages` policy. `prepare` scans every inventoried message table in bounded SQL pages, identifies self-authored messages only by authenticated account-ID equality, and hydrates every eligible row. A hashed table that cannot be mapped to a source conversation is retained under a stable unresolved conversation alias; `unmatchedMessageTable` remains a coverage limitation because its identity is unavailable. Preparation atomically creates:

`deliveryOrder` accepts `accountHolderRelevance` or `chronological`. The former is recommended for a new wiki. It changes only immutable unit scheduling; the canonical evidence set and eventual full traversal are unchanged. Legacy v1 policies still produce account-holder-active episode corpora and remain readable, but cannot prove whole-database review.

```text
NEW_CORPUS/
  manifest.json
  coverage.json
  contacts.jsonl
  conversations.jsonl
  activity.jsonl
  evidence.jsonl
  batches/index.json
  batches/U######.json
```

New corpora use a compact v2 `batches/index.json`: evidence runs are represented
by their first ordinal, repeated target paths are represented by person/sender
keys, and verbose identity records stay deduplicated in sidecars. An all-message
state does not repeat every canonical unit selection. Model pages then expose
the real identity record once per used person or conversation rather than on
every message, while stable `E#########` citations remain compact. Existing v1
indexes remain readable when their identity sidecars are complete.

Do not feed these sidecars wholesale to the model. `memory next` is the token-bounded interface.

## Choose evidence scope and summary subject

Put the scope directly on every `memory next` invocation. There is no scope JSON file:

```sh
greenbubbles memory next CORPUS \
  --state RUN_STATE.json --wiki WIKI --max-text-bytes 524288 \
  --conversation C000449 --conversation C000731 \
  --conversation-kind group \
  --from 2023-12-01T00:00:00+08:00 \
  --through 2023-12-31T23:59:59+08:00 \
  --sender self --sender P000123 \
  --subject account-holder
```

The evidence filters are:

- repeatable `--conversation`: an exact source conversation ID or stable `C######` key;
- repeatable `--conversation-kind`: `direct`, `group`, `official`, or `service`;
- `--from` and `--through`: inclusive RFC 3339 bounds with an explicit numeric offset or `Z`;
- repeatable `--sender`: a source person ID or `P######` key. `self` and `accountHolder` select the authenticated account holder; `unknown` explicitly selects unresolved senders.

Values within one repeatable category are ORed. The conversation, kind, time, and sender categories intersect. Omitting all four categories means every hydrated message in the canonical corpus; omitting `--sender` means every sender, never self-only.

`--subject` controls wiki focus without changing evidence selection. It defaults to `account-holder`; use `person:<source-id-or-P######>` for another person or `none` for conversation-centric memory. Unknown, duplicate, oversized, ambiguous, inverted, offset-free, or malformed values fail before state creation.

RFC 3339 fractional bounds are applied exactly to the source's whole-second timestamps: a fractional `--from` advances to the next whole second, while a fractional `--through` retains its containing whole second. Returned scope times are the effective bounds normalized to the corpus timezone.

## Process one durable batch

Create the wiki and state parent as owner-only directories. A practical Gemini
3.7 Flash starting bound is 524288 text bytes; it must be at least
`manifest.json.largestUnitTextBytes`. Page output remains independently capped,
so increasing the batch bound does not make a page unreadable.

```sh
greenbubbles memory next NEW_CORPUS \
  --state RUN_STATE.json --wiki WIKI --max-text-bytes 524288 \
  [SCOPE ARGUMENTS]
```

Keep `RUN_STATE.json` and `WIKI` outside the project and source control. Repeat the exact scope arguments on every `memory next`; supplying no evidence filters explicitly requests the whole corpus. A state cannot change scope while a batch is outstanding or the current scope is incomplete. After completion, a different set of arguments serially rebinds the state, preserves the committed wiki snapshot, and records the completed scope.

For an unfiltered canonical scope, state records the all-message binding and
count without serializing one redundant all-selected record for every unit.
Sender-filter planning first uses compact sender-presence metadata as a safe
negative filter, then verifies exact matches inside candidate units.

`next`
returns only a small envelope. Inspect `delivery` for the page count, progress,
fixed output ceiling, `deliveryOrder`, and the resolved scope summary. The persisted state already
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
bytes, below Pi's 51200-byte built-in tool-output limit. A harness with a lower
tool-output ceiling must have it raised, or it will truncate a complete page. A
retry before acknowledgement is byte-identical. Never acknowledge output marked
as truncated.
The page contains:

- `pageToken` and `pageSHA256`: deterministic bindings retained for audit and
  explicit replay; ordinary acknowledgement resolves the uniquely current
  delivered page from state.
- `page.number`, `page.pageCount`, `messageCount`, and `textByteCount`.
- `targetPages`: the only Markdown paths this page can inform.
- `accountHolder`: the authenticated owner's real source ID and best available display name, plus distinct remark, nickname, and WeChat alias fields when the source contains them.
- `people`: stable `P######` join keys to real source IDs, display names, remarks, nicknames, and WeChat aliases for this page.
- `conversations`: stable `C######` join keys to real source IDs, titles, and kinds for this page.
- `scope`: filter counts, whether the evidence scope is all messages, and the resolved summary subject.
- `episodes`: chronological prepared-unit fragments. `u` is the unit alias,
  `o` is the fragment's zero-based message offset, and `n` is the unit's total
  message count; the same unit may continue on the next page.
- In each episode, `c` is the stable conversation alias and `m` is its message array.
- In each message, `e` is the evidence alias, `a` is `self`, `other`, or
  `unknown`, `p` is an optional person join key, `t` is RFC 3339 in the corpus timezone, `k` is payload
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
  conversations/C######.md
```

Treat conversation pages as chronological leaf memory, person and `me.md` pages as durable rollups, and `index.md` as navigation plus a compact global overview. A page for an account-holder subject can target `me.md`, relevant people, and relevant conversations. A person subject targets that person's page and relevant conversations. A `none` subject targets conversation pages, not `me.md`. Do not promote every line upward: retain durable facts, relationship changes, dated events, conflicts, and patterns while leaving transient chatter represented only by explicit review accounting.

Use real names and titles in every heading, link label, and prose reference. Keep `P######` and `C######` only in collision-safe paths and machine joins. When the source has no display label, use its real source ID; never invent `Person P######`, `Group C######`, or another anonymous substitute. Preserve distinct remark, nickname, and alias values in page metadata when they differ.

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
  `committedMessageCount`, `eligibleMessageCount`, `corpusMessageCount`, resolved
  `scope`, `completedScopeCount`, source/content coverage flags, unmatched-table count,
  and aggregate `limitationCodes`. `corpusMessageCount` is the hydrated canonical evidence count; `selectedMessageCount` is the current scope's matched count. Use these fields for the final handoff; do
  not open private corpus sidecars from the agent.
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
  batch and `nextUnitIndex == unitCount`. This proves whole-corpus review only when `scope.allMessages` is true on a canonical v2 corpus. Otherwise it proves completion of that exact scoped view.
