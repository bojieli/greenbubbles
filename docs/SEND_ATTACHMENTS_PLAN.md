# Plan: image and file sending

Status: **implemented (A1–A5); A0 spike partially answered against the live
client.** The plan below is the design of record; §13 records what the live run
actually found, including one incident and the fix it forced. This document specifies how the
send adapter (`SEND_ADAPTER.md`) would gain image and file sending, what it
would cost in privilege, and which questions must be answered by a read-only
spike before any code is written. It does not open the capability.

Text sending is implemented and ships closed. Attachments are today *refused*
rather than absent: PRECHECK rejects any draft carrying attachments, the minted
capability is hard-coded to `textSend`, and the helper has no file primitive of
any kind. `GATE_READINESS.md` P4-FILE treats file sending as a **later
independent capability, not inherited from text send**. This plan honours that:
it adds a second capability with its own gates, its own rollout, and its own
exit criteria.

---

## 1. Scope

In scope: sending **one** local image or **one** local file to an already
allow-listed conversation, under the same approval, idempotency, outbox, and
reconciliation machinery that text sending uses.

WeChat treats these as two different sends, and the difference is visible to the
recipient, so the draft must state which one it means rather than let the
adapter infer it:

| Intent | What the recipient gets | Notes |
| --- | --- | --- |
| `imageSend` | an inline image bubble | the client re-encodes and downscales; the bytes the recipient receives are **not** the bytes we staged |
| `fileSend` | a file attachment bubble | byte-preserving; shows name and size |

That distinction drives a hard consequence for verification (§4): for
`imageSend` the digest we approved can never be the digest that arrives, so the
contract must bind the *source* artifact and record that the transmitted form is
a client-produced derivative. Silently conflating the two would let an approval
for "send this exact file" be satisfied by something else entirely.

Out of scope for this plan: multiple attachments per message, attachment plus
caption in one send, video and voice notes, forwarding an existing artifact from
the replica, and pasting an image directly from the pasteboard as bitmap data
without a source file. Each is a further capability.

## 2. Mechanism survey

The owner named two paths. There is a third that fits this architecture better,
and one of the named two is not achievable in the background at all.

### Path A — paste a file reference into the compose box (recommended primary)

Write the staged file's `fileURL` onto a pasteboard, focus the compose box, and
press Cmd+V, exactly as text sending already does. WeChat then stages the file
in the compose area, usually behind a confirmation sheet.

*Why this is the right primary:* it is mechanically **identical to what already
works and is already gated**. It needs no new anchor, no Finder, no modal panel,
no second window to locate, and no cursor. The only new effector primitive is
"write a file reference to the pasteboard" instead of "write a string". Every
existing invariant — background posting to the pid, no raise, no cursor warp,
human-collision yield, clipboard restore on every exit path — carries over
unchanged.

*Risks:* whether WeChat's compose box accepts a pasted `fileURL`, and whether it
distinguishes image-as-image from image-as-file on paste, are **empirical
questions** (§10 Q1, Q2). Both are cheap to answer in a dry run.

### Path B — attach control, then the open panel (fallback, and required if A fails)

Click the attach control in the compose toolbar, wait for the file open panel,
navigate it, and confirm.

The panel can be driven **keyboard-first**, which matters enormously: rather
than hunting a file list by pixel, press **Cmd+Shift+G** ("Go to Folder"), paste
the absolute path, and press Return. That is deterministic, needs no coordinates
inside the panel, and needs no OCR of a directory listing. It also keeps the
"no arbitrary typing" invariant, because the path arrives by pasteboard like
every other string this adapter types.

WeChat on this machine is **unsandboxed** — `codesign -d --entitlements` on the
installed 4.1.13 bundle returns nothing, consistent with the owner's ad-hoc
re-sign — so the panel is an in-process `NSOpenPanel` owned by WeChat's own pid.
That means the existing effector and capture both reach it without a second
target in the bounded manifest. **This must be re-detected at runtime, not
assumed**: a Tencent-signed or Mac App Store build would raise the panel through
Powerbox in a *different* process, which would need an explicit second manifest
entry. The discriminator is observable — which pid owns the new window — and is
itself a gate (§10 Q3).

*Risks:* the click that opens the panel is the dangerous step. The compose
toolbar holds neighbouring controls, and a mis-aimed click could invoke
something other than "attach". The mitigation is to make the panel's appearance
a **gate**: after the click, a new window owned by the expected pid, of panel
proportions, must exist within a bounded time. If it does not, press Escape,
abort, and report `attachPanelNotPresented`. Nothing has been staged and nothing
can be sent.

### Path C — drag and drop from Finder (rejected for background operation)

This cannot work under this design, for a mechanism reason rather than a
difficulty reason.

A macOS drag is not a sequence of mouse events. It is a session mediated by the
WindowServer: a source calls `beginDraggingSession`, and the WindowServer tracks
the drag against the **single global cursor** and delivers `draggingEntered` /
`draggingUpdated` / `performDragOperation` to whichever window is under that
cursor. Posting mouse-down, moved, and up to a specific pid with
`CGEvent.postToPid` creates no session, so the destination receives ordinary
mouse events and no `NSDraggingInfo` ever arrives.

Making a real session requires either driving Finder as the source — which moves
the user's physical cursor and needs both windows visible and unoccluded — or
making our helper the drag source, which still tracks the real cursor. Either
way it violates the invariant that the whole design exists to hold: the user's
mouse, keyboard, and screen are untouched and takeover always wins. Drag and
drop belongs to the foreground co-pilot model that
`AI_DESKTOP_AGENT_HANDOFF.md` §4.5 and §4.8 describe and that this product
deliberately did not adopt.

The confirmation sheet the owner described after a drop is not specific to
dragging — Path A very likely raises the same sheet — so nothing is lost.

### Decision

Implement **Path A** as primary and **Path B** as the fallback selected by the
signed calibration profile, not by code. A profile that carries attach-panel
anchors enables Path B for that build; one that does not, does not. Path C is
excluded.

## 3. What the recipient gate does *not* have to change

GATE 1 is unchanged and stays exactly as strict. Addressing the conversation and
verifying its title has nothing to do with what is being sent, and the
wrong-recipient failure remains the one catastrophic case. An attachment send
performs the identical address-and-verify sequence before anything is staged.

## 4. The verification problem, and how to close it

For text, GATE 2 is "OCR the compose region and compare it to the approved
body". That works because the payload *is* the visible text. For an attachment
the payload is bytes that never appear on screen. The compose area shows a
filename, an icon or thumbnail, and a size. So the on-screen gate can prove
*which file was staged* but never *that its contents are the approved contents*.

The plan therefore splits GATE 2 into two independent halves.

**GATE 2a — on-screen identity.** OCR the compose region and require the
normalized display filename to equal the approved `displayFileName`, at or above
the profile's confidence floor, with exactly one candidate. For `imageSend`,
where a thumbnail may replace the filename, require instead that the staged
region became non-empty and that no filename other than the expected one is
present. This is weaker than the text gate and the plan says so plainly.

**GATE 2b — off-screen content, by descriptor.** Immediately before staging,
re-read the file and re-hash it, and require the digest to equal the draft's
`attachments[].sha256`. This is the half that actually protects the bytes.

Between hashing and staging there is a time-of-check-to-time-of-use window: the
file could be replaced after we hash it and before WeChat reads it. Closing it:

1. The control plane copies the approved file into a **single-use staging
   directory** it creates mode `0700` under its own private root, with a random
   basename and the approved display name preserved as the final path component.
2. It re-hashes **from the staged copy** and compares to the draft digest.
3. Only the staged path is placed in the capability. The user's original file is
   never referenced again, so replacing it afterwards changes nothing.
4. The staging directory is removed when the outbox entry reaches a terminal
   state, including on every abort path.

This is the same discipline `P4-FILE` asks for — "descriptor-level revalidation
immediately before attempt" — expressed so that the revalidated object and the
staged object are provably the same inode.

**GATE 3** becomes: the compose region cleared, and the newest outgoing bubble
contains the expected display filename (`fileSend`) or an image bubble appeared
where none was (`imageSend`). As with text, this is evidence, never a verdict.

## 5. Reconciliation

`observedSent` must still come only from the account's own data. The existing
text reconciler matches a normalized body digest against replica rows; an
attachment send has no body to match. Replace the predicate:

- search the replica for an outgoing message in the bound conversation, after
  the attempt time, whose `artifact_references` are non-empty;
- match on the artifact's `display_file_name` and `byte_count` for `fileSend`,
  and additionally on `sha256` when the replica exposes a source digest;
- for `imageSend`, match on artifact kind and time window only, and record
  explicitly that the transmitted bytes are a client-produced derivative of the
  approved source, so the audit trail never claims a byte-for-byte match it
  cannot support.

`artifact.rs` and `live_attachment.rs` already model artifact references and
their availability, so this is a new predicate over existing structures rather
than new extraction.

## 6. Contract changes

Additive, and every addition is inside a signature or a binding digest.

**Capability envelope** (`send_contract.rs`, mirrored in `SendContract.swift`) —
one optional attachment block, appended to the canonical binding bytes after the
existing fields so the digest covers it:

```jsonc
"attachment": {
  "intent": "imageSend" | "fileSend",
  "stagedPath": "/…/staging/<random>/<display name>",
  "displayFileName": "quarterly.pdf",
  "byteCount": 182931,
  "sha256": "…",                 // of the staged copy, re-verified before use
  "uniformTypeIdentifier": "com.adobe.pdf",
  "stagingDirectory": "/…/staging/<random>"
}
```

A capability may carry a body **or** an attachment, not both, until captions are
a separate capability. `permitSend` and the rollout stage govern it exactly as
before.

**Bounded manifest** — one new tool, `clipboardWriteFileReference`, and for
Path B one new tool, `openPanelNavigate`. The manifest keeps **no file roots**:
the helper is handed one already-staged path per action and may not enumerate,
glob, or re-open anything else. Any path outside the capability's own
`stagingDirectory` is a `manifestViolation`.

**Calibration profile** — new optional sections, so a build without them simply
cannot do attachments:

```jsonc
"attachmentAnchors": { "attachControl": {…}, "confirmSendButton": {…} },
"attachmentRegions": { "composeAttachment": {…}, "confirmSheet": {…} },
"attachmentSelftest": { "expectedPanelMinimumWidthPartsPerMillion": … }
```

Schema stays 1; the fields are appended to the canonical signing bytes behind a
presence flag, exactly as `globalKillSwitchEngaged` was.

**Failure codes** — `attachmentInvalid`, `attachmentDigestMismatch`,
`attachPanelNotPresented`, `attachmentStagingFailed`,
`attachmentVerifyFailed`, `unsupportedAttachmentType`. Each maps to one operator
action, as all existing codes do.

**Outbox** — the entry gains `attachment_sha256`, `display_file_name`, and
`intent`. It continues to hold no bytes and no body.

**Action capability** — the existing `ActionCapability::FileSend` finally
becomes reachable, and a new `ImageSend` variant is added. Because the guard
checks the minted capability against `allowList.capabilities`, an operator must
allow-list each one explicitly; today's configurations, which list only
`textSend`, keep refusing attachments with no change.

## 7. The privilege question, stated honestly

This is the part of the plan that deserves the most scrutiny.

Today the helper holds Accessibility and Screen Recording and **no file access
whatsoever** — "no file roots" is a stated property of its bounded manifest.
Attachment sending necessarily gives the process that can control and observe
every application a way to read a file. That widens exactly the component the
privilege split exists to keep narrow.

Mitigations, in order of how much they actually buy:

1. **The helper never chooses a file.** It receives one staged path inside one
   single-use directory, both minted by the control plane, both covered by the
   capability's binding digest. It cannot ask for a different file.
2. **The staging directory is the only readable root**, is mode `0700`, holds
   exactly one file, and is deleted when the attempt terminates.
3. **The helper does not read the file's contents at all.** It writes a
   *reference* to the pasteboard, or types a path into a panel. The bytes flow
   from the filesystem to WeChat without passing through the helper, so a
   compromised helper gains a path, not a copy.
4. Content revalidation stays in the **control plane**, which already has the
   keys and the replica, so no new trust is placed in the helper.

Residual risk that no mitigation removes: a compromised helper can put an
arbitrary pasteboard payload in front of a user who then pastes it elsewhere,
and can read the staged file's path. Both are strictly smaller than what the
Accessibility grant already implies, but they are new and should be recorded in
the threat model rather than waved past.

## 8. Rollout

Attachments get their **own** stage ladder, orthogonal to the text one, and both
must be open for an attachment to be sent:

- **A-dry** — address, stage, run GATE 2a and 2b, then clear the compose box and
  stop. Zero send risk. This is where Path A vs Path B is decided empirically.
- **A-self** — send to File Transfer only.
- **A-peer** — the reviewed allow-listed peer, under a *separate*, tighter
  volume cap than text, because an attachment send is louder and less
  reversible.

Attachments additionally require a release-tier profile carrying attachment
anchors, so a development profile can never send one.

## 9. Milestones

| ID | Deliverable | Gate |
| --- | --- | --- |
| **A0** | Read-only spike answering §10 Q1–Q5 against the live client. No send, no staging, notebook-style record. | none |
| **A1** | Staging, digest revalidation, TOCTOU-closed staging directory, capability and outbox schema, control-plane validation and tests. No helper change. | pure, offline |
| **A2** | Profile schema additions, signing, canonical vectors regenerated in both languages, loader and self-test. | offline |
| **A3** | Helper: `clipboardWriteFileReference`, GATE 2a, GATE 2b wiring, abort paths, clipboard restore. Path A only. | A-dry |
| **A4** | Path B: attach control, panel-appeared gate, Go-to-Folder navigation, Escape-and-abort. Selected by profile data. | A-dry |
| **A5** | Reconciliation predicate over artifact references; `imageSend` derivative recording; audit and doctor surfacing. | A-self, then A-peer |

A1 and A2 are implementable today and depend on nothing empirical. A3 onward
depend on the A0 answers.

## 10. Open questions for the A0 spike

These cannot be answered by reading the binary and must not be guessed at.

- **Q1** Does WeChat's compose box accept a pasted `fileURL`, and does it stage
  the file rather than pasting its path as text?
- **Q2** Does a pasted image file become an inline image or a file attachment,
  and is there a modifier or a per-conversation setting that chooses? If paste
  cannot distinguish them, `imageSend` needs Path B and `fileSend` may not.
- **Q3** When the attach control is clicked, which process owns the resulting
  panel window — WeChat, or Powerbox? Does the answer change for a
  Tencent-signed build?
- **Q4** Does a background `Cmd+Shift+G` reach the panel when it is posted to
  the pid rather than delivered through the front-most application?
- **Q5** Is there a confirmation sheet, what does it read, and does Return
  confirm it or does it need an anchored click?

The spike is read-only in the sense that matters: it may stage and then cancel,
but it presses no send control, and it runs against `File Transfer` only.

## 11. Explicitly out of scope

Cross-application drag and drop; multiple attachments per send; caption plus
attachment; video, voice, and sticker sends; forwarding an artifact already in
the replica; reading any file the owner has not named in an approved draft;
enumerating directories; and any attachment send that has not passed both the
text rollout gate and the attachment rollout gate.

---

## 13. Live findings and one incident (2026-08-29)

The A0 spike ran against the owner's own live client, on their explicit
instruction, in dry-run mode only. No message and no attachment was ever sent:
all three runs ended `attempted: false`.

### What was confirmed

- **The environment is reachable.** Both TCC grants present, WeChat 4.1.13.269579
  running and signed in, window located, background capture and Apple Vision
  recognition working at confidence 1.0.
- **The client is unsandboxed.** `codesign -d --entitlements` on the installed
  bundle returns nothing, so an open panel would belong to WeChat's own process
  (§2, Path B). Still re-detected at runtime rather than assumed.
- **GATE 1 works exactly as designed.** With the wrong conversation open it read
  the on-screen title, compared it to the approved recipient, and aborted before
  touching any compose box. That gate is what kept a misfire from becoming a
  message.
- **Synthesized events are not self-detected.** Measured, not assumed:
  `collision-probe` reports `synthesizedEventsCountAsHumanInput: false`, so the
  helper's own clicks and keys never trip its own collision guard.

### The incident

While the owner was typing a message to a real contact, three spike runs pasted
the search key into the client and interleaved with their typing.

**Cause.** The collision guard had been loosened, during this same session, from
"any recent input aborts" to "only input while the target is frontmost aborts",
so that a spike could run on a machine that was in use. Two things made that
wrong:

1. `NSWorkspace.frontmostApplication` is unreliable when sampled from a non-GUI
   process; it read "not frontmost" while the person was typing in the client.
2. More fundamentally, **a background click does not merely avoid raising the
   target — it moves keyboard focus inside it.** A person typing into the
   compose box can find their next keystrokes arriving in the search box the
   moment the skill focuses it. Interference does not require a window to come
   forward, so "the target is not frontmost" was never evidence of safety.

**Fix.** The guard is now stricter than it was originally:

- the machine must be idle for a full window (5 s) before the skill touches
  anything, rather than merely "no input in the last 1.5 s";
- the target being frontmost *raises* the requirement (15 s) and can never waive
  it;
- the check runs before **every** focus change, not only at stage boundaries,
  because taking focus is itself the interfering act;
- the policy is a pure, unit-tested function (`HumanCollisionPolicy`) rather than
  untestable code inside the effector, and the tests pin both directions.

**Standing lesson.** A safety guard must not be relaxed to make a test run. If
the guard blocks the test, the environment is telling the truth: the machine is
in use and the adapter should wait.

### Still open

Q1, Q2, and Q5 (does the compose box accept a pasted file reference; does paste
distinguish image-as-image from image-as-file; is there a confirmation sheet)
remain unanswered, because answering them requires driving the live client and
the machine has been in use. They need one quiet window on an idle machine,
targeting File Transfer only.

---

## 14. Mechanism findings that change the picture (2026-08-29, second live session)

> **Superseded in part by §17.** The central claim below — that a background
> click cannot move keyboard focus — was wrong. The click was malformed. Read
> §17 before relying on anything in this section.

A second live session, on an idle machine, answered the question the incident
raised and produced a harder result than expected. All runs were dry runs;
nothing was sent.

### Background clicks do not move keyboard focus

`CGEvent.postToPid` delivers **keystrokes** to whatever field the target already
has focused, but a posted **click does not move that focus**. Measured
directly: the skill clicked the search box and pasted, and the text landed in
the compose box of the conversation that was already open.

This falsifies the design's stated methodology — "mouse focuses, keyboard acts"
(`SEND_INTEGRATION_DESIGN.md` §3) — for background operation on WeChat
4.1.13 / macOS 26. The earlier spike's claim that a background click focuses the
Qt search and compose boxes does not reproduce.

The consequence is severe and is exactly what the incident was: **every
keystroke the skill sends goes wherever the person last put the caret.** A
select-all plus delete there destroys their unsent text.

### GATE 0 makes the failure non-destructive

Addressing now pastes **without clearing first**, then reads the search field
back and requires the search key to appear in it. A click that missed is caught
before anything destructive happens, and the worst case degrades from "the
person's draft is deleted" to "a few stray words appear in a field they can
clear". Verified live: the run aborts with `addressingFocusFailed`, having
touched nothing else.

### A keyboard shortcut *does* move focus

Posting **Cmd+F** to the target moves focus to the search field in the
background — no raise, no cursor movement, focus ring and caret confirmed by
capture. Pasting then lands in the search field, as intended.

This is a strictly better addressing primitive than clicking: it has no
coordinate to mis-aim, so it cannot land in the wrong field at all. If
background addressing is pursued further, it should be keyboard-only and the
`searchBox` anchor should be deleted rather than re-measured.

### But the search does not execute while the client is inactive

With "File Transfer" sitting in the focused search field, **the results list
never filtered**. WeChat appears to run the search only when its window is key.
Waiting did not help.

So background addressing can focus the field and enter the text, but cannot
complete: there is no result to select, and GATE 1 therefore never sees the
right title.

### Where that leaves the send path

On this build, **a fully background send cannot complete.** The options are:

1. **Keep it closed.** Current behaviour, and the honest default: GATE 0 and
   GATE 1 both refuse, non-destructively.
2. **Accept a brief foreground activation** for the addressing step only — the
   "burst" model of `AI_DESKTOP_AGENT_HANDOFF.md` §4.5. This trades the
   zero-interference property for a working path and needs an explicit owner
   decision, because it is a different product.
3. **Address without the search box**, if a conversation can be opened by some
   other route that works while inactive. Unexplored.

None of these is a code change to make unilaterally. Attachments are unaffected
as a capability — the staging, gates, and reconciliation all stand — but they
inherit this limitation, because every attachment send has to address a
recipient first.

---

## 15. Q1, Q2 and Q5 answered live (2026-08-29, third session)

Answered against File Transfer, the owner's own self-chat, with the compose box
already focused. Nothing was sent: the compose area was cleared after each
probe and no Return was ever pressed.

**Q1 — does the compose box accept a pasted file reference?** **Yes.** Writing a
`fileURL` to the pasteboard and pressing Cmd+V stages the file. Path A is
viable, and it needs no anchor, no panel, and no cursor.

**Q2 — does paste distinguish image-as-image from image-as-file?** **Yes,
automatically, by type**, and the two look different on screen:

| Pasted | Staged as | On-screen evidence |
| --- | --- | --- |
| `.png` | inline image thumbnail | **no text at all** |
| `.txt` | file chip | the name and size, e.g. `gbspike-probe.txt` / `65B` |

**Q5 — is there a confirmation sheet?** None appeared for either kind on this
build. `presentsConfirmationSheet` stays false in the measured profile, and the
sheet-handling code remains for builds that do raise one.

### The gap this exposed, and the fix

GATE 2a as first written required the display name to be read back out of the
compose area. That works for a file chip and **can never work for an image**,
which stages as a bare thumbnail carrying no text. Every image send would have
failed the gate.

The gate is now kind-specific, which is what §4 originally called for:

- **file** — the compose region must change *and* the chip must carry the
  approved name;
- **image** — the compose region's pixels must change, which is the only
  evidence available, and the outcome records `attachmentNameMatched: false`
  rather than implying a match that was never made.

That required a new perception primitive, `regionFingerprint`, because text
recognition has nothing to read in a thumbnail.

### What this means overall

The attachment mechanism is **solved and measured**: staging works, both kinds
are distinguishable, and each has a gate matched to what is actually observable.
What remains blocked is addressing (§14) — reaching a chosen conversation — and
that blocks text and attachment sends alike.

A `send-to-the-currently-open-conversation` mode would sidestep addressing
entirely: the title gate already proves which conversation is open, and every
other gate is unchanged. It is a weaker product, but it is one that works today
and is worth an explicit decision.

---

## 16. The addressing decision, and what focus actually costs (2026-08-29)

The owner chose the mode that works today, in the background, with attachments:
**send to the conversation the client already has open.**

### The mode

`SendAddressingMode::CurrentConversation` skips navigation entirely. The skill
reads the window, runs GATE 1 against the title, and refuses if the open
conversation is not the approved recipient. It is not merely a workaround; it is
the **safest** mode available, because it performs **no input whatsoever before
the recipient gate**. A misfire is a read-only abort that cannot disturb
anything the person was doing — the property the search mode could never offer.

A capability in this mode carries an **empty search key**, so there is nothing
to type even if the state machine were wrong, and the binding digest covers the
mode itself.

### What focus costs, measured

Three further live measurements shaped the implementation:

1. **A posted click does not take focus** — established earlier.
2. **A posted click appears to *lose* whatever focus existed.** After the skill
   clicked, subsequent pastes landed nowhere at all: neither the compose box nor
   the search field received them.
3. **Lost focus cannot be recovered from the background.** Cmd+F followed by
   Escape, then a paste, put text in neither field.

Keystrokes therefore land only while the client already has a focused text
field, which is the state the *person* leaves behind when they are in a
conversation.

### What that means for the skill

In `currentConversation` mode the skill now **clicks nothing and clears
nothing**. It pastes into the focus that already exists and lets GATE 2 decide
whether that was really the compose box.

Clearing is likewise conditional. A select-all plus delete into an unknown field
is precisely the destructive act this whole session has been about, and in a
chat window an unknown field might be the message list. So the compose box is
cleared only once GATE 2 has *proven* where the caret is. When the gate fails,
the run aborts without clearing and says so, leaving at worst some stray text
rather than risking a destructive keystroke.

### The resulting contract with the user

The mode sends to the conversation you are in, which means the product's
precondition is honest and small: **have the conversation open, with the compose
box focused** — exactly the state you are in when you are about to type. GATE 1
proves the recipient, GATE 2 proves the content, and either failing is
non-destructive.

---

## 17. Correction: the click was malformed, and background sending works

The conclusion in §14 was wrong, and the error was mine rather than the
platform's.

### What was actually broken

The synthesized click omitted `kCGMouseEventClickState`. Without it a Qt control
treats the down/up pair as stray button traffic and never takes focus from it,
which is exactly the symptom §14 recorded: the click reached the process, moved
no caret, and left the next keystroke to land wherever the person had last put
it. Adding the click state — plus a `mouseMoved` prelude and a short gap between
events, so the pair is legible as a press and a release — makes it work.

Measured with `focus-probe`, which clicks an anchor, pastes a marker, and reads
the region back:

| Anchor | Click takes focus |
| --- | --- |
| compose box | **yes** |
| search box | no |

So the design's original methodology — *mouse focuses, keyboard acts* — holds
after all, for the anchor that matters. The search box still does not take focus
from a background click, so search-based addressing remains unusable, but the
chosen no-navigation mode never needed it.

### End-to-end validation, fully autonomous

All three payload kinds now complete the dry run against File Transfer with no
human involvement: the runner waits for the machine to go idle, then runs.

| Payload | GATE 1 title | GATE 2 | Attempted | Stage reached |
| --- | --- | --- | --- | --- |
| text | matched, conf 1.0 | `composeMatched` | no | `contentVerify` |
| image | matched, conf 1.0 | region changed, no name (as designed) | no | `contentVerify` |
| file | matched, conf 1.0 | region changed **and** name matched | no | `contentVerify` |

Every run stopped before Return, cleared the compose box, restored the
clipboard, and left the client exactly as it was found.

### What the earlier failure bought

The wrong conclusion still produced the two gates that make the path safe, and
both are worth keeping now that the click works:

- **GATE 0** catches a click that misses, instead of typing into whatever the
  person was using.
- **The empty-compose precondition** means the skill never sends a select-all or
  a delete at all: it refuses rather than overwriting an unsent draft, and it
  refuses before clicking anything.

### Standing lesson

"The platform cannot do this" is a conclusion that needs the same scrutiny as a
passing test. Here it was three live sessions of accumulating theory — clicks
do not focus, focus cannot be restored, background sending is impossible — on
top of one missing field in an event. Check the boring explanation first.
