# Passive acquisition feasibility

This document records the Phase 0.5 evaluation of official, user-mediated
acquisition workflows and owner-supplied inputs. It does not establish that an
official workflow produces a portable GreenBubbles archive, and it does not
authorize a more invasive fallback.

## Pinned static evidence

The reusable inspection command is:

```sh
swift run greenbubbles acquisition-surfaces
```

It accepts only the exact signed WeChat macOS build pinned by GreenBubbles:

- bundle and signing identifier `com.tencent.xinWeChat`;
- marketing version `4.1.12`, build `269365`;
- team identifier `5A4RE8SF68`;
- executable SHA-256
  `2c61ba7f64c2b98e897553cd226364642a1eb213b5b7f74556c6fc2efc363e32`;
- CodeDirectory SHA-256
  `fa11b242567cbe161e2b332139dbc459c534b85f3855a8603614252bf908106e`;
- `arm64` and `x86_64`, Hardened Runtime, and a valid deep signature.

The command reads only the regular, single-link signed-bundle resource at the
relative path `Contents/Resources/wechat.dylib`. It opens the file read-only
with symlink following disabled, limits its size, detects mutation while it is
being read, and emits no absolute application path. It does not launch WeChat,
invoke a UI or service, attach to a process, read memory, access an account,
inspect user content, or export a credential.

On 2026-08-27, the pinned resource was 341,447,152 bytes with SHA-256
`9109337319f72712d3a69cc6bbdced7916303cfae35005ec1c7762899fad7111`.
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

GreenBubbles currently supports two non-invasive, owner-controlled acquisition
paths:

1. An owner-authorized consistent copy of the local SQLite database, WAL, and
   SHM sets, decrypted offline with a stable 32-byte passphrase supplied only
   through standard input. The passphrase is never accepted on the command
   line, written into an archive, or emitted in JSON.
2. Owner-supplied plaintext SQLite snapshots that already satisfy the snapshot
   and source-integrity contract. GreenBubbles does not need to know how the
   owner lawfully obtained a plaintext export, but it still validates the
   pinned schema and reports incomplete coverage rather than guessing.

The restoration result remains incomplete until a real, authorized current-
version corpus proves every message-bearing table, logical message type,
relationship, and local/missing media state. Static workflow evidence cannot
satisfy that requirement.

## Preferred acquisition order and stop rule

The engineering order is:

1. Prefer an official user-created portable export or backup if its documented
   format and full conversation/media coverage can be established.
2. Otherwise accept an owner-supplied plaintext snapshot or database
   passphrase through the existing passive, offline boundary.
3. If neither path is available, stop acquisition work for that account.

GreenBubbles has no automated passphrase or key acquisition. It will not fall
back to process attachment, memory scanning, debugger use, injection,
re-signing, reusable session-credential export, security-control bypass, or
anti-detection work. Static evidence that an internal workflow exists does not
change this stop rule.

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
does not prove that every private or future route is impossible. It establishes
that “other projects can decrypt WeChat” currently means either an invasive key
acquisition mechanism, an already supplied key, a different/older backup
surface, or a different bot relationship. GreenBubbles will not download, run,
port, or automate the invasive mechanisms.

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
