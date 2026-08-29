# Send adapter: implementation, operation, and limits

Status: **implemented, shipped closed.** This document describes what is built,
how to operate it, and what it deliberately refuses to do. It is the
implementation counterpart to `SEND_INTEGRATION_DESIGN.md`; where the two
differ, the deviations are recorded in §11 with their reasons.

The adapter sends a message by driving the real WeChat client's user interface
from a privilege-separated, first-party helper. Nothing is injected into
WeChat, no wire protocol is reimplemented, no private network API is called,
and no third-party binary is downloaded or run. `SEND_PATH_RE_FINDINGS.md`
establishes why no other same-identity path exists.

**A default build cannot send.** The release verifying key is empty unless a
release pipeline pins one, so no release calibration profile verifies, so no
rollout stage above `dryRun` can open. Turning the send path on is a deliberate,
multi-step, owner-driven act, and every step of it is auditable.

---

## 1. Components and where the privilege lives

| Component | Process | Grants | Holds |
| --- | --- | --- | --- |
| Control plane (`greenbubbles send …`) | main application | **none** | keys, encrypted replica, policy, drafts, outbox, audit journal |
| `greenbubbles-send` | short-lived child | **none** | nothing; a bounded XPC bridge |
| `GreenBubblesInputHelper` | login-item agent | Accessibility + Screen Recording | one capability at a time, nothing else |

The split exists for two grounded reasons. First, this project's own
prompt-injection threat model (`AI_TOOL_BOUNDARY.md`): desktop-wide input and
capture must not sit on the process that parses attacker-controlled message
content and holds the decryption keys. Second, crash isolation: an effector is
stall- and crash-prone, and a separate process can be watchdog-killed and
relaunched without wedging the read side.

The helper's XPC listener pins its peers with
`setCodeSigningRequirement`, so only a binary signed by the same team may
connect. That verified peer identity is what lets the XPC surface stay
high-level — three methods, no raw "type anywhere" primitive.

## 2. The bound action capability

The helper never receives a key, a replica handle, a policy, or any message
history. It receives one single-use capability
(`Sources/GreenBubblesSendKit/SendContract.swift`,
`Native/GreenBubbles/src/send_contract.rs`):

```jsonc
{
  "capabilityId": "…", "actionId": "…", "draftId": "…", "approvalId": "…",
  "idempotencyKey": "…",
  "searchKey":     "File Transfer",   // what to type into the search box
  "expectedTitle": "File Transfer",   // GATE 1 must read exactly this
  "body": "…", "bodySha256": "…", "normalizedBodySha256": "…",
  "calibrationProfileId": "…", "calibrationProfileSha256": "…",
  "rolloutStage": "dryRun", "permitSend": false,
  "issuedAtUnixNanoseconds": …, "validUntilUnixNanoseconds": …,
  "bindingSha256": "…"
}
```

The control plane has already resolved the recipient from the replica, so the
helper enforces the recipient gate **without the database**. Consequences: a
runtime-compromised control plane can only submit capabilities that pass
PRECHECK; it cannot mint approval evidence, cannot target a recipient the
capability is not bound to, and the helper refuses if the on-screen title is
not `expectedTitle`. `permitSend` is false in every dry run, and a capability
whose stage is `dryRun` but whose `permitSend` is true fails its own
self-consistency check on both sides of the boundary.

## 3. The mechanical skill and its gates

`MechanicalSendSkill` (`Sources/GreenBubblesSendKit/SendSkill.swift`) is the
platform-neutral state machine. The methodology from the 2026-08-29 spike is
preserved exactly: **the mouse only focuses; the keyboard acts.**

```
0. PRECHECK        capability self-consistency, profile identity, manifest
1. CALIBRATE       read the live window frame -> window-relative anchors
2. ADDRESS         click search; Cmd+A; Delete; paste search key; click result
3. GATE 1          OCR the title region == expectedTitle, high confidence,
                   exactly one candidate               -> else ABORT
4. COMPOSE         click compose; Cmd+A; Delete; paste body
5. GATE 2          OCR the compose region == body      -> else ABORT (clear it)
   -- dryRun stops here, having cleared the compose box --
6. SEND            yield if a human is active, then press Return
7. GATE 3          compose cleared AND newest bubble contains body
                   -> confirmed, else unconfirmed
```

Every abort clears a partial compose and restores the user's clipboard. Any
real hardware input observed on the client aborts the run before Return:
takeover always wins.

The effector posts events with `CGEvent.postToPid`, a public CoreGraphics
entry point, so the target receives the click or keystroke while the user's
physical cursor never moves and no application is raised. Capture uses
ScreenCaptureKit's window filter, so WeChat can stay backgrounded and occluded
throughout. Recognition is Apple Vision: first-party, public, on device.
WeChat's own `wxocr` is deliberately not used (see `SEND_INTEGRATION_DESIGN.md`
§10).

**Two independent verification sources at GATE 1 by design.** OCR proves what
is on screen now; the DB replica proved the identity mapping for the bound
`conversation_id` when the capability was minted. A wrong-recipient send is the
one catastrophic failure, so GATE 1 is abort-closed on any doubt — including
a second plausible title in the region, which is treated as an ambiguous search
result rather than a guess.

## 4. Calibration profiles and the compatibility matrix

Both are **signed data, not code**, so a WeChat layout change is fixed by
shipping a profile rather than rebuilding the application.

Window-relative geometry is carried as integer **parts-per-million**, not
floating point, so the signed bytes are exactly reproducible in both languages.
The canonical encoders are hand-written in Rust and Swift and pinned against
one another by `docs/send-canonical-vectors.json`, which both test suites
assert on; the Swift suite additionally verifies a *Rust-signed* profile with
CryptoKit, so a profile shipped to the field is provably accepted by the
component that enforces it.

Verification fails closed on: an empty trust root, a malformed or non-verifying
signature, an unsupported schema, a structurally invalid document, a validity
window not yet open or already closed, a profile the matrix does not name for
this client build, and a (macOS build × WeChat build) pair the matrix does not
mark `supported`. An unknown pair is reported `unverified`, which never permits
a send.

```sh
# Once, on the release machine, offline:
greenbubbles send profile-keygen ~/.greenbubbles/send/signing-key.json
# Pin the printed public key into the binaries at build time (see §8).

greenbubbles send profile-template > /tmp/profile-body.json   # then measure
greenbubbles send profile-sign /tmp/profile-body.json \
  ~/.greenbubbles/send/calibration-profile.json --signing-key-file …
greenbubbles send matrix-sign  /tmp/matrix-body.json \
  ~/.greenbubbles/send/compatibility-matrix.json --signing-key-file …
```

### The field kill switch

`globalKillSwitchEngaged` lives inside the **signed** compatibility matrix.
Publishing a matrix with it set disables the send path everywhere without an
application update, and it cannot be cleared in the field without the release
key. Letting a matrix expire does the same thing passively. The configuration
also carries a local `globalKillSwitchEngaged`; either one closes the path.

## 5. The durable outbox

`Native/GreenBubbles/src/send_outbox.rs` is the adapter-owned atomic
reservation store. It is single-flight, and every mutation runs inside one
exclusive `flock` transaction persisted with write-temporary, `fsync`,
`rename`, `fsync`-directory.

The idempotency key is **deterministic** in (gate decision, draft, approval),
so retrying the same approved draft reuses the key and is refused rather than
sending twice. A new send needs a new approval.

A crash can leave exactly two states, and both are recoverable without
resending:

- `reserved` — persisted **before** dispatch, so Return provably never
  happened; recovery closes it as `observedFailed`.
- `attempted` — persisted before dispatching a send-permitting capability;
  recovery parks it as `awaitingReconciliation`, which blocks further sends
  until the replica answers. Recovery never re-dispatches.

The store also holds the attempt window, the consumed approval identities, and
the circuit breaker, which opens after N consecutive failures and stays open
for a cooldown. It records body digests, never bodies.

## 6. Lifecycle: how `observedSent` is created

`ACTION_SAFETY_CONTRACT.md` requires that an adapter acknowledgement can never
create `observedSent`. It does not here. A visually confirmed send is recorded
as **evidence** and parked; `observedSent` is created only by
`send reconcile`, which searches the account's own encrypted replica for an
outgoing message in the bound conversation whose normalized text digest matches
the sent body. If the message is absent once the grace window has elapsed, the
attempt is closed as `observedFailed`. Until then it stays parked, and no
further send may start.

A deliberate dry run is not a failure: it completes as `dryRunCompleted` and
creates no lifecycle state at all, because it never became `attempted`.

## 7. Failure taxonomy

Every refusal is one of 26 codes, each mapping to exactly one operator action
(`SendFailureCode`). `send doctor` reports the blocking set with its actions:

```
grantsMissing  wechatNotRunning  notLoggedIn  unknownBuild  calibrationDrift
recipientVerifyFailed  contentVerifyFailed  sendUnconfirmed
engineStall  engineUnavailable  humanCollision  manifestViolation  windowNotFound
killSwitchEngaged  stageNotPermitted  configurationInvalid  profileInvalid
draftInvalid  approvalInvalid  capabilityExpired  capabilityMismatch
idempotencyConflict  rateLimited  circuitOpen  outboxBusy  reconciliationPending
```

## 8. Rollout stages

| Stage | Reachable recipients | Presses Return |
| --- | --- | --- |
| `dryRun` | any allow-listed conversation | **no** — stops after GATE 2 |
| `selfSend` | the account's own File Transfer only | yes |
| `allowListed` | File Transfer plus one reviewed peer | yes |

Stages above `dryRun` additionally require a **release-tier** calibration
profile. A development-signed profile is for rehearsal only and can never
unlock Return, so the rollout gate cannot be bypassed by pointing the
configuration at a locally signed profile.

Configuration validation is strict rather than forgiving: at `selfSend` the
allow list must be exactly the self-send conversation, and at `allowListed` it
must contain the self-send conversation and at most two entries in total. A
wider allow list than the stage permits is a configuration error, not something
to silently intersect.

The allow list authorizes a **conversation identifier and the recipient title
that identifier presents on screen**, as `recipientTitles`:

```json
"allowList": {
  "accountIds": ["…"],
  "conversationIds": ["filehelper"],
  "recipientTitles": { "filehelper": "File Transfer" },
  "capabilities": ["textSend"]
}
```

Both halves are load-bearing, because the opaque identifier is not what routes a
message. The send path proves a recipient by matching the human-readable title
in GATE 1, so a draft that kept an allow-listed identifier while carrying a
different title would direct the send at whatever conversation that title
matched, while the outbox and audit recorded the allow-listed one. Policy
therefore refuses any draft whose title is not the authorized one for its
conversation (`recipientTitleNotAllowed`), before a single keystroke is
delivered. A conversation with no authorized title configured cannot be sent to
at all: an allow list that does not cover its conversations exactly, or that
maps one to a blank title, is rejected as `configurationInvalid`.

**Known limitation — a title can collide.** Titles are display names, and a
remote party controls their own. GATE 1 proves the open conversation is *titled*
what was authorized; it cannot prove it is the conversation whose identifier was
authorized. A contact who renames themselves to an allow-listed title, and who
happens to be the open conversation when a send runs, would pass the recipient
gate. Two properties bound this. Title comparison is exact after whitespace
folding, never a prefix or substring, so a lookalike must be an exact
impersonation rather than a near miss. And reconciliation queries only the
approved `conversationId`, so a message delivered elsewhere is never observed
there: the entry reaches `observedFailed` at grace expiry rather than being
confirmed. Misdelivery is therefore detected and never falsely reported as
sent — but it is not prevented, and preventing it needs an identity signal the
remote party does not control. Keep the allow list to conversations whose titles
you control, and treat an `observedFailed` on a send you believed succeeded as a
recipient question, not a transport one.

A draft carrying a `replyTarget` is refused as `draftInvalid`. Threading is not
implemented, so such a draft would post as a standalone message: the right body
to the right recipient, but not the action that was approved.

## 9. Operator runbook

```sh
# 0. One-time: install and grant.
greenbubbles-send install-helper
greenbubbles-send onboarding --open        # deep-links to the exact panes
greenbubbles-send helper-status

# 1. Is the path open, and if not, exactly why?
greenbubbles send doctor ~/.greenbubbles/send/config.json

# 2. Gate the profile before first use (no send).
greenbubbles send selftest ~/.greenbubbles/send/config.json

# 3. Approve one draft, explicitly. The recipient and the body digest are
#    printed before the evidence file is written.
greenbubbles send approval-binding <config> <draft.json>
greenbubbles send approve <config> <draft.json> <approval.json> \
  --approver local-owner --validity-seconds 600 --confirm

# 4. Evaluate, then run.
greenbubbles send precheck <config> <draft.json> <approval.json>
greenbubbles send submit   <config> <draft.json> <approval.json>

# 5. Settle the lifecycle against the replica.
greenbubbles send outbox-status <config>
greenbubbles send reconcile <config> <draft.json> \
  --idempotency-key <hex> --replica <path> --replica-key-stdin

# 6. If a message must be taken back.
greenbubbles send recall-window <config> --idempotency-key <hex>
```

### Recall

Recall is performed by the owner in the client, deliberately. It is a
context-menu action whose item position varies with message type, age, and
locale, and a mis-aimed click there would delete or forward instead of
recalling — precisely the catastrophic class the on-screen gates exist to
prevent, and one the adapter cannot verify before committing to the click. The
adapter therefore reports the remaining window and the exact steps rather than
automating a gesture it cannot gate. This is a deliberate narrowing of
`SEND_INTEGRATION_DESIGN.md` §23 M4; see §11.

## 10. Packaging, distribution, uninstall

`scripts/package-send-helper.sh` builds every executable from source, assembles

```
GreenBubbles.app/Contents/
  MacOS/{greenbubbles-history,greenbubbles-send,greenbubbles}
  Library/LaunchAgents/me.greenbubbles.InputHelper.plist
  Library/LoginItems/GreenBubblesInputHelper.app/
  Resources/{build-provenance.json,sbom.json,NOTICE.md}
```

signs inside-out with Developer ID and **Hardened Runtime**, refuses to ship a
helper that disables library validation, builds a DMG, and notarizes and
staples it. Nothing is fetched at run time. Distribution is **Developer-ID
direct only**; the Mac App Store forbids the cross-application control the
helper needs, and the App Sandbox does too.

The release verifying key is injected at build time from
`GREENBUBBLES_SEND_RELEASE_PUBLIC_KEYS` (Swift by a reverted source
substitution the script always restores, Rust by a compile-time environment
variable). Omitting it produces a build that trusts no release profile — the
safe default, and CI asserts the repository never carries a release key.

Uninstall: `greenbubbles-send uninstall-helper` unregisters the agent and
prints the two `tccutil reset` commands that revoke the grants. Nothing
third-party was installed, so nothing third-party remains.

## 11. Deviations from the design, and why

1. **The effector is first-party, not a vendored third-party engine.**
   `SEND_INTEGRATION_DESIGN.md` §16 decision 3 chose Path C: vendor a pinned
   commit of the MIT cua-driver source and build it in our CI. What ships
   instead is a first-party effector built on public macOS API —
   `CGEvent.postToPid` for background input, ScreenCaptureKit for occluded
   window capture, Vision for recognition. This is *stronger* on every axis the
   decision cared about: no third-party code at all rather than audited
   third-party code, no private-framework (SkyLight) dependency and therefore
   none of the per-OS-release fragility that motivated Path C over Path B, and
   nothing to fast-follow upstream on. The design's §14 platform-neutral
   `SendSkill` interface is preserved exactly, so a Windows UIA/`PostMessage`
   or Android AccessibilityService backend still implements the same two
   protocols and reuses the state machine, the gates, and the contract mapping
   unchanged.
2. **Automated recall is not shipped.** See §9. The window and the procedure
   are surfaced instead.
3. **Geometry is integer parts-per-million, not the `[0..1]` fractions** shown
   in §21 of the design, so the signed bytes are exactly reproducible across
   two languages.
4. **The remote kill switch rides the signed compatibility matrix** rather than
   a separate channel, which keeps the number of things that can disable the
   path small and all of them signed.

## 11a. Attachments

Text only. Image and file sending are refused rather than absent: PRECHECK
rejects any draft carrying attachments, the minted capability is hard-coded to
`textSend`, and the helper has no file primitive at all — its bounded manifest
grants six tools, none of which can name a path. Allow-listing `fileSend` in the
configuration enables nothing; the guard then finds `textSend` missing from the
allow list and denies, which is the fail-closed answer.

`SEND_ATTACHMENTS_PLAN.md` specifies how the capability would be added, what it
costs in privilege, and which questions a read-only spike must answer first.
`GATE_READINESS.md` P4-FILE keeps it a separate gate.

## 11b. Addressing: the conversation already open

The adapter reaches its recipient by **not navigating**. `currentConversation`
addressing verifies that the conversation the client already has open *is* the
approved recipient, and refuses otherwise. It is the default, and it is the
safest mode available: the skill performs **no input at all before GATE 1**, so
a wrong conversation produces a read-only abort that cannot disturb anything the
person was doing.

Two preconditions make it non-destructive, both checked before anything is
clicked or typed:

* GATE 1 must read the approved title out of the live window.
* The compose box must be **empty**. The skill refuses rather than overwriting
  an unsent draft, and because it never has to clear anything, it never sends a
  select-all or a delete at all.

Search-based addressing (`SendAddressingMode::Search`) is retained but is not
usable on this build: the search field does not take focus from a background
click, and the client does not run its search while its window is inactive.
GATE 0 catches the first of those non-destructively.

Validated live on 2026-08-29 against File Transfer, autonomously, for all three
payload kinds — text, image, and file — each passing GATE 1 and GATE 2 and
stopping before Return. `SEND_ATTACHMENTS_PLAN.md` §17 records the measurements,
including the malformed-click bug that made an earlier session believe
background sending was impossible.

## 12. What is still gated

The adapter is built and closed. Opening it needs, in order: a provisioned
release signing key; a measured and signed calibration profile for the exact
WeChat build; a signed compatibility matrix marking that (host × client) pair
`supported`; the four gate-evidence flags in the configuration, which stand for
the acquisition, restoration, mechanism, and legal-review decisions recorded in
`GATE_READINESS.md` and `ACTION_SAFETY_CONTRACT.md`; the two TCC grants; and a
rollout-stage change made deliberately. None of those is a code change, and
none of them happens by default.

## 13. Testing

- **Deterministic gate tests** with a scripted effector and perception: every
  gate aborts closed, no abort leaves text behind, and a helper that claims a
  send a dry run forbade is treated as a mismatch rather than a send.
- **Adversarial control-plane tests**: kill switch, missing grants, unknown
  build, wrong recipient, tampered draft body, expired capability, a stage that
  cannot reach the conversation, and a development profile at a send-permitting
  stage — each denies before the helper is called.
- **Fault injection**: engine stall after a send-permitting dispatch parks the
  attempt and blocks the next one; a restart mid-attempt recovers exactly once
  and never re-dispatches; the same approval can never be dispatched twice.
- **Cross-language vectors**: canonical profile, matrix, capability, and text
  normalization digests, plus Rust-signed-verified-in-Swift, asserted from both
  test suites and re-derived in CI.
- **Journal integrity**: the audit chain verifies after a run and contains no
  message body.
