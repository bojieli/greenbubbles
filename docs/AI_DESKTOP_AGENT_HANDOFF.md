# AI desktop-agent architecture: session findings and design handoff

Date: 2026-08-28. Status: research summary for external consultation. No code
beyond lab prototypes; no messages were ever sent; all live work was read-only
or reversible. Raw evidence lives in `docs/SEND_PATH_RE_FINDINGS.md`,
`.tmp/re-search/STATIC_FINDINGS.md`, `.tmp/re-search/RUNTIME_FINDINGS.md`, and
`.tmp/re-search/ax-tree.txt` (directory mode 0700).

## 1. Origin of the inquiry

GreenBubbles today is a read-only restoration/replica/connector pipeline over
the owner's WeChat macOS data, with an explicitly gated (Phase 4) write path
(`docs/ACTION_SAFETY_CONTRACT.md`, `docs/AI_TOOL_BOUNDARY.md`). The owner asked
whether the project could be extended to **programmatic sending** — an AI
application driving WeChat to send messages — and the discussion generalized
into: **an AI that freely operates the user's desktop, in the background,
without interfering with the user's real mouse, keyboard, or screen, with the
user always able to take over.** The owner explicitly targets the general
problem, not a single-app integration.

## 2. Hard platform facts established by direct experiment

### 2.1 WeChat macOS 4.1.13 (build 269579, arm64, ad-hoc re-signed by owner)

- The 345 MB `Contents/Resources/wechat.dylib` is **not encrypted** (no
  `LC_ENCRYPTION_INFO`); **symbols are stripped** (only `_WeChatMain` and
  `_SetWeixinCallbackFunc` exported).
- The client is a **Qt/C++ application** (unified "xwechat" codebase). The main
  executable registers zero ObjC classes; the dylib's 123 ObjC classes are a
  platform shim (Sparkle, Qt-Cocoa bridge, notifications, stickers).
- Classic 3.x-era messaging classes (`MessageService`, `CMessageMgr`,
  `CMessageWrap`, `CContactMgr`) are **absent** (0 hits among 71,940 runtime
  classes). All published 3.x tweak techniques are inapplicable.
- The messaging core is in-process C++ (Mars `newsendmsg` reqid 237, ilink,
  `kernel::MessageWrapper`, `message_service`, `SendMessageCheck`), with **no
  IPC bridge** — mini-program/helper processes expose no chat surface. There is
  no UI→core protocol to mimic.
- Anti-tamper: code-sign self-check imports and exception-port swaps present;
  **no `ptrace`/`PT_DENY_ATTACH`/AMFI**. Static analysis is unimpeded.
- LLDB attaches same-user without root (owner's ad-hoc re-sign stripped
  Hardened Runtime). One-shot register-read capture (the existing passphrase
  path) works on any build.

### 2.2 Accessibility on this build — falsified live

- With VoiceOver enabled and attached (`AXEnhancedUserInterface=1`), verified
  both programmatically and by the owner manually, WeChat's window exposes
  **only the titlebar** — zero content children. `AXManualAccessibility`/
  `AXContentForceAccessible` are rejected. The chat UI has no accessibility
  surface (custom-rendered or accessibility compiled out).
- Consequence: the out-of-band automation path macOS offers (AX) is closed for
  this app, and in general AX coverage cannot be assumed on macOS.

### 2.3 Rendering/input stacks, cross-platform

- **macOS**: apps render via CoreAnimation/Metal into surfaces handed to a
  **singleton WindowServer per session**, which owns the framebuffer and the
  single input stream (one cursor, one keyboard focus). No swappable display
  server (no Xvfb analog for native apps), no per-app input injection API,
  virtual displays are output-only.
- **X11/Linux**: the X server *is* a user-space swappable process — Xvfb,
  Xephyr, Xpra give virtual framebuffers with full input injection (xdotool/
  XTEST) and trivial capture. On **Wayland** hosts this is unavailable for
  session apps (portals + consent for capture; no input injection), but Xvfb
  remains fully functional for X11 apps regardless of host session, and a
  nested headless compositor + wayvnc provides the native-Wayland equivalent
  including virtual-pointer/virtual-keyboard protocols.
- **Windows**: same centralization (win32k/DWM per session) but with unique
  per-window primitives: `PostMessage`/`SendMessage` injects raw input into any
  window's queue (no cursor movement, no focus), UI Automation is widely
  implemented (Qt included), **Windows.Graphics.Capture** captures occluded
  windows (minimized needs the offscreen-park workaround; PrintWindow/
  PW_RENDERFULLCONTENT and DWM thumbnails cover minimized). First-class virtual
  display drivers (IddCx). **Desktop objects** (`CreateDesktop`/`SwitchDesktop`)
  give same-user, hotkey-switchable desktops with per-desktop input focus —
  the "shadow desktop" tools of the XP/7 era (Sysinternals Desktops et al.),
  now the basis of Task View. Client SKUs allow one interactive session.
- **Android**: most agent-friendly — AccessibilityService (full node tree +
  gesture injection, public API), VirtualDisplay, work profiles for extra app
  instances; scrcpy is a ready-made zero-latency mirror + input + file-drop
  channel.
- **iOS**: no third-party UI control, no virtual displays; WebDriverAgent is a
  revocable gray area. Not a viable host.

### 2.4 The WeChat session constraint (decisive)

WeChat permits one mobile + one desktop session per account. Typical users
already consume both, so **any architecture requiring a second concurrent
WeChat login is invalid** for operating as the user's own identity. A separate
bot account was considered and the owner rejected it as the primary design
target. Note an unresolved empirical question: whether macOS-desktop and
Linux-desktop logins of one account can coexist (multi-device rules have been
loosening; 5-minute test).

## 3. Design goals (as stated during the session)

- **G1 — Same identity**: operate the user's real, already-logged-in WeChat
  (same account); no bot account as the primary path.
- **G2 — General-purpose**: target the general problem — an AI operating an
  arbitrary desktop, not just WeChat.
- **G3 — Zero interference + takeover**: agent works in the background; the
  user's real mouse/keyboard/screen are untouched; the user can always watch
  and take over. Inspired by the "Doubao phone" model and Android's virtual
  display concept: a virtual screen with virtualized input.
- **G4 — Pixel-first**: the perception loop reasons over pixels (industry
  direction: OpenAI/Anthropic computer-use models take screenshots, not
  accessibility trees). Capture of background windows must work.
- **G5 — Integration fidelity**: preserve macOS richness — drag-and-drop,
  native file permissions, system services. The owner flagged filesystem
  bridging (WSL as the cautionary tale: transparent whole-filesystem mounts
  with permission/semantic mismatches) as a primary objection to VMs,
  containers, and separate sessions.
- **G6 — Cross-platform potential**: the architecture should port across
  macOS, Windows, Linux (incl. Wayland), Android; iOS understood to be
  excluded as a host.

## 4. Design options evaluated (with verdicts)

### 4.1 In-process injection into WeChat (macOS)

Drive the stripped C++ kernel (`message_service`/`kernel::MessageWrapper`/Mars
`newsendmsg`) from an injected helper behind an owner-only socket. Technically
anchored (unencrypted binary, RTTI + string anchors, no anti-debug), zero
interference, same account, same session. Cost: per-build disassembly campaign
on every WeChat update; all account risk on the personal account; works for
WeChat only. **Viable for G1, wrong shape for G2.**

### 4.2 AX automation (macOS)

Ideal profile (out-of-band, background) but **falsified**: WeChat 4.1.13
exposes no tree even under VoiceOver. Not general on macOS. (On Windows, UIA
is the first thing to try against Qt WeChat.)

### 4.3 Second user session + Screen Sharing mirror (macOS)

A second macOS user's session runs concurrently with its own WindowServer,
input, and virtual display; a local Screen Sharing window mirrors it at
near-zero latency. Key insight: WeChat's one-session rule is per *WeChat
account*, not per *macOS user* — the user's real WeChat can run exclusively in
the second session (console runs none), same account, zero interference, native
takeover (input from the mirror window and the agent merge in that session's
queue). Cost: second macOS user, per-user TCC, file bridge (see 4.7), loss of
console-side drag-and-drop integration with the user's other apps. Same user
twice in two sessions: **not supported** on macOS.

### 4.4 VM appliance (tart / Hyper-V)

Full virtual Mac; strongest containment and snapshot/reset; VM images
distributable as OCI artifacts. Cost: 14 GB IPSW, per-VM resources, file
bridge, same integration loss as 4.3. **The right answer for containment of
untrusted general agents, regardless of other choices.**

### 4.5 Burst focus-stealing (macOS, in-session)

Perception works fully backgrounded (occluded-window capture is native). Only
execution surfaces: activate target, warp cursor, act (paste via dedicated
pasteboard), restore previous app and cursor. A single scripted action fits in
~200 ms, but collision with in-flight human input is statistically constant,
fullscreen apps trigger Space-switch animation, and vision-driven
observe→act→verify loops run seconds per turn — bursts compress execution of
known actions, not interaction loops. **Acceptable for occasional scripted
sends; fails G2/G3 for general use.**

### 4.6 Private SkyLight per-connection event posting (macOS) — OPEN

Undocumented WindowServer routines (historically `SLPSPostEventRecordTo` and
per-connection keyboard posting) can deliver events directly to a specific
application's connection, bypassing focus — the missing macOS analog of
Windows' `PostMessage`. No cursor movement, no activation, one mechanism for
all apps. Version-fragile, unsupported, needs re-verification per OS release.
**The highest-value open experiment: if it works on macOS 26, true
background computer use becomes possible in-session with full G5 fidelity.**
Bounded spike: days, lab-notebook style, against hostile targets (WeChat,
Catalyst, Electron).

### 4.7 File-bridge design (answers G5 for 4.3/4.4)

WSL's pain comes from transparent, total, bidirectional mounts. The
counter-design: narrow, explicit, copy-at-the-boundary handoff — one drop
directory (`in/`, `out/`), per-task grants, clipboard bridge, and reuse of the
mirror channel's native file payloads (Screen Sharing drag-and-drop, scrcpy
drag-and-drop, virtiofs, SFTP loopback, container bind mount with matching
uid). Agent-internal state (the bulk of file I/O) never crosses. The boundary
is the same property as the input/display isolation requested in G3 — it
should be designed as a user-controlled grant, not dissolved.

### 4.8 Codex-style co-pilot (macOS, in-session, foreground sharing)

Agent works in the foreground; human takeover is instant (any human input
event aborts the agent's burst). Full session fidelity, no infrastructure,
zero file friction — at the cost of concurrency. This is the only
zero-research macOS-native shape for G2, and matches a "watch the agent work"
UX.

## 5. Recommended architecture (current position)

One product, three layers; only the substrate adapter varies per platform:

- **Control plane (one codebase):** task queue, input-ownership policy
  (agent yields on human activity; explicit takeover), audit log
  (generalizes the GreenBubbles action-safety contract), snapshot/reset.
- **Perception/action loop (one codebase):** pixels in, window-relative
  coordinates out, verification by re-capture.
- **Substrate adapters:** macOS — VM (tart) or second-user session; Windows —
  in-session WGC + `PostMessage`/UIA by default, Hyper-V/Sandbox as secure
  mode; Linux — container + Xvfb or nested headless compositor + wayvnc
  (host-session-agnostic); Android — device/emulator + AccessibilityService,
  scrcpy as mirror; iOS — integration target only.

**Sequencing for macOS (the owner's platform):**

1. **Co-pilot mode now** (4.8 + 4.5 mechanics): shippable with no research;
   establishes the control plane and pixel loop.
2. **SkyLight spike in parallel** (4.6): decides whether macOS upgrades from
   co-pilot to true background. If positive, the owner's virtual-display
   arrangement (background apps on a second visible screen, agent
   imperceptible) becomes real in-session.
3. **Separate-world tier regardless** (4.4/4.3): for containment of untrusted
   agent activity — isolation-for-security and isolation-for-input are
   different requirements; a general agent needs the former even if the
   SkyLight path works.

## 6. Open questions for the consultant

1. **SkyLight viability on macOS 26** (4.6): do per-connection event-posting
   routines still work, against which app archetypes, and how fragile across
   OS updates? Is relying on them product-defensible vs. co-pilot only?
2. **Interference tolerance**: for the target use cases, is Codex-style
   foreground sharing (4.8) acceptable UX, making background input a
   nice-to-have rather than a requirement?
3. **G5 vs. G3 weighting**: is the file-bridge design (4.7) sufficient to make
   the separate-world tier feel native, or does drag-and-drop/system
   integration fidelity strictly require in-session operation?
4. **WeChat session arithmetic** (2.4): can macOS-desktop and Linux-desktop
   sessions of one account coexist? If yes, an Xvfb-hosted Linux WeChat of the
   user's own account becomes possible without touching the Mac client at all.
5. **Account-risk posture**: for each pathway (injection 4.1, SkyLight 4.6,
   co-pilot 4.8, separate-world 4.3/4.4), what is the realistic
   detection/enforcement exposure, and does it change the sequencing?
6. **Bot-account revisit**: given G1's constraints materially drive the
   architecture, is a dedicated agent identity genuinely unacceptable, or
   acceptable as a secondary mode?
7. **Legal/distribution review**: independent of engineering, what is
   distributable (the GreenBubbles docs already treat this as an open Phase 0.5
   gate)?

## 7. Environment and artifacts (for reproduction)

- Host: Apple Silicon (arm64), macOS 26.6.2, 96 GB RAM, 12 cores, 109 GB free
  disk; Homebrew 6.0.19; tart 2.32.1 installed (unused beyond install).
- WeChat: `/Applications/WeChat.app`, pid changed across the session
  (65614 → 51797 → 53099); ad-hoc signed (`flags=0x2(adhoc)`, no team ID).
- Artifacts: `.tmp/re-search/` (0700) — `nm-arm64.txt`, `strings-arm64.txt`,
  `otool-l-arm64.txt`, `runtime-classes.txt`, `classes-by-image.json`,
  `methods-*.txt`, `image-addresses.txt`, `ax-tree.txt`, `ax_enum.swift`,
  `STATIC_FINDINGS.md`, `RUNTIME_FINDINGS.md`; consolidated findings in
  `docs/SEND_PATH_RE_FINDINGS.md` (including the AX negative result and the
  residual-seam analysis).
- System state restored: VoiceOver re-disabled after the AX probe; WeChat
  relaunched and healthy; no lldb sessions remain attached; no WeChat internal
  function was ever invoked.
