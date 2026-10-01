# Query profiles

**Most people don't need this page.** After you
[capture your key](PASSPHRASE_ACQUISITION.md), commands work with no setup:
they find your WeChat data on their own and read the key from
`~/.greenbubbles-acquire/passphrase.txt`.

```sh
greenbubbles chats
greenbubbles messages list --conversation "Alice"
```

Read on if you want to:

- use a key file saved somewhere else, or pick which WeChat account to read
  ([change the defaults](#change-the-defaults));
- read a second WeChat account or a backup by name
  ([named profiles](#named-profiles)).

## Change the defaults

Put simple settings in `~/.greenbubbles/config.toml`. It holds file paths only,
never a key:

```toml
[source]
root = "/absolute/path/to/db_storage"
passphrase_file = "/absolute/path/to/passphrase.txt"

[output]
format = "brief"
```

- **`root`**: which WeChat data folder to read. Leave it out to use the account
  WeChat wrote to most recently. If two accounts were both used in the last 14
  days, GreenBubbles won't guess; it asks you to set `root`.
- **`passphrase_file`**: where your key is. Leave it out to use
  `~/.greenbubbles-acquire/passphrase.txt`.
- **`format`**: `brief` (the default, one line per message) or `json` (full
  details). `--brief` or `--json` on a command overrides it for that command.

To make a named profile (below) the default instead, leave `[source]` out and
add:

```toml
[profile]
default = "archive"
```

## Named profiles

A named profile remembers a data folder and the file that unlocks it, so you
can read a second account or a backup without typing paths each time:

```sh
greenbubbles conversations list --profile archive
```

Profiles live in `~/.greenbubbles/query-profiles.json`. Run
`greenbubbles profile path` to print the exact location. (You can point the
`GREENBUBBLES_QUERY_PROFILES_FILE` environment variable at another absolute
path; the same permission checks apply.)

**Once `query-profiles.json` exists, commands without `--profile` use its
`defaultProfile`,** and the `[source]` settings in `config.toml` are ignored.

The profile file stores paths to credential files, never a key, passphrase, or
recovery phrase itself. So you can open and read it safely, and you can replace
a credential file without editing it.

### Set one up

1. Create private folders:

   ```sh
   install -d -m 700 "$HOME/.greenbubbles"
   install -d -m 700 "$HOME/.greenbubbles/credentials"
   ```

2. Write a starting template and open it in your editor. **This overwrites the
   file**, so skip it if you already have one you want to keep.

   ```sh
   umask 077
   greenbubbles profile template > "$HOME/.greenbubbles/query-profiles.json"
   chmod 600 "$HOME/.greenbubbles/query-profiles.json"
   ${EDITOR:-vi} "$HOME/.greenbubbles/query-profiles.json"
   ```

3. If the profile needs a key or passphrase you typed yourself, create an
   empty private file and type it in with your editor. That way the secret
   never appears on the command line or in your shell history.

   ```sh
   install -m 600 /dev/null "$HOME/.greenbubbles/credentials/wechat-database-key"
   ${EDITOR:-vi} "$HOME/.greenbubbles/credentials/wechat-database-key"
   ```

   A WeChat key file holds one 64-character hex value (or exactly 32 raw
   bytes). A passphrase file holds one line of 12 to 1,024 bytes. Recovery-kit
   and local-credential files are created by the backup commands; don't write
   those by hand.

4. Check that it works:

   ```sh
   greenbubbles profile validate
   ```

### Example file

Live WeChat as the default, plus a backup named `archive`:

```json
{
  "schema": "greenbubbles.query-profiles.v1",
  "formatVersion": 1,
  "defaultProfile": "live",
  "profiles": {
    "live": {
      "sourceRoot": "/Users/you/Library/Containers/.../db_storage",
      "access": {
        "mode": "liveWeChatKeyFile",
        "credentialFile": "/Users/you/.greenbubbles/credentials/wechat-database-key"
      }
    },
    "archive": {
      "sourceRoot": "/Volumes/Private Backups/WeChat/snapshot-2026-08-29",
      "access": {
        "mode": "snapshotLocalCredential",
        "credentialFile": "/Users/you/.greenbubbles/credentials/snapshot-local-credential"
      }
    }
  }
}
```

- `sourceRoot` and `credentialFile` must be full paths starting with `/`.
- Profile names can be up to 64 characters: letters, digits, `.`, `_`, `-`.
- Unknown fields are rejected, so a typo produces an error instead of being
  silently ignored.

`mode` says what kind of data the profile opens and what unlocks it:

| `mode` | Unlocked by | Use it for |
| --- | --- | --- |
| `liveWeChatKeyFile` | a file holding your WeChat key | live WeChat data |
| `snapshotLocalCredential` | the local credential file made with the backup | a backup, on the Mac that made it |
| `snapshotRecoveryKit` | the 24-word recovery-kit file | a backup on another Mac, or a restore test |
| `snapshotPassphraseFile` | a file holding the backup's passphrase | a backup you protected with a passphrase |
| `snapshotRawKeyFile` | a 32-byte key file | old format-1 backups only |
| `decrypted` | nothing | data that is already unencrypted |

For everyday access to a backup, use `snapshotLocalCredential`, and keep the
recovery kit somewhere else. If you lose this Mac, you shouldn't lose the
backup too.

### Manage profiles

None of these commands print the contents of a credential file:

```sh
greenbubbles profile list                 # list profiles
greenbubbles profile show live            # show one profile's settings
greenbubbles profile validate             # test the default profile
greenbubbles profile validate archive     # test a named profile
greenbubbles profile set-default archive  # change the default
```

`profile validate` really unlocks and opens the data (read-only) and reports
counts, so a pass means the profile works, not just that the file is valid.

### Use a profile

Leave out `--profile` to use the default, or name one:

```sh
greenbubbles conversations list --limit 100
greenbubbles messages list --conversation <conversation-id> --limit 100
greenbubbles source status --profile archive
```

For search, use `--query "text"`; the key comes from the profile. Use
`--query-stdin` when typing interactively or piping the query:

```sh
greenbubbles messages search --profile archive --query-stdin --limit 50 \
  < <owner-only-query-file>
```

Profiles apply only to reading commands, including `chats find` and `messages recent`: `source status`, `conversations list`,
`messages list`, `messages search`, and `message get`. They don't affect
creating or restoring backups, or anything else that writes.

## Without a profile

For scripts or a one-off folder, give the path and access method directly:

```sh
greenbubbles conversations list <source-root> --decrypted
cat <private-key-file> | greenbubbles conversations list \
  <source-root> --passphrase-stdin
```

GreenBubbles refuses mixtures that could read the wrong data by mistake:
`--profile` can't be combined with a folder path, and access options like
`--passphrase-stdin` need a folder path.

## File permissions

The profile file and every credential file must:

- be owned by you and be a regular file (not a symbolic link, one hard link);
- be readable only by you (normally mode `0600`);
- sit in a folder owned by you that only you can open (normally `0700`);
- stay under a size limit.

If a check fails, the command returns an `invalidProfile` error that never
includes a path, key, or credential contents.

Keep `query-profiles.json` private even though it holds no secret: its paths
show where your history and your keys are. Never commit it, or any credential
file, to version control.
