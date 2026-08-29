# Notices

GreenBubbles is Copyright (c) 2026 Bojie Li and is distributed under the MIT
License in [`LICENSE`](LICENSE).

The macOS binary distribution statically includes Rust dependencies and bundled
native source. The complete target-specific notices are reproduced in
[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md), including:

- the MIT-licensed `wx-cli` packages pinned to commit
  `2abe708f55bfe135539a385df856fdc58f97fc74` (Copyright (c) 2026
  pandorafuture);
- Zetetic's BSD-style SQLCipher notice;
- the MIT `silk-rs` wrapper notice and Skype Limited's separate SILK C-source
  notice, including its patent disclaimer;
- Meta's BSD notice for bundled Zstandard source;
- the MIT notice for acquisition code derived from
  `TANGandXUE/wcdb-key-tool` (Copyright (c) 2025 CloudDreamAI / TANGandXUE);
- the resolved Rust package inventory and every applicable upstream license
  text, preserving package-specific copyright statements.

`Native/GreenBubbles/about.toml` records the reviewed macOS arm64
license policy and fail-closed clarification for the `wx-*` package-metadata
omission. Regenerate the notice bundle with `cargo-about 0.9.2` and
`Native/GreenBubbles/about.hbs` whenever the locked dependency graph or
release target changes. The dependency boundary is independently checked by
`scripts/check-distribution-inventory.swift`.

GreenBubbles is an independent research project. It is not affiliated with,
endorsed by, or sponsored by Tencent or WeChat. WeChat and other product names
are trademarks of their respective owners.
