# Third-party components

The native restoration prototype depends on selected crates from
[`pandorafuture/wx-cli`](https://github.com/pandorafuture/wx-cli), pinned to
commit `2abe708f55bfe135539a385df856fdc58f97fc74`. The upstream repository has an
MIT root license and declares MIT at the workspace level, but the five selected
package manifests do not inherit that field and consequently report an unknown
license through Cargo metadata. This discrepancy must be reviewed before
distribution. The dependency supplies independently tested WCDB/SQLCipher page
and WAL decryption, typed message decoding, and media-format primitives.
GreenBubbles adds its own immutable snapshot, lossless restoration, integrity,
policy, and AI-facing layers.

The resolved native build statically compiles bundled SQLCipher carrying a
Zetetic BSD-style license and bundled SILK C sources carrying a separate Skype
Limited BSD-style notice and patent disclaimer. Cargo reports the `silk-rs`
wrapper itself as MIT, which is not a complete description of its bundled C
sources. A public binary would require the applicable source/binary notices and
target-specific review.

Transitive Rust and Swift dependencies retain their respective licenses. Run
`cargo metadata --manifest-path Native/GreenBubblesRestore/Cargo.toml` and
`swift package show-dependencies` to inspect the resolved dependency graphs.
The reviewed factual inventory and fail-closed baseline are documented in
[`docs/DISTRIBUTION_INVENTORY.md`](docs/DISTRIBUTION_INVENTORY.md). This notice
is not yet a complete public binary notice bundle.
