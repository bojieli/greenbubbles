# Local setup

The skills are portable instructions; the data tools currently require a local
Mac with the GreenBubbles CLI and access to the user's WeChat files or snapshot.
A cloud agent without access to that Mac cannot capture or open its history.
Python 3 runs the doctor; Git is needed for memory project versioning.

## CLI

Install the command-line release from https://github.com/bojieli/greenbubbles/releases,
or build the Rust CLI from a checkout:

```sh
cargo build --locked --release --manifest-path Native/GreenBubbles/Cargo.toml
```

The binary is `Native/GreenBubbles/target/release/greenbubbles`. Use an absolute
path or put it on PATH. A similarly named Swift discovery executable is not
the memory CLI. `greenbubbles memory --help` must include `--format` and
`acknowledge`. Never silently use a different executable after a failed check.

## Open the live database

After capture, do not create a profile for the ordinary live source:

```sh
greenbubbles source status
greenbubbles chats --limit 20
```

With no profile file and no settings file, these commands open the newest
installed WeChat `db_storage` directory and read
`~/.greenbubbles-acquire/passphrase.txt`. They do not print the key or the
source path. A missing passphrase file or an unreadable database returns
`invalidProfile`, with a plain-language explanation on the terminal.

A different database or passphrase file belongs in
`~/.greenbubbles/config.toml`, not on the command line:

```toml
[source]
root = "/absolute/path/to/db_storage"
passphrase_file = "/absolute/path/to/passphrase.txt"
```

Leave a line out to keep its default. The file stores paths only. Never write
the key into it, and never print the file's paths into chat.

Create a profile only for a second account or a snapshot. Use an existing
working profile when one is already configured:

```sh
greenbubbles profile list
greenbubbles profile validate NAME
greenbubbles source status --profile NAME
```

These commands do not expose credential contents.
When creating a profile, use `greenbubbles profile path` to locate the effective
configuration and `greenbubbles profile template` for its schema. Read the existing
configuration before changing it; preserve other profiles and the existing default.
Create parent directories mode 0700 and the config mode 0600. Store only absolute
source and credential file paths, never the key itself:

```json
{
  "schema": "greenbubbles.query-profiles.v1",
  "formatVersion": 1,
  "defaultProfile": "live",
  "profiles": {
    "live": {
      "sourceRoot": "/absolute/path/to/db_storage",
      "access": {
        "mode": "liveWeChatKeyFile",
        "credentialFile": "/absolute/private/path/passphrase.txt"
      }
    }
  }
}
```

For a snapshot use `snapshotLocalCredential`, `snapshotRecoveryKit`, or
`snapshotPassphraseFile` as appropriate to the supplied credential. `decrypted`
is only for an explicitly plaintext source, never a fallback for failed decryption.
Validate the selected profile before proceeding. Do not read credential contents
into agent context. Use stdin redirection from its private file if explicit
`--passphrase-stdin` access is needed.

## Initial capture when no credential exists

Explain the concrete requirements: capture attaches a debugger to the owner's
running WeChat and needs root; the owner may need to ad-hoc re-sign their copy
of WeChat and restart it, then log out and in during capture. Ordinary reading
and incremental memory updates do not need repeated capture.

Check `greenbubbles-acquire --help` for the installed helper. The owner-operated
sequence documented by GreenBubbles is:

```sh
sudo codesign --force --deep --sign - /Applications/WeChat.app
# Restart WeChat after re-signing.
sudo greenbubbles-acquire preflight
sudo greenbubbles-acquire capture
# Log out and back in during the capture window.
```

The helper does not automate re-signing or sudo. Keep these owner-operated steps
visible; do not run them as a side effect of a query or setup check. The default
key output is `~/.greenbubbles-acquire/passphrase.txt` for the invoking environment;
confirm the actual helper output path and file ownership before configuring a
profile. Use explicit `--output` and `--db-root` from helper help when needed.
Never request the key in chat. If capture fails, report its prerequisite failure;
do not repeatedly attach or modify the application.

## Model boundary

Preparation/decryption runs locally. Message pages read by this agent go to its
configured model provider. Follow the user's chosen scope and destination. The
skill does not install a scheduler, start a background agent, or send messages.
