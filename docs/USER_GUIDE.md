# User guide

This guide walks you through the usual path:

1. Install GreenBubbles.
2. Browse and search your WeChat history.
3. Make a backup that still works if you lose WeChat or this Mac.
4. Open that backup again later.

Two things to know first:

- **Everything happens on your Mac.** Browsing and searching send nothing over
  the network. GreenBubbles only reads WeChat's files, never changes them, and
  shows one page of results at a time instead of copying your whole history.
- **GreenBubbles can't send messages.** Public builds have sending turned off.
  `greenbubbles send doctor` lists why, and [SEND_ADAPTER.md](SEND_ADAPTER.md)
  explains each reason.

**Never paste a key, passphrase, recovery phrase, or private message into an AI
prompt, a GitHub issue, or a chat.**

## What you need

- A Mac with Apple silicon and macOS 14 or later.
- WeChat for Mac, signed in to your own account.
- Your database key. If you don't have it yet, follow the
  [key setup guide](PASSPHRASE_ACQUISITION.md).
- Free disk space, if you plan to make a backup.
- Swift 6 and Rust, only if you build from source.

## Everyday browsing

After key setup, the CLI finds your account and saved key automatically:

```sh
greenbubbles chats --limit 20
greenbubbles chats find "Alice"
greenbubbles messages list --conversation "Alice" --limit 50
greenbubbles messages recent --limit 50
greenbubbles messages search --query "keyword"
```

Use a nickname, remark, alias, or exact ID for `--conversation`. Names match
case-insensitively; an ambiguous name shows the matching IDs so you can choose.
`chats find` also accepts partial names. `messages recent` shows the newest
messages across identifiable chats and includes the chat name and ID on each
line. Add `--json` for full message IDs and diagnostic warnings.

The defaults are 100 results for lists/recent messages and 50 search hits.
Use `--limit` for a smaller page and `--cursor` for older pages. To check new
activity, run `messages recent` again without a cursor. `--since <unix-seconds>`
limits the window; allow a small overlap and deduplicate message IDs if using
this for synchronization. See the [CLI reference](CLI_REFERENCE.md).

## Pick what you want to do

| I want to… | Do this |
| --- | --- |
| Browse my current history | App → **Browse Live or Snapshot…** → **Live WeChat (read-only)** |
| Search from a terminal | Run `greenbubbles chats` once the key is set up |
| Have my coding agent write notes from my chats | [Agent skills](AGENT_SKILLS.md) |
| See how much disk space WeChat really uses | Open your live history in the app and look at **Overview** |
| Make a backup that doesn't depend on WeChat | App → **File → Create Recoverable Snapshot…** |
| Open a backup day to day on this Mac | Unlock it with macOS Keychain |
| Recover after losing this Mac, WeChat, or the key | Your backup copy plus your 24-word recovery phrase, stored separately |
| Export everything for forensic work | The restoration workflow, not normal browsing |

The setup to aim for: **your 24-word recovery phrase stored somewhere else, plus
Keychain for everyday convenience.** Keychain is a convenience; it doesn't
replace the words.

## Install

**Command-line tool:** follow the [Homebrew guide](HOMEBREW.md). You don't
need Swift or Rust.

**Mac app:**

1. Download `GreenBubbles-<version>-macos-arm64.dmg` and its `SHA256SUMS` file
   from [GitHub Releases](https://github.com/bojieli/greenbubbles/releases).
2. Check the download as shown in
   [Check a download](HOMEBREW.md#check-a-download).
3. Open the DMG and drag **GreenBubbles** to Applications.

The app includes its own copy of the `greenbubbles` command-line tool and finds
it automatically.

**Build from source** (from the repository folder):

```sh
cargo build --locked --release --manifest-path Native/GreenBubbles/Cargo.toml
swift build --product greenbubbles-history
swift run greenbubbles-history
```

When the app asks for the command-line tool, choose
`Native/GreenBubbles/target/release/greenbubbles`. The app may find a debug
build on its own, but the release build is much faster.

## Find your WeChat data

WeChat keeps each account's chats in a folder named `db_storage`. It contains
at least `contact`, `session`, and `message` folders. When GreenBubbles asks
for your data, choose this folder: not the WeChat app folder above it, and not
a single `.db` file inside it.

To find it:

```sh
greenbubbles-discover accounts --include-paths
```

This comes with the command-line tool. From a source checkout, run it with
`swift run`. Keep its output private: the paths can contain your account ID.

## Browse your history

### From a terminal

Once the key is set up:

```sh
greenbubbles chats --limit 20
```

You don't need to give a folder or key. GreenBubbles opens the newest WeChat
data on your Mac and reads the key from
`~/.greenbubbles-acquire/passphrase.txt`.

### In the app

1. Choose **Browse Live or Snapshot…**.
2. Confirm the built-in `greenbubbles` tool, or choose the one you built.
3. Set **Access** to **Live WeChat (read-only)**.
4. Choose your `db_storage` folder (see [Find your WeChat data](#find-your-wechat-data)).
5. Enter the database key and click **Connect**. If you used the default key
   setup, the key is in `~/.greenbubbles-acquire/passphrase.txt`.

The app hands the key to the command-line tool privately for this connection
only. It isn't saved in the app's settings or visible in the list of running
programs.

The app checks the databases, measures their size, and loads up to 100 chats.
It doesn't copy anything. Then you can use:

- **Overview**: how big your WeChat data is, and whether it looks consistent;
- **Chats**: messages, 100 at a time;
- **Search**: search your messages;
- **Load More**: fetches the next page when you need it.

WeChat keeps several separate databases and keeps writing to them while it
runs. So a page of results reflects each database at the moment it was read,
not one single moment across all of them. If you need results that don't
change between queries, use a backup (snapshot) instead.

## Understanding the size numbers

In **Overview**:

- **SQLite files** is the size of WeChat's `.db` database files.
- **WAL** and **SHM** are temporary files WeChat keeps next to them while it
  runs.
- **Total** adds those together, plus any other journal files in the folder.

This total is often much smaller than older export tools produce, and that's
expected. A full export copies every row into JSON, re-encodes binary data,
builds indexes, and copies media. That can grow a few gigabytes of WeChat data
into more than 30 GB. Browsing with GreenBubbles creates none of that. The
numbers are in [MEASUREMENTS.md](MEASUREMENTS.md).

## Make a backup (recoverable snapshot)

A snapshot is an encrypted copy of your history that you can open later
without WeChat or its key. It is protected by 24-word recovery phrase.

1. Choose **File → Create Recoverable Snapshot…**.
2. Choose the command-line tool and your `db_storage` folder.
3. Leave **Source is a complete stable acquisition capture** off. Turn it on
   only if the folder is a complete GreenBubbles capture that includes its
   manifest file.
4. Choose a new folder for the snapshot and a new file for the recovery kit.
   Neither may exist yet.
5. Choose how to unlock the snapshot day to day:
   - **macOS Keychain**: easiest on this Mac;
   - **a hidden credential file**, readable only by you;
   - **None**, if you'll always use the recovery phrase or a passphrase.
6. Optionally add a passphrase you can remember. It is an extra way to unlock
   the snapshot; it does **not** replace the recovery phrase.
7. Click **Create Recovery Words**.
8. Write down or save all 24 words, in order, somewhere separate: on paper, or
   in a password manager. Never reuse a cryptocurrency wallet phrase.
9. Answer the four checks, which ask for words at random positions, and confirm
   you've saved a separate copy.
10. Click **Confirm Words and Create Snapshot** and wait for it to finish.

Good to know:

- The recovery kit is saved *before* the long copy begins, and is kept even if
  the copy fails or you cancel.
- The snapshot has its own new random key. Your WeChat key is used only to read
  WeChat's data and is never stored in the snapshot.
- The snapshot is encrypted from the very first byte written. No unencrypted
  copy exists at any point.

**Don't keep the only copy of your recovery kit next to the only copy of your
snapshot.** A real backup is a working snapshot *and* a copy of the recovery
words, kept in different places.

## Open a backup

Choose **Browse Live or Snapshot…**, choose the snapshot folder, and pick how
to unlock it:

| Access mode | When to use it |
| --- | --- |
| Snapshot unlock in macOS Keychain | Everyday use on the Mac that made the snapshot |
| Snapshot hidden-file unlock | Everyday use with your private credential file |
| Snapshot passphrase (Argon2id) | If you set a passphrase |
| Snapshot recovery phrase (portable) | Testing your recovery, or recovering on another Mac |
| Legacy snapshot raw key | Only for very old (format 1) snapshots |

**Lost the Keychain entry or the hidden file?** You can't recreate it; it was
a random key. Open the snapshot with your recovery phrase, then set up a new
unlock method as described in
[RECOVERABLE_SNAPSHOTS.md](RECOVERABLE_SNAPSHOTS.md).

## Test your recovery

GreenBubbles checks the snapshot when it creates it. But test it again after
you copy it to where you'll actually keep it:

```sh
greenbubbles snapshot verify <snapshot-folder> \
  --snapshot-recovery-kit <recovery-kit-file>
```

The recovery kit file must be readable only by you. A successful test shows:

```json
{
  "recoveryVerifiedWithoutWechatKey": true,
  "independentOfWechatKey": true,
  "encryptedAtRest": true,
  "sqliteIntegrityVerified": true,
  "manifestHashesVerified": true
}
```

The first line matters most: the snapshot opened without your WeChat key.
Test again whenever you copy the snapshot, change how it's unlocked, move it to
new storage, or change your backup system.

## Using the command line

The app runs the same commands you can run yourself. Once the key is set up,
you don't need to give a folder, profile, or key:

```sh
greenbubbles source status
greenbubbles conversations list --limit 100
greenbubbles messages list --conversation <conversation-id> --limit 100
```

If you built from source, use the full path
`Native/GreenBubbles/target/release/greenbubbles` instead of `greenbubbles`.

`messages list` and `messages search` print one line per message: who sent it,
whether it was you, the local time, and the text. Photos and files show a path
you can open. Phone numbers, email addresses, ID numbers, and links are shown
as-is; add `--redact` to hide them. `chats rank` lists your chats to help you
choose which ones matter. Add `--json` to see full details.

To read a backup or a second WeChat account, create a
[query profile](QUERY_PROFILES.md), then pass its name:

```sh
greenbubbles conversations list --profile archive --limit 100
```

A profile stores folder paths and how to unlock them. Keys and passphrases are
kept in separate private files, never in the profile itself.

To read one specific folder, keep its key in a private file and pass it on
standard input:

```sh
GB_SOURCE="<WeChat-db_storage-folder>"
GB_KEY_FILE="<private-WeChat-key-file>"

cat "$GB_KEY_FILE" | greenbubbles source status "$GB_SOURCE" --passphrase-stdin
cat "$GB_KEY_FILE" | greenbubbles conversations list "$GB_SOURCE" \
  --passphrase-stdin --limit 100
cat "$GB_KEY_FILE" | greenbubbles messages list "$GB_SOURCE" \
  --passphrase-stdin --conversation <conversation-id> --limit 100
```

Every command is listed in [CLI_REFERENCE.md](CLI_REFERENCE.md).

## When something goes wrong

Start with the [FAQ](FAQ.md). Slow search, missing contact names, searches that
find nothing, and pages that say coverage is incomplete are all covered there.
Most of them are known behavior, not bugs.

To test GreenBubbles against your own WeChat data and get a report that's safe
to share (it contains no personal content), run this from a source checkout:

```sh
swift scripts/check-live-database.swift
```

It builds the tools, finds your WeChat accounts, tries your key on each, and
checks that listing chats, reading messages, searching, and paging all work.
Useful options:

- `--key-file <path>`: use a key stored somewhere other than the default;
- `--skip-build`: don't rebuild, when you run it repeatedly;
- `--search-query-file <path>`: give it a word to search for, if it can't find
  one on its own.

Both files must be readable only by you, owned by you, and in a folder only you
can open.

The script prints one JSON report of counts and warning codes. It never
includes paths, account or message IDs, search words, or message text. It exits
with status 0 if every account it could open passed. It checks a sample, not
your whole history, and doesn't test backups, attachments, or the AI tools.

## Next steps

- [Give an AI access to some of your chats](AI_CONTEXT_CLI.md)
- [More about backups: changing unlock methods, keeping, and recovering](RECOVERABLE_SNAPSHOTS.md)
- [Known limitations](KNOWN_LIMITATIONS.md)
