# GreenBubbles

GreenBubbles is an experimental, local-first bridge for making a user's own
WeChat data available to narrowly scoped AI tools on macOS.

The project begins with passive, read-only discovery. It does **not** currently
decrypt databases, inject code into WeChat, call private network APIs, or send
messages.

## Current milestone

The first slice provides a Swift command-line tool that:

- discovers known WeChat application and sandbox locations on macOS;
- inventories likely databases, SQLite sidecars, indexes, and media by metadata;
- redacts filesystem paths by default;
- supports synthetic test roots so format research never needs live user data;
- never opens or modifies candidate artifacts.

See [PLAN.md](PLAN.md) for the phased roadmap and safety gates.

## Build and test

```sh
swift build
swift test
swift run greenbubbles discover
swift run greenbubbles inventory
```

`inventory` reports opaque path identifiers by default. For local debugging,
`--include-paths` may be used explicitly. Do not paste that output into issues
or logs because paths can contain stable account identifiers. Opaque identifiers
are stable hashes intended for correlation, not a substitute for access control.

```sh
swift run greenbubbles inventory --include-paths
swift run greenbubbles inventory --root /path/to/synthetic/fixture
```

## Scope and authorization

Use GreenBubbles only with data and accounts you own or are explicitly
authorized to access. Group chats contain other people's data even when the
database belongs to the local user. The connector must enforce per-conversation
consent and data minimization before any model integration is enabled.

This repository is private and no open-source license has been selected yet.
No permission to redistribute the code is granted until a license is added.
