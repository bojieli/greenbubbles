# Contributing

GreenBubbles sits where personal data, closed and changing formats, local AI,
and desktop automation all meet. A good contribution makes that boundary more
understandable, more verifiable, or more conservative. It is fine — often
better — for a change to make the tool refuse to do something.

Contributions are welcome under the [MIT License](LICENSE). By submitting one
you agree it may be distributed under that license and confirm you have the
right to submit it. Do not submit code copied from another project, or material
whose redistribution rights are unclear.

**Security reports never go in an issue.** Follow [SECURITY.md](SECURITY.md).

## The most useful thing you can report

A message type, table, relationship or media variant that GreenBubbles reads
*incompletely*. Compatibility with a closed format is the work that never
finishes, and a structural description of something the decoder gets wrong is
worth more than almost any patch.

Describe it structurally: type codes, table shapes, column names, counts, what
the decoder produced versus what it should have. Attach **no** message content,
no identifiers, and no absolute paths.

## Before opening an issue

- Search existing issues and the [roadmap](docs/ROADMAP.md).
- Check [KNOWN_LIMITATIONS.md](docs/KNOWN_LIMITATIONS.md) and the
  [FAQ](docs/FAQ.md) — it may already be a documented constraint.
- Strip every message, name, account ID, database key, path, media file, and
  any hash derived from a private file.
- Reduce a format problem to a synthetic fixture or a structural description.
- State the commit, macOS version, architecture, WeChat version, the command,
  what you expected, and what happened.
- Say whether the source was live, a snapshot, or a synthetic fixture. **Do not
  upload the source.**

Most commands emit a content-free JSON report specifically so you have
something safe to paste. `swift scripts/check-live-database.swift` produces one
for the whole bounded read path.

## Development setup

macOS 14 or later, Swift 6, and Rust/Cargo. From the repository root:

```sh
git config core.hooksPath scripts/git-hooks

swift format lint --strict --recursive Package.swift Sources Tests
swift test
swift build -c release

swift scripts/check-distribution-inventory.swift
swift scripts/check-secret-hygiene.swift
swift scripts/check-pinned-build-profile.swift

cd Native/GreenBubbles
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo install cargo-audit --locked --version 0.22.2
cargo audit --file Cargo.lock
```

The first line installs the pre-commit secret guard. Do it once per clone. The
CI workflow is the final source of truth for what is required.

## Fixtures: synthetic only

Only synthetic, generated, or independently redistributable fixtures may enter
this repository. Never commit:

- real SQLite/WCDB pages, WAL frames, schemas copied from a real database, or
  database fragments;
- messages, contact data, media, Moments data, account identifiers, local
  paths, or stable digests of any of those;
- keys, passphrases, recovery words, signing seeds, cookies, session material,
  logs, crash reports, or memory captures;
- proprietary application binaries, extracted resources, disassembly, or
  decompiled source;
- a "sanitized" artefact, unless how it was generated and why it is
  redistributable are both documented.

Prefer a fixture *generator* that makes the interesting structure obvious over
a blob that happens to work. And test both the accepted case and the nearest
fail-closed cases — in this codebase the refusal is usually the feature.

## Changing a boundary

If a change touches privacy, authorization, acquisition, the connector, or the
send path, the pull request should say:

1. the exact owner intent, and the data or action scope;
2. the trust boundary, and which inputs are attacker-controlled;
3. fixed limits, authorization checks, and failure behaviour;
4. what is logged, persisted, disclosed, and deliberately omitted;
5. how source identity, freshness and partial coverage stay visible;
6. synthetic tests for tampering, ambiguity, retries and unsafe paths;
7. any new dependency, license, platform or distribution consequence.

One rule sits above the rest: **do not broaden a read path into acquisition, a
query path into bulk export, or a draft path into an action as an incidental
refactor.** Those three walls are the product. Widening one is a deliberate
change with its own review, never a side effect of cleaning something up.

## Pull requests

Keep them focused and lead with the user-visible outcome. Include the problem
and the solution, the safety and privacy impact, the tests you ran and anything
you deliberately left untested, documentation updates for changed commands,
formats or boundaries, and any dependency or notice changes.

Please also follow the [Code of Conduct](CODE_OF_CONDUCT.md).
