# Reusable CLI query profiles

Query profiles let the bounded read-only CLI remember which SQLite source to
open and how to unlock it. After one local setup, ordinary commands become:

```sh
greenbubbles source status
greenbubbles conversations list
greenbubbles messages list --conversation <id>
greenbubbles messages search --query-stdin
greenbubbles conversations list --profile archive
```

The existing explicit form remains available for scripts and one-off sources:

```sh
greenbubbles conversations list <source-root> --decrypted
cat <private-key-file> | greenbubbles conversations list \
  <source-root> --passphrase-stdin
```

Profiles affect only bounded query commands: `source status`, `conversations
list`, `messages list`, `messages search`, and `message get`. They do not change
restoration, snapshot creation, or mutation behavior.

## Where the settings live

The default file is:

```text
~/.greenbubbles/query-profiles.json
```

Use `greenbubbles profile path` to print the effective location. An
advanced installation may set `GREENBUBBLES_QUERY_PROFILES_FILE` to another
absolute path. The selected file is still subject to all ownership and
permission checks.

The configuration file stores source paths and credential-file references. It
does **not** store a raw WeChat key, raw snapshot key, passphrase, or 24-word
recovery phrase. Keeping location settings separate from secrets makes the
configuration safe to inspect and lets credentials be rotated independently.

## Create the private files

Create owner-only directories before writing either settings or credentials:

```sh
install -d -m 700 "$HOME/.greenbubbles"
install -d -m 700 "$HOME/.greenbubbles/credentials"
```

For a first-time configuration, emit the strict template under a private
umask, then edit only the placeholder paths and modes you need:

```sh
umask 077
greenbubbles profile template \
  > "$HOME/.greenbubbles/query-profiles.json"
chmod 600 "$HOME/.greenbubbles/query-profiles.json"
${EDITOR:-vi} "$HOME/.greenbubbles/query-profiles.json"
```

Do not run the redirection over a configuration you intend to keep. Use
`profile show`, `profile list`, or your editor for an existing file.

For a live WeChat key or snapshot passphrase, create an empty private file and
enter the value with a local editor. This keeps the value out of process
arguments and shell history:

```sh
install -m 600 /dev/null \
  "$HOME/.greenbubbles/credentials/wechat-database-key"
${EDITOR:-vi} "$HOME/.greenbubbles/credentials/wechat-database-key"
```

A live-key file accepts one 64-character hexadecimal value or exactly 32 raw
bytes, with an optional final line ending. A snapshot-passphrase file accepts
one UTF-8 line of 12 through 1,024 bytes. Recovery-kit and local-credential
files must be created by the snapshot commands; do not rewrite their formats.

## Configuration format

This example makes the live WeChat database the default and adds a recoverable
snapshot named `archive`:

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

Every `sourceRoot` and `credentialFile` must be an absolute path. Profile names
use at most 64 ASCII letters, digits, periods, underscores, or hyphens. Unknown
JSON fields and unsupported schema versions are rejected instead of ignored.

Supported access objects are:

| `mode` | Credential | Intended source |
|---|---|---|
| `liveWeChatKeyFile` | `credentialFile` containing the 32-byte WeChat key | Live encrypted `db_storage` |
| `snapshotLocalCredential` | GreenBubbles local-credential file | Recoverable snapshot on the same installation |
| `snapshotRecoveryKit` | GreenBubbles 24-word recovery-kit file | Portable recovery or a recovery drill |
| `snapshotPassphraseFile` | One-line passphrase file | Snapshot with an Argon2id protector |
| `snapshotRawKeyFile` | 32-byte key file | Legacy format-1 snapshot only |
| `decrypted` | No credential field | Explicit plaintext SQLite source |

For routine snapshot access, prefer `snapshotLocalCredential`. Keep the
portable recovery kit separately so loss of this Mac or its local credential
does not make the backup unreadable.

## Inspect and validate profiles

The management commands never print credential contents:

```sh
greenbubbles profile list
greenbubbles profile show live
greenbubbles profile validate
greenbubbles profile validate archive
greenbubbles profile set-default archive
```

`profile validate` loads the selected private credential, opens the required
SQLite databases read-only, and returns content-free database counts and byte
totals. `set-default` atomically rewrites the existing configuration with mode
`0600`; it does not alter any source or credential file.

## Query with the default or another profile

Omit both the source and access arguments to use `defaultProfile`:

```sh
greenbubbles conversations list --limit 100
greenbubbles messages list \
  --conversation <conversation-id> --limit 100
```

Choose another configured source with `--profile`:

```sh
greenbubbles source status --profile archive
greenbubbles conversations list --profile archive
```

For search, stdin contains only the search text because the profile credential
is read from its separate file:

```sh
greenbubbles messages search \
  --profile archive --query-stdin --limit 50 \
  < <owner-only-query-file>
```

GreenBubbles deliberately rejects ambiguous combinations. `--profile` cannot
be combined with a positional source root, and explicit access flags cannot be
used without an explicit source root. This prevents a typo from querying the
wrong live or archived database.

## Permission and disclosure rules

The configuration file and every credential file must be:

- owned by the current user;
- a regular file with exactly one hard link;
- inaccessible to group and other users (normally mode `0600`);
- inside a current-user-owned real directory inaccessible to group and other
  users (normally mode `0700`);
- a real path rather than a symbolic link; and
- within its fixed input-size limit.

Query failures return a stable `invalidProfile` JSON error without printing a
source path, key, passphrase, or credential content. Profile management output
may show the configured source and credential **paths**, but never reads a
secret into that output.

Treat `query-profiles.json` as private even though it contains no secret value:
its paths reveal where personal history and unlock material are stored. Do not
commit it or any credential file to version control.
