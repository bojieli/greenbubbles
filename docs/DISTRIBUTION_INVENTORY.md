# Dependency and distribution inventory

What the source and the resolved build actually contain, audited 2026-08-29.
This is a factual engineering record, not legal advice.

On 2026-08-29 the repository owner selected MIT and explicitly authorized
public source plus Developer ID signed, Apple-notarized macOS arm64 binaries.
That decision does not publish real user data, waive third-party terms, or
reach a legal conclusion about every use or jurisdiction.

## Current project status

- The repository is licensed under MIT. The native Cargo package declares
  `license = "MIT"` and remains `publish = false`; crates.io publication is not
  part of this release.
- `THIRD_PARTY_NOTICES.md` is the complete reviewed notice bundle for the
  macOS arm64 target. It is generated from the locked runtime graph and
  augmented with SQLCipher, SILK, Zstandard, and derived-code notices.
- `Native/GreenBubbles/about.toml` pins the target license policy and
  anchors the `wx-*` MIT clarification to the exact upstream LICENSE digest.
- Public release covers repository source, documentation, synthetic fixtures,
  hosted metadata, and the described binaries. It never authorizes publishing
  messages, databases, keys, media, captures, or other owner-private material.

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

`Package.swift` contains no external Swift package dependency. All three
libraries, all three executables, and their tests use only this repository's
Swift targets and Apple platform frameworks exposed by the SDK. This means
there is currently no third-party Swift package notice set, but Apple SDK
terms and the project's own license decision still apply to distribution.

The `greenbubbles-acquire` executable and its `GreenBubblesAcquire` library
implement the WeChat passphrase capture, PBKDF2 derivation, and SQLCipher4
HMAC verification ported from wcdb-key-tool
(https://github.com/TANGandXUE/wcdb-key-tool), which is MIT licensed. Each
derived source file carries a one-line attribution header; the port does not
copy upstream files into this repository, and distribution review must treat
it as MIT-licensed derived material.

## Rust dependency boundary

The restoration engine has 18 normal direct dependencies and one development
dependency. `Cargo.lock` is committed. At this audit the important direct
families are:

| Direct dependency | Resolved version | Cargo license metadata | Distribution-relevant role |
| --- | --- | --- | --- |
| `base64` | 0.22.1 | MIT OR Apache-2.0 | payload encoding |
| `hex` | 0.4.3 | MIT OR Apache-2.0 | identifiers and digests |
| `libc` | 0.2.189 | MIT OR Apache-2.0 | descriptor and permission primitives |
| `md5` | 0.7.0 | Apache-2.0/MIT | source-compatible media identifiers |
| `prost` | 0.13.5 | Apache-2.0 | protobuf decoding |
| `roxmltree` | 0.21.1 | MIT OR Apache-2.0 | bounded nested-message XML normalization |
| `rusqlite` | 0.40.2 | MIT | SQLite/SQLCipher access; native source is bundled |
| `serde`, `serde_json` | 1.0.229, 1.0.151 | MIT OR Apache-2.0 | structured records |
| `sha2` | 0.10.9 | MIT OR Apache-2.0 | integrity digests |
| `tempfile` | 3.27.0 | MIT OR Apache-2.0 | private temporary workspaces |
| `thiserror` | 2.0.20 | MIT OR Apache-2.0 | errors |
| `walkdir` | 2.5.0 | Unlicense/MIT | bounded media traversal |
| `zeroize` | 1.9.0 | Apache-2.0 OR MIT | secret cleanup |
| `zstd` | 0.13.3 | MIT | compressed private restoration-ordering spool |
| `wx-db`, `wx-decrypt`, `wx-media` | 0.7.4 at the revision below | absent on package records | pinned decoder primitives |
| `filetime` (development only) | 0.2.29 | MIT/Apache-2.0 | timestamp tests |

The locked graph contains 185 non-local package records across target and build
configurations. Cargo metadata reports permissive expressions for 179 and no
license value for the six pinned `wx-*` package records. License metadata is a
useful index, not proof that every bundled source file is covered by that one
expression; the native-source review below demonstrates why source inspection
is also necessary.

## Pinned `wx-cli` source

The three direct git dependencies and their two selected transitives resolve
from:

```text
repository: https://github.com/pandorafuture/wx-cli
commit:     2abe708f55bfe135539a385df856fdc58f97fc74
packages:   wx-context, wx-db, wx-decrypt, wx-media, wx-keychain, wx-paths
version:    0.7.4
```

At the exact checkout:

- the workspace declares `license = "MIT"` in `[workspace.package]`;
- the repository root contains an MIT license whose SHA-256 is
  `4d97412ef3e92a7f816240a39e5aae454dfe64c1b716e3702c21f64aa53e310e`;
- none of the six selected crate manifests declares `license.workspace =
  true` or its own `license`, so Cargo metadata reports their license as
  unknown.

The repository-level evidence resolves the metadata omission for this release.
The owner accepted the pinned packages under that MIT root license, and
`about.toml` makes the exact license digest a fail-closed build input.
GreenBubbles does not copy the upstream source into this repository; Cargo
fetches the exact pinned revision to build it.

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

### Zstandard staging compression

`wx-db` already selected `zstd`; GreenBubbles now also declares it directly for
the private, ephemeral restoration-ordering spool. The resolved Rust wrappers
report MIT (`zstd`) and MIT OR Apache-2.0 (`zstd-safe`), while `zstd-sys`
reports MIT/Apache-2.0 and compiles bundled Zstandard 1.5.7 C sources. Those
sources carry Meta's BSD license and require the copyright, conditions, and
disclaimer to accompany source or binary redistribution. The fail-closed
dependency inventory now tracks `zstd-sys` explicitly; a binary notice bundle
must include the bundled Zstandard notice.

## Publication categories

These are separate review units; approval of one does not approve the others.

| Category | Current contents or example | Public 0.1.1 decision |
| --- | --- | --- |
| GreenBubbles source | Swift/Rust implementation, scripts, CI | Publish under MIT. |
| Source build dependencies | pinned `wx-cli`, crates.io sources, bundled C | Publish lock/config references; preserve upstream terms and exact notices. |
| Prebuilt binaries | app DMG plus complete CLI archive | Publish macOS arm64 only after Developer ID signing, Apple Accepted verdicts, ticket verification, SBOM, and checksums. |
| Schema/format documentation | storage signatures, SQLCipher profile, row/type mappings | Publish as research documentation with the stated authorization and compatibility caveats. |
| Sanitized fixtures | generated databases and synthetic payloads | Publish only where repository provenance and privacy checks pass. |
| Real restored data or captures | messages, media, database fragments, absolute paths, IDs, digests | Never publish or commit; owner-private artifacts only. |
| Hosted repository metadata | commit history, issues, CI logs, release assets | Publish with private vulnerability reporting, issue hygiene, release holds, and takedown procedures. |
| Research evidence | redacted build fingerprints and aggregate measurements | Publish only the already reviewed content-free or aggregate evidence. |

The source/binary distinction is especially important: a source license notice
in an upstream checkout does not automatically produce the documentation and
notices required beside a statically linked binary.

## Release decision and remaining limits

The owner-approved public 0.1.1 boundary is deliberately narrower than the
project's research roadmap:

1. MIT applies to GreenBubbles; every third-party component retains its own
   terms and notices.
2. The six `wx-*` manifests' missing metadata is explicitly accepted only at
   the pinned MIT-licensed revision and license digest.
3. Binary approval is macOS arm64-specific. Another architecture or platform
   requires a fresh runtime graph, native-source review, notice generation,
   signing path, and clean-machine qualification.
4. The release does not endorse automated acquisition or sending. Acquisition
   remains advanced and owner-run; public builds ship the send path closed.
5. Real data and diagnostic artifacts remain private regardless of repository
   visibility.
6. Qualified legal review, Tencent permission, disposable-account evidence,
   and broader compatibility remain open risk and research items in
   [`ROADMAP.md`](ROADMAP.md), not
   claims implied by this research-alpha release.
