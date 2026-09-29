# Direct agent CLI workflow

Use these commands in the current agent's shell. No driver, embedded agent,
or separate API key is needed. Examples use `greenbubbles`; substitute the
verified absolute Rust CLI path when it is not on PATH. Quote all paths.

## Prerequisites and preparation

```sh
greenbubbles memory --help
greenbubbles profile list
greenbubbles source status --profile NAME
```

The memory help must include `--format`. Resolve source failures with the setup
skill. Use the user's existing profile and output choice. Create new private
working directories under `umask 077` (directories 0700, files 0600). Keep the
corpus and state outside the git-versioned memory project.

Copy [selection-policy.json](selection-policy.json) to a private work directory,
review its timezone and included conversation kinds, and obtain missing scope
choices from the user. Its `allMessages` mode prepares all eligible history
locally; `memory next` limits what this agent sees. It is not a remote-model
release policy. Do not silently broaden a user's requested source scope.

```sh
greenbubbles memory prepare /private/work/corpus-v1 \
  --selection-policy /private/work/selection-policy.json --profile NAME
```

The new corpus path must not exist. Never overwrite or edit a prepared corpus.
Do not read its evidence/contact/message sidecars directly. A small manifest
may be inspected for `largestUnitTextBytes` or generation metadata.

Initialize the output as a private git repository if it is new. Read an existing
manifest first and preserve user changes. For Markdown create `manifest.md`
and `domains/`; use the format reference for their contents. Keep run state
outside that repository. The CLI parameter is called `--wiki` for every format,
including domain Markdown and Python.

## Review one batch

```sh
greenbubbles memory next /private/work/corpus-v1 \
  --state /private/work/run-v1.json --wiki /private/memory/me \
  --format markdown --max-text-bytes 65536 --max-messages 250
```

Use `--format python` for Python; choose the format on the first `next` and keep
it unchanged. Repeat the same scope on every `next`:

- `--conversation ID` (repeatable), `--conversation-kind direct|group|official|service`;
- `--sender self` or a source person ID / `P######` (repeatable);
- `--from RFC3339 --through RFC3339` with explicit timezone offsets;
- `--subject account-holder|person:ID|none` controls focus, not evidence filtering.

Selections within a category are ORed; categories intersect. Empty filters mean
all prepared messages, including other senders. Do not silently select self-only
and discard conversational context. Bounds are inclusive. `--max-messages` is a
soft batch bound; an indivisible prepared unit can exceed it. If the CLI says a
unit exceeds `--max-text-bytes`, increase only that bound enough to fit the unit
and the current agent's context budget (CLI range 16384..2097152).

`next` contains a batch envelope, not messages. If `complete=true` with no batch,
report status and stop; do not try to commit a nonexistent batch.

```sh
greenbubbles memory page /private/work/corpus-v1 --state /private/work/run-v1.json
```

A page is at most 49152 bytes including JSON. Ensure the shell tool's output
limit can return it fully. Each message carries `e` (evidence alias), `a`
(self/other/unknown), `t` (date/time), `k` (kind), and `x` (text). `tr=true` is a
source-message text limit, not tool-output truncation. Preserve that limitation.
Person/conversation aliases join the page's real identity metadata; use names
in prose, aliases in citations. Do not infer self from a display name.

Read every message, patch the relevant domain files, and cite retained evidence,
for example `*(corpus: corpus-v1; source: E000000123; 2026-09-01)*`.
The page's `targetPages` describes the legacy wiki, not the domain-format paths.
Then acknowledge the current page with exactly its aliases retained in the project:

```sh
greenbubbles memory acknowledge /private/work/corpus-v1 \
  --state /private/work/run-v1.json --retain-evidence E000000123,E000000141
```

When all messages were reviewed but none contributed durable evidence:

```sh
greenbubbles memory acknowledge /private/work/corpus-v1 \
  --state /private/work/run-v1.json --reviewed-no-durable-memory
```

Repeat page → patch → acknowledge until review is complete. Repeated `page`
before acknowledgement returns the same page. Once acknowledged, it cannot be
reclassified; do not acknowledge speculative candidate facts before saving them.
For unchanged facts, either retain and record their new supporting evidence or
record no new durable memory; do not append duplicate facts.

Update the manifest and verify the diff and attribution, then:

```sh
greenbubbles memory commit /private/work/corpus-v1 \
  --state /private/work/run-v1.json --wiki /private/memory/me
greenbubbles memory status /private/work/corpus-v1 --state /private/work/run-v1.json
```

**`--format` belongs to `next`, not `commit`.** Commit rejects undelivered or
unacknowledged pages. Domain Markdown validation checks `manifest.md` and the
`## Schema`, `## State`, and `## History` headings in domain files; it does not
enforce the wiki's citation checks. Verify exact citations and accuracy yourself.
A batch with no new facts can still use the normal domain-format commit after
review and acknowledgement. Avoid fabricating a change to obtain a commit.

Stage only project files changed for this task and git-commit them if changed.
Do not sweep unrelated user edits into a commit. If Git identity is unavailable,
report that separately; do not falsify identity or roll back successful CLI progress.
Repeat `next` with the same state until complete or the user's limit is reached.

## Resume and incremental refresh

On interruption, use the same corpus, state, output format, and scope. `next`
returns the outstanding batch. Check status before creating any new state.
Never delete state to get past a rejection. Fix the reported problem and retry;
if the same failure persists after one targeted correction, report it with the
outstanding batch preserved.

To include new source data, first complete the previous run, then:

```sh
greenbubbles memory prepare /private/work/corpus-v2 \
  --extend /private/work/corpus-v1 \
  --selection-policy /private/work/selection-policy.json --profile NAME
```

A state is bound to one immutable corpus: **use a new state path for v2**, keep
the same memory project, and retain v1 and its state for provenance/recovery.
For a routine incremental pass, set `--from` to the previous completed run's
explicit `--through` bound (inclusive overlap) and set a new `--through` bound.
Persist these bounds and the corpus/state paths in the project's manifest only
after that entire scoped run completes. On retries reuse the recorded pending
scope, never replace it with the wall-clock time. Unchanged facts are deduplicated.

Timestamp-scoped updates can miss late-imported historical messages and edits
with old timestamps. If the previous run lacks a trustworthy completed bound,
or the user requests historical reconciliation, run a new state over the full
extended corpus and deduplicate against existing domain state. This costs more
model context but does not silently discard old-dated additions. Do not describe
timestamp filtering as a complete change feed or claim deletions from absence.

For unattended scheduling or sharding the repository has an optional
`scripts/personal-memory-parallel.py tick` driver. It launches separate agents;
it is not needed by these installed skills. Prefer one writer (`--shards 1
--parallel 1`) for a shared domain project. Scheduling and provider billing
remain the user's choices.

## Report completion honestly

Report `committedMessageCount`, current scope, completion/outstanding status,
`sourceCoverageComplete`, `contentComplete`, and `limitationCodes` from status.
After commit, the previous batch counters live under `lastCommitted`;
`reviewComplete` may be null when no batch is outstanding. Whole-corpus review
requires completion of an unfiltered canonical all-message scope, not merely a
successful batch or a recent timestamp window.
