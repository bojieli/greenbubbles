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
}

/// The tools the bounded capability manifest can grant.
public enum SendTool: String, Equatable, Sendable, CaseIterable {
  case click
  case hotkey
  case pressKey
  case clipboardWrite
  case windowRead
  case windowCapture
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

      stage = .address
      try yieldToHuman(&evidence)
      try clearAndPaste(capability.searchKey, at: profile.anchors.searchBox, in: frame)
      effector.settle(milliseconds: pacing.afterSearchMilliseconds)
      try manifest.authorize(.click, bundleIdentifier: targetBundleIdentifier)
      try effector.click(at: WindowGeometry.point(profile.anchors.firstResultRow, in: frame))
      effector.settle(milliseconds: pacing.afterClickMilliseconds)

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
      try clearAndPaste(capability.body, at: profile.anchors.composeBox, in: frame)

      stage = .contentVerify
      let composed = try recognize(profile.ocrRegions.compose, in: frame)
      evidence.composeMatched = SendText.matches(composed.text, capability.body)
      guard evidence.composeMatched else {
        try clearCompose(in: frame)
        throw SendFailure(
          .contentVerifyFailed,
          detail: "the composed text did not match the approved body"
        )
      }

      guard capability.permitSend else {
        // The dry-run stage stops exactly here, with both gates satisfied and
        // the compose box cleared so nothing is left behind for a human to
        // send by accident.
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
      effector.settle(milliseconds: pacing.afterReturnMilliseconds)

      stage = .sendVerify
      let afterCompose = try recognize(profile.ocrRegions.compose, in: frame)
      evidence.composeCleared = SendText.normalized(afterCompose.text).isEmpty
      let bubble = try recognize(profile.ocrRegions.newestOutgoing, in: frame)
      evidence.newestOutgoingMatched = SendText.normalized(bubble.text)
        .contains(SendText.normalized(capability.body))
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

  private func clearAndPaste(
    _ text: String,
    at anchor: WindowRelativePoint,
    in frame: WindowFrame
  ) throws(SendFailure) {
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
