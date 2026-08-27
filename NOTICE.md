# Third-party components

The native restoration prototype depends on selected crates from
[`pandorafuture/wx-cli`](https://github.com/pandorafuture/wx-cli), pinned to
commit `2abe708f55bfe135539a385df856fdc58f97fc74` and used under its MIT license.
The dependency supplies independently tested WCDB/SQLCipher page and WAL
decryption, typed message decoding, and media-format primitives. GreenBubbles
adds its own immutable snapshot, lossless restoration, integrity, policy, and
AI-facing layers.

Transitive Rust and Swift dependencies retain their respective licenses. Run
`cargo metadata --manifest-path Native/GreenBubblesRestore/Cargo.toml` and
`swift package show-dependencies` to inspect the resolved dependency graphs.
