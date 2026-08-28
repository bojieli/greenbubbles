# Owner-authorized passphrase acquisition

This document records the deliberately gated, owner-authorized active
acquisition path implemented by the separate `greenbubbles-acquire` executable.
It exists because the owner reversed the previous blanket prohibition on
debugger-based acquisition on 2026-08-27, after the mechanism was validated
live on the owner's own machine against the owner's own WeChat account. The
passive pipeline (discovery, snapshot, restore, replica, connector) does not
use this mechanism and retains its non-invasive guarantees unchanged.

This path is acquisition route (3) in the preference order recorded in
[ACQUISITION_FEASIBILITY.md](ACQUISITION_FEASIBILITY.md). It is a fallback for
the owner's own account when no official portable export and no owner-supplied
plaintext/passphrase input is available; it is not a general capability.

## Mechanism

The pinned WeChat macOS build (marketing version `4.1.13`, build `269579`)
derives each database's encryption key from a stable 32-byte account passphrase
at login time by calling the exported system CommonCrypto symbol
`CCKeyDerivationPBKDF`. `greenbubbles-acquire capture`:

1. Attaches `lldb` to the already running WeChat process and sets a breakpoint
   on `CCKeyDerivationPBKDF`, conditioned on the password-length argument being
   exactly 32.
2. When the owner logs the account out and back in, WeChat calls the symbol
   with the passphrase in the argument registers. The breakpoint reads the
   32-byte passphrase from the password-pointer argument (`x1` on arm64, `rsi`
   on x86_64; the length argument is `x2`/`rdx` and must equal 32), prints it
   as a hexdump, and the debugger detaches. One register-pointed value is read
   once; nothing else in the process is inspected or modified.
3. For every database in the salt inventory, the tool derives the database key
   locally with PBKDF2-HMAC-SHA512, 256,000 rounds, using that database's own
   16-byte salt (the first 16 bytes of page 1, read read-only).
4. Correctness is proven, not assumed: each derived key is checked against the
   SQLCipher4 page-1 HMAC-SHA512 (mac key = PBKDF2 of the derived key over the
   salt XOR `0x3A`, 2 rounds; HMAC over page-1 bytes 16...4032 plus the
   little-endian page number). A passphrase that fails page-1 verification is
   not reported as captured.

The passphrase is written only to the owner-specified `--output` file as raw
64-lowercase-hex plus a newline, with the file at mode `0600`, its parent
directory at mode `0700`, and no silent overwrite (`--overwrite` is required to
replace an existing file). It never appears on a command line, in a JSON
report, or in logs. The file is shaped so it can be piped directly:

```sh
cat <passphrase-file> | greenbubbles-restore restore \
  <snapshot-directory> <private-output-directory> \
  --account-root <authorized-account-directory> --passphrase-stdin
```

## Commands

```sh
greenbubbles-acquire preflight
greenbubbles-acquire capture --output <path> --owner-authorized \
  [--timeout-seconds 300] [--db-root <path>] [--overwrite]
greenbubbles-acquire verify --passphrase-stdin [--db-root <path>]
```

- `preflight` emits an aggregate JSON readiness report and exits non-zero when
  blocked: pinned-build fingerprint, WeChat process presence, hardening status,
  `lldb` availability, root privileges, and the discovered salt count. When the
  client still needs re-signing, the report prints the exact command for the
  owner to run manually; the tool never runs it.
- `capture` refuses to run without the explicit `--owner-authorized` flag,
  re-runs the preflight checks and fails closed, then waits for the owner's
  logout/re-login up to the timeout. On capture it derives and HMAC-verifies
  before writing anything.
- `verify` re-derives and re-verifies a stored passphrase read from standard
  input, without any process attachment. Databases WeChat creates after the
  capture are covered by this re-derivation; a new capture is not needed for
  them because the account passphrase is stable.

## Owner requirements

- Root privileges for the capture step: attaching with `lldb` requires
  `task_for_pid`, which the sandbox denies to non-root callers.
- A one-time manual ad-hoc re-sign of the client, run by the owner in their own
  sudo session, followed by a WeChat restart:

  ```sh
  sudo codesign --force --deep --sign - /Applications/WeChat.app
  ```

  This strips the Hardened Runtime flag that would otherwise cause the attach
  to be refused. `greenbubbles-acquire` never automates `codesign` and never
  invokes sudo itself; modifying the client's security controls remains an
  explicit, visible owner action.
- An account logout and re-login inside the capture window. The passphrase
  crosses `CCKeyDerivationPBKDF` only when WeChat derives keys; an already
  logged-in idle client does not expose it.

## Threat model and limitations

- The mechanism modifies the client's code signature. Reinstalling WeChat or
  letting it auto-update restores the signed state, and re-signing must be
  repeated before any further capture. The pinned-build policy still applies to
  the passive pipeline; `preflight` reports the re-signed state honestly
  instead of claiming a pristine signature.
- The debugger reads one register-pointed value once and detaches. It does not
  scan memory, inject code, hook functions persistently, or touch network
  protocols.
- This path exists only for the owner's own account on the owner's own device.
  Group-chat data belongs partly to other people; acquiring the passphrase does
  not change any consent, minimization, or distribution obligation recorded in
  `README.md` and `PLAN.md`.
- The captured passphrase is a long-lived secret. It grants offline decryption
  of every local database copy, including old snapshots. The `0600`/`0700`
  file boundary, the no-logging rule, and the secret-hygiene checks
  (`scripts/check-secret-hygiene.swift`, the `scripts/git-hooks` pre-commit
  hook, and the CI step) exist to keep it out of the repository and out of
  transcripts.

## Failure modes (all fail closed)

- An unknown or unpinned client build refuses before any attach.
- If the Hardened Runtime flag is still present (the owner has not re-signed),
  the attach is refused by the kernel and no capture is attempted.
- If the timeout expires without a logout/re-login, nothing is captured and no
  output file is written.
- If page-1 HMAC verification fails for a captured value, the passphrase is
  not reported or written as valid.

## Validation evidence (2026-08-27)

- A synthetic CommonCrypto mechanism test reproduced the register-read and
  PBKDF2 derivation byte-exactly before any live use.
- A live capture on the owner's own machine and account, on the pinned 4.1.12
  build, verified 25 of 25 databases present at capture time by SQLCipher4
  page-1 HMAC.
- One database created later (`third_app_icon.db`) was covered by
  re-derivation with the same passphrase via `verify`, bringing the total to
  26 of 26 databases HMAC-verified without a second capture.

## Attribution

The capture and derivation mechanism is ported from the MIT-licensed
[`TANGandXUE/wcdb-key-tool`](https://github.com/TANGandXUE/wcdb-key-tool),
which itself credits kkocdko, wxchat-export, and ylytdeng/wechat-decrypt. See
[NOTICE.md](../NOTICE.md). The standalone external tool is not downloaded, run,
or automated by GreenBubbles; only the mechanism is reimplemented as the gated
path above.
