# Query profiles

Typing a 200-character source path and re-supplying a key for every query is
how people end up leaving a key in shell history. A profile stores the boring
half — which source, and which credential file unlocks it — so ordinary
commands become:

```sh
greenbubbles source status
greenbubbles conversations list
greenbubbles messages list --conversation <id>
greenbubbles messages search --query-stdin
greenbubbles conversations list --profile archive
```

The explicit form still works, for scripts and one-off sources:

```sh
greenbubbles conversations list <source-root> --decrypted
cat <private-key-file> | greenbubbles conversations list \
  <source-root> --passphrase-stdin
```

Profiles affect only the bounded query commands — `source status`,
`conversations list`, `messages list`, `messages search` and `message get`.
They change nothing about restoration, snapshot creation, or anything that
writes.

## Settings and secrets stay separate

The default file is `~/.greenbubbles/query-profiles.json`
(`greenbubbles profile path` prints the effective location; an advanced
installation can point `GREENBUBBLES_QUERY_PROFILES_FILE` at another absolute
path, still subject to every ownership and permission check).

**The configuration stores paths and credential-file references. It never
stores a raw WeChat key, a raw snapshot key, a passphrase, or recovery words.**
That split is what lets you inspect the configuration freely and rotate a
credential without touching it.

## Set it up

Owner-only directories first:

```sh
install -d -m 700 "$HOME/.greenbubbles"
install -d -m 700 "$HOME/.greenbubbles/credentials"
```

Emit the strict template under a private umask, then edit only the placeholders
you need:

```sh
umask 077
greenbubbles profile template > "$HOME/.greenbubbles/query-profiles.json"
chmod 600 "$HOME/.greenbubbles/query-profiles.json"
${EDITOR:-vi} "$HOME/.greenbubbles/query-profiles.json"
```

Do not run that redirection over a configuration you want to keep — use
`profile show`, `profile list`, or your editor on an existing file.

For a live key or a snapshot passphrase, create an empty private file and type
the value into it with an editor. This is the point: the value never becomes a
process argument or a shell-history line.

```sh
install -m 600 /dev/null "$HOME/.greenbubbles/credentials/wechat-database-key"
${EDITOR:-vi} "$HOME/.greenbubbles/credentials/wechat-database-key"
```

A live-key file holds one 64-character hex value or exactly 32 raw bytes, with
an optional trailing newline. A passphrase file holds one UTF-8 line of
12–1,024 bytes. Recovery-kit and local-credential files are created by the
snapshot commands — do not hand-write their formats.

## The file

Live WeChat as the default, plus a snapshot named `archive`:

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

Every `sourceRoot` and `credentialFile` must be absolute. Profile names are up
to 64 ASCII letters, digits, periods, underscores or hyphens. Unknown JSON
fields and unsupported schema versions are rejected rather than ignored.

| `mode` | Credential | For |
| --- | --- | --- |
| `liveWeChatKeyFile` | file holding the 32-byte WeChat key | live encrypted `db_storage` |
| `snapshotLocalCredential` | local-credential file | a snapshot on this installation |
| `snapshotRecoveryKit` | 24-word recovery-kit file | portable recovery, or a drill |
| `snapshotPassphraseFile` | one-line passphrase file | a snapshot with an Argon2id protector |
| `snapshotRawKeyFile` | 32-byte key file | legacy format-1 snapshots only |
| `decrypted` | none | an explicitly plaintext source |

For routine snapshot access use `snapshotLocalCredential`, and keep the
portable recovery kit somewhere else — losing this Mac should not cost you the
backup.

## Inspect and validate

None of these print credential contents:

```sh
greenbubbles profile list
greenbubbles profile show live
greenbubbles profile validate
greenbubbles profile validate archive
greenbubbles profile set-default archive
```

`profile validate` actually loads the credential, opens the required databases
read-only, and returns content-free counts and byte totals — so it tells you
the profile works, not merely that it parses. `set-default` atomically rewrites
the configuration with mode `0600` and touches no source or credential file.

## Query with one

Omit source and access arguments to use `defaultProfile`, or name another with
`--profile`:

```sh
greenbubbles conversations list --limit 100
greenbubbles messages list --conversation <conversation-id> --limit 100
greenbubbles source status --profile archive
```

For search, stdin carries only the query text, because the credential comes
from its own file:

```sh
greenbubbles messages search --profile archive --query-stdin --limit 50 \
  < <owner-only-query-file>
```

**Ambiguous combinations are rejected on purpose.** `--profile` cannot be
combined with a positional source root, and explicit access flags cannot be
used without an explicit source root. One typo should not query the wrong live
or archived database.

## Permissions

The configuration and every credential file must be owned by you, a regular
file with exactly one hard link, inaccessible to group and other (normally
`0600`), inside a current-user-owned directory that is also inaccessible to
group and other (normally `0700`), a real path rather than a symlink, and
within its fixed size limit.

A failure returns a stable `invalidProfile` JSON error that prints no source
path, key, passphrase or credential content. Management commands may show
configured **paths**, but never read a secret into their output.

Treat `query-profiles.json` as private even though it holds no secret: its
paths say where your personal history and your unlock material live. Do not
commit it, or any credential file, to version control.
