# Changelog

Notable changes to GreenBubbles are documented here. The project follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and intends to use
[Semantic Versioning](https://semver.org/).

## Unreleased

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
- Added a Pi-discoverable `greenbubbles-personal-memory` Agent Skill and
  project `.pi/settings.json` integration. Pi remains the default ReAct runtime
  and the one the examples use, but the skill is the whole contract: no
  extension, custom tool, or daemon is required of any agent that runs it.

### Fixed

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
