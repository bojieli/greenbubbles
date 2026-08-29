# Send-path reverse-engineering findings

Status: completed passive survey (2026-08-28). Read-only research only: no
injection, no method invocation, no user content read, no network use. Live
work was limited to short LLDB attach windows (immediate detach) and static
analysis of the installed bundle. Raw artifacts live in `.tmp/re-search/`
(mode 0700): `STATIC_FINDINGS.md`, `RUNTIME_FINDINGS.md`, symbol/string dumps,
per-image class maps, and method-list dumps.

Scope: feasibility of programmatically driving WeChat macOS 4.1.13 (build
269579, arm64, ad-hoc re-signed by the owner) to send messages, as research
input to the gated Phase 4 write-path decision recorded in
`ACTION_SAFETY_CONTRACT.md`. This document is a private technical assessment;
it is not permission to publish code, schemas, fixtures, or binaries, and it
does not open the Phase 4 gate.

## Target state (confirmed)

- Bundle: `com.tencent.xinWeChat`, universal (x86_64 + arm64), Hardened
  Runtime stripped by owner ad-hoc re-sign (`flags=0x2(adhoc)`, no team ID),
  which is why same-user LLDB attach succeeds without root.
- `Contents/Resources/wechat.dylib`: 345,985,680 bytes, arm64 slice at offset
  178,896,896. **No `LC_ENCRYPTION_INFO`** — not FairPlay-encrypted,
  statically analyzable in the clear.
- **Symbols stripped**: exactly two defined exports (`_WeChatMain`,
  `_SetWeixinCallbackFunc`); all other ~2,355 symbols are undefined imports.
  Internal logic reachable only via ObjC metadata sections and string xrefs.
- The bundle ships duplicate framework copies under `Contents/Frameworks/` and
  `Contents/Resources/` — correlate by UUID, not path.

## Architecture (confirmed)

WeChat 4.1.x macOS is a **Qt/C++ application** (unified "xwechat" codebase,
crashpad module `xwechat_mac`), not an ObjC app:

- Main `WeChat` executable registers **zero** ObjC classes.
- `wechat.dylib` carries only 123 ObjC classes — a platform shim (Sparkle
  updater, Qt-Cocoa bridge, ScreenRecorder, FileProvider XPC, Mars
  reachability, RTC, `MMNotificationService`, `WeTypeStickerService`).
- Messaging core is C++ inside the main process: Mars
  (`mars::stn::ShortLink...::SendRequest`), ilink, ProtobufLite
  (`WXPBGeneratedMessage`), mm kernel names (`kernel::MessageWrapper`,
  `message_service::~MessageService`, `SendMessageCheck`, `OnSendMessage`,
  `HandleSendFailed`, `AddMsgCmdHandler`, `GetMsgSendSource_*`).
- UI side is C++/Qt too (`mmui::SessionSendConfirmWindow`,
  `mmui::ChatBotSendMsgCardPanel`, `onSendToFriendClicked`).

## What does not exist on this build

- Classic 3.x-era ObjC messaging classes — `MessageService`, `CMessageMgr`,
  `CMessageWrap`, `MessageData`, `CContactMgr`: **zero hits** statically and
  at runtime (71,940 live classes checked). Every published 3.x tweak
  technique is inapplicable.
- Trap noted: `MMService` / `MMMessagesInterface` / `MMContactsService` in a
  class listing are Apple's AOSUI.framework (iCloud), not WeChat. The only
  genuine WeChat `MM*` class is `MMNotificationService` (local banners).
- **No IPC bridge for chat send.** Mini-programs (`WeChatAppEx`, `WeApp`,
  Chromium + `libmmmojo.dylib`) and helpers (`wxocr`, `wxplayer`,
  `libwxutility`, `roam_server`) are separate processes, but none expose a
  messaging send surface. The core talks to Tencent servers directly,
  in-process. There is no UI→core protocol to mimic: the UI calls core
  services via in-process C++ virtual calls.
- Near-miss selectors, for the record: `MMNotificationService
  postNotification:content:chatName:uniqueId:AllowReply:` (local banners),
  `WeTypeStickerService sendMessageToWeTypeWithEncodedData:` (input-method
  IPC), `IlinkStreamChannel sendStreamFragment:` (raw stream transport).

## Network surface (static strings)

Embedded CGI config confirms the send endpoints: `newsendmsg` (reqid 237),
`sendmsg` (2), `sendappmsg` (107), `sendemoji` (68), `revokemsg` (536),
`transferresendmsg` (611) — reachable only through the Mars/MMTLS session,
i.e. effectively not an integration seam.

## Anti-tamper

- Present: code-sign self-check imports (`SecStaticCodeCheckValidity*`),
  exception-port swaps (partly the bundled crashpad handler),
  `dlopen`/`dlsym`/`mprotect`/`sysctl`.
- Absent: no `ptrace`, no `PT_DENY_ATTACH`, no `csops`, no AMFI markers.
  Static analysis is unimpeded; dynamic-instrumentation friction was not
  observed but also not fully exercised (out of scope: no injection tried).

## Feasibility assessment

| Approach | Verdict |
| --- | --- |
| ObjC swizzle / classic `MessageService` driving | Dead — classes absent in 4.x |
| Mimic a UI→core protocol | No protocol exists — in-process C++ calls |
| Hook stripped C++ kernel / Mars `newsendmsg` | Technically possible (RTTI + string anchors, binary unencrypted, no anti-debug) but requires per-build disassembly and persistent injection into the session-holding process |
| Reimplement wire protocol | Heaviest option; highest account-risk |
| **Accessibility (AX) automation** | **Probed and rejected for the chat surface** — WeChat 4.1.13 exposes no AX content tree even with VoiceOver attached (see below); matches the Qt/C++ architecture finding |

Background-isolation options for AX, in increasing strength: same-session AX
(out-of-band, no physical cursor; focus theft occasionally needed),
virtual-display parking (BetterDisplay), second macOS user session via Screen
Sharing (separate WeChat account = separate bot identity, mouse-safe by
construction), macOS VM via tart (hardest isolation).

## AX automation probe (negative result, 2026-08-28)

Tested against the live 4.1.13 client (owner-approved, read-only):

- AX permission granted; `AXWindows` returns the main window; only the three
  titlebar buttons are exposed as children.
- Enabled VoiceOver and relaunched WeChat (Qt activates accessibility only
  when a screen reader is detected at launch). `AXEnhancedUserInterface=1`
  confirmed VoiceOver attached — yet the window still exposes **zero content
  children**. The owner's manual check agreed: VoiceOver announces only
  "WeChat, window" and no interior elements.
- `AXManualAccessibility`/`AXContentForceAccessible` bootstrap attributes are
  rejected (`-25205`); no WebKit/Chromium-style inert-tree workaround exists.
- Conclusion: this Qt build exposes no accessible interface for the chat UI
  (custom-rendered controls or accessibility compiled/disabled out). **AX
  automation of the main window is a dead end on this build** without
  injection. The standalone AX-adapter plan from the 2026-08-28 survey is
  retracted for the chat surface; menus remain AX-visible but cannot send.

## Residual non-injection seam

Pixel-driven computer use (screenshot + synthesized input events) does not
need AX cooperation, but on the owner's own session it moves the physical
cursor — the original objection. It only satisfies the isolation requirement
inside a separate graphical session: a second macOS user account reached via
Screen Sharing, or a tart macOS VM. Note this implies a **separate WeChat
account** for the agent identity (WeChat permits only one desktop login per
account), which matches the disposable-account requirement in
`ACTION_SAFETY_CONTRACT.md`.

## Next steps

1. Read-only AX tree enumeration of the live chat window (element paths for
   conversation list, input editor, send control) — recorded in
   `.tmp/re-search/`.
2. Background send-flow proof on a disposable conversation: set input via
   dedicated `NSPasteboard` + AX press, never touching the physical cursor —
   only after the Phase 4 owner decision and on an allow-listed test peer, per
   `ACTION_SAFETY_CONTRACT.md`.
