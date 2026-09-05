# Public release checklist

The 0.1.1 source and macOS arm64 binary release is approved; a hosted launch
still requires a green Release workflow. Last reviewed 2026-08-29.

The 0.3.0 release is approved on the same boundary, extended to cover the
personal-memory surface and the binary's first outbound network client. See
[0.3.0 review](#030-review) for what changed and what the owner decided.

This records the owner's explicit release decision and the mechanical gates CI
must pass. Unchecked boxes are things that have genuinely not happened yet, and
are left visible rather than tidied away.

It is not legal advice, and release **never** authorizes publishing real
conversations, databases, credentials, media, captures, or owner-private
diagnostic artefacts.

## 0.3.0 review

Reviewed 2026-09-05. 0.2.0 was tagged but never published: CI was failing at
the notice-reproduction step, and the release workflow gates on CI. 0.3.0 is
the first release carrying the personal-memory work.

What it adds beyond the 0.1.1 boundary:

- The personal-memory / UserAsCode extraction surface: `memory prepare`
  (including `--extend`), the corpus protocol, and
  `scripts/personal-memory-parallel.py`. A prepared corpus can duplicate every
  eligible message into its own index, and `tick` hands that text to whichever
  model the chosen coding-agent harness talks to. Both facts are now stated in
  the README section that introduces the feature, not only in PRIVACY.md.
- `ai-summarize-direct` and, with it, the first outbound HTTPS client in the
  shipped binary (`ureq` and its `rustls` stack). The connector boundary still
  decides what may reach a remote model; this is the client that carries it.
- The driver script. Its decision logic and project setup are unit tested in
  CI (`scripts/test_personal_memory_parallel.py`), but nothing tests an
  end-to-end extraction pass — that needs a real corpus and a real agent, so
  the tick/revise loop is exercised only by hand.

- [x] The owner extends the approved distribution boundary to 0.3.0, covering
      the personal-memory surface and the binary's outbound network client. The
      categories in the table below are otherwise unchanged, and sending still
      ships closed.
- [x] `CDLA-Permissive-2.0` is accepted for the shipped notice bundle. It
      covers Mozilla's CA root store as redistributed by `webpki-roots`, a
      permissive data license whose redistribution condition is that its text
      and disclaimers travel with the data; `THIRD_PARTY_NOTICES.md` carries
      both.
- [x] Version bumped to `0.3.0`, CHANGELOG entry dated, and the README install
      block updated to the assets `v0.3.0` will publish.
- [ ] Tag `v0.3.0` annotated on this commit and run
      `bash scripts/check-public-release.sh v0.3.0` against it.
- [ ] Confirm the tagged Release workflow is green, then independently verify
      the published assets as the binary-release gates below require.

## Approved 0.1.1 boundary

| Category | Decision |
| --- | --- |
| GreenBubbles source and documentation | Publish under MIT |
| Synthetic fixtures and aggregate/content-free research evidence | Publish after repository privacy checks |
| Native history app | Publish for macOS 14+, Apple silicon, as a signed/notarized/stapled DMG |
| Complete CLI/tool set | Publish for macOS 14+, Apple silicon, as a signed/notarized ZIP |
| Real user data and diagnostic artifacts | Never publish |
| Passphrase acquisition | Source is public; remains owner-run, root-requiring, and outside the AI boundary |
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
      roadmap, notices, and changelog reflect the public boundary.
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
- [x] The unsigned `v0.1.0` prerelease was withdrawn; only its tag remains and
      no release serves its assets. Verified 2026-09-05.
- [x] The repository is public, private vulnerability reporting is enabled, and
      `main` is protected: required `test` status check with strict
      up-to-date-ness, required conversation resolution, force-push and deletion
      blocked. Secret scanning and push protection are on. Verified 2026-09-05.
- [x] The hosted `v0.1.1` prerelease is built from the annotated `v0.1.1` tag on
      `main`. Verified 2026-09-05.

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
- [x] The app ZIP, CLI ZIP, and final app DMG are separate Apple notarization
      submissions. A non-`Accepted` verdict stops publication and emits the
      Apple log.
- [x] Bare CLI tickets must resolve as `Notarized Developer ID`; the DMG must
      staple and validate successfully before release creation.
- [x] The workflow publishes SHA-256 checksums, a dependency SBOM, build
      provenance inside the app, full notices, and all three Apple notarization
      logs.
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
python3 -m unittest discover -s scripts -p 'test_*.py'

cd Native/GreenBubbles
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
