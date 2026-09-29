# Getting your database key

WeChat encrypts the chat databases it keeps on your Mac. To read them,
GreenBubbles needs your account's key. This page shows how to copy that key
out of your own running WeChat app. You only do this once.

The [README](../README.md#getting-your-database-key) has the short version.

![Account key capture and per-database key derivation](../assets/key-flow.svg)

## Before you start

- **You need administrator access.** The capture step runs with `sudo`.
- **You will re-sign your copy of WeChat.** This replaces Apple's security
  signature on WeChat with a local one, so the capture tool is allowed to
  attach to it. WeChat keeps working normally. When WeChat updates, it gets its
  original signature back, and you re-sign it again only if you need to capture
  the key again.
- **You will log out of WeChat and log back in** during the capture.
- **The key cannot be changed.** It opens every copy of your WeChat data,
  including old backups. The tool saves it in a file only your account can
  read. Keep it that way, and never paste it into an AI prompt, an issue, or a
  chat.

## Steps

1. **Start WeChat and log in.**

2. **Check that everything is ready:**

   ```sh
   greenbubbles-acquire preflight
   ```

   It prints a checklist and says `Ready to capture.` when nothing is missing.
   If something is wrong, it prints how to fix it. Common fixes:

   - `lldb is not available`: install Apple's developer tools with
     `xcode-select --install`.
   - `Hardened Runtime is still active`: re-sign WeChat (next step).
   - `not running as root`: expected for this check. The capture itself runs
     with `sudo`.

3. **Re-sign WeChat, then quit and restart it.** Run this yourself;
   GreenBubbles never runs it for you:

   ```sh
   sudo codesign --force --deep --sign - /Applications/WeChat.app
   ```

4. **Start the capture:**

   ```sh
   sudo greenbubbles-acquire capture
   ```

5. **In WeChat, log out of your account (don't just quit the app), then log
   back in.** You have 5 minutes by default.

When the login finishes, the tool checks the key against your databases and
saves it to `~/.greenbubbles-acquire/passphrase.txt`. Now you can run ordinary
commands with no extra setup:

```sh
greenbubbles chats --limit 20
```

## Command options

```sh
greenbubbles-acquire preflight [--json] [--db-root <path>]
greenbubbles-acquire capture [--output <path>] [--timeout-seconds 300] \
  [--db-root <path>] [--overwrite]
greenbubbles-acquire verify --passphrase-stdin [--db-root <path>]
```

- **`preflight`** checks that WeChat is installed and running, whether it has
  been re-signed, whether `lldb` is available, whether you are root, and how
  many databases it found. It exits with an error if anything blocks the
  capture. `--json` prints the report as JSON.
- **`capture`** runs the same checks, then waits for you to log out and back
  in. `--timeout-seconds` changes the 300-second wait. `--output` saves the key
  somewhere other than `~/.greenbubbles-acquire/passphrase.txt`. It never
  replaces an existing key file unless you add `--overwrite`.
- **`verify`** checks a saved key against your databases without touching
  WeChat. Pipe the key file into it. Use it after WeChat creates new database
  files; the same key opens them, so you don't need to capture again.
- **`--db-root`** points at a specific WeChat data folder. Normally the tool
  picks the account whose data was written most recently.

The WeChat version is shown for information only. The tool does not refuse a
version it hasn't seen, but it has only been tested on WeChat 4.1.12 and 4.1.13.

## If something goes wrong

The capture never saves a key it could not verify. If anything fails, you are
left with no key file rather than a wrong one.

- **"Hardened Runtime is still active":** you haven't re-signed WeChat, or you
  didn't restart it after re-signing. macOS blocks the attach.
- **Timed out:** you didn't log out and back in within the time limit. Run
  `capture` again. An idle, logged-in WeChat never reveals the key.
- **Verification failed:** the captured value didn't open your databases, so
  nothing was saved. Check that you logged back into the same account.
- **WeChat updated:** re-sign it and restart it before capturing again. A key
  you already saved keeps working as long as `verify` succeeds.

## Where the key is saved

The key is saved as 64 hexadecimal characters in
`~/.greenbubbles-acquire/passphrase.txt`. The file can be read only by your
account (mode `0600`, in a `0700` folder). It is not encrypted; those file
permissions are what protect it. The key never appears on the command line, in
a report, or in a log.

To use a key saved somewhere else, or to read a second account or a backup,
create a [query profile](QUERY_PROFILES.md). You can also pipe a key directly
into a command:

```sh
cat <passphrase-file> | greenbubbles conversations list \
  <db_storage-directory> --passphrase-stdin --limit 20
```

If you think the key has leaked, see
[OPERATIONAL_RESPONSE_PLAN.md](OPERATIONAL_RESPONSE_PLAN.md). Because the key
can't be changed, the response is about finding and removing copies.

## How it works

You don't need this section to use GreenBubbles.

WeChat uses SQLCipher to encrypt each database file. Your account has one
32-byte key. For each file, WeChat combines that key with a random value stored
at the start of the file (the salt) to make the file's own encryption key. So
there is one account key, not one key per file or table. See the
[SQLCipher design](https://www.zetetic.net/sqlcipher/design/).

WeChat makes those per-file keys only while it opens its databases, which
happens when you log in. It does this by calling a macOS system function,
`CCKeyDerivationPBKDF`. That is the one moment the account key can be seen, and
it is why you have to log out and back in.

`greenbubbles-acquire capture` does this:

1. Attaches Apple's `lldb` debugger to WeChat and pauses on
   `CCKeyDerivationPBKDF` only when the password passed to it is exactly 32
   bytes long.
2. When you log back in, it reads those 32 bytes once (from register `x1` on
   Apple silicon, `rsi` on Intel), then detaches. It reads nothing else and
   changes nothing in WeChat. Because it pauses on a macOS function rather than
   on WeChat's own code, it does not depend on a particular WeChat version.
3. On WeChat 4.1.13, logging out ends the WeChat process and logging in starts
   a new one. The tool notices and follows the new process.
4. For each database file, it computes the file's key on your Mac
   (PBKDF2-HMAC-SHA512, 256,000 rounds, with that file's 16-byte salt).
5. It proves each computed key is correct by checking SQLCipher 4's
   authentication code on the file's first page. A key that fails this check is
   never saved.

Only the account key is saved. The per-file keys exist in memory only during
the check and are then erased.

GreenBubbles features that don't read live WeChat data, such as opening an
existing backup, never use this capture.

## What has been tested

- **2026-08-27, WeChat 4.1.12:** a test with made-up data reproduced the read
  and key computation exactly. Then a real capture on the author's own Mac and
  account verified all 25 databases. A database WeChat created later was
  verified with `verify` using the same key, without a second capture.
- **2026-08-28, WeChat 4.1.13:** after WeChat auto-updated and was re-signed, a
  capture verified all 26 databases in 45 seconds. Logging out ended the WeChat
  process and logging in started a new one; the capture followed it.

That is two successful runs on one Mac and one account. It is not a promise
that it will work on your Mac, your WeChat version, or your account.

## Credits

The capture method is ported from the MIT-licensed
[`TANGandXUE/wcdb-key-tool`](https://github.com/TANGandXUE/wcdb-key-tool),
which credits kkocdko, wxchat-export, and ylytdeng/wechat-decrypt. See
[NOTICE.md](../NOTICE.md). GreenBubbles does not download or run that tool; it
reimplements the method.

How other projects get the same key, and why this method was chosen, is in
[`archive/ACQUISITION_FEASIBILITY.md`](archive/ACQUISITION_FEASIBILITY.md).
