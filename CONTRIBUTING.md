# Contributing to GreenBubbles

GreenBubbles sits at the intersection of personal data, closed and changing
formats, local AI, and desktop automation. Contributions should make that
boundary more understandable, verifiable, and conservative.

## Contribution status

Contributions are welcome under the project's [MIT License](LICENSE). By
submitting a contribution, you agree that it may be distributed under that
license and confirm that you have the right to submit it. Do not submit code
copied from another project or material whose redistribution rights are
unclear.

Security reports never belong in an issue. Follow
[SECURITY.md](SECURITY.md).

## Before opening an issue

- Search existing issues and the [roadmap](PLAN.md).
- Use the [user guide](docs/USER_GUIDE.md) for setup questions.
- Remove all messages, names, account IDs, database keys, paths, media, hashes
  derived from private files, and other identifying material.
- Reduce format problems to a synthetic fixture or a structural description.
- State the GreenBubbles commit, macOS version, architecture, WeChat version,
  command, expected behavior, and actual behavior.
- Say whether the source was live, a GreenBubbles snapshot, or a synthetic
  fixture. Do not upload the source.

## Development setup

GreenBubbles currently requires macOS 14 or later, Swift 6, and Rust/Cargo.
From the repository root:

```sh
git config core.hooksPath scripts/git-hooks

swift format lint --strict --recursive Package.swift Sources Tests
swift test
swift build -c release

swift scripts/check-distribution-inventory.swift
swift scripts/check-secret-hygiene.swift
swift scripts/check-pinned-build-profile.swift

cd Native/GreenBubblesRestore
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo install cargo-audit --locked --version 0.22.2
cargo audit --file Cargo.lock
```

The CI workflow is the final source of truth for required checks.

## Data and fixture rules

Only synthetic, generated, or independently redistributable fixtures may enter
the repository.

Do not commit:

- real SQLite/WCDB pages, WAL frames, schemas copied from a user's database, or
  database fragments;
- messages, contact data, media, Moments data, account identifiers, local
  paths, or stable digests of those artifacts;
- keys, passphrases, recovery words, signing seeds, cookies, session material,
  logs, crash reports, or memory captures;
- proprietary application binaries, extracted resources, disassembly, or
  decompiled source;
- a sanitized artifact unless its generation and redistribution provenance is
  documented.

Prefer a fixture generator that makes the important structure obvious. Tests
should prove both the accepted case and the nearest fail-closed cases.

## Design expectations

Changes to a privacy, authorization, acquisition, connector, or send boundary
should document:

1. the exact owner intent and data/action scope;
2. the trust boundary and attacker-controlled inputs;
3. fixed limits, authorization checks, and failure behavior;
4. what is logged, persisted, disclosed, and deliberately omitted;
5. how source identity, freshness, and partial coverage remain visible;
6. synthetic tests for tampering, ambiguity, retries, and unsafe paths;
7. any new dependency, license, platform, or distribution consequence.

Do not broaden a read path into acquisition, a query path into bulk export, or
a draft path into an action as an incidental refactor.

## Pull requests

Keep pull requests focused and explain the user-visible outcome first. Include:

- a concise problem and solution;
- safety and privacy impact;
- tests run and any intentionally untested real-world behavior;
- documentation updates for changed commands, formats, or boundaries;
- dependency and notice changes, when applicable.

By submitting a contribution, you confirm that you have the right to license it
to GreenBubbles under the MIT License.

Please also follow the [Code of Conduct](CODE_OF_CONDUCT.md).
