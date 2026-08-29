# Public release checklist

Status: **0.1.1 source and macOS arm64 binary release approved; hosted launch
requires the green Release workflow**

Last reviewed: 2026-08-29

This checklist records the repository owner's explicit public-release decision
and the mechanical gates that GitHub Actions must pass. It is not legal advice.
Public release never authorizes publishing real conversations, databases,
credentials, media, captures, or owner-private diagnostic artifacts.

## Approved 0.1.1 boundary

| Category | Decision |
| --- | --- |
| GreenBubbles source and documentation | Publish under MIT |
| Synthetic fixtures and aggregate/content-free research evidence | Publish after repository privacy checks |
| Native history app | Publish for macOS 14+, Apple silicon, as a signed/notarized/stapled DMG |
| Complete CLI/tool set | Publish for macOS 14+, Apple silicon, as a signed/notarized ZIP |
| Real user data and diagnostic artifacts | Never publish |
| Passphrase acquisition | Source is public; remains advanced, owner-run, invasive, and outside the AI boundary |
| Sending | Source is public; release binary ships cryptographically closed and dry-run only |
| Other architectures/platforms | Not approved by this target-specific review |

The project remains a research alpha. The owner's distribution decision does
not claim Tencent approval, permanent format compatibility, a sanctioned
acquisition route, or qualified legal advice for every jurisdiction.

## Source-release gates

- [x] The owner selected the MIT License and committed its full text.
- [x] Rust package metadata says `MIT`, version `0.1.1`, and retains
      `publish = false`.
- [x] README, contribution terms, security policy, distribution inventory,
      gate audit, roadmap, notices, and changelog reflect the public boundary.
- [x] The exact source, documentation, synthetic-fixture, acquisition-source,
      closed-send-source, hosted-metadata, and research-evidence categories have
      an owner decision.
- [x] The six pinned `wx-*` packages' missing manifest metadata is explicitly
      accepted at the exact MIT root-license digest and pinned commit only.
- [x] Repository files and all reachable history were scanned for common
      credentials and private-data artifacts; no candidate credential was
      accepted into the release.
- [x] Existing fixture rules allow only synthetic, generated, or independently
      redistributable content.
- [x] The existing personal author email in Git metadata is intentionally
      retained; no history rewrite is required.
- [x] CI has least-privilege permissions, full-SHA-pinned external Actions,
      strict formatting, tests, RustSec, secret hygiene, dependency drift, and
      closed-send checks.
- [x] CODEOWNERS, Dependabot, issue/PR templates, contribution guidance, and a
      code of conduct are present.
- [x] The repository owner is the release/security owner. Private vulnerability
      reporting, a three-business-day acknowledgement target, release holds,
      artifact revocation, and takedown procedures are defined.
- [ ] Immediately before visibility changes, withdraw the published unsigned
      `v0.1.0` prerelease so old assets cannot become public accidentally.
- [ ] Make the repository public, enable private vulnerability reporting, and
      configure the strongest branch rules available on the account plan.
- [ ] Record the exact approved commit and annotated `v0.1.1` tag in the hosted
      release.

## Binary-release gates

- [x] `THIRD_PARTY_NOTICES.md` contains the complete locked macOS arm64 runtime
      package inventory plus full SQLCipher, SILK, Zstandard, `wx-cli`, and
      `wcdb-key-tool`-derived notices.
- [x] `cargo-about 0.9.2` configuration and template are committed; CI can
      regenerate the notice bundle byte-for-byte.
- [x] The build-only `bindgen 0.59.2` advisory chain is recorded and accepted
      for this macOS target: two packages are unmaintained, and the unaligned
      read advisory is Windows-only. It is not part of the shipped runtime
      dependency notice graph.
- [x] The full tool archive is intentional: discovery, acquisition, history,
      article, send, input-helper, restoration, and change-consumer executables
      are present so source and binary surfaces agree. Release notes identify
      the advanced/closed components.
- [x] Valid Developer ID certificate and App Store Connect Notary credentials
      are stored as GitHub Actions Secrets. No credential is committed.
- [x] Release builds complete before signing credentials are imported.
- [x] The workflow imports only the release certificate into an ephemeral
      Keychain, makes it available to `codesign`, and always removes it.
- [x] Every shipped Mach-O and app bundle is signed with Developer ID,
      Hardened Runtime, and a secure timestamp, then verified for identity,
      Team ID, runtime flag, and strict signature validity.
- [x] The CLI ZIP and app DMG are separate Apple notarization submissions. A
      non-`Accepted` verdict stops publication and emits the Apple log.
- [x] Bare CLI tickets must resolve as `Notarized Developer ID`; the DMG must
      staple and validate successfully before release creation.
- [x] The workflow publishes SHA-256 checksums, a dependency SBOM, build
      provenance inside the app, full notices, and both Apple notarization logs.
- [x] The archived helper is rescanned for diagnostic bypass commands and the
      send trust root remains empty.
- [x] Supported architecture and minimum macOS version are explicit: Apple
      silicon (`arm64`) and macOS 14 or later.
- [ ] The green tagged Release workflow is the clean controlled-runner
      qualification for the exact release commit. Do not publish manually or
      override a failing job.
- [ ] Download the public assets and independently verify checksums, every
      signature, CLI ticket resolution, DMG staple, and Gatekeeper verdict.

## Release-candidate commands

Run from the repository root:

```sh
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
cargo install cargo-about --locked --features cli --version 0.9.2
cargo about generate --locked --fail -o ../../THIRD_PARTY_NOTICES.md about.hbs
```

Before tagging, also run:

```sh
bash scripts/check-public-release.sh
```

The final launch record must cite the exact commit/tag, main CI and Release run,
Apple submission IDs, public source/release URLs, checksums, signature identity,
notarization/staple verdicts, repository visibility, security intake state, and
the disposition of `v0.1.0`.
