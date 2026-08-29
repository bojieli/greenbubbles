# GreenBubbles user guide

This guide covers the normal workflow: inspect storage, browse WeChat history
without restoring it, create an independently recoverable snapshot, and reopen
that snapshot later. For protector rotation and retention operations, see the
[recoverable snapshot operator guide](RECOVERABLE_SNAPSHOTS.md).
For repeated terminal queries without retyping source and credential options,
see the [query-profile guide](QUERY_PROFILES.md).

GreenBubbles is local software. The query path does not send messages, upload
history, or copy the full message corpus into JSON. It opens owner-authorized
SQLite/WCDB files read-only and returns one bounded page at a time.

Sending is a separate, owner-run path that ships closed and is never reachable
from a query or a tool call. If you want to enable it, read
[the send adapter guide](SEND_ADAPTER.md) first; `greenbubbles send
doctor` will tell you precisely why it is disabled and what each reason
requires.

## Choose a workflow

| Need | Recommended workflow |
|---|---|
| Browse current history on this Mac | History app → **Browse Live or Snapshot…** → **Live WeChat (read-only)** |
| Query live and snapshot history repeatedly from a terminal | Configure a private default and named profiles in `~/.greenbubbles/query-profiles.json` |
| See how much storage WeChat actually uses | Open the live source; read **SQLite files**, **WAL**, and **Total** in Overview |
| Keep a durable backup independent of WeChat | History app → **Create Recoverable Snapshot…** |
| Reopen a snapshot routinely on the same Mac | **Snapshot unlock in macOS Keychain** |
| Recover after losing this Mac, WeChat, or its key | A copied snapshot plus the separately stored 24-word recovery kit |
| Export the whole corpus for forensics or interchange | Use the explicit restoration/export workflow, not normal browsing |

The recommended arrangement is a portable 24-word recovery kit plus macOS
Keychain for routine access. A hidden local credential is available when
Keychain is unsuitable. Neither local convenience option replaces the words.

## Requirements

- macOS 14 or later;
- Swift 6 and Rust/Cargo installed;
- an owner-authorized WeChat `db_storage` directory;
- for encrypted live access, the matching 32-byte key or 64-character
  hexadecimal key;
- enough free space for a snapshot when creating one.

The source directory is the directory named `db_storage` that contains at least
the `contact`, `session`, and `message` subdirectories. Do not select the whole
WeChat container and do not select an individual `.db` file.

If the location is unknown, run this locally:

```sh
swift run greenbubbles-discover accounts --include-paths
```

Path-bearing output may contain a stable account identifier. Keep it private.
If the database key has not yet been acquired, follow the separate
[owner-authorized acquisition guide](PASSPHRASE_ACQUISITION.md). Never paste a
key, passphrase, or recovery phrase into a model prompt, issue, or chat.

## Build and launch

From the repository root, build the native query tool and the History app:

```sh
cargo build --release \
  --manifest-path Native/GreenBubbles/Cargo.toml
swift build --product greenbubbles-history
swift run greenbubbles-history
```

When the app asks for the local CLI, choose:

```text
Native/GreenBubbles/target/release/greenbubbles
```

The app may find a previously built debug CLI automatically. Selecting the
release binary explicitly gives better query performance.

## Browse the live database

1. Launch the History app and choose **Browse Live or Snapshot…**.
2. Choose the `greenbubbles` executable built above.
3. Set **Access** to **Live WeChat (read-only)**.
4. Choose the account's `db_storage` directory.
5. Enter the matching WeChat database key and choose **Connect**.

The key is written only to the local CLI's standard input for this connection.
It is not placed in process arguments or app preferences.

The app first authenticates the core databases, measures SQLite storage, and
loads up to 100 conversations. It does not start restoration. Use:

- **Overview** for source size and consistency information;
- **Chats** to load one 100-message page at a time;
- **Search** for bounded native FTS or a bounded, no-write fallback scan;
- **Load More** only when another page is needed.

Live reads are consistent within each SQLite statement and database. WeChat
uses several independent databases, so pages are not one global historical
instant. Use a recoverable snapshot when repeated queries must target a stable
generation.

## Understand the storage numbers

**SQLite files** is the sum of the actual `.db` files. **WAL** and **SHM** are
SQLite sidecars used by the live application. **Total** includes those files
and any rollback journals found under the selected root.

This number can be much smaller than an old restored output. A forensic
restoration may duplicate each row as typed JSON and raw provenance, Base64
encode binary values, build indexes, create a staging database, and eagerly
materialize media. Those derivatives can exceed 30 GB even when the source
SQLite files are only a few gigabytes. Normal GreenBubbles browsing creates
none of those corpus-sized derivatives.

## Create an independently recoverable snapshot

Use the graphical flow unless an automated CLI workflow is required:

1. Choose **File → Create Recoverable Snapshot…**.
2. Select the native CLI and the source `db_storage` directory.
3. Leave **Source is a complete stable acquisition capture** off for a normal
   live source. Turn it on only when the selected directory is a complete
   GreenBubbles acquisition snapshot with its acquisition manifest.
4. Choose a new snapshot directory and a new recovery-kit file. Neither output
   may already exist.
5. Keep **macOS Keychain** selected for convenient reopening, or choose an
   owner-only hidden credential file. **None** is valid when routine reopening
   will use the recovery kit or optional passphrase.
6. Optionally add a distinct snapshot passphrase. It is processed with Argon2id
   and does not replace the recovery words.
7. Choose **Create Recovery Words**.
8. Copy all 24 words, in order, to an independent offline or password-manager
   location. Never reuse a cryptocurrency wallet phrase.
9. Answer the four randomly selected word-position checks and confirm that an
   independent copy exists.
10. Choose **Confirm Words and Create Snapshot** and allow conversion to finish.

The recovery-kit file is created before the long conversion and is retained if
conversion is cancelled or fails. The snapshot receives a new random SQLCipher
key; the WeChat key is used only to read the source and is not copied into the
snapshot. Temporary destination databases are encrypted from their first
SQLite write—there is no plaintext SQLite staging database.

Do not store the only recovery-kit copy beside the only snapshot copy. A useful
backup requires both an intact snapshot generation and one working portable
recovery copy.

## Reopen a snapshot

Choose **Browse Live or Snapshot…**, select the snapshot directory, then choose
one access mode:

| Access mode | When to use it |
|---|---|
| **Snapshot unlock in macOS Keychain** | Routine access on the Mac that created the Keychain entry |
| **Snapshot hidden-file unlock** | Routine access with the separately stored owner-only local credential |
| **Snapshot passphrase (Argon2id)** | Access using the optional memorized passphrase |
| **Snapshot recovery words (portable)** | Recovery drill or recovery on another installation using the kit file |
| **Legacy snapshot raw key** | Compatibility with old format-1 snapshots only |

If a Keychain item or hidden file is lost, do not recreate it by guessing. Open
the same snapshot with the recovery-kit file, then create a new immutable
protector generation using the operator guide.

## Perform a portable recovery drill

Creation verifies the snapshot before reporting success. Also test the recovery
copy after moving the snapshot or kit to its intended backup location:

```sh
Native/GreenBubbles/target/release/greenbubbles \
  snapshot verify <snapshot-directory> \
  --snapshot-recovery-kit <owner-only-recovery-kit-file>
```

Success includes:

```json
{
  "recoveryVerifiedWithoutWechatKey": true,
  "independentOfWechatKey": true,
  "encryptedAtRest": true,
  "sqliteIntegrityVerified": true,
  "manifestHashesVerified": true
}
```

Verification reads no WeChat key. Repeat this drill after copying, protector
rotation, storage migration, or a significant backup-system change.

## CLI quick start

The graphical app uses the same commands. For repeated terminal use, configure
an owner-only [query profile](QUERY_PROFILES.md). With a default profile, the
common commands need neither a source path nor a key/passphrase option:

```sh
GB_CLI="Native/GreenBubbles/target/release/greenbubbles"

"$GB_CLI" profile validate
"$GB_CLI" source status
"$GB_CLI" conversations list --limit 100
"$GB_CLI" messages list \
  --conversation <conversation-id> --limit 100
"$GB_CLI" conversations list --profile archive --limit 100
```

The profile file stores paths and access modes. Live keys and snapshot
passphrases remain in separately referenced owner-only files; they are not
placed in the general JSON settings. A local snapshot credential or portable
recovery kit is already file-backed and can be referenced directly.

For one-off queries and scripts that intentionally supply every input, set
private paths in the shell session and keep the key in an owner-only file:

```sh
GB_CLI="Native/GreenBubbles/target/release/greenbubbles"
GB_SOURCE="<WeChat-db_storage-root>"
GB_KEY_FILE="<owner-only-WeChat-key-file>"
```

Inspect size and list the first page:

```sh
cat "$GB_KEY_FILE" | "$GB_CLI" \
  source status "$GB_SOURCE" --passphrase-stdin

cat "$GB_KEY_FILE" | "$GB_CLI" \
  conversations list "$GB_SOURCE" --passphrase-stdin --limit 100
```

Use a returned conversation `id` to page messages:

```sh
cat "$GB_KEY_FILE" | "$GB_CLI" \
  messages list "$GB_SOURCE" --passphrase-stdin \
  --conversation <conversation-id> --limit 100
```

Pass `page.nextCursor` back with `--cursor <opaque-cursor>` for the next page.
The hard maximum is 500 conversations/messages or 200 search hits per response.
There is deliberately no ordinary `--all` option.

Search text also belongs on standard input. Put sensitive query text in an
owner-only file so it does not enter shell history:

```sh
{
  cat "$GB_KEY_FILE"
  cat <owner-only-query-file>
} | "$GB_CLI" messages search "$GB_SOURCE" \
  --passphrase-stdin --query-stdin --limit 50
```

For snapshot queries, replace `--passphrase-stdin` with one of:

```text
--snapshot-local-credential <owner-only-file>
--snapshot-recovery-kit <owner-only-file>
--snapshot-passphrase-stdin
```

With a file-backed snapshot protector, only search text goes to standard input.
With `--snapshot-passphrase-stdin`, the passphrase is the first line and the
search text follows it.

With any configured profile, only search text goes to standard input; the CLI
loads the referenced credential separately. A positional source plus explicit
access mode is mutually exclusive with `--profile`.

## Common problems

### “Required database is unavailable”

The wrong directory was selected, or a required core file is missing. Select
the `db_storage` directory containing `contact/contact.db` and
`session/session.db`.

### The encrypted source cannot be opened

Confirm that the key belongs to the selected account and that its file contains
only the supported raw 32-byte or 64-hex-character value plus a final newline.
Failures are intentionally nondisclosing; GreenBubbles does not print the key
or database path in the JSON error.

### A recovery kit or local credential is rejected

The file and its parent must be owned by the current user. The file must be a
regular, single-link mode-`0600` file inside a mode-`0700` directory. Symbolic
links and group/world-readable secret files are rejected.

### A query profile is rejected

Run `greenbubbles profile validate <name>`. The configuration file,
credential file, and their containing directories must satisfy the same
owner-only rules. Paths in the JSON must be absolute, the named profile must
exist, and `defaultProfile` must name one of the configured profiles. Do not
combine `--profile` with an explicit source or access flag.

### A new snapshot path is rejected

Snapshot generations are immutable and never overwritten. Choose a new path in
an owner-only parent directory. Rewrap or rekey operations also publish a new
generation rather than modifying the old one.

### Search returns no hits but says `hasMore: true`

Native FTS was unavailable and the fallback scanned only its fixed source
window. Continue with `page.nextCursor`; an empty intermediate window is not a
claim that the remaining history has no match.

### A response reports incomplete coverage

Read the `warnings` array. An unreadable optional message shard can produce a
partial page with an opaque shard identifier. Do not infer that an absent
message was deleted or never existed. Retry after WeChat is idle or use a
verified recoverable snapshot.

## Safety checklist

- Keep WeChat keys, snapshot passphrases, local credentials, and recovery words
  out of command arguments, logs, prompts, issues, and version control.
- Keep the 24-word recovery copy separate from the only snapshot copy.
- Prefer Keychain or a hidden local credential for daily access; use the words
  for drills and disaster recovery.
- Verify a copied snapshot using only its portable recovery kit.
- Treat every published snapshot directory as immutable.
- Use bounded pages for browsing. Run a full export only when a full export is
  explicitly required.
