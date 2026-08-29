# Send-integration design: deterministic UI-automation adapter (production)

Date: 2026-08-29. Status: **implemented; superseded as the operational
reference by `SEND_ADAPTER.md`.** This document remains the design of record —
the reasoning, the options compared, and the decisions locked with the owner.
What was built follows it, with four recorded deviations (`SEND_ADAPTER.md`
§11), the largest being that the effector is first-party public-API code rather
than a vendored third-party engine.

The adapter ships **closed**: a build with no pinned release verifying key
cannot verify a release calibration profile, so no rollout stage above `dryRun`
can open, and the gate-evidence flags in §10's PRECHECK still stand for the
mechanism, legal, and account-safety decisions that have not been made.

Exploratory work to date was read-only or, on the owner's explicit instruction,
a single reversible self-message to `File Transfer` (`GBSPIKE-BG 102651`,
recallable). No message has been sent by the implemented adapter.

Companion evidence: `SEND_ADAPTER.md` (as built), `SEND_PATH_RE_FINDINGS.md`,
`AI_DESKTOP_AGENT_HANDOFF.md`, the 2026-08-29 cua-driver spike,
`ACTION_SAFETY_CONTRACT.md`, `AI_TOOL_BOUNDARY.md`.

---

## Part I — Mechanism (established)

### 1. Why sending is categorically different from receiving

Receiving is data at rest: WeChat has written messages to local SQLite;
GreenBubbles reads them (policy-gated, `tools.rs` / `live_query.rs`). Sending
has no data-at-rest equivalent — a sent message is a network transmission only
WeChat's in-process C++ pipeline (Mars `newsendmsg`, MMTLS) can produce.
`SEND_PATH_RE_FINDINGS.md` established there is **no DB write, no IPC seam, no
protocol to mimic** without per-build injection and account risk. So the only
same-identity, non-injection send path is to **drive the real client's UI**,
which the spike proved is automatable in the background.

### 2. Mechanism proven by the spike (2026-08-29)

macOS 26.6.2 / WeChat 4.1.13 / cua-driver 0.22.2, WeChat backgrounded
throughout. Proven (each re-screenshot-verified): AX-dead chat surface;
background CGEvent-to-pid **click focuses** the Qt search and compose boxes with
no raise and no cursor warp; background **paste** and **synthesized keys**
(`Cmd+A`, `Delete`, `Return`) land; **end-to-end background send** delivered a
self-message. Caveats recorded: a one-off ~2-minute daemon stall (send had
already succeeded), and the private-framework path is version-fragile.

### 3. Methodology: mouse focuses, keyboard acts

Mouse click (pixel-located, background) = *choose/focus* a box or press a
control — the only way to focus a field on WeChat (no AX tree). Keyboard
(background) = *type, paste, send* into whatever was focused; sending is
`Return`. Invariant: **click to focus, then keys.** OCR-based verification uses
on-device Apple Vision (no LLM, no network) — validated: title `"File
Transfer"` at confidence 1.00 in ~277 ms; sent bubble read at conf 1.00.

---

## Part II — Production integration

### 4. Integration model: vendor the engine, ship a first-party helper

**The prototype used the upstream `CuaDriver.app` daemon installed via
`curl | bash`. That is explicitly rejected for production.** No shipped product
should require users to run a third-party installer, grant permissions to a
third-party app identity (`com.trycua.driver`), or depend on an out-of-band
release cadence.

Three options were compared:

| Option | TCC identity | Update control | Verdict |
| --- | --- | --- | --- |
| **A. Runtime `curl \| bash` of upstream daemon** | third party | none | **Rejected** — supply chain, third-party identity, unmanaged |
| **B. Bundle upstream `CuaDriver.app`, manage its lifecycle** | third party (or re-signed) | pinned release | Interim only; converges to C once re-signed |
| **C. Vendor the MIT engine into a first-party, own-signed helper** | **ours (notarized)** | **ours** | **Recommended** |

cua-driver is MIT and supports **embedded / app-hosted integration** (not only a
standalone daemon), and its Rust core is UniFFI-bindable. The private frameworks
it uses (SkyLight) are **Apple-signed system libraries**, so a **Developer-ID,
notarized, hardened-runtime app can `dlopen` them and pass notarization**
(notarization scans for malware, not private-API use). Therefore:

- Vendor a **pinned commit** of the MIT engine, build it in **our CI**, and ship
  a **first-party effector** signed with **our Developer ID** and notarized.
- Distribution is **Developer-ID direct (DMG/pkg), not the Mac App Store** — the
  MAS forbids private frameworks. This is a hard, stated constraint.
- MIT attribution is preserved in the bundle and about box.

**Privilege separation.** The effector is a **separate first-party helper**
(`GreenBubblesInputHelper.app`), not the main app, so the powerful grants
(Accessibility + Screen Recording, i.e. "control/observe any app") are confined
to a minimal component that does only input/capture. The main GreenBubbles app
(which holds the encrypted replica, decryption keys, policy, audit) holds
**no** input grants and talks to the helper over an **owner-only, authenticated
local IPC** (XPC or a `0600` Unix socket with a per-launch token). Compromise or
misuse surface is thereby minimized and auditable.

### 5. Packaging, signing, distribution

- **Deliverable:** notarized DMG (or signed `pkg`) containing the main app and
  the embedded `GreenBubblesInputHelper.app`. No network fetch of executables at
  runtime; everything is in the signed bundle.
- **Code signing:** Developer-ID Application; **Hardened Runtime enabled**
  (required for notarization). Entitlements: the helper links only Apple-signed
  system frameworks + our same-team-signed vendored engine, so **library
  validation stays on** (no `disable-library-validation` needed); no JIT / no
  unsigned-dylib loading. Screen Recording + Accessibility are TCC grants, not
  entitlements.
- **Not sandboxed** (the helper needs cross-app control; the App Sandbox would
  forbid it) — consistent with Developer-ID-only distribution.
- **Reproducible builds + SBOM** for the vendored engine; pinned commit hash
  recorded in the bundle for support/audit.

### 6. Installation & first-run onboarding

Installation is a normal notarized-app install (drag-to-Applications or `pkg`).
The **one unavoidable manual step is TCC** — macOS does not let any software
grant itself Accessibility/Screen Recording (confirmed in the spike; the
operator could not flip the toggles). Production handles this with a guided
**permissions onboarding**:

1. Detect missing grants via a live capability probe (attempt a scoped
   no-op capture/synthetic event against a first-party window; never assume the
   toggle state).
2. Deep-link to the exact panes
   (`x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility`
   and `…?Privacy_ScreenCapture`), with inline instructions and the macOS 26
   direct-capture consent handled.
3. **Poll + verify** until the probe passes; the send path stays **closed**
   until it does. Grants attribute to `GreenBubblesInputHelper` (our identity),
   so the System Settings entry is our name, not a third party's.

Preflight also detects: WeChat installed, running, logged in, and a **known
WeChat build** (see §8). Any failure surfaces an actionable message and keeps
send disabled — fail-closed.

### 7. Engine lifecycle & supervision

- **Launch:** the helper is started **by the main app** (managed
  `SMAppService` / per-user LaunchAgent), never by the user manually and never
  via `curl | bash`. It runs only while GreenBubbles needs it and can be pinned
  to run at login if the product wants background sends.
- **Confinement:** the engine runs in cua-driver **`bounded` permission mode
  with a capability manifest** (`CUA_DRIVER_CAPABILITY_MANIFEST_FILE`,
  `CUA_DRIVER_CAPABILITY_MANIFEST_APPROVED`) scoped to **WeChat only** — one app,
  the specific tools needed (`click`, `hotkey`, `press_key`, `clipboard_write`,
  window read/capture), and no file roots or browser origins. Holding broad TCC
  grants but exercising a narrow manifest is the least-privilege posture.
- **Supervision:** every effector call is wrapped in a **bounded timeout** with
  a **watchdog** (the spike's 2-minute stall must never block a caller); on
  stall, the call is abandoned, state is verified **out-of-band** (independent
  capture), and the helper is health-checked and restarted if unresponsive.
- **Single-flight outbox + idempotency:** at most one send in flight; the
  idempotency key (from the approved draft) is reserved before Return and
  consumed only after reconciliation, so a crash/restart mid-send **never
  double-sends** — recovery re-reconciles against the replica instead of
  resending.

### 8. Versioning, compatibility, and updates

Two independent fragility axes — the macOS private-API path and WeChat UI layout
— require managed compatibility, not a hardcoded prototype:

- **Compatibility matrix** of (macOS build × WeChat build) states: `supported`,
  `unverified`, `blocked`. Unknown/`unverified` combinations **fail closed**
  (send disabled) until validated.
- **Calibration profiles are data, not code.** Window-relative anchors and OCR
  regions (§10) live in a **signed, remotely-updatable profile** keyed by WeChat
  build, so a WeChat layout change is fixed by shipping a profile — **not** an
  app rebuild. A **calibration self-test** (locate + focus the search box,
  OCR-confirm, no send) gates every profile before first use.
- **Engine updates** (when a macOS point release breaks the SkyLight path) ride
  the normal app-update channel; because we vendor and sign, we can fast-follow
  upstream and re-verify.
- **Remote kill-switch** can disable the send path in the field independently of
  updates, honoring the contract's global kill-switch.

### 9. Observability, diagnostics, support

- **Structured logs** (helper + adapter), plus the existing tamper-evident
  **audit journal** (`CONNECTOR_AUDIT.md`) for every attempt/denial (bodies
  excluded).
- A **`health/doctor` preflight** (grants, WeChat state, build match,
  calibration validity, engine responsiveness) that a user or support can run to
  answer "why is send disabled/failing" with a precise cause.
- **Failure taxonomy** surfaced to the user: `grants-missing`,
  `wechat-not-running`, `not-logged-in`, `unknown-build`, `calibration-drift`,
  `recipient-verify-failed`, `content-verify-failed`, `send-unconfirmed`,
  `engine-stall`. Each maps to an action, and all of them keep send closed.

### 10. Reliability of the send action (state machine + gates)

Input: one **approved draft** binding account, conversation, human-readable
recipient evidence, exact body, reply target, attachment digests, expiry,
requester, policy decision, checkpoint, and a one-use idempotency key.

```
0. PRECHECK (action.rs; before ANY effector call)
     kill-switch off; approval valid+unexpired; idempotency unreserved;
     rate/attempt-window capacity; grants present; WeChat running+logged-in;
     WeChat build supported; calibration profile valid.
1. CALIBRATE  read live window frame -> window-relative anchor pixels
2. ADDRESS    click search box; Cmd+A+Delete; paste recipient key; click top result
3. GATE 1 RECIPIENT VERIFY (abort-closed)
     OCR title (Apple Vision) == draft.recipient_display_name
     AND cross-check vs DB replica identity (tools.rs ListConversations /
         CanonicalConversation for the bound conversation_id)
     mismatch / ambiguous / low-confidence -> ABORT, audit denial
4. COMPOSE    click compose box; paste exact body
5. GATE 2 CONTENT VERIFY (abort-closed)
     OCR compose region == draft.body (normalized) else ABORT (clear box)
6. SEND       press Return   (idempotency key reserved just before)
7. GATE 3 SEND VERIFY / RECONCILE
     compose cleared AND newest outgoing bubble OCR == body
       -> attempted -> observedSent, reconcile vs replica (reconcile.rs)
     inconclusive -> unknown -> deferred reconciliation, NEVER a blind resend
8. COMMIT     consume idempotency key; append audit event(s)
```

Cross-cutting reliability: bounded timeout + watchdog on every step; circuit
breaker disables the send path after N consecutive gate/engine failures
(fail-closed); **human-collision yield** — any real user activity on WeChat
aborts the in-flight skill before Return and clears a partial compose;
takeover always wins.

Two independent verification sources at GATE 1 by design: **OCR** proves what is
on screen *now*; the **DB replica** proves the identity mapping for the bound
`conversation_id`. A wrong-recipient send is the one catastrophic failure, so
GATE 1 is abort-closed on any doubt. OCR is **Apple Vision** (first-party,
public, on-device). WeChat's own `wxocr` is deliberately **not** used: it is an
internal, closed dylib loaded only by WeChat's own helper with WeChat-specific
resources — not a public or open-source API — so calling it would mean loading a
third-party private library into our process (RE-fragile, per-build, unsupported,
injection-adjacent), the exact dependency class this design exists to avoid. If a
redundant engine is ever wanted, it is a **we-bundle** open-source one (e.g.
Tesseract), never WeChat's.

### 11. Mapping onto the existing safety contract

| Contract element (`ACTION_SAFETY_CONTRACT` / `AI_TOOL_BOUNDARY`) | Realization |
| --- | --- |
| Immutable draft bound to recipient evidence | Skill input |
| Approval evidence (SHA-256 binding, local approver, validity interval) | PRECHECK |
| Idempotency unused / rate capacity / kill-switch off | PRECHECK; key consumed at COMMIT |
| Denial before any client/network invocation | All gates abort-closed before Return |
| `attempted` / `observedSent` / `observedFailed` / `unknown` | Steps 6–7 via OCR + replica reconciliation (`reconcile.rs`); never effector-asserted |
| Adapter-owned atomic reservation incl. restart | Single-flight outbox + idempotency |
| Redacted reconciliation evidence | GATE 3 + audit append |

The cua-driver effector is the concrete form of the contract's abstract
"selected adapter boundary." `observedSent` is only ever created by later
replica/OCR reconciliation, exactly as required.

### 12. Uninstall & teardown

A supported uninstall: stop and remove the helper (`SMAppService` unregister /
LaunchAgent removal), delete both bundles, and guide the user to revoke the two
TCC grants (with `tccutil reset Accessibility` / `ScreenCapture` for our bundle
id as the scripted path). No third-party remnants, since nothing third-party was
installed.

### 13. Security & supply chain

Vendored engine pinned by commit hash and built in our CI (no runtime binary
download); reproducible build + SBOM; own-signing + notarization; least-privilege
via bounded manifest; privilege-separated helper with authenticated owner-only
IPC; owner-only `0600` audit + calibration state; MIT attribution retained.

### 14. Cross-platform abstraction

The adapter sits behind a platform-neutral `SendSkill` interface (focus,
enter-text, activate/send, capture, verify). This macOS implementation is one
backend; Windows (UIA/`PostMessage`) and Android (AccessibilityService/scrcpy)
adapters from `AI_DESKTOP_AGENT_HANDOFF.md` can implement the same interface
without changing the control plane, gates, or contract mapping.

---

## Part III — Rollout and open decisions

### 15. Phased rollout (each gated; no auto-progression)

- **A — Dry run, no send.** First-party helper + onboarding + bounded manifest;
  run steps 1–5 against `File Transfer` including both verification gates
  (with deliberate mismatch tests); **stop before Return**. Zero send risk.
- **B — Self-send.** Send to `File Transfer` only, behind full PRECHECK,
  idempotency, reconciliation, recall path.
- **C — Allow-listed recipient.** One reviewed disposable/test peer, volume caps,
  reconciliation.

Production readiness (Part II) is a prerequisite for B, not an afterthought.

### 16. Decisions (locked 2026-08-29 with owner)

1. **IPC — XPC.** macOS-native; the helper can require the caller be signed by
   our team (`setCodeSigningRequirement`), giving verified peer identity for
   free. A raw `0600` socket + hand-rolled token reinvents auth for a component
   that holds powerful grants. Pick XPC.
2. **Grant-holder — separate first-party helper** (a login-item process via
   `SMAppService`, shipped inside the one app bundle: **one install, two
   processes**). It alone holds Accessibility + Screen Recording; the main app
   (decryption keys + plaintext replica + LLM + untrusted message content) holds
   none. Two grounded reasons: (i) **privilege separation vs. this project's own
   prompt-injection threat model** (`AI_TOOL_BOUNDARY.md`) — desktop-wide
   input+capture must not sit on the process that parses attacker-controlled
   messages and holds the keys; (ii) **crash isolation** — the effector is
   observably stall/crash-prone (the 2-minute stall; documented per-release
   SIGABRTs), and a separate process can be watchdog-killed and relaunched
   without wedging the read side.
3. **Path — C (vendor the MIT source, build in our CI).** See §16a. A throwaway
   B spike is permitted only to validate the flow; nothing built from an
   unaudited prebuilt binary ships.
4. **OCR — Apple Vision.** First-party, public, on-device, validated (conf 1.00).
   WeChat's `wxocr` is not an option (closed internal dylib; see §10).
5. **Calibration profiles — signed, app-verified, fail-closed, out-of-band
   updatable.** Profiles ship signed with our key; the app verifies before load
   and refuses unknown/invalid; a WeChat layout change is pushed as a signed
   profile without a full app release.
6. **Distribution — Developer-ID direct only (settled; no MAS); personal-use
   first.** Ship as the owner's own-account tool first; keep disposable-identity
   for any later distribution; the distributable decision and its identity/
   account-risk posture stay behind the Phase 0.5 legal gate.

### 16a. The speed-vs-control trade-off (Path B vs Path C)

- **B — bundle the upstream prebuilt binary, re-signed with our identity.**
  *Faster:* reuse their tested artifact, minimal build integration, ship sooner.
  *Less control:* pinned to their release cadence; we ship a binary we did not
  compile (weaker supply-chain assurance even when re-signed); when a macOS point
  release breaks the SkyLight path we **wait for upstream** to fix it — the send
  path can be dark meanwhile.
- **C — vendor a pinned commit of the MIT source, build it in our CI.**
  *Slower to set up:* integrate their Rust build (UniFFI, cross-compile, CI),
  audit the code. *More control:* we compile from audited source (build-what-you-
  read), can **patch the private-framework shims ourselves** the day macOS breaks
  them, get reproducible builds + SBOM naturally, and strip unused features.

The trade is **time-to-first-build vs. field self-sufficiency and auditability**.
Because the path is version-fragile and the capability is sensitive, C is the
production target.

### 17. Explicitly out of scope / excluded

Runtime `curl | bash`; dependence on a third-party daemon identity; Mac App
Store distribution; in-process injection or wire reimplementation; any LLM in
the send decision loop; bulk/marketing automation; a bot account as the primary
identity; absolute-screen coordinates; trusting the effector's own success
without out-of-band verification.

---

## Part IV — Implementation plan

### 18. Component & toolchain map

| Component | Process / identity | Grants | Toolchain | Responsibility |
| --- | --- | --- | --- | --- |
| **Main control plane** | main app (existing identity) | **none** | Rust (`Native/GreenBubbles`) + Swift host (`Sources/GreenBubblesHistoryApp`) | policy, approval PRECHECK, DB/keys, mint the bound action capability, reconcile, audit, XPC **client** |
| **`GreenBubblesInputHelper`** | login-item process (own identity, `SMAppService`) | Accessibility + Screen Recording | Swift/AppKit login item linking the **vendored cua-driver Rust engine** (static lib, C ABI) + **Apple Vision** | XPC **server**; execute the mechanical send-skill in `bounded` mode scoped to WeChat; capture + OCR; enforce on-screen gates against the capability |

New Rust module `send_adapter` (extends `action.rs`): PRECHECK integration,
capability minting, reconciliation via `reconcile.rs`, audit append. It performs
no input and holds no grants.

Data flow: main app resolves recipient from the replica (keys + `tools.rs`
`ListConversations`/`CanonicalConversation`) → mints a **single-use bound action
capability** → XPC → helper executes the skill and checks its own capture/OCR
against the capability → returns outcome → main app does authoritative
reconciliation against the replica.

### 19. Trust split — the bound action capability

The helper **never sees keys or the replica**. It receives only a single-use,
authenticated capability:

```
ActionCapability {
  search_key         // what to type into the search box
  expected_title     // GATE 1: OCR of the opened conversation title must equal this
  body               // GATE 2/GATE 3: exact text to type, and to confirm in the bubble
  idempotency_key    // one-use; reserved before Return
  approval_ref       // binds to the approver's evidence (main app already verified)
  valid_until        // short expiry
}
```

Because the main app has already resolved the DB → `expected_title` mapping and
bound it into the capability, the helper enforces the recipient gate **without
the DB**. Consequences: the helper's XPC surface is **high-level and narrow** (no
raw "type-anywhere" primitive is exposed), so a runtime-compromised main app can
only submit capabilities that pass PRECHECK — it cannot mint approval evidence,
cannot target a recipient the capability is not bound to, and the helper refuses
if the on-screen title ≠ `expected_title`. The `bounded` manifest independently
confines the engine to WeChat.

### 20. XPC contract (surface)

Three methods; peer identity pinned via `setCodeSigningRequirement` (same team);
all timeouts owned by the client (helper stalls never block the caller):

- `capability_status() -> { grants, wechat_state, engine_health, active_profile }`
  — read-only preflight for onboarding and `doctor`.
- `run_calibration_selftest(signed_profile) -> { ok, drift_report }` — locate +
  focus the search box, OCR-confirm, **no send**.
- `execute_send(ActionCapability) -> SendOutcome { attempted, visual_confirmed |
  unconfirmed, evidence }` — the whole mechanical skill (§10) with GATE 1/2 and
  the immediate post-send visual check; authoritative `observedSent` is decided
  later by the main app via replica reconciliation.

### 21. Calibration-profile format

Signed data, not code (§8), keyed by WeChat build × macOS major; loader verifies
signature + build match and **fails closed** on unknown/invalid:

```jsonc
{
  "schema": 1,
  "wechat_build": "4.1.13.269579",
  "macos_major": 26,
  "anchors": {                       // window-relative fractions [0..1]
    "search_box":        {"x": 0.235, "y": 0.036},
    "first_result_row":  {"x": 0.235, "y": 0.115},
    "compose_box":       {"x": 0.715, "y": 0.870}
  },
  "ocr_regions": {                   // window-relative rects
    "title":          {"x": 0.44, "y": 0.02, "w": 0.30, "h": 0.05},
    "newest_outgoing":{"x": 0.62, "y": 0.70, "w": 0.28, "h": 0.20}
  },
  "selftest": { "focus_indicator": "search_caret" },
  "signature": "…"                   // our key; verified before use
}
```

### 22. Build, signing, distribution pipeline

Vendor a **pinned cua-driver commit** (submodule); build the engine static lib +
C header in **our CI**; link into the helper; Developer-ID sign **both** app and
helper; **Hardened Runtime**; **notarize + staple** the DMG; embed the pinned
commit hash + an **SBOM**. No runtime executable download. **No Mac App Store.**

### 23. Milestones (each gated to §15; no auto-progression to a send)

**Implementation status, 2026-08-29.** M0-M3 and M6 are complete; M4 and M5 are
complete in code and gated shut by configuration and by the absent release key.
The one narrowing is automated recall (M4), which is deliberately not shipped:
recall is a context-menu action whose item position varies with message type,
age, and locale, so a mis-aimed click there is exactly the catastrophic class
the gates exist to prevent. The adapter surfaces the recall window and the
exact manual steps instead (`send recall-window`).

- **M0 — Skeleton.** Helper login item + XPC handshake + `capability_status` +
  permissions onboarding + `bounded` manifest scoped to WeChat. No capture, no
  send. *(builds Rollout A infra)*
- **M1 — Perception.** Capture + Apple Vision OCR; calibration-profile format,
  signed loader, self-test. *(Rollout A)*
- **M2 — Dry-run send.** `execute_send` runs steps 1–5 and **stops before
  Return**; GATE 1/2 enforced against the capability; adversarial mismatch tests.
  *(Rollout A complete)*
- **M3 — Control-plane integration.** `send_adapter`: capability minting from an
  approved draft, PRECHECK via `action.rs`, idempotency/outbox, audit,
  reconciliation via `reconcile.rs`. Still dry-run.
- **M4 — Self-send.** Enable Return + GATE 3 + reconciliation for **`File
  Transfer` only**; recall path. *(Rollout B)*
- **M5 — Allow-listed recipient.** One reviewed peer; volume caps. *(Rollout C)*
- **M6 — Hardening.** Watchdog/circuit-breaker, compat matrix, remote kill-switch
  + profile update channel, `doctor` diagnostics, supported uninstall.

### 24. Testing & verification

Deterministic trajectory replay; **adversarial gate tests** (wrong title, wrong
content, ambiguous search → all must ABORT before Return); **fault injection**
(engine stall, crash, mid-send restart → idempotency proves no double-send);
calibration-drift simulation → fail-closed; onboarding-state matrix; and
reconciliation correctness checked against the encrypted replica.
