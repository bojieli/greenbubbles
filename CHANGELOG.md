# Changelog

Notable changes to GreenBubbles are documented here. The project follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and intends to use
[Semantic Versioning](https://semver.org/).

## Unreleased

### Added

- `tick` command: process-level exclusive flock on `.greenbubbles-tick.lock`
  prevents concurrent cron invocations from racing on the same user project.
  Same guard applied to `manifest-refresh` and `revise`.
- `tick` driver: `_is_api_error()` detects transient Gemini quota / rate-limit
  / FAILED_PRECONDITION errors (distinct from genuine agent stalls), with
  exponential backoff (base 120 s, doubles per retry, cap 30 min).  `--max-api-retries`
  (default 5) and `--api-retry-seconds` (default 120) on both `run` and `tick`
  subcommands.
- `tick` driver: per-project `threading.Lock` in `git_commit_user_project`
  prevents concurrent shards from interleaving `git add -A` and `git commit`.

### Fixed

- `tick` driver: `lastTickTime` is no longer advanced when API errors occurred
  and 0 messages were committed.  Quota exhaustion no longer silently skips
  processing windows; the same window is retried once quota recovers.
- `tick` driver: cap `--parallel` at 1.  All tick shards share one user-project
  directory; running agents concurrently caused last-writer-wins races on domain
  files, silently dropping extracted facts.  Multi-shard runs still benefit from
  shorter per-shard corpus batches — they simply execute one shard at a time.
- `tick` driver: `.greenbubbles-tick.lock` and `.greenbubbles-revise.log` added
  to `.gitignore` so internal process files are never committed to the user
  project git history.
- `run_shard` and `run_tick_shard`: log `[exit N: first-error-line]` on every
  non-zero agent exit for easier post-mortem debugging.
- `_first_error_line`: skip informational harness notices (Gemini CLI ripgrep
  fallback banner, approval-mode header) so the displayed error snippet reflects
  the actual failure cause rather than masking it.
- `_is_api_error`: add network-level failure patterns (`fetch failed`,
  `econnrefused`, `econnreset`, `etimedout`, `socket hang up`) so transient
  DNS/TLS/TCP failures trigger the API-error retry path instead of the stall
  detector.  Without this fix, a network blip caused the driver to mark a scope
  stalled and advance `lastTickTime`, silently skipping unprocessed messages.
- Agent command: `--skip-trust` flag added for Gemini CLI ≥ 0.46.0 headless
  mode (loop-detection upgrade required an explicit trusted-directory flag).
- Tick agent prompt: agents now read only 2–3 domain files per batch (not all)
  to reduce tool-call counts and avoid triggering loop-detection false positives.
- Markdown commit step now carries the same urgency language as Python: names
  both validation checks and warns that skipping causes duplication on retry.

## 0.2.0 - 2026-09-03

### Added

- Added `memory prepare --extend` for chained corpus generation: loads a base
  corpus, re-scans metadata in full, hydrates only new rows, inherits alias
  maps, allocates fresh counters for new conversations and senders, carries
  existing unit files byte-for-byte, and appends new units. A new manifest
  field `extends` records the base manifest hash, generation, and first new
  unit index. Fail-closed: a missing or mutated base message is a hard error;
  the extended corpus must be prepared from scratch.
- Added format-aware `memory commit` and `--format python|markdown|wiki` on
  `memory next`. The run state records the output format on first bind and
  enforces it on every subsequent commit. Python format validation checks that
  `manifest.py` exists, every `.py` file parses without syntax errors, and no
  binary or disallowed-extension files are present; hidden VCS files (`.git/`,
  `.gitignore`, `.greenbubbles-runs/`) are excluded from the walk.  Markdown
  format validation checks that `manifest.md` exists and every `domains/*.md`
  carries `## Schema`, `## State`, and `## History` sections. The legacy wiki
  format path is unchanged and backward-compatible.
- Added `tick`, `manifest-refresh`, and `revise` commands to
  `scripts/personal-memory-parallel.py` for UserAsCode incremental extraction.
  `tick` finds the `lastTickTime` watermark, computes an `--from` bound, plans
  and runs one agent batch of new messages, and advances the watermark on a
  successful commit. `manifest-refresh` re-executes constraints and regenerates
  the manifest without touching domain state. `revise` runs a holistic agent
  pass to consolidate stale facts, split or merge domains, and update the
  manifest. `--user-project` and `--format` route domain output to the
  UserAsCode project directory instead of a wiki.
- Added UserAsCode two-phase extraction pipeline: Phase 1 delivers message
  units via `memory next/page/acknowledge`; Phase 2 CRUD-patches domain files
  (Python dataclasses or structured Markdown). Domain files are organically
  created by the agent, deduplicated per run, and version-controlled in the
  user project as a git repository. The agent writes executable constraints
  (`constraints/*.py`) that produce `ACTIVE_ALERTS` in `manifest.py`, and
  invariant tests (`tests/test_*.py`) runnable with `pytest`.
- Added `skills/greenbubbles-personal-memory/references/format-python.md` and
  `references/format-markdown.md` with format-specific agent guidance for the
  UserAsCode methodology: ontology taxonomy, CRUD-patch deduplication rules,
  schema and state structure, constraint lifecycle, and manifest regeneration.

### Changed

- Rewrote `skills/greenbubbles-personal-memory/SKILL.md` for the UserAsCode
  methodology. The skill now covers both Python and Markdown output formats,
  two-phase fact extraction, domain ontology classification, CRUD-patch
  semantics, and constraint-driven alerting.
- Rewrote `docs/PERSONAL_MEMORY.md` around a living knowledge project model:
  "Prepare once" framing replaced by UserAsCode incremental extraction,
  Python vs. Markdown format guidance, ontology taxonomy, constraint lifecycle,
  and multi-timescale cron scheduling examples with cost tables.
- Updated `docs/KNOWN_LIMITATIONS.md` to remove the "not resumable/incremental"
  entry: `memory prepare --extend` (input side) and UserAsCode CRUD-patch
  (output side) together enable minute-level incremental extraction.
- Updated `docs/CLI_REFERENCE.md` with `--extend`, `--format`, `tick`,
  `manifest-refresh`, and `revise` command entries.
- Updated `README.md` with an AI feature coverage section and a
  "Turning your history into a living knowledge project" section explaining the
  UserAsCode paradigm, Python and Markdown format examples, `memory prepare
  --extend` for incremental input, and cron setup.

### Added

- Added authenticated live `isAccountHolder` message attribution with `You`
  display normalization and policy-preserving omission for unknown/withheld
  senders.
- Added `ai-summarize-direct`, an explicit Gemini 3.7 Flash memory compiler
  with compact citation aliases, validated structured and Markdown output,
  private provenance sidecars, atomic publication, and coverage/token evidence.
- Added source-bound `contacts list` pagination with conservative contact kinds,
  exact account-holder marking, and optional remark/nickname/alias details.
- Added the production corpus-scale `memory prepare/next/page/acknowledge/commit/status`
  workflow: canonical all-message preparation, repeatable command-line
  conversation/kind/sender filters, inclusive RFC 3339 time bounds, independent
  account-holder/person/none summary subjects, full source identities and names
  in model pages without repeating them on every message,
  stable citations across scopes, unmatched-table row preservation, immutable
  private evidence, deterministic `page`/`acknowledge` delivery below
  Pi's tool-output ceiling, citation/wiki validation including retained-to-cited
  completeness, weighted account-holder/active-month relevance ordering that
  still schedules every unit, an eight-citation representative-evidence ceiling
  on factual prose, self-authored support enforcement for every account-holder
  fact, explicit unchanged-wiki disposition for reviewed low-value
  batches, state-resolved `current` batch/page selectors that avoid copying
  opaque identifiers, idempotent commits, unambiguous last-committed status,
  cumulative committed/selected message and coverage status, and content-free
  preparation progress.
- Added a compact v2 prepared-unit index and compact all-message run state after
  real 100,000-plus-unit validation exposed the original 64-MiB control-index
  ceiling. Stable evidence aliases and legacy v1 index readability are
  preserved; sender scopes use compact presence metadata as a safe planning
  prefilter before exact unit verification.
- Added `scripts/personal-memory-parallel.py`, which filters conversations by
  kind, account-holder participation, month window and budget, packs them into
  balanced shards (splitting an oversized conversation into month ranges so no
  single conversation becomes the critical path), runs eight agents at a time by
  default against the shared immutable corpus, and merges the shard wikis.
  `--group-min-self-per-month` keeps direct chats whole but reads a group only
  in the months the account holder actually spoke there, which is where most of
  the cost of a large WeChat history sits. Shards bind their scopes one at a
  time in order, and scopes are grouped by time window rather than by
  conversation, so invocation overhead follows the windows, not the thousands
  of conversations. The run summary now counts what an unfinished scope has
  already committed, so stopping at the batch budget no longer reports zero.
- Added harness and provider choice to `scripts/personal-memory-parallel.py`,
  because the model bill, not the corpus, is what makes a full run expensive.
  `--agent` runs the batches under Pi, Claude Code, Codex, or Gemini CLI, and
  `--agent command` under any other harness, so a coding-agent subscription can
  do work that would otherwise be charged per message to an API key. `--base-url`
  points a run at a third-party router such as OpenRouter or Krill AI instead of
  the first-party API; for Pi, which has no endpoint variable, the driver writes
  a run-local `models.json` and leaves the user's own configuration alone. Keys
  are passed by variable name and never written to the plan, the log, or the
  prompt. Harnesses that do not discover the project skill receive its text in
  the prompt and run from their own shard directory, and
  `plan --usd-per-1k-messages` re-prices the estimate for the provider actually
  in use.
- Added `memory next --max-messages`, because `--max-text-bytes` bounds stored
  chat text and says nothing about the roughly 130-byte envelope every delivered
  message carries. A thread of one-word replies therefore filled far more
  delivery pages than its text bytes predicted, and a batch has to fit in one
  agent's context window. The bound is soft: it stops a batch taking another
  unit and never splits or refuses the unit a batch must deliver whole.
  `scripts/personal-memory-parallel.py` now derives both bounds from
  `--context-window` rather than a fixed 512 KiB, so a 200K-context harness gets
  a batch it can actually hold, and `--language` keeps every shard writing one
  language so a line-by-line merge does not state each fact twice in two.
- Added a Pi-discoverable `greenbubbles-personal-memory` Agent Skill and
  project `.pi/settings.json` integration. Pi remains the default ReAct runtime
  and the one the examples use, but the skill is the whole contract: no
  extension, custom tool, or daemon is required of any agent that runs it.

### Changed

- Delivery pages now render WeChat markup envelopes as the human text inside
  them instead of verbatim XML. Stickers are 2.7% of a real 1.7M-message corpus
  but 48% of its delivered text bytes, all of it CDN URLs, MD5 sums and buffer
  lengths; location and system envelopes do carry meaning, so their place names
  and templates survive while the plumbing does not. Measured on one real
  2,664-message scope: 18 delivery pages became 10, page one went from 130 to
  257 messages, and `memory next` fell from 16.4s to 6.1s. The prepared corpus
  is not rewritten, so existing corpora get this without re-preparation.
- The account holder now reaches the agent as `Me` rather than a raw `wxid_…`
  source id, which three different harnesses had faithfully used as the title of
  `me.md`. The source id still travels beside the label.
- A rejected `memory commit` now reports every problem at once, with one-based
  line numbers for uncited and citation-dumping prose lines and the actual
  aliases that were unknown, unexpected, or retained but never cited. Reporting
  one problem per rejection made an agent pay a full batch invocation per fix.
- Wiki ownership and shape errors now name the offending path, its mode and its
  link count, so an agent that created a subdirectory under the process umask
  can see which one to `chmod` instead of guessing.
- `memory commit` no longer decodes the whole evidence sidecar to resolve the
  actors behind `me.md`'s citations. The sidecar is 1.5 GB at corpus scale and a
  commit cites a handful of aliases, so only the cited lines are parsed; every
  byte is still hashed, which is what binds the file to its manifest. Measured on
  the real 1,724,948-message corpus, a commit that changes `me.md` fell from
  7.3s to 5.6s, against 1.0s for a commit that does not.

### Fixed

- `scripts/personal-memory-parallel.py` now creates `wiki/conversations` and
  `wiki/people` itself at mode 700. An agent creating them mid-batch got the
  process umask, and the not-owner-only directory then failed the commit that
  batch had already been paid for.
- Made the source database's explicit `Name2Id` sender relation authoritative
  over legacy group-content prefix parsing, and reject malformed/XML-like
  content-derived sender identifiers before attribution or corpus publication.
- Removed production-length canonical message IDs and verbose connector
  metadata from model prompts while preserving exact local citation evidence.

## 0.1.1 - 2026-08-29

First public source-and-binary research release.

### Added

- MIT project license and a generated, target-specific third-party notice
  bundle covering Rust dependencies, SQLCipher, SILK, Zstandard, and derived
  acquisition code.
- Developer ID signing, Apple notarization, a stapled app disk image, a signed
  CLI archive, SBOMs, SHA-256 checksums, and notarization logs in GitHub
  Releases.
- Public contribution, security, conduct, issue, pull-request, CLI reference,
  and release-checklist documentation.

### Changed

- Renamed the native Rust engine from `greenbubbles-restore` to
  `greenbubbles`. Restoration is one of roughly eighty subcommands, so the old
  name described a fraction of the tool and read poorly in every other command
  family.
- Renamed the Swift discovery executable from `greenbubbles` to
  `greenbubbles-discover`, matching its default subcommand and freeing the
  primary name for the main command-line entry point. There is no compatibility
  alias for either old name; update saved paths, scripts, and the command-line
  tool selected in the history browser.
- Moved the Rust workspace from `Native/GreenBubblesRestore` to
  `Native/GreenBubbles` so the source layout follows the primary CLI name.
- Reframed the project around private, local AI context and created a concise
  public-facing entry point.
- Reorganized and substantially rewrote the public documentation. The README
  now opens with the measured corpus that motivated the project, shows a real
  query-envelope shape, carries measured numbers from `docs/MEASUREMENTS.md`,
  and states what remains unproven.
- Added `docs/README.md` as a task-oriented index, plus dedicated FAQ, known
  limitations, comparison, threat-model, roadmap, auditing, replica-operations,
  and privacy documents.
- Consolidated the connector contract and consumer example, replica operations,
  restoration pipeline, audit guides, and measurement evidence into one
  current document for each subject; archived superseded plans and feasibility
  records now identify what replaced them.
- Added a branded application and project icon plus two accessible diagrams
  showing the disclosure boundary and the read path.
- Hardened CI and release permissions, pinned external Actions, and added
  fail-closed public-release and secret-hygiene checks.
- Updated Rust package metadata and versioning for the MIT-licensed release.

### Fixed

- Replaced deprecated secure UTF-8 validation with zeroizable byte validation
  and added malformed and Unicode regression coverage.
- Corrected the release workflow's Hardened Runtime assertion to recognize the
  `CodeDirectory ... flags=...runtime...` form emitted by `codesign`.

## 0.1.0 - 2026-08-29

First tagged research prerelease:

- read-only discovery and bounded live/snapshot history queries;
- native history browser and independently recoverable snapshots;
- lossless restoration, encrypted replica, scoped connectors, and AI context
  projection;
- owner-run passphrase acquisition helper;
- experimental text/image/file send adapter that ships closed by default.

The 0.1.0 binaries were unsigned and unnotarized and were withdrawn before the
repository became public. Use 0.1.1 or later.
