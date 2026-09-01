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
  workflow: owner-active monthly/session selection, selected-row hydration,
  immutable private evidence, deterministic `page`/`acknowledge` delivery below
  Pi's tool-output ceiling, citation/wiki validation including retained-to-cited
  completeness, weighted account-holder/active-month relevance ordering that
  still schedules every unit, an eight-citation representative-evidence ceiling
  on factual prose, self-authored support enforcement for every account-holder
  fact, explicit unchanged-wiki disposition for reviewed low-value
  batches, state-resolved `current` batch/page selectors that avoid copying
  opaque identifiers, idempotent commits, unambiguous last-committed status,
  cumulative committed/selected message and coverage status, and content-free
  preparation progress.
- Added a Pi-discoverable `greenbubbles-personal-memory` Agent Skill and
  project `.pi/settings.json` integration; Pi remains the only ReAct runtime.

### Fixed

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
