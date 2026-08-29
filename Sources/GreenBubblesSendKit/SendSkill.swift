import CoreGraphics
import Foundation

/// The frame of the target window in global screen points.
public struct WindowFrame: Equatable, Sendable {
  public let origin: CGPoint
  public let size: CGSize

  public init(x: Double, y: Double, width: Double, height: Double) {
    origin = CGPoint(x: x, y: y)
    size = CGSize(width: width, height: height)
  }

  /// A stable, content-free digest of the frame, recorded as gate evidence so
  /// calibration drift is visible in the audit trail without leaking pixels.
  public var digest: String {
    let description =
      "\(Int(origin.x.rounded())),\(Int(origin.y.rounded())),"
      + "\(Int(size.width.rounded())),\(Int(size.height.rounded()))"
    return SendDigest.sha256Hex(Data(description.utf8))
  }

  /// Whether the window is plausibly WeChat's main window rather than its
  /// login panel or a transient sheet.
  public var isPlausibleMainWindow: Bool {
    size.width >= 700 && size.height >= 500
  }
}

/// Converts window-relative parts-per-million into screen points.
public enum WindowGeometry {
  /// The absolute point for a window-relative anchor.
  public static func point(_ anchor: WindowRelativePoint, in frame: WindowFrame) -> CGPoint {
    let scale = Double(CalibrationProfileConstants.partsPerMillion)
    return CGPoint(
      x: frame.origin.x + frame.size.width * Double(anchor.xPartsPerMillion) / scale,
      y: frame.origin.y + frame.size.height * Double(anchor.yPartsPerMillion) / scale
    )
  }

  /// The absolute rectangle for a window-relative region.
  public static func rect(_ region: WindowRelativeRect, in frame: WindowFrame) -> CGRect {
    let scale = Double(CalibrationProfileConstants.partsPerMillion)
    return CGRect(
      x: frame.origin.x + frame.size.width * Double(region.xPartsPerMillion) / scale,
      y: frame.origin.y + frame.size.height * Double(region.yPartsPerMillion) / scale,
      width: frame.size.width * Double(region.widthPartsPerMillion) / scale,
      height: frame.size.height * Double(region.heightPartsPerMillion) / scale
    )
  }
}

/// One recognition result from the on-device text recognizer.
public struct RecognizedRegionText: Equatable, Sendable {
  public let text: String
  public let confidencePartsPerMillion: UInt32
  /// How many distinct candidate lines the region produced. More than one
  /// plausible conversation title means an ambiguous search result, which is
  /// an abort condition rather than a guess.
  public let candidateCount: Int

  public init(text: String, confidencePartsPerMillion: UInt32, candidateCount: Int = 1) {
    self.text = text
    self.confidencePartsPerMillion = confidencePartsPerMillion
    self.candidateCount = candidateCount
  }
}

/// The keys the mechanical skill is allowed to synthesize. There is
/// deliberately no "type arbitrary text" primitive: text always arrives by
/// pasteboard, so the skill cannot be repurposed into a keylogger's inverse.
public enum SendKey: String, Equatable, Sendable {
  case returnKey
  case delete
  case selectAll
  case paste
  case escape
  /// Cmd+Shift+G in an open panel. Used only by the panel fallback, and only
  /// ever followed by a pasted path, so no text is ever typed key by key.
  case goToFolder
}

/// The tools the bounded capability manifest can grant.
public enum SendTool: String, Equatable, Sendable, CaseIterable {
  case click
  case hotkey
  case pressKey
  case clipboardWrite
  case windowRead
  case windowCapture
  /// Put a reference to one already-staged file on the pasteboard. The helper
  /// never reads the file's contents; it hands WeChat a path.
  case clipboardWriteFileReference
  /// Navigate an open panel to one already-staged path.
  case openPanelNavigate
}

/// The engine's least-privilege confinement: one application, a fixed set of
/// tools, no file roots, no browser origins. The helper holds broad TCC grants
/// but may only exercise what this manifest names.
public struct BoundedCapabilityManifest: Equatable, Sendable {
  public let allowedBundleIdentifiers: Set<String>
  public let allowedTools: Set<SendTool>

  public init(allowedBundleIdentifiers: Set<String>, allowedTools: Set<SendTool>) {
    self.allowedBundleIdentifiers = allowedBundleIdentifiers
    self.allowedTools = allowedTools
  }

  /// The only manifest this product ships: WeChat, and only the six tools the
  /// mechanical send skill actually needs.
  public static let weChatOnly = BoundedCapabilityManifest(
    allowedBundleIdentifiers: ["com.tencent.xinWeChat"],
    allowedTools: Set(SendTool.allCases)
  )

  /// The scope reported to the control plane and shown by `send doctor`.
  public var scopeDescription: [String] {
    allowedBundleIdentifiers.sorted().map { bundle in
      "\(bundle):\(allowedTools.map(\.rawValue).sorted().joined(separator: "+"))"
    }
  }

  /// Authorizes one tool against one target. A refusal is a defect signal, not
  /// a configuration problem: the skill should never ask for anything else.
  public func authorize(_ tool: SendTool, bundleIdentifier: String) throws(SendFailure) {
    guard allowedBundleIdentifiers.contains(bundleIdentifier), allowedTools.contains(tool) else {
      throw SendFailure(
        .manifestViolation,
        detail: "\(tool.rawValue) is outside the bounded manifest for \(bundleIdentifier)"
      )
    }
  }
}

/// Reading the screen. Implementations capture the target window even when it
/// is occluded and recognize text on device; nothing leaves the machine.
public protocol ScreenPerception: AnyObject {
  /// The target window's frame, or `windowNotFound`.
  func windowFrame() throws(SendFailure) -> WindowFrame
  /// On-device text recognition inside one window-relative region.
  func recognizeText(in rect: CGRect) throws(SendFailure) -> RecognizedRegionText
  /// A content digest of one region's pixels.
  ///
  /// Needed because an image staged into the compose area is a *thumbnail with
  /// no filename*, measured live on WeChat 4.1.13: text recognition returns
  /// nothing to compare against. Comparing the region before and after staging
  /// is the only on-screen evidence an image send can offer.
  func regionFingerprint(in rect: CGRect) throws(SendFailure) -> String
  /// How many captures have been taken, for gate evidence.
  var captureCount: UInt32 { get }
}

/// Driving the input. Mouse clicks only ever focus; the keyboard performs every
/// mutation. Every method posts to the target process directly, so the user's
/// cursor never moves and no application is brought to the front.
public protocol InputEffector: AnyObject {
  func click(at point: CGPoint) throws(SendFailure)
  func press(_ key: SendKey) throws(SendFailure)
  func writeClipboard(_ text: String) throws(SendFailure)
  /// Places a reference to one file on the pasteboard. The path is always the
  /// staged copy named by the capability, never a path the helper chose.
  func writeClipboardFileReference(_ path: String) throws(SendFailure)
  /// The number of windows the target process currently owns, used to detect
  /// that an open panel or a confirmation sheet actually appeared.
  func targetWindowCount() -> Int
  func restoreClipboard()
  /// Whether real user activity has been observed on the target since the run
  /// started. Takeover always wins.
  func humanActivityObserved() -> Bool
  /// Lets the client settle between synthesized events.
  func settle(milliseconds: UInt64)
}

/// How long the skill waits between steps. Values are deliberately generous:
/// the whole run is bounded by the caller's watchdog, so waiting is cheaper
/// than acting on a half-drawn window.
public struct SendPacing: Equatable, Sendable {
  public var afterClickMilliseconds: UInt64
  public var afterKeyMilliseconds: UInt64
  public var afterPasteMilliseconds: UInt64
  public var afterSearchMilliseconds: UInt64
  public var afterReturnMilliseconds: UInt64

  public init(
    afterClickMilliseconds: UInt64 = 150,
    afterKeyMilliseconds: UInt64 = 80,
    afterPasteMilliseconds: UInt64 = 250,
    afterSearchMilliseconds: UInt64 = 700,
    afterReturnMilliseconds: UInt64 = 900
  ) {
    self.afterClickMilliseconds = afterClickMilliseconds
    self.afterKeyMilliseconds = afterKeyMilliseconds
    self.afterPasteMilliseconds = afterPasteMilliseconds
    self.afterSearchMilliseconds = afterSearchMilliseconds
    self.afterReturnMilliseconds = afterReturnMilliseconds
  }
}

/// The platform-neutral send skill (`SEND_INTEGRATION_DESIGN.md` §14). A
/// Windows UIA/`PostMessage` backend or an Android AccessibilityService backend
/// implements the same two protocols above and reuses this state machine
/// unchanged, along with every gate and the contract mapping.
public struct MechanicalSendSkill {
  private let profile: CalibrationProfileBody
  private let manifest: BoundedCapabilityManifest
  private let targetBundleIdentifier: String
  private let effector: InputEffector
  private let perception: ScreenPerception
  private let pacing: SendPacing
  private let helperVersion: String
  private let engineVersion: String
  private let clock: @Sendable () -> UInt64

  public init(
    profile: CalibrationProfileBody,
    manifest: BoundedCapabilityManifest = .weChatOnly,
    targetBundleIdentifier: String = "com.tencent.xinWeChat",
    effector: InputEffector,
    perception: ScreenPerception,
    pacing: SendPacing = SendPacing(),
    helperVersion: String,
    engineVersion: String,
    clock: @escaping @Sendable () -> UInt64
  ) {
    self.profile = profile
    self.manifest = manifest
    self.targetBundleIdentifier = targetBundleIdentifier
    self.effector = effector
    self.perception = perception
    self.pacing = pacing
    self.helperVersion = helperVersion
    self.engineVersion = engineVersion
    self.clock = clock
  }

  /// Runs the whole mechanical skill under one bound capability. It never
  /// throws: every refusal becomes an outcome carrying its taxonomy code, so
  /// the control plane always learns exactly how far the run got.
  public func execute(_ capability: ActionCapabilityEnvelope) -> HelperSendOutcome {
    var evidence = HelperGateEvidence()
    var stage = SendStage.precheck
    var attempted = false
    var confirmation = VisualConfirmation.notAttempted
    var attachmentBaseline = ""
    let started = clock()
    do {
      try capability.validate(nowUnixNanoseconds: started)
      guard capability.calibrationProfileID == profile.profileID else {
        throw SendFailure(
          .calibrationDrift,
          detail: "the capability names a different calibration profile"
        )
      }
      for tool in SendTool.allCases {
        try manifest.authorize(tool, bundleIdentifier: targetBundleIdentifier)
      }

      stage = .calibrate
      try manifest.authorize(.windowRead, bundleIdentifier: targetBundleIdentifier)
      let frame = try perception.windowFrame()
      evidence.windowFrameDigest = frame.digest
      guard frame.isPlausibleMainWindow else {
        throw SendFailure(
          .windowNotFound, detail: "the located window is too small to be the chat window")
      }

      // In the no-navigation mode the skill performs no input at all before
      // the recipient gate: it reads the screen, checks the title, and refuses
      // if the wrong conversation is open. A misfire is therefore read-only and
      // cannot disturb whatever the person was doing.
      if capability.addressingMode.typesBeforeRecipientGate {
        stage = .address
        try yieldToHuman(&evidence)
        // Addressing pastes *without* clearing first. A click that fails to
        // take focus leaves the caret wherever the person left it, and a
        // select-all plus delete there would destroy their unsent text.
        // Pasting only ever adds, and GATE 0 below catches the miss.
        try focusAndPaste(capability.searchKey, at: profile.anchors.searchBox, in: frame)
        effector.settle(milliseconds: pacing.afterSearchMilliseconds)

        // GATE 0: prove the click actually focused the search field by reading
        // the key back out of it. Without this, a failed focus silently sends
        // the rest of the skill's keystrokes into whatever the person was using.
        let echoed = try recognize(profile.ocrRegions.search, in: frame)
        evidence.searchKeyEchoed = SendText.normalized(echoed.text)
          .localizedCaseInsensitiveContains(SendText.normalized(capability.searchKey))
        guard evidence.searchKeyEchoed else {
          throw SendFailure(
            .addressingFocusFailed,
            detail: "the search field did not echo the search key, so the click missed its target"
          )
        }

        try yieldToHuman(&evidence)
        try manifest.authorize(.click, bundleIdentifier: targetBundleIdentifier)
        try effector.click(at: WindowGeometry.point(profile.anchors.firstResultRow, in: frame))
        effector.settle(milliseconds: pacing.afterClickMilliseconds)
      }

      stage = .recipientVerify
      try yieldToHuman(&evidence)
      let title = try recognize(profile.ocrRegions.title, in: frame)
      evidence.titleConfidencePartsPerMillion = title.confidencePartsPerMillion
      evidence.ambiguousSearchResult = title.candidateCount > 1
      evidence.titleMatched = SendText.matches(title.text, capability.expectedTitle)
      guard
        evidence.titleMatched,
        !evidence.ambiguousSearchResult,
        title.confidencePartsPerMillion >= profile.selftest.minimumTitleConfidencePartsPerMillion
      else {
        throw SendFailure(
          .recipientVerifyFailed,
          detail: "the opened conversation title did not match the approved recipient"
        )
      }

      stage = .compose
      // The compose box must be empty before the skill uses it. Two reasons:
      // it must never overwrite an unsent draft the person left there, and
      // requiring it empty means the skill never has to send a destructive
      // keystroke — it only ever pastes. Checked before anything is clicked, so
      // a refusal here touches nothing at all.
      let existing = try recognize(profile.ocrRegions.compose, in: frame)
      guard SendText.normalized(existing.text).isEmpty else {
        throw SendFailure(
          .composeNotEmpty,
          detail: "the compose box already holds unsent text"
        )
      }
      var composeFocusProven = false
      if let attachment = capability.attachment {
        // Recorded before staging so GATE 2a can prove the region changed,
        // which is the only evidence an image thumbnail offers.
        attachmentBaseline = try fingerprint(
          try requireAttachmentRegions().composeAttachment,
          in: frame
        )
        try stageAttachment(attachment, in: frame, evidence: &evidence)
      } else {
        try focusAndPaste(capability.body, at: profile.anchors.composeBox, in: frame)
      }

      stage = .contentVerify
      if let attachment = capability.attachment {
        // GATE 2a. An attachment's bytes never appear on screen, so this proves
        // *which* file was staged, not what it contains. The digest half of the
        // gate already ran in the control plane, against the staged copy.
        //
        // What is provable differs by kind, measured live: a file stages as a
        // chip carrying its name and size, which can be read back and matched;
        // an image stages as a bare thumbnail with no text at all, so the only
        // available evidence is that the compose region changed. The weaker
        // case is recorded as such rather than dressed up as a name match.
        let regions = try requireAttachmentRegions()
        let after = try fingerprint(regions.composeAttachment, in: frame)
        evidence.attachmentRegionChanged = after != attachmentBaseline
        evidence.attachmentStaged = evidence.attachmentRegionChanged
        if capability.capability == .imageSend {
          evidence.attachmentNameMatched = false
          evidence.composeMatched = evidence.attachmentStaged
          composeFocusProven = evidence.attachmentStaged
          guard evidence.attachmentStaged else {
            try clearComposeIfFocusProven(in: frame, focusProven: false)
            throw SendFailure(
              .attachmentVerifyFailed,
              detail: "the compose area did not change, so no image was staged"
            )
          }
        } else {
          let staged = try recognize(regions.composeAttachment, in: frame)
          evidence.attachmentNameMatched = SendText.normalized(staged.text)
            .localizedCaseInsensitiveContains(SendText.normalized(attachment.displayFileName))
          evidence.composeMatched = evidence.attachmentNameMatched
          composeFocusProven = evidence.attachmentStaged
          guard evidence.attachmentStaged, evidence.attachmentNameMatched else {
            try clearComposeIfFocusProven(in: frame, focusProven: composeFocusProven)
            throw SendFailure(
              .attachmentVerifyFailed,
              detail: "the staged attachment's name was not read back from the compose area"
            )
          }
        }
      } else {
        let composed = try recognize(profile.ocrRegions.compose, in: frame)
        evidence.composeMatched = SendText.matches(composed.text, capability.body)
        guard evidence.composeMatched else {
          try clearCompose(in: frame)
          throw SendFailure(
            .contentVerifyFailed,
            detail: "the composed text did not match the approved body"
          )
        }
      }

      guard capability.permitSend else {
        // The dry-run stage stops exactly here, with both gates satisfied and
        // the compose box cleared so nothing is left behind for a human to
        // send by accident. GATE 2 has just proven where the caret is, so
        // clearing is safe on this path in either mode.
        try clearCompose(in: frame)
        evidence.captureCount = perception.captureCount
        evidence.elapsedMilliseconds = elapsedMilliseconds(since: started)
        return outcome(
          capability,
          stage: .contentVerify,
          attempted: false,
          confirmation: .notAttempted,
          failure: nil,
          evidence: evidence
        )
      }

      stage = .send
      // The last possible moment to yield: after this the message may exist.
      try yieldToHuman(&evidence, clearComposeIn: frame)
      try manifest.authorize(.pressKey, bundleIdentifier: targetBundleIdentifier)
      try effector.press(.returnKey)
      attempted = true
      if capability.attachment != nil,
        let attachments = profile.attachments,
        attachments.presentsConfirmationSheet
      {
        // Some builds raise a sheet before an attachment goes out. It is an
        // extra verification point, not an obstacle: the sheet must name the
        // same file before it is confirmed.
        effector.settle(milliseconds: pacing.afterClickMilliseconds)
        let sheet = try recognize(attachments.confirmSheet, in: frame)
        let named = SendText.normalized(sheet.text)
          .localizedCaseInsensitiveContains(
            SendText.normalized(capability.attachment?.displayFileName ?? "")
          )
        if named {
          try manifest.authorize(.click, bundleIdentifier: targetBundleIdentifier)
          try effector.click(at: WindowGeometry.point(attachments.confirmSendButton, in: frame))
          evidence.confirmationSheetConfirmed = true
        } else {
          // Refuse to confirm a sheet that names something else, and leave it
          // on screen for the owner rather than dismissing it blind.
          effector.restoreClipboard()
          evidence.captureCount = perception.captureCount
          evidence.elapsedMilliseconds = elapsedMilliseconds(since: started)
          return outcome(
            capability,
            stage: .sendVerify,
            attempted: true,
            confirmation: .unconfirmed,
            failure: .attachmentVerifyFailed,
            evidence: evidence
          )
        }
      }
      effector.settle(milliseconds: pacing.afterReturnMilliseconds)

      stage = .sendVerify
      let afterCompose = try recognize(profile.ocrRegions.compose, in: frame)
      evidence.composeCleared = SendText.normalized(afterCompose.text).isEmpty
      let bubble = try recognize(profile.ocrRegions.newestOutgoing, in: frame)
      if let attachment = capability.attachment {
        // The bubble shows a name and a size, never the bytes, so this is the
        // strongest on-screen claim available: the right file appeared.
        evidence.newestOutgoingMatched = SendText.normalized(bubble.text)
          .localizedCaseInsensitiveContains(SendText.normalized(attachment.displayFileName))
      } else {
        evidence.newestOutgoingMatched = SendText.normalized(bubble.text)
          .contains(SendText.normalized(capability.body))
      }
      confirmation =
        evidence.composeCleared && evidence.newestOutgoingMatched ? .confirmed : .unconfirmed
      // The body must not be left sitting in the user's pasteboard once the
      // run is over, on this path as on every refusal path.
      effector.restoreClipboard()
      evidence.captureCount = perception.captureCount
      evidence.elapsedMilliseconds = elapsedMilliseconds(since: started)
      return outcome(
        capability,
        stage: .sendVerify,
        attempted: true,
        confirmation: confirmation,
        failure: confirmation == .confirmed ? nil : .sendUnconfirmed,
        evidence: evidence
      )
    } catch let failure as SendFailure {
      evidence.captureCount = perception.captureCount
      evidence.elapsedMilliseconds = elapsedMilliseconds(since: started)
      effector.restoreClipboard()
      return outcome(
        capability,
        stage: stage,
        attempted: attempted,
        confirmation: attempted ? .unconfirmed : .notAttempted,
        failure: failure.code,
        evidence: evidence
      )
    } catch {
      evidence.captureCount = perception.captureCount
      evidence.elapsedMilliseconds = elapsedMilliseconds(since: started)
      effector.restoreClipboard()
      return outcome(
        capability,
        stage: stage,
        attempted: attempted,
        confirmation: attempted ? .unconfirmed : .notAttempted,
        failure: .engineUnavailable,
        evidence: evidence
      )
    }
  }

  /// Locates and focuses the search box, confirms by capture, and never sends.
  /// This is the gate every calibration profile passes before first use.
  public func runCalibrationSelfTest() -> CalibrationSelfTestReport {
    var drift: [String] = []
    var focused = false
    var confidence: UInt32 = 0
    var digest = String(repeating: "0", count: 64)
    var failure: SendFailureCode?
    do {
      try manifest.authorize(.windowRead, bundleIdentifier: targetBundleIdentifier)
      let frame = try perception.windowFrame()
      digest = frame.digest
      if !frame.isPlausibleMainWindow {
        drift.append("window is smaller than a signed-in chat window")
        throw SendFailure(.windowNotFound)
      }
      try manifest.authorize(.click, bundleIdentifier: targetBundleIdentifier)
      try effector.click(at: WindowGeometry.point(profile.anchors.searchBox, in: frame))
      effector.settle(milliseconds: pacing.afterClickMilliseconds)
      let title = try recognize(profile.ocrRegions.title, in: frame)
      confidence = title.confidencePartsPerMillion
      focused = true
      if confidence < profile.selftest.minimumTitleConfidencePartsPerMillion {
        drift.append(
          "title region confidence \(confidence) is below the profile minimum "
            + "\(profile.selftest.minimumTitleConfidencePartsPerMillion)"
        )
        failure = .calibrationDrift
      }
      if SendText.normalized(title.text).isEmpty {
        drift.append("title region recognized no text at the profile's coordinates")
        failure = .calibrationDrift
      }
    } catch let error as SendFailure {
      failure = error.code
      if !error.detail.isEmpty { drift.append(error.detail) }
    } catch {
      failure = .engineUnavailable
    }
    return CalibrationSelfTestReport(
      calibrationProfileID: profile.profileID,
      passed: failure == nil,
      searchBoxFocused: focused,
      titleConfidencePartsPerMillion: confidence,
      windowFrameDigest: digest,
      driftReport: drift,
      failure: failure,
      observedAtUnixNanoseconds: clock()
    )
  }

  /// Focuses a field and pastes into it without clearing it first.
  ///
  /// Used for addressing, where a mis-aimed click must never be destructive.
  /// The search field is transient, so an appended paste is recoverable and is
  /// caught immediately by GATE 0.
  private func focusAndPaste(
    _ text: String,
    at anchor: WindowRelativePoint,
    in frame: WindowFrame
  ) throws(SendFailure) {
    guard !effector.humanActivityObserved() else {
      throw SendFailure(.humanCollision, detail: "user activity observed before taking focus")
    }
    try manifest.authorize(.click, bundleIdentifier: targetBundleIdentifier)
    try effector.click(at: WindowGeometry.point(anchor, in: frame))
    effector.settle(milliseconds: pacing.afterClickMilliseconds)
    try manifest.authorize(.clipboardWrite, bundleIdentifier: targetBundleIdentifier)
    try effector.writeClipboard(text)
    try manifest.authorize(.hotkey, bundleIdentifier: targetBundleIdentifier)
    try effector.press(.paste)
    effector.settle(milliseconds: pacing.afterPasteMilliseconds)
  }

  private func clearAndPaste(
    _ text: String,
    at anchor: WindowRelativePoint,
    in frame: WindowFrame
  ) throws(SendFailure) {
    // Checked again here rather than only at the stage boundary: focusing a
    // field is itself interference, so the machine must still be idle at the
    // moment we take focus, not merely when the stage began.
    guard !effector.humanActivityObserved() else {
      throw SendFailure(.humanCollision, detail: "user activity observed before taking focus")
    }
    try manifest.authorize(.click, bundleIdentifier: targetBundleIdentifier)
    try effector.click(at: WindowGeometry.point(anchor, in: frame))
    effector.settle(milliseconds: pacing.afterClickMilliseconds)
    try manifest.authorize(.hotkey, bundleIdentifier: targetBundleIdentifier)
    try effector.press(.selectAll)
    effector.settle(milliseconds: pacing.afterKeyMilliseconds)
    try effector.press(.delete)
    effector.settle(milliseconds: pacing.afterKeyMilliseconds)
    try manifest.authorize(.clipboardWrite, bundleIdentifier: targetBundleIdentifier)
    try effector.writeClipboard(text)
    try effector.press(.paste)
    effector.settle(milliseconds: pacing.afterPasteMilliseconds)
  }

  /// Places the staged file into the compose area.
  ///
  /// The pasteboard path is preferred because it is mechanically identical to
  /// the text send that is already gated: focus the compose box and press
  /// Cmd+V. The panel fallback is used only when the signed profile says this
  /// build does not accept a pasted file reference, and it is navigated
  /// keyboard-first so no coordinate inside the panel is ever guessed.
  private func stageAttachment(
    _ attachment: ActionAttachment,
    in frame: WindowFrame,
    evidence: inout HelperGateEvidence
  ) throws(SendFailure) {
    let attachments = try requireAttachmentRegions()
    if attachments.composeAcceptsPastedFile {
      guard !effector.humanActivityObserved() else {
        throw SendFailure(.humanCollision, detail: "user activity observed before pasting")
      }
      // The compose box was verified empty above, so focusing it is all that
      // is needed; nothing has to be deleted.
      try manifest.authorize(.click, bundleIdentifier: targetBundleIdentifier)
      try effector.click(at: WindowGeometry.point(profile.anchors.composeBox, in: frame))
      effector.settle(milliseconds: pacing.afterClickMilliseconds)
      try manifest.authorize(
        .clipboardWriteFileReference,
        bundleIdentifier: targetBundleIdentifier
      )
      try effector.writeClipboardFileReference(attachment.stagedPath)
      try manifest.authorize(.hotkey, bundleIdentifier: targetBundleIdentifier)
      try effector.press(.paste)
      effector.settle(milliseconds: pacing.afterPasteMilliseconds)
      return
    }
    try stageAttachmentThroughPanel(attachment, in: frame, evidence: &evidence)
  }

  /// The panel fallback: click attach, prove a panel appeared, then navigate it
  /// with Go to Folder and a pasted path.
  private func stageAttachmentThroughPanel(
    _ attachment: ActionAttachment,
    in frame: WindowFrame,
    evidence: inout HelperGateEvidence
  ) throws(SendFailure) {
    let attachments = try requireAttachmentRegions()
    guard !effector.humanActivityObserved() else {
      throw SendFailure(.humanCollision, detail: "user activity observed before taking focus")
    }
    let windowsBefore = effector.targetWindowCount()
    try manifest.authorize(.click, bundleIdentifier: targetBundleIdentifier)
    try effector.click(at: WindowGeometry.point(attachments.attachControl, in: frame))
    effector.settle(milliseconds: pacing.afterSearchMilliseconds)
    // The click that opens the panel is the dangerous step: a neighbouring
    // control would do something else entirely. Requiring a new window before
    // going further turns that into a gate rather than a hope.
    guard effector.targetWindowCount() > windowsBefore else {
      try manifest.authorize(.pressKey, bundleIdentifier: targetBundleIdentifier)
      try? effector.press(.escape)
      throw SendFailure(
        .attachPanelNotPresented,
        detail: "the attach control did not present a file panel"
      )
    }
    try manifest.authorize(.openPanelNavigate, bundleIdentifier: targetBundleIdentifier)
    try effector.press(.goToFolder)
    effector.settle(milliseconds: pacing.afterKeyMilliseconds)
    try manifest.authorize(.clipboardWrite, bundleIdentifier: targetBundleIdentifier)
    try effector.writeClipboard(attachment.stagedPath)
    try effector.press(.paste)
    effector.settle(milliseconds: pacing.afterKeyMilliseconds)
    try effector.press(.returnKey)
    effector.settle(milliseconds: pacing.afterSearchMilliseconds)
    try effector.press(.returnKey)
    effector.settle(milliseconds: pacing.afterPasteMilliseconds)
    evidence.attachmentStaged = true
  }

  /// The attachment section of the active profile, or a refusal. A build whose
  /// profile has no attachment section cannot send one.
  private func requireAttachmentRegions() throws(SendFailure) -> CalibrationAttachments {
    guard let attachments = profile.attachments else {
      throw SendFailure(
        .profileInvalid,
        detail: "the active calibration profile has no attachment section"
      )
    }
    return attachments
  }

  /// Clears the compose box only when the caret's location has been proven.
  private func clearComposeIfFocusProven(
    in frame: WindowFrame,
    focusProven: Bool
  ) throws(SendFailure) {
    guard focusProven else {
      effector.restoreClipboard()
      return
    }
    try clearCompose(in: frame)
  }

  private func clearCompose(in frame: WindowFrame) throws(SendFailure) {
    try manifest.authorize(.click, bundleIdentifier: targetBundleIdentifier)
    try effector.click(at: WindowGeometry.point(profile.anchors.composeBox, in: frame))
    effector.settle(milliseconds: pacing.afterClickMilliseconds)
    try manifest.authorize(.hotkey, bundleIdentifier: targetBundleIdentifier)
    try effector.press(.selectAll)
    try effector.press(.delete)
    effector.restoreClipboard()
  }

  private func yieldToHuman(
    _ evidence: inout HelperGateEvidence,
    clearComposeIn frame: WindowFrame? = nil
  ) throws(SendFailure) {
    guard effector.humanActivityObserved() else { return }
    evidence.humanActivityObserved = true
    if let frame { try? clearCompose(in: frame) }
    throw SendFailure(.humanCollision, detail: "real user activity was observed on the client")
  }

  /// A content digest of one window-relative region.
  private func fingerprint(
    _ region: WindowRelativeRect,
    in frame: WindowFrame
  ) throws(SendFailure) -> String {
    try manifest.authorize(.windowCapture, bundleIdentifier: targetBundleIdentifier)
    return try perception.regionFingerprint(in: WindowGeometry.rect(region, in: frame))
  }

  private func recognize(
    _ region: WindowRelativeRect,
    in frame: WindowFrame
  ) throws(SendFailure) -> RecognizedRegionText {
    try manifest.authorize(.windowCapture, bundleIdentifier: targetBundleIdentifier)
    return try perception.recognizeText(in: WindowGeometry.rect(region, in: frame))
  }

  private func elapsedMilliseconds(since started: UInt64) -> UInt64 {
    let now = clock()
    return now > started ? (now - started) / 1_000_000 : 0
  }

  private func outcome(
    _ capability: ActionCapabilityEnvelope,
    stage: SendStage,
    attempted: Bool,
    confirmation: VisualConfirmation,
    failure: SendFailureCode?,
    evidence: HelperGateEvidence
  ) -> HelperSendOutcome {
    // A capability that forbids sending can never produce an attempted
    // outcome, whatever the state machine believed.
    let attempted = attempted && capability.permitSend
    return HelperSendOutcome(
      capabilityID: capability.capabilityID,
      capabilityBindingSHA256: capability.bindingSHA256,
      helperVersion: helperVersion,
      engineVersion: engineVersion,
      calibrationProfileID: profile.profileID,
      stageReached: stage,
      attempted: attempted,
      visualConfirmation: attempted ? confirmation : .notAttempted,
      failure: failure,
      evidence: evidence,
      observedAtUnixNanoseconds: clock()
    )
  }
}

/// When real user activity counts as a collision with an in-flight skill.
///
/// This is a policy, not a measurement, so it lives here where it can be tested
/// rather than inside the effector where it cannot.
///
/// The rule is deliberately strict, and it was made stricter after a live
/// incident: a background click does not merely avoid raising the target, it
/// *moves keyboard focus inside it*. A person typing into the compose box can
/// therefore find their keystrokes arriving in the search box the instant the
/// skill focuses it — interference without any window ever coming forward.
///
/// So the machine must be idle before the skill touches anything, and any
/// input at all during a run aborts it. Scoping the check to "is the target
/// frontmost" is not sound: that signal is unreliable when sampled from a
/// non-GUI process, and treating "not frontmost" as permission to act is
/// exactly what produced the incident. Frontmost is used only to make the
/// requirement *stricter*, never to waive it.
public enum HumanCollisionPolicy {
  /// How long the machine must have been idle before the skill may act.
  /// A background sender can afford to wait; a person mid-sentence cannot.
  public static let defaultIdleThresholdSeconds: TimeInterval = 5

  /// The longer idle window required when the target application is frontmost,
  /// which means the person is most likely working in it right now.
  public static let frontmostIdleThresholdSeconds: TimeInterval = 15

  /// Whether an in-flight skill must yield.
  ///
  /// - Parameters:
  ///   - targetIsFrontmost: whether the client is frontmost. Only ever raises
  ///     the bar; it can never lower it.
  ///   - idleSecondsByEventType: seconds since the last hardware event of each
  ///     sampled type.
  ///   - thresholdSeconds: the idle window required on a quiet machine.
  ///   - frontmostThresholdSeconds: the idle window required when the target is
  ///     frontmost.
  public static func mustYield(
    targetIsFrontmost: Bool,
    idleSecondsByEventType: [TimeInterval],
    thresholdSeconds: TimeInterval = defaultIdleThresholdSeconds,
    frontmostThresholdSeconds: TimeInterval = frontmostIdleThresholdSeconds
  ) -> Bool {
    let required =
      targetIsFrontmost ? max(thresholdSeconds, frontmostThresholdSeconds) : thresholdSeconds
    return idleSecondsByEventType.contains { $0 < required }
  }
}
