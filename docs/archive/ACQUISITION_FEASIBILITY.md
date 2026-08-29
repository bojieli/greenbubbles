# Passive acquisition feasibility

This document records the Phase 0.5 evaluation of official, user-mediated
acquisition workflows and owner-supplied inputs, and the owner-authorized
active capture path added after that evaluation. It does not establish that an
official workflow produces a portable GreenBubbles archive. The previous
blanket prohibition on debugger-based acquisition was lifted by explicit owner
decision on 2026-08-27; the resulting gated path is documented in
[PASSPHRASE_ACQUISITION.md](PASSPHRASE_ACQUISITION.md).

## Pinned static evidence

The reusable inspection command is:

```sh
swift run greenbubbles-discover acquisition-surfaces
```

It accepts only the exact signed WeChat macOS build pinned by GreenBubbles:

- bundle and signing identifier `com.tencent.xinWeChat`;
- marketing version `4.1.13`, build `269579`;
- team identifier `5A4RE8SF68`;
- executable SHA-256
  `041f2632f8c9f4208f0b1ad26d574384e0b854952097a851f7d9c7c6f64a8542`;
- CodeDirectory SHA-256
  `c6b9f9587044784456eb96314f685c965fbd7d88bdacb72387284b8df551df4f`;
- `arm64` and `x86_64`, Hardened Runtime, and a valid deep signature.

The command reads only the regular, single-link signed-bundle resource at the
relative path `Contents/Resources/wechat.dylib`. It opens the file read-only
with symlink following disabled, limits its size, detects mutation while it is
being read, and emits no absolute application path. It does not launch WeChat,
invoke a UI or service, attach to a process, read memory, access an account,
inspect user content, or export a credential.

On 2026-08-28, the pinned resource was 345,985,680 bytes with SHA-256
`e1b802637eb9d9154e2d98a4e315b041d5bba34938112b8e3803a2f8e934fc37`.
Bounded feature markers were observed for all four workflow families:

| Workflow family | Static conclusion | Supported interpretation |
| --- | --- | --- |
| Backup and restore | observed | The client contains a user-mediated backup/restore workflow. |
| Chat-history migration | observed | The client contains local and remote migration workflow code. |
| Device transfer | observed | The client contains phone/desktop import or export workflow code. |
| File export | observed | The client contains a generic, user-mediated file export workflow. |

These are feature-presence clues, not reverse-engineered callable interfaces.
The report deliberately keeps all of the following conclusions false:

- a portable plaintext conversation export is proven;
- GreenBubbles can consume the official backup format;
- the workflow covers every conversation and downloaded attachment;
- a reusable credential was exported;
- the live client was invoked or controlled.

An unknown or modified build fails before its resource is interpreted. Marker
absence produces `notObserved`; it never becomes inferred support.

## Inputs GreenBubbles supports

GreenBubbles currently supports three owner-controlled acquisition paths, in
preference order:

1. An official user-created portable export or backup, if its documented
   format and full conversation/media coverage can be established. No such
   coverage is proven today.
2. An owner-supplied plaintext SQLite snapshot that already satisfies the
   snapshot and source-integrity contract, or an owner-authorized consistent
   copy of the local SQLite database, WAL, and SHM sets, decrypted offline
   with a stable 32-byte passphrase supplied only through standard input. The
   passphrase is never accepted on the command line, written into an archive,
   or emitted in JSON. GreenBubbles does not need to know how the owner
   lawfully obtained a plaintext export, but it still validates the pinned
   schema and reports incomplete coverage rather than guessing.
3. An owner-authorized active passphrase capture through the separate
   `greenbubbles-acquire` executable, gated by a manual owner-run re-sign of
   the client and SQLCipher4 page-1 HMAC proof of correctness. The capture
   mechanism
   breakpoints a system library symbol, so it is build-agnostic and performs
   no client version, hash, or signature gating. See
   [PASSPHRASE_ACQUISITION.md](PASSPHRASE_ACQUISITION.md). The passive
   pipeline itself — discovery, snapshot, restore, replica, connector — never
   invokes this path and retains its non-invasive guarantees unchanged.

The restoration result remains incomplete until a real, authorized current-
version corpus proves every message-bearing table, logical message type,
relationship, and local/missing media state. Static workflow evidence cannot
satisfy that requirement.

## Preferred acquisition order and stop rule

The engineering order is the three paths above: official export first, then
the passive owner-supplied boundary, then the gated owner-authorized capture.
If none of the three is available or authorized for an account, acquisition
work for that account stops.

The owner-authorized capture in path 3 is a deliberate policy reversal decided
by the owner on 2026-08-27, after the LLDB-based mechanism was validated live
on the owner's own machine and account (26/26 databases HMAC-verified on the
pinned 4.1.12 build). It is strictly bounded: it attaches a debugger once to
read one register-pointed value, requires manual owner re-signing that the
tool never automates, and writes the passphrase only to an owner-specified
permission-locked file. On 2026-08-28 the owner removed the helper's
pinned-build gate as well: the breakpoint targets a system library symbol, so
the mechanism works with any WeChat build, and the helper discovers the active
account's database root automatically. GreenBubbles
still performs no memory scanning, injection, reusable session-credential
export, security-control bypass beyond the owner's own explicit re-sign, or
anti-detection work. Static evidence that an internal workflow exists does not
change the preference order.

## Current public-project survey

A fresh public-source review on 2026-08-27 found no documented non-invasive
source for the macOS 4.1.12 database passphrase:

- [`pandorafuture/wx-cli`](https://github.com/pandorafuture/wx-cli), whose
  decoder crates GreenBubbles pins for offline format support, documents its
  optional key acquisition as disabling macOS SIP and intercepting a live
  WeChat PBKDF2 call with LLDB. An already known key can be supplied manually,
  but the project does not identify a supported export or ordinary user-visible
  passphrase source.
- [`robbin/wechat-exporter`](https://github.com/robbin/wechat-exporter)
  documents re-signing WeChat, privileged memory scanning, and key extraction.
  That is client modification and live credential extraction, not an official
  backup importer.
- Contemporary Windows projects likewise describe live-process memory scanning
  or injected hooks. Different platform mechanics do not make that an
  owner-controlled portable export or a macOS passphrase source.
- [`PyWxDump`](https://github.com/xaoyaoo/PyWxDump) and
  [`chatlog`](https://github.com/sjzar/chatlog) removed their implementation and
  history after reporting WeChat legal notices in October 2025; neither remains
  an available supported acquisition path.
- [`WechatExporter`](https://github.com/BlueMatthew/WechatExporter) consumes an
  unencrypted iTunes/iOS backup and documents much older tested mobile versions.
  It neither obtains nor replaces the current macOS desktop database
  passphrase.
- [`WxBackup`](https://github.com/weibeifen/wxbackup) describes a proprietary,
  phone-confirmed NAS backup/restore product. Its public repository does not
  document a portable plaintext format or expose the desktop WCDB passphrase.
- Tencent's
  [`openclaw-weixin`](https://github.com/Tencent/openclaw-weixin) provides a
  QR-authorized bot/channel relationship. It does not expose the existing
  desktop conversation archive or its database key.

This survey is descriptive, not an endorsement or legal conclusion. It also
does not prove that every private or future route is impossible. Its core
finding remains factually true: no documented **non-invasive** source for the
current macOS database passphrase exists in the surveyed projects. “Other
projects can decrypt WeChat” currently means either an invasive key
acquisition mechanism, an already supplied key, a different/older backup
surface, or a different bot relationship. Following the owner's 2026-08-27
decision, GreenBubbles now embeds one such mechanism — the LLDB
`CCKeyDerivationPBKDF` capture ported from the MIT-licensed
[`TANGandXUE/wcdb-key-tool`](https://github.com/TANGandXUE/wcdb-key-tool) — as
the gated, owner-authorized path 3 described above and in
[PASSPHRASE_ACQUISITION.md](PASSPHRASE_ACQUISITION.md). The standalone
external tools surveyed here remain unused, undownloaded, and unautomated by
GreenBubbles.

## Remaining evidence required

This evaluation completes the plan item to examine official workflows and
owner-supplied inputs before invasive alternatives. It does not pass the Phase
0.5 technical gate. Passing still requires, on authorized disposable/test
data:

- a useful current-version conversation and attachment corpus;
- proof that the chosen non-invasive input reproduces all locally available
  rows and artifacts without modifying the official client;
- bootstrap and incremental synchronization measurements for real client
  persistence, edits, recalls, deletions, missed hints, and crash recovery;
- a separately reviewed, user-mediated ordinary-contact action experiment if
  write feasibility is pursued.

Legal and public-distribution review is independent. This private technical
assessment is not permission to publish code, schemas, fixtures, or binaries.
