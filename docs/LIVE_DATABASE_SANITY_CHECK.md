# Live database developer sanity check

`scripts/check-live-database.swift` is the repeatable developer check for the
bounded read-only CLI against an owner's real local WeChat databases. It never
creates or accepts a fixture database. It is deliberately not a hosted-CI job:
the command requires the current user's installed account storage and private
database key.

## Run it

From the repository root:

```sh
swift scripts/check-live-database.swift
```

By default the command:

1. builds the current release `greenbubbles` discovery executable;
2. builds the current release `greenbubbles-restore` executable with the locked
   Cargo graph;
3. securely opens `~/.greenbubbles-acquire/passphrase.txt`;
4. discovers readable installed-account `db_storage` roots through
   `greenbubbles accounts --include-paths`;
5. tries the key against every discovered source and checks every source that
   it authenticates.

Use another owner-only key file when necessary:

```sh
swift scripts/check-live-database.swift --key-file /private/key.txt
```

During local iteration, reuse already-built release binaries:

```sh
swift scripts/check-live-database.swift --skip-build
```

Run the command once per account key when installed accounts use different
keys. The command intentionally has no `--source`, `--decrypted`, snapshot,
archive, or replica option. A source enters the check only when installed-account
discovery returns it as a readable `db_storage` root.

## What it checks

For each live source authenticated by the supplied key, the command checks:

- content-free source status and the required contact, session, and message
  database families;
- a bounded conversation page, complete reported shard coverage, and
  non-overlapping cursor continuation when another page exists;
- bounded message lookup across up to 20 real conversations, complete reported
  shard coverage, and non-overlapping cursor continuation when available;
- exact hydration of one opaque list identity back to the same source message;
- a positive search using a private term derived from a decoded text message;
- exact hydration of one opaque search identity back to its source message;
- search cursor continuation when another result page exists.

No query is fabricated by default. If the bounded sample has no suitable text,
provide a private UTF-8 query that is expected to match the source:

```sh
swift scripts/check-live-database.swift \
  --search-query-file /private/owner-only-query.txt
```

Both the key file and optional query file must be current-user-owned regular
files with mode `0600`, one hard link, and an owner-only parent directory. Key
material and search text are supplied to the native CLI only through standard
input.

## Output and failure behavior

Standard output is one `greenbubbles.live-database-check.v1` JSON report. It
contains aggregate counts, consistency guarantees, coverage flags, and warning
codes. It omits source paths and identities, account IDs, conversation and
message IDs, keys, queries, snippets, and message content. Build and progress
messages go to standard error.

Exit status zero means every authenticated source completed the required live
checks, including a positive search hit and exact hit hydration. A missing or
stale native FTS hit is a failure even though ordinary interactive search
correctly reports its freshness as unverified; this stricter behavior is useful
for developer sanity validation. The JSON preserves
`nativeSearchIndexFreshnessUnverified` when the native search path returns it.

The command is bounded rather than exhaustive. It does not claim full-corpus,
line, branch, schema-variant, or cross-database atomic coverage. Snapshot,
attachment, connector, replica, export, and UI acceptance remain separate test
surfaces.
