# Dependency and distribution inventory

Audit date: 2026-08-27

Repository state: private; no public-release approval

This is a factual engineering inventory, not legal advice and not a license or
publication decision. It records what the current source and resolved build
contain so that the repository owner and qualified counsel can assess source,
binary, documentation, fixture, and hosted-repository distribution separately.

## Current project status

- The repository has no project-wide open-source license. The native Cargo
  package declares `license = "Proprietary"` and `publish = false`.
- `NOTICE.md` identifies important third-party components but is not a complete
  binary notice bundle.
- The repository must remain private until the owner selects a project license,
  the publication categories below are reviewed, and Phase 0.5's legal and
  distribution gate is explicitly passed.
- No conclusion in this document authorizes a public source repository, a
  downloadable binary, or distribution of WeChat-derived material.

## Reproducible boundary

Run this from the repository root:

```sh
swift scripts/check-distribution-inventory.swift
```

The checker derives the Swift package graph and locked Cargo graph, inspects the
resolved repository-level license for pinned git packages, and compares a
compact inventory with `docs/distribution-dependencies.json`. It fails closed
if a direct dependency declaration or resolved version, feature, pinned git
revision, git-repository license digest, selected native package, package
publication state, or unknown-license set changes.

`--print` emits the candidate inventory for review. A mismatch is not fixed by
blindly replacing the baseline: inspect the new source and bundled components,
update this document and notices as required, then deliberately update the
baseline. The check does not replace a full source-code/license scan or legal
review.

## Swift dependency boundary

`Package.swift` contains no external Swift package dependency. Both libraries,
both executables, and their tests use only this repository's Swift targets and
Apple platform frameworks exposed by the SDK. This means there is currently no
third-party Swift package notice set, but Apple SDK terms and the project's own
license decision still apply to distribution.

## Rust dependency boundary

The restoration engine has 16 normal direct dependencies and one development
dependency. `Cargo.lock` is committed. At this audit the important direct
families are:

| Direct dependency | Resolved version | Cargo license metadata | Distribution-relevant role |
| --- | --- | --- | --- |
| `base64` | 0.22.1 | MIT OR Apache-2.0 | payload encoding |
| `hex` | 0.4.3 | MIT OR Apache-2.0 | identifiers and digests |
| `libc` | 0.2.189 | MIT OR Apache-2.0 | descriptor and permission primitives |
| `md5` | 0.7.0 | Apache-2.0/MIT | source-compatible media identifiers |
| `prost` | 0.13.5 | Apache-2.0 | protobuf decoding |
| `rusqlite` | 0.40.2 | MIT | SQLite/SQLCipher access; native source is bundled |
| `serde`, `serde_json` | 1.0.229, 1.0.151 | MIT OR Apache-2.0 | structured records |
| `sha2` | 0.10.9 | MIT OR Apache-2.0 | integrity digests |
| `tempfile` | 3.27.0 | MIT OR Apache-2.0 | private temporary workspaces |
| `thiserror` | 2.0.20 | MIT OR Apache-2.0 | errors |
| `walkdir` | 2.5.0 | Unlicense/MIT | bounded media traversal |
| `zeroize` | 1.9.0 | Apache-2.0 OR MIT | secret cleanup |
| `wx-db`, `wx-decrypt`, `wx-media` | 0.7.4 at the revision below | absent on package records | pinned decoder primitives |
| `filetime` (development only) | 0.2.29 | MIT/Apache-2.0 | timestamp tests |

The locked graph contains 184 non-local package records across target and build
configurations. Cargo metadata reports permissive expressions for 179 and no
license value for the five pinned `wx-*` package records. License metadata is a
useful index, not proof that every bundled source file is covered by that one
expression; the native-source review below demonstrates why source inspection
is also necessary.

## Pinned `wx-cli` source

The three direct git dependencies and their two selected transitives resolve
from:

```text
repository: https://github.com/pandorafuture/wx-cli
commit:     2abe708f55bfe135539a385df856fdc58f97fc74
packages:   wx-db, wx-decrypt, wx-media, wx-keychain, wx-paths
version:    0.7.4
```

At the exact checkout:

- the workspace declares `license = "MIT"` in `[workspace.package]`;
- the repository root contains an MIT license whose SHA-256 is
  `4d97412ef3e92a7f816240a39e5aae454dfe64c1b716e3702c21f64aa53e310e`;
- none of the five selected crate manifests declares `license.workspace =
  true` or its own `license`, so Cargo metadata reports their license as
  unknown.

The repository-level evidence resolves the earlier question of whether the
checkout has an apparent license file: it does. The package metadata omission
remains a packaging ambiguity to review before distribution. GreenBubbles does
not copy the upstream source into this repository; Cargo fetches the exact
pinned revision to build it.

One further implementation observation matters to privilege review: selecting
`wx-media` also selects its `wx-keychain` and `wx-paths` code even though
GreenBubbles supplies the WeChat database passphrase only through standard
input and does not expose automated key acquisition. A future publication or
binary review must assess what is compiled and reachable, not infer the
GreenBubbles product boundary solely from upstream package names.

## Bundled native source and binary notices

The native build compiles more than Rust source:

### SQLCipher and SQLite

The direct `rusqlite` dependency enables `backup` and `bundled-sqlcipher`.
Selected upstream crates also enable `rusqlite`'s `bundled` feature. Cargo
feature unification therefore selects `libsqlite3-sys` 0.38.2 with bundled and
bundled-SQLCipher build features.

- `rusqlite` and `libsqlite3-sys` report MIT.
- The vendored SQLCipher source carries Zetetic's BSD-style license. Its terms
  require preservation of the copyright, conditions, and disclaimer in source
  redistribution and reproduction of them in documentation or other material
  accompanying a binary redistribution.
- The bundled SQLite portions are identified by the upstream package as public
  domain.
- The macOS build uses a system crypto provider for the selected
  `bundled-sqlcipher` feature; it does not select the separately named vendored
  OpenSSL feature. Binary linkage and platform-notice requirements must be
  rechecked on every intended release target.

### SILK audio decoder

The `audio` feature on `wx-media` selects `silk-rs` 0.2.0. The crate declares
MIT for its Rust wrapper and ships an MIT root license. It also compiles bundled
SILK C sources whose headers carry a separate Skype Limited 2006–2012
BSD-style notice, source/binary reproduction conditions, a non-endorsement
condition, and an explicit statement that no patent license is granted.

Consequently, a binary notice bundle cannot rely on Cargo's single `MIT`
metadata value for `silk-rs`. It must include the applicable bundled SILK
notice, and patent/supportability questions remain for qualified review. This
inventory makes no patent or distribution conclusion.

## Publication categories

These are separate review units; approval of one does not approve the others.

| Category | Current contents or example | Current status before public release |
| --- | --- | --- |
| GreenBubbles source | Swift/Rust implementation, scripts, CI | No project license selected; dependency and mechanism review required. |
| Source build dependencies | pinned `wx-cli`, crates.io sources, bundled C | Preserve upstream terms; resolve package metadata ambiguity and nested native notices. |
| Prebuilt binaries | CLI, restoration engine, service/MCP processes | Requires complete binary notice bundle, linkage review, target-specific inventory, and exact mechanism/legal approval. |
| Schema/format documentation | storage signatures, SQLCipher profile, row/type mappings | Assess separately under Phase 0.5; documentation may expose private implementation details even without code. |
| Sanitized fixtures | generated databases and synthetic payloads | Publish only with provenance proving generation or redistribution permission and a privacy scan. |
| Real restored data or captures | messages, media, database fragments, absolute paths, IDs, digests | Never publish or commit; owner-private artifacts only. |
| Hosted repository metadata | commit history, issues, CI logs, release assets | Requires a deliberate hosting decision and procedures for takedown, complaints, security reports, and accidental private-data exposure. |
| Research evidence | redacted build fingerprints and aggregate measurements | Review mechanism, contractual, privacy, and re-identification risk before publication. |

The source/binary distinction is especially important: a source license notice
in an upstream checkout does not automatically produce the documentation and
notices required beside a statically linked binary.

## Open items

This audit does not complete any legal/distribution checkbox in `PLAN.md`.
Before a public release, the remaining work includes:

1. repository-owner selection of a GreenBubbles license;
2. qualified review of the exact source, binary, schema, fixture, research, and
   hosting categories and intended jurisdictions;
3. resolution or explicit acceptance of the selected `wx-*` package metadata
   omission and the compiled `wx-keychain` boundary;
4. complete notice generation and verification for each binary target,
   including SQLCipher and bundled SILK terms;
5. fixture provenance and privacy review;
6. Tencent-route, mechanism-supportability, response-plan, and publication
   decisions required by Phase 0.5.

Until those items produce an explicit decision, the correct engineering state
is `private`, `publish = false`, and no downloadable binary.
