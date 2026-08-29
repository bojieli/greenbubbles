# Acquiring your database key

WeChat encrypts its local databases with a 32-byte account key that it derives
at login and never writes down. Getting a copy of that key is the first step of
setting GreenBubbles up, and this page is the long version of it: the three
commands, how each one works, and what to do when one of them stops.

The [README](../README.md#getting-your-database-key) has the short version if
you just want to run it.

A few practical notes before you start:

- You need **root** for the capture step, and you will re-sign your own copy of
  WeChat, which replaces Apple's signature until you reinstall it or it
  auto-updates. Repeat that step after an update.
- The key is **stable**. Capture it once; databases WeChat creates later are
  covered by `verify` without another capture.
- It is also **long-lived and not rotatable by you** — it decrypts every local
  database copy you have, including old snapshots. Keep it in an owner-only
  file, which is what the tool writes by default.

The passive pipeline — discovery, snapshot, restore, replica, connector — never
uses this mechanism, so nothing about the rest of GreenBubbles changes whether
or not you run it.

## What actually happens

WeChat's macOS client derives each database key from a stable 32-byte account
passphrase **at login**, by calling the exported CommonCrypto symbol
`CCKeyDerivationPBKDF`. Because the breakpoint targets a *system library*
symbol rather than anything in the client binary, the mechanism is
build-agnostic: `greenbubbles-acquire` does no version, hash or signature
gating, and has been validated on 4.1.12 and 4.1.13.

`greenbubbles-acquire capture`:

1. Attaches `lldb` to the running WeChat process and sets a breakpoint on
   `CCKeyDerivationPBKDF`, conditioned on the password-length argument being
   exactly 32.
2. When you log out and back in, WeChat calls that symbol with the passphrase
   in argument registers. The breakpoint reads the 32 bytes from the
   password-pointer argument (`x1` on arm64, `rsi` on x86-64; the length is
   `x2`/`rdx` and must equal 32), prints a hexdump, and detaches. One
   register-pointed value is read once. Nothing else in the process is
   inspected or modified.
3. For every database in the salt inventory, it derives that database's key
   locally with PBKDF2-HMAC-SHA512, 256,000 rounds, using the database's own
   16-byte salt — the first 16 bytes of page 1, read read-only.
4. Correctness is **proven, not assumed**: each derived key is checked against
   the SQLCipher4 page-1 HMAC-SHA512 (mac key = PBKDF2 of the derived key over
   the salt XOR `0x3A`, 2 rounds; HMAC over page-1 bytes 16…4032 plus the
   little-endian page number). A passphrase that fails page-1 verification is
   never reported as captured.

### Why you have to log out and back in

WeChat 4.1+ keeps only the 32-byte account passphrase. The per-database keys
are derived from it *only while the account's databases are being opened*,
which happens at login. Afterwards the passphrase may still exist somewhere in
the process, but it never again crosses the exported symbol — and that crossing
is the one moment a breakpoint on a system function can observe.

So the capture stages itself around a fresh derivation: log in first (so a
process exists to attach to and something to verify against), arm the
breakpoint, then log out and back in. The passphrase crosses exactly once, the
debugger reads it and immediately detaches. Nothing is injected or persistently
hooked.

On 4.1.13, logging out makes the main process exit and logging back in starts a
*new* process with a new PID. `capture` detects the target exit and re-arms on
the new process automatically, so a logout that used to stall the capture now
succeeds.

## Commands

```sh
greenbubbles-acquire preflight
greenbubbles-acquire capture [--output <path>] [--timeout-seconds 300] \
  [--db-root <path>] [--overwrite]
greenbubbles-acquire verify --passphrase-stdin [--db-root <path>]
```

**`preflight`** prints a checklist (`--json` for machine output) and exits
non-zero when blocked: WeChat process presence, hardening status, `lldb`
availability, root privileges, and the discovered salt count. The active
account's database root is discovered automatically — the account whose
databases were most recently written — and `--db-root` overrides it. Client
version and signing state are reported for information only and never gate the
capture. When the client still needs re-signing, the report prints the exact
command *for you to run*; the tool never runs it.

**`capture`** re-runs preflight, fails closed, then waits for your logout and
re-login up to the timeout (default 300 seconds). It derives and HMAC-verifies
before writing anything. With no options it writes to
`~/.greenbubbles-acquire/passphrase.txt`.

**`verify`** re-derives and re-verifies a stored passphrase from standard
input, with no process attachment at all. Databases WeChat creates *after* the
capture are covered by this re-derivation — the account passphrase is stable,
so a new capture is not needed for them.

### Where the key goes

Only to the `--output` file, as 64 lowercase hex characters plus a newline,
mode `0600` in a mode-`0700` parent, with no silent overwrite (`--overwrite`
is required to replace one). It never appears on a command line, in a JSON
report, or in a log.

The file is shaped to be piped:

```sh
cat <passphrase-file> | greenbubbles restore \
  <snapshot-directory> <private-output-directory> \
  --account-root <authorized-account-directory> --passphrase-stdin
```

## What you have to do yourself

**Root, for the capture step.** Attaching with `lldb` needs `task_for_pid`,
which the sandbox denies to non-root callers.

**A one-time ad-hoc re-sign of the client, in your own sudo session:**

```sh
sudo codesign --force --deep --sign - /Applications/WeChat.app
```

then restart WeChat. This strips the Hardened Runtime flag that would otherwise
cause the attach to be refused. `greenbubbles-acquire` never automates
`codesign` and never invokes sudo itself — modifying your client's security
controls stays an explicit, visible action you take.

**A logout and re-login inside the capture window.** An idle logged-in client
never exposes the passphrase.

## Failure modes

All fail closed, and all leave you with nothing rather than something
unverified:

- An unknown or unpinned client build refuses before any attach.
- If the Hardened Runtime flag is still present — you have not re-signed — the
  kernel refuses the attach and no capture is attempted.
- If the timeout expires with no logout and re-login, nothing is captured and
  no output file is written.
- If page-1 HMAC verification fails, the value is not written as valid.

## Keeping it out of everything

The captured passphrase decrypts every local database copy you have, forever.
The `0600`/`0700` boundary, the no-logging rule, and the secret-hygiene checks
(`scripts/check-secret-hygiene.swift`, the `scripts/git-hooks` pre-commit hook,
and the CI step) exist to keep it out of the repository and out of transcripts.

Never paste it into a model prompt, an issue, a commit or a chat. If you think
it may have leaked, see
[OPERATIONAL_RESPONSE_PLAN.md](OPERATIONAL_RESPONSE_PLAN.md) — and note that
you cannot rotate it, which is why the response is about containing copies.

## What has actually been run

**2026-08-27.** A synthetic CommonCrypto test reproduced the register read and
PBKDF2 derivation byte-exactly before any live use. A live capture on the
author's own machine and account, on the pinned 4.1.12 build, verified 25 of 25
databases present at capture time by SQLCipher4 page-1 HMAC. One database
created later (`third_app_icon.db`) was covered by re-derivation with the same
passphrase via `verify`, bringing it to 26 of 26 without a second capture.

**2026-08-28.** A live capture on 4.1.13 — the client had auto-updated and been
re-signed — verified 26 of 26 databases in 45 seconds. This run observed the
logout terminating the main process (PID 64330 exiting) and the login spawning
a replacement (PID 65614); target-exit detection carried the capture through to
the new process.

Two successful runs on one machine and one account. That is what the evidence
is, and it is not a claim that this works on your build, your hardware, or your
account.

## Where this came from

The capture and derivation mechanism is ported from the MIT-licensed
[`TANGandXUE/wcdb-key-tool`](https://github.com/TANGandXUE/wcdb-key-tool),
which itself credits kkocdko, wxchat-export and ylytdeng/wechat-decrypt. See
[NOTICE.md](../NOTICE.md). GreenBubbles does not download, run or automate that
tool; only the mechanism is reimplemented as the gated path above.

The survey of how comparable projects obtain the same key, and why this route
was chosen, is archived in
[`archive/ACQUISITION_FEASIBILITY.md`](archive/ACQUISITION_FEASIBILITY.md).
