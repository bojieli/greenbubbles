# Changelog

Notable changes to GreenBubbles are documented here. The project follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and intends to use
[Semantic Versioning](https://semver.org/).

## 0.10.0 - 2026-10-01

### Added

- `chats find <name>` finds known identities by nickname, remark, alias, or ID.
- Message list, search, and exact-message retrieval accept unambiguous chat
  names; ambiguous names show the matching IDs.
- `messages recent` shows a globally time-ordered page across identifiable
  chats, with chat labels, time filters, source-bound pagination, and full
  message IDs for debugging and synchronization.
- `--version` prints the installed CLI version.

### Fixed

- `chats --limit` and other options no longer get mistaken for subcommands.
- Top-level help includes `Usage`, matching the Homebrew smoke test.

## 0.9.5 - 2026-10-01

### Changed

- Expanded the WeChat database guide with database purposes, exact observed
  table schemas, field meanings, attachment/resource tables, and safe schema
  inspection guidance.

## 0.9.4 - 2026-10-01

### Added

- Added a documented guide to the live WeChat database layout, message table
  fields, auxiliary stores, encryption, and message decoding.

### Fixed

- The live CLI now reads business message shards and media databases stored
  beside current WeChat message shards.

## 0.9.3 - 2026-09-30

### Changed

- Removed the flaky macOS process-launch media tests from the CI suite; the
  remaining Swift tests run deterministically.
- Simplified Homebrew publication: the release's full CI gate runs once, then
  the formula is verified from the signed assets and updated with
  `HOMEBREW_TAP_TOKEN` without a second staged-branch CI run.

## 0.9.2 - 2026-09-30

### Changed

- Reorganized the personal-memory skill so each rule has one home, grouped
  by topic, and included `priorities.md` in batch-agent prompts.
- Rewrote the AI context, backup, and command-line guides for first-time
  readers, while preserving their commands, fields, limits, and security
  rules.
- Standardized the documentation terms: a recovery phrase is the 24 words;
  a recovery kit is the file that stores them.

## 0.9.1 - 2026-09-30

### Changed

- The personal-memory skill keeps credentials, payroll and reservation
  details, ssh and proxy lines, pasted scripts and prompts, and valuation
  figures out of the notes, and files the occasion instead.
- The Homebrew workflow fast-forwards main with the `HOMEBREW_TAP_TOKEN`
  secret after CI passes on the staged formula commit.

## 0.9.0 - 2026-09-30

### Changed

- The personal-memory skill keeps articles readable as they grow: a short
  lead, short paragraphs, and dated or nested lists for long episodes, with
  no sentences that only deny an inference or announce an omission.
- The Homebrew workflow commits the formula to a staging branch, runs CI on
  it, and fast-forwards main, so releases update the tap without bypassing
  branch protection.

### Fixed

- The live media tests no longer time out when a CI runner is slow to
  approve a freshly written test script.

## 0.8.0 - 2026-09-29

### Changed

- `ai-summarize-direct` now calls `gemini-3.8-flash` instead of
  `gemini-3.7-flash`.
- README, PRIVACY, and the onboarding guides are rewritten in plain language
  for first-time users.

## 0.7.1 - 2026-09-29

### Fixed

- The release check treats Clippy warnings as errors. Naming a sender now uses
  `?`, so the Rust 1.98 `question_mark` lint no longer stops the build.

## 0.7.0 - 2026-09-29

### Added

- `messages list` and `messages search` accept `--redact`. It omits a resident
  identity number, a mainland mobile, an email address, and an `http` or
  `https` link from the reading page, including a quote, a link title, a file
  name, a transfer note, and a display name. A video-meeting invitation that
  includes a meeting id or a join link becomes `[meeting]`. Without the flag,
  those values stay in the text. A bare hostname stays either way.

### Changed

- A text body that is only bracketed emoji names becomes `[emoji]`. Words
  beside those names stay. A short recall notice becomes `[revoked]`. A longer
  sentence that mentions a recall stays text.
- `messages list` accepts up to 24 `--conversation` values in one database
  open, so one time slice can be read across chats. Each chat keeps its own
  page and cursor.

## 0.6.0 - 2026-09-29

### Changed

- `messages list` and `messages search` now default to a compact JSON Lines
  reading page. Each line names the sender, says whether it is the account
  holder, and uses local time. An image, video, or document includes a local
  path an agent can open. Voice without a transcript stays `[voice]`. `--json`
  still prints the full envelope. The default can also be set with
  `[output] format` in `~/.greenbubbles/config.toml`.
- `chats rank` prints the same kind of compact page, with a name on every chat,
  and continues with `--cursor`. Contact names are resolved in batches, so a
  long ranking no longer drops display names. `--json` prints the full report.
- Quote and unrecognized messages keep readable words when those words are
  present in the message. Identifiers and markup stay out of the reading page.

## 0.5.0 - 2026-09-29

### Changed

- Ordinary live queries no longer require a query profile. With no profile
  file, the CLI opens the newest installed WeChat `db_storage` directory and
  reads `~/.greenbubbles-acquire/passphrase.txt`. A different database or
  passphrase file goes in `~/.greenbubbles/config.toml`. Named profiles remain
  for extra accounts and snapshots.
- `greenbubbles` and `greenbubbles help` show the everyday commands. The
  complete list is `greenbubbles help --all`. `greenbubbles chats` lists
  conversations. `greenbubbles chats rank` orders chats by how many messages
  the account holder sent, with direct chats before groups. Query errors now
  say what to check and what to run next.
- The live default is the one WeChat account currently being written. A
  leftover account directory is ignored. Two accounts written within 14 days
  require `source.root` in `~/.greenbubbles/config.toml`. An untouched profile
  template no longer hides the live database.
- Replaced the project icon with a simpler conversation-and-memory mark.
- Shortened the README, clarified Homebrew skill paths and first-time setup,
  and corrected privacy boundaries, incremental-update guidance, and release status.

## 0.4.0 - 2026-09-29

### Added

- Portable setup, context, and personal-memory skills for existing coding-agent
  sessions, with optional discovery for Codex, Claude Code, OpenCode, Kimi Code,
  Gemini CLI, and Grok Build. Skills can be read directly without installation.
- A standalone skill bundle, conflict-safe optional installer, and read-only
  environment/profile doctor. Release CLI archives include the skills and scripts.
- An upstream Homebrew tap and release automation that verifies the signed CLI
  archive's checksum before updating the formula for the exact published tag.
- Documentation for using existing agent subscription allowances, embedded-agent
  API billing, capture/decryption setup, provenance, and incremental memory updates.

### Fixed

- Updated Rustls to 0.23.45 to address RUSTSEC-2026-0285.
- Release preflight catches a stale package version in the dependency inventory.
- Incremental memory runs retain their pending corpus and fixed time window on
  failure; checkpoints advance only after all planned shards complete.
- Domain-memory prompts and skill references follow the actual page,
  acknowledgement, and commit protocol, with correctly quoted CLI arguments.
- CLI help exposes domain formats and corpus extension, and documentation
  distinguishes structural validation from semantic/provenance review.

## 0.3.1 - 2026-09-05

### Added

- `tick` and `revise`: `--language` flag selects the output language for all
  extracted content — domain file text, field values, manifest summaries,
  constraint messages, and any prose. Free-form value: any natural language a
  model understands (`Chinese (Simplified)`, `English`, `日本語`, `Français`, …).
  When omitted on the first tick, the language is auto-detected from the OS
  locale (`LANGUAGE` / `LANG` / `LC_ALL` / `LC_MESSAGES` env vars, then
  `locale.getlocale()`). The resolved language is stored in
  `.greenbubbles-tick-state.json` so every subsequent tick and revise pass uses
  the same language without repeating the flag. `revise` reads the stored
  language from state and only falls back to OS locale when neither the flag
  nor state is present.
- `detect_os_language()`: internal helper that maps POSIX locale codes to
  human-readable language names; covered by 7 new unit tests.
- Tick agent prompt: **canonical domain names**. The driver now supplies a
  closed list of 12 fixed domain names (`identity`, `work`, `family`, `social`,
  `health`, `finance`, `travel`, `home`, `vehicles`, `education`,
  `entertainment`, `legal`) with per-domain scope definitions. All shards must
  use the exact canonical name — never synonyms. This prevents cross-shard
  fragmentation (e.g. `household` vs `home` vs `housing` landing in separate
  files). The agent may still create a new name when a fact genuinely falls
  outside all 12.
- `family` domain scope tightened to **people only** — spouse, parents,
  siblings, relatives, their relationships, health, and milestones. Household
  purchases, equipment, and logistics now route to `home` instead.

### Fixed

- Tick agent prompt: **sub-bullet State format**. Complex State entries — those
  with multiple facts, time periods, or source citations — must now use
  sub-bullets (`- **Field**:\n  - sub-fact *(source)*`) rather than one long
  single-line bullet. This prevents runaway line lengths in generated domain
  files while keeping the structure machine-readable.

## 0.3.0 - 2026-09-05

### Added

- `tick` and `revise` driver commands: `--language` selects the output language
  for all extracted content — domain file text, field values, manifest summaries,
  constraint messages, and prose. Free-form: any value a model understands works
  (`Chinese (Simplified)`, `English`, `日本語`, `Français`, …). When omitted on
  the first tick, the language is auto-detected from the OS locale (via the
  `LANGUAGE` / `LANG` / `LC_ALL` / `LC_MESSAGES` environment variables, then
  `locale.getlocale()`). The resolved language is persisted in
  `.greenbubbles-tick-state.json` so every subsequent tick and revise pass uses
  the same language automatically without repeating the flag. `revise` reads the
  stored language from state and only falls back to OS locale when neither the
  flag nor state is present.
- `detect_os_language()`: internal helper that maps POSIX locale codes to
  human-readable language names; covered by 7 new unit tests in
  `scripts/test_personal_memory_parallel.py`.
- Language prompt injection covered by 3 new unit tests (63 total).
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
- `scripts/test_personal_memory_parallel.py`: 53 unit tests over the driver's
  decision logic and project setup — the API-error retry classifier, the stderr
  snippet, timestamp comparison across timezones, scope splitting/fusing/
  packing, batch bounds, harness and model selection, the `tick` concurrency
  cap, the project lock, git init and commit, and the remote-privacy warning.
  Nothing in the suite launches an agent, reads a corpus, or makes a network
  request. CI runs it as its own step; it was previously only byte-compiled.
- `tick_parallelism()` extracted from `command_tick` so the concurrency cap is
  testable on its own. Behaviour is unchanged: always 1, and it says so.

### Fixed

- `THIRD_PARTY_NOTICES.md` regenerated against the current lockfile. The bundle
  had not been reproduced since before the live-AI and personal-memory work
  landed, so 43 shipped runtime crates carried no notice: the `ureq` HTTPS
  client `ai-summarize-direct` uses, its `rustls` / `rustls-webpki` /
  `webpki-roots` TLS stack, the ICU crates, `flate2`, `chrono-tz`, `url` and
  `log`. `CDLA-Permissive-2.0` is now an accepted license in `about.toml`,
  covering Mozilla's CA root store as redistributed by `webpki-roots`; its
  text and disclaimers ship in the bundle. This unblocks CI, which fails the
  notice-reproduction step, and with it the release workflow that gates on CI.
- `personal-memory` publication no longer renames a directory it has already
  made read-only. `protect_immutable_corpus_tree` sealed the staging root to
  `0500` and the next statement renamed it into place; Darwin refuses to rename
  a directory its owner cannot write, so preparation failed with
  `Permission denied` on that platform. The root is now sealed by
  `seal_published_corpus_root` immediately after the rename, at the final path,
  so the finalized tree is identical and the ordering no longer depends on
  platform rename semantics. The extend path was never affected: it finalizes
  writable and renames a `0700` root. Covered by
  `published_corpus_is_finalized_read_only`, which asserts the end state — root
  `0500`, every file `0400` — rather than the order it is reached in.
- `HistoryLiveMediaResolver` no longer reports a starved reader as a malformed
  response. Draining the local CLI's stdout had a fixed two-second budget, and
  on expiry it closed the pipe and returned whatever had arrived; the short
  buffer then failed to decode, so the caller saw `invalidResponse` rather than
  the truncation that actually happened. `finish()` runs only after the child
  has exited, so the reader has a bounded amount left to read — but a loaded
  machine can leave its thread unscheduled past two seconds, which is how this
  surfaced as an intermittent CI failure. The budget is now 30 seconds and a
  drain that does not reach end of file raises `requestTimedOut` instead of
  handing partial data to the decoder. The two sibling copies of this class,
  `DirectBoundedProcessStream` and `SnapshotBoundedProcessStream`, already
  waited for end of file and surfaced read errors; this brings the third into
  line.
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

### Documentation

- README: the memory-extraction section now states that `tick` sends message
  text to whichever model the chosen harness talks to, and that the prepared
  corpus can duplicate every eligible message. Its example ticked the old
  corpus straight after extending to a new one, and paired `--agent claude`
  with a sentence recommending Gemini 3.8 Flash, which that harness ignores
  without `--model`. Executable constraints are now attributed to the Python
  format only; the Markdown format's alerts are hand-maintained notes.
- `docs/PERSONAL_MEMORY.md`: the sharding section still showed
  `--shards 4 --parallel 4` running concurrently and claimed per-shard commits
  serialized overlapping writes. `tick` has capped `--parallel` at 1 since the
  write-race fix; the section now says so and explains what sharding still
  buys. Cost table no longer attributes the measured rate to a model it was
  not measured on.
- `docs/KNOWN_LIMITATIONS.md`: records what the UserAsCode project itself does
  not prove — agent-assigned domains, agent-detected contradictions and
  duplicates, constraints that cover only recorded state, single-agent
  throughput — and stops describing the extraction agent as Pi specifically.
- Both format references now show the `.gitignore` the driver actually writes,
  including the process lock and revise log.

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
