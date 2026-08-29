# User guide

This is the normal path: find your data, browse it without restoring anything,
make a backup that survives losing WeChat, and reopen that backup later.

Two things to know before you start.

**GreenBubbles is local software.** The query path sends nothing, uploads
nothing, and does not copy your corpus into JSON. It opens your files read-only
and returns one bounded page at a time.

**Sending is a separate path and it ships closed.** It is never reachable from
a query or a tool call. If you want to know why, `greenbubbles send doctor`
will tell you exactly which conditions are unmet, and
[SEND_ADAPTER.md](SEND_ADAPTER.md) explains each one.

## What you need

- macOS 14 or later;
- Swift 6 and Rust/Cargo, if you are building from source;
- an owner-authorized WeChat `db_storage` directory;
- the matching 32-byte (or 64-hex-character) database key, for encrypted live
  access;
- free space for a snapshot, if you create one.

If you do not have the key yet, capture it first — three commands, about a
minute, in [PASSPHRASE_ACQUISITION.md](PASSPHRASE_ACQUISITION.md).

**Never paste a key, passphrase, recovery phrase or private message into a
model prompt, an issue, or a chat.**

## Which workflow do you want?

| If you want to… | Do this |
| --- | --- |
| Browse current history on this Mac | History app → **Browse Live or Snapshot…** → **Live WeChat (read-only)** |
| Query repeatedly from a terminal | Set up a [query profile](QUERY_PROFILES.md) |
| See how much space WeChat actually uses | Open the live source; read **SQLite files**, **WAL** and **Total** in Overview |
| Keep a backup independent of WeChat | History app → **Create Recoverable Snapshot…** |
| Reopen a snapshot routinely on this Mac | Snapshot unlock via macOS Keychain |
| Recover after losing this Mac, WeChat, or its key | A copied snapshot plus your separately stored 24 words |
| Export the whole corpus for forensics | The explicit restoration workflow, not normal browsing |

The arrangement to aim for is **24 words stored somewhere else, plus Keychain
for daily convenience.** Neither local convenience option replaces the words.

## Find your data

The source is the directory named `db_storage` that contains at least
`contact`, `session` and `message`. Do not select the WeChat container, and do
not select an individual `.db` file.

If you do not know where it is:

```sh
swift run greenbubbles-discover accounts --include-paths
```

Path-bearing output may contain a stable account identifier — keep it private.

## Install or build

For the signed release, download the `GreenBubbles-*-macos-arm64.dmg` and its
checksum file from [GitHub Releases](https://github.com/bojieli/greenbubbles/releases),
verify them as shown in the [repository README](../README.md#install), open the
disk image, and copy **GreenBubbles** to Applications. The packaged app finds
the `greenbubbles` CLI inside its own bundle automatically.

To build from source, from the repository root:

```sh
cargo build --release --manifest-path Native/GreenBubbles/Cargo.toml
swift build --product greenbubbles-history
swift run greenbubbles-history
```

When the app asks for the local CLI, choose
`Native/GreenBubbles/target/release/greenbubbles`. It may auto-detect a debug
build; pointing it at the release binary explicitly is noticeably faster.

## Browse live history

1. **Browse Live or Snapshot…**
2. Confirm the bundled `greenbubbles` executable, or choose the one you built
   from source.
3. Set **Access** to **Live WeChat (read-only)**.
4. Choose the account's `db_storage` directory.
5. Enter the database key and **Connect**.

The key goes to the CLI's standard input for that connection only. It is not
placed in process arguments or in app preferences.

The app authenticates the core databases, measures storage, and loads up to 100
conversations. It does not start a restoration. From there:

- **Overview** — source size and consistency information;
- **Chats** — one 100-message page at a time;
- **Search** — bounded native FTS, or the bounded no-write fallback;
- **Load More** — only when you actually need the next page.

Live reads are consistent within each statement and each database. WeChat uses
several independent databases, so a page is not one global instant. When
repeated queries need a stable target, use a snapshot generation.

## Reading the storage numbers

**SQLite files** is the sum of the actual `.db` files. **WAL** and **SHM** are
the sidecars the live application uses. **Total** adds those plus any rollback
journals under the selected root.

That figure is often far smaller than an old restored output, and the
difference is not an error. A forensic restoration duplicates every row as
typed JSON plus raw provenance, base64-encodes binary values, builds indexes,
creates a staging database and materializes media. Those derivatives can exceed
30 GB from a source of a few gigabytes. Normal browsing creates none of them —
that is the whole point of the design, and the numbers behind it are in
[MEASUREMENTS.md](MEASUREMENTS.md).

## Create a recoverable snapshot

Use the graphical flow unless you need automation.

1. **File → Create Recoverable Snapshot…**
2. Select the native CLI and the source `db_storage` directory.
3. Leave **Source is a complete stable acquisition capture** off for a normal
   live source. Turn it on only when the directory is a complete GreenBubbles
   acquisition snapshot with its manifest.
4. Choose a new snapshot directory and a new recovery-kit file. Neither may
   already exist.
5. Keep **macOS Keychain** for convenient reopening, or choose an owner-only
   hidden credential file. **None** is valid if you will always use the
   recovery kit or a passphrase.
6. Optionally add a snapshot passphrase. It is processed with Argon2id and does
   **not** replace the recovery words.
7. **Create Recovery Words.**
8. Copy all 24 words, in order, somewhere independent — offline or in a
   password manager. Never reuse a cryptocurrency wallet phrase.
9. Answer the four randomly chosen word-position checks and confirm an
   independent copy exists.
10. **Confirm Words and Create Snapshot**, and let the conversion finish.

The recovery kit is written *before* the long conversion and is kept even if
conversion is cancelled or fails. The snapshot gets a new random SQLCipher key;
the WeChat key is used only to read the source and never lands in the snapshot.
Destination databases are encrypted from their first write — there is no
plaintext staging database at any point.

**Do not store the only copy of the recovery kit beside the only copy of the
snapshot.** A working backup is an intact snapshot generation *and* one
reachable portable recovery copy, in different places.

## Reopen a snapshot

**Browse Live or Snapshot…**, select the snapshot directory, then pick an
access mode:

| Access mode | When to use it |
| --- | --- |
| Snapshot unlock in macOS Keychain | Routine access on the Mac that created the entry |
| Snapshot hidden-file unlock | Routine access with the separately stored owner-only credential |
| Snapshot passphrase (Argon2id) | Access with the optional memorized passphrase |
| Snapshot recovery words (portable) | A recovery drill, or recovery on another installation |
| Legacy snapshot raw key | Format-1 snapshots only |

Lost the Keychain item or hidden file? Do not try to reconstruct it — it was a
random key. Open the snapshot with the recovery kit and create a new protector
generation, as described in [RECOVERABLE_SNAPSHOTS.md](RECOVERABLE_SNAPSHOTS.md).

## Run a recovery drill

Creation verifies the snapshot before reporting success, but that verification
happened where the snapshot was born. Test the copy again once it reaches its
actual backup location:

```sh
greenbubbles snapshot verify <snapshot-directory> \
  --snapshot-recovery-kit <owner-only-recovery-kit-file>
```

A passing drill reports:

```json
{
  "recoveryVerifiedWithoutWechatKey": true,
  "independentOfWechatKey": true,
  "encryptedAtRest": true,
  "sqliteIntegrityVerified": true,
  "manifestHashesVerified": true
}
```

The first line is the one that matters: it opened with no WeChat key at all.
Repeat the drill after copying the snapshot, rotating protectors, migrating
storage, or changing your backup system.

## The command line

The app runs the same commands you can. For repeated terminal use, set up an
owner-only [query profile](QUERY_PROFILES.md) so you stop retyping a source
path and re-supplying a key:

```sh
GB_CLI="Native/GreenBubbles/target/release/greenbubbles"

"$GB_CLI" profile validate
"$GB_CLI" source status
"$GB_CLI" conversations list --limit 100
"$GB_CLI" messages list --conversation <conversation-id> --limit 100
"$GB_CLI" conversations list --profile archive --limit 100
```

The profile stores paths and access modes. Keys and passphrases stay in
separately referenced owner-only files, never in the general JSON settings.

Without a profile, supply everything explicitly and keep the key in a file:

```sh
GB_CLI="Native/GreenBubbles/target/release/greenbubbles"
GB_SOURCE="<WeChat-db_storage-root>"
GB_KEY_FILE="<owner-only-WeChat-key-file>"

cat "$GB_KEY_FILE" | "$GB_CLI" source status "$GB_SOURCE" --passphrase-stdin
cat "$GB_KEY_FILE" | "$GB_CLI" conversations list "$GB_SOURCE" \
  --passphrase-stdin --limit 100
cat "$GB_KEY_FILE" | "$GB_CLI" messages list "$GB_SOURCE" \
  --passphrase-stdin --conversation <conversation-id> --limit 100
```

The full surface is in [CLI_REFERENCE.md](CLI_REFERENCE.md).

## When something is wrong

Start with the [FAQ](FAQ.md) — slow search, missing contact names, a search
that finds nothing, or a page that reports incomplete coverage are all covered
there, and most of them are documented behaviour rather than faults.

To check the bounded CLI against your own real databases and get a
content-free report you can safely share:

```sh
swift scripts/check-live-database.swift
```

It builds the release binaries, discovers readable accounts, tries your key
against each, and for every source it authenticates verifies source status, a
bounded conversation page, message lookup across up to 20 conversations, exact
hydration of both a list identity and a search identity, and cursor
continuation. Useful flags: `--key-file <path>` for a key elsewhere,
`--skip-build` during iteration, and `--search-query-file <path>` when the
bounded sample contains no suitable text to search for. Both files must be
mode-`0600`, single-link, current-user-owned, in an owner-only directory.

Its output is one JSON report with aggregate counts, coverage flags and
warning codes — no paths, account IDs, conversation or message IDs, queries,
snippets or content. Exit status zero means every authenticated source passed,
including a positive search hit and its exact hydration. The check is bounded,
not exhaustive: it makes no claim about full-corpus, schema-variant or
cross-database coverage, and snapshot, attachment, connector and replica
behaviour are separate test surfaces.

## Next

- [Give an AI access to some of this](AI_CONTEXT_CLI.md)
- [Snapshot operations: rotation, retention, recovery](RECOVERABLE_SNAPSHOTS.md)
- [What is not proven yet](KNOWN_LIMITATIONS.md)
