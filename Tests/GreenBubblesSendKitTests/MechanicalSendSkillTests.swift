import CoreGraphics
import Foundation
import Testing

@testable import GreenBubblesSendKit

/// Adversarial coverage of the mechanical send skill. Every gate must abort
/// closed, every abort must leave the client clean, and a capability that
/// forbids sending must never produce an attempted outcome.
struct MechanicalSendSkillTests {
  private enum Region { case title, compose, newestOutgoing, unknown }

  private final class FakePerception: ScreenPerception {
    var frame: WindowFrame
    var titleResult: Result<RecognizedRegionText, SendFailure>
    var composeResult: Result<RecognizedRegionText, SendFailure>
    var composeAfterSendResult: Result<RecognizedRegionText, SendFailure>?
    var newestOutgoingResult: Result<RecognizedRegionText, SendFailure>
    var frameFailure: SendFailure?
    private(set) var captures: UInt32 = 0
    private var composeReads = 0
    private let profile: CalibrationProfileBody

    init(profile: CalibrationProfileBody, frame: WindowFrame) {
      self.profile = profile
      self.frame = frame
      titleResult = .success(RecognizedRegionText(text: "", confidencePartsPerMillion: 0))
      composeResult = .success(RecognizedRegionText(text: "", confidencePartsPerMillion: 0))
      newestOutgoingResult = .success(
        RecognizedRegionText(text: "", confidencePartsPerMillion: 0)
      )
    }

    var captureCount: UInt32 { captures }

    func windowFrame() throws(SendFailure) -> WindowFrame {
      if let frameFailure { throw frameFailure }
      return frame
    }

    func recognizeText(in rect: CGRect) throws(SendFailure) -> RecognizedRegionText {
      captures &+= 1
      switch classify(rect) {
      case .title: return try titleResult.get()
      case .compose:
        composeReads += 1
        if composeReads > 1, let after = composeAfterSendResult { return try after.get() }
        return try composeResult.get()
      case .newestOutgoing: return try newestOutgoingResult.get()
      case .unknown:
        throw SendFailure(.calibrationDrift, detail: "unclassified region")
      }
    }

    private func classify(_ rect: CGRect) -> Region {
      for (region, kind) in [
        (profile.ocrRegions.title, Region.title),
        (profile.ocrRegions.compose, Region.compose),
        (profile.ocrRegions.newestOutgoing, Region.newestOutgoing),
      ] where WindowGeometry.rect(region, in: frame).equalTo(rect) {
        return kind
      }
      return .unknown
    }
  }

  private final class FakeEffector: InputEffector {
    private(set) var actions: [String] = []
    private(set) var restoreCount = 0
    var humanActive = false
    var clipboard: String?
    var clickFailure: SendFailure?

    func click(at point: CGPoint) throws(SendFailure) {
      if let clickFailure { throw clickFailure }
      actions.append("click(\(Int(point.x)),\(Int(point.y)))")
    }

    func press(_ key: SendKey) throws(SendFailure) {
      actions.append("press(\(key.rawValue))")
    }

    func writeClipboard(_ text: String) throws(SendFailure) {
      clipboard = text
      actions.append("clipboard(\(text))")
    }

    func restoreClipboard() {
      restoreCount += 1
      actions.append("restoreClipboard")
    }

    func humanActivityObserved() -> Bool { humanActive }

    func settle(milliseconds: UInt64) {}

    var pastedTexts: [String] {
      actions.compactMap { action in
        action.hasPrefix("clipboard(") ? String(action.dropFirst(10).dropLast()) : nil
      }
    }
  }

  private let body = "adapter self-check"
  private let title = "File Transfer"

  private func profile(profileID: String = "test-profile") -> CalibrationProfileBody {
    CalibrationProfileBody(
      schema: 1,
      profileID: profileID,
      wechatBundleIdentifier: "com.tencent.xinWeChat",
      wechatMarketingVersion: "4.1.13",
      wechatBuild: "4.1.13.269579",
      clientBuildProfileID: "wechat-macos-4.1.13-269579",
      macosMajor: 26,
      anchors: CalibrationAnchors(
        searchBox: WindowRelativePoint(xPartsPerMillion: 235_000, yPartsPerMillion: 36_000),
        firstResultRow: WindowRelativePoint(xPartsPerMillion: 235_000, yPartsPerMillion: 115_000),
        composeBox: WindowRelativePoint(xPartsPerMillion: 715_000, yPartsPerMillion: 870_000)
      ),
      ocrRegions: CalibrationOCRRegions(
        title: WindowRelativeRect(
          xPartsPerMillion: 440_000,
          yPartsPerMillion: 20_000,
          widthPartsPerMillion: 300_000,
          heightPartsPerMillion: 50_000
        ),
        compose: WindowRelativeRect(
          xPartsPerMillion: 400_000,
          yPartsPerMillion: 830_000,
          widthPartsPerMillion: 560_000,
          heightPartsPerMillion: 110_000
        ),
        newestOutgoing: WindowRelativeRect(
          xPartsPerMillion: 620_000,
          yPartsPerMillion: 640_000,
          widthPartsPerMillion: 280_000,
          heightPartsPerMillion: 150_000
        )
      ),
      selftest: CalibrationSelfTestExpectation(
        focusIndicator: "search_caret",
        minimumTitleConfidencePartsPerMillion: 900_000
      ),
      issuedAtUnixSeconds: 1,
      expiresAtUnixSeconds: 4_000_000_000
    )
  }

  private func capability(
    permitSend: Bool,
    profileID: String = "test-profile",
    issuedAt: UInt64 = 1_000,
    validUntil: UInt64 = 9_000
  ) -> ActionCapabilityEnvelope {
    func build(binding: String) -> ActionCapabilityEnvelope {
      ActionCapabilityEnvelope(
        formatVersion: SendContract.version,
        capabilityID: String(repeating: "1", count: 64),
        actionID: String(repeating: "2", count: 64),
        draftID: String(repeating: "3", count: 64),
        approvalID: String(repeating: "4", count: 64),
        idempotencyKey: String(repeating: "5", count: 64),
        accountID: "account",
        conversationID: "filehelper",
        searchKey: title,
        expectedTitle: title,
        body: body,
        bodySHA256: SendDigest.sha256Hex(Data(body.utf8)),
        normalizedBodySHA256: SendText.normalizedSHA256(body),
        clientBuildProfileID: "wechat-macos-4.1.13-269579",
        calibrationProfileID: profileID,
        calibrationProfileSHA256: String(repeating: "6", count: 64),
        rolloutStage: permitSend ? .selfSend : .dryRun,
        permitSend: permitSend,
        issuedAtUnixNanoseconds: issuedAt,
        validUntilUnixNanoseconds: validUntil,
        bindingSHA256: binding
      )
    }
    let draft = build(binding: "")
    return build(binding: draft.computedBindingSHA256 ?? "")
  }

  private func makeSkill(
    profile: CalibrationProfileBody,
    effector: FakeEffector,
    perception: FakePerception,
    manifest: BoundedCapabilityManifest = .weChatOnly,
    now: UInt64 = 2_000
  ) -> MechanicalSendSkill {
    MechanicalSendSkill(
      profile: profile,
      manifest: manifest,
      targetBundleIdentifier: "com.tencent.xinWeChat",
      effector: effector,
      perception: perception,
      pacing: SendPacing(
        afterClickMilliseconds: 0,
        afterKeyMilliseconds: 0,
        afterPasteMilliseconds: 0,
        afterSearchMilliseconds: 0,
        afterReturnMilliseconds: 0
      ),
      helperVersion: "1.0.0",
      engineVersion: "1.0.0",
      clock: { now }
    )
  }

  private func perception(
    _ profile: CalibrationProfileBody,
    titleText: String? = nil,
    titleConfidence: UInt32 = 1_000_000,
    titleCandidates: Int = 1,
    composeText: String? = nil
  ) -> FakePerception {
    let perception = FakePerception(
      profile: profile,
      frame: WindowFrame(x: 100, y: 80, width: 1_200, height: 800)
    )
    perception.titleResult = .success(
      RecognizedRegionText(
        text: titleText ?? title,
        confidencePartsPerMillion: titleConfidence,
        candidateCount: titleCandidates
      )
    )
    perception.composeResult = .success(
      RecognizedRegionText(
        text: composeText ?? body,
        confidencePartsPerMillion: 1_000_000
      )
    )
    return perception
  }

  @Test("a dry run reaches both gates and stops before Return")
  func dryRunStopsBeforeReturn() {
    let profile = profile()
    let effector = FakeEffector()
    let perception = perception(profile)
    let outcome = makeSkill(profile: profile, effector: effector, perception: perception)
      .execute(capability(permitSend: false))
    #expect(!(outcome.attempted))
    #expect(outcome.failure == nil)
    #expect(outcome.stageReached == .contentVerify)
    #expect(outcome.visualConfirmation == .notAttempted)
    #expect(outcome.evidence.titleMatched)
    #expect(outcome.evidence.composeMatched)
    #expect(!(effector.actions.contains("press(returnKey)")))
    // The compose box is emptied and the user's clipboard put back.
    #expect(
      effector.actions.suffix(3) == ["press(selectAll)", "press(delete)", "restoreClipboard"])
    #expect(effector.pastedTexts == [title, body])
  }

  @Test("a wrong recipient title aborts before the body is ever pasted")
  func wrongRecipientAborts() {
    let profile = profile()
    let effector = FakeEffector()
    let perception = perception(profile, titleText: "Someone Else")
    let outcome = makeSkill(profile: profile, effector: effector, perception: perception)
      .execute(capability(permitSend: true))
    #expect(outcome.failure == .recipientVerifyFailed)
    #expect(outcome.stageReached == .recipientVerify)
    #expect(!(outcome.attempted))
    #expect(!(outcome.evidence.titleMatched))
    #expect(!(effector.pastedTexts.contains(body)))
    #expect(!(effector.actions.contains("press(returnKey)")))
  }

  @Test("a low-confidence or ambiguous title aborts")
  func lowConfidenceOrAmbiguousTitleAborts() {
    for (confidence, candidates, ambiguous) in [
      (UInt32(500_000), 1, false), (UInt32(1_000_000), 2, true),
    ] {
      let profile = profile()
      let effector = FakeEffector()
      let perception = perception(
        profile,
        titleConfidence: confidence,
        titleCandidates: candidates
      )
      let outcome = makeSkill(profile: profile, effector: effector, perception: perception)
        .execute(capability(permitSend: true))
      #expect(outcome.failure == .recipientVerifyFailed)
      #expect(outcome.evidence.ambiguousSearchResult == ambiguous)
      #expect(!(effector.actions.contains("press(returnKey)")))
    }
  }

  @Test("a content mismatch aborts and clears the compose box")
  func contentMismatchAborts() {
    let profile = profile()
    let effector = FakeEffector()
    let perception = perception(profile, composeText: "a different body")
    let outcome = makeSkill(profile: profile, effector: effector, perception: perception)
      .execute(capability(permitSend: true))
    #expect(outcome.failure == .contentVerifyFailed)
    #expect(outcome.stageReached == .contentVerify)
    #expect(!(outcome.attempted))
    #expect(!(effector.actions.contains("press(returnKey)")))
    // The compose box is emptied before the refusal is reported, and the
    // user's clipboard is put back on the way out.
    let bodyPaste = effector.actions.firstIndex(of: "clipboard(\(body))")
    let clearIndex = effector.actions.lastIndex(of: "press(delete)")
    #expect(bodyPaste != nil)
    #expect(clearIndex != nil)
    #expect(bodyPaste ?? 0 < clearIndex ?? 0)
    #expect(effector.actions.last == "restoreClipboard")
  }

  @Test("real user activity aborts the run and takeover wins")
  func humanCollisionAborts() {
    let profile = profile()
    let effector = FakeEffector()
    effector.humanActive = true
    let perception = perception(profile)
    let outcome = makeSkill(profile: profile, effector: effector, perception: perception)
      .execute(capability(permitSend: true))
    #expect(outcome.failure == .humanCollision)
    #expect(outcome.evidence.humanActivityObserved)
    #expect(!(outcome.attempted))
    #expect(effector.pastedTexts.isEmpty)
  }

  @Test("a permitted send is confirmed only when both post-conditions hold")
  func permittedSendConfirmation() {
    let profile = profile()
    let effector = FakeEffector()
    let perception = perception(profile)
    perception.composeAfterSendResult = .success(
      RecognizedRegionText(text: "", confidencePartsPerMillion: 1_000_000)
    )
    perception.newestOutgoingResult = .success(
      RecognizedRegionText(text: "10:32  \(body)", confidencePartsPerMillion: 1_000_000)
    )
    let outcome = makeSkill(profile: profile, effector: effector, perception: perception)
      .execute(capability(permitSend: true))
    #expect(outcome.attempted)
    #expect(outcome.visualConfirmation == .confirmed)
    #expect(outcome.failure == nil)
    #expect(outcome.stageReached == .sendVerify)
    #expect(effector.actions.contains("press(returnKey)"))
    // The approved body never stays in the user's pasteboard.
    #expect(effector.actions.last == "restoreClipboard")
  }

  @Test("a permitted send whose compose box still has text is unconfirmed")
  func permittedSendUnconfirmed() {
    let profile = profile()
    let effector = FakeEffector()
    let perception = perception(profile)
    perception.composeAfterSendResult = .success(
      RecognizedRegionText(text: body, confidencePartsPerMillion: 1_000_000)
    )
    perception.newestOutgoingResult = .success(
      RecognizedRegionText(text: "", confidencePartsPerMillion: 1_000_000)
    )
    let outcome = makeSkill(profile: profile, effector: effector, perception: perception)
      .execute(capability(permitSend: true))
    #expect(outcome.attempted)
    #expect(outcome.visualConfirmation == .unconfirmed)
    #expect(outcome.failure == .sendUnconfirmed)
    #expect(!(outcome.evidence.composeCleared))
  }

  @Test("a capability for another calibration profile is refused before any input")
  func foreignProfileRefused() {
    let profile = profile()
    let effector = FakeEffector()
    let perception = perception(profile)
    let outcome = makeSkill(profile: profile, effector: effector, perception: perception)
      .execute(capability(permitSend: true, profileID: "some-other-profile"))
    #expect(outcome.failure == .calibrationDrift)
    #expect(outcome.stageReached == .precheck)
    #expect(effector.actions.filter { $0 != "restoreClipboard" }.isEmpty)
  }

  @Test("an expired capability is refused before any input")
  func expiredCapabilityRefused() {
    let profile = profile()
    let effector = FakeEffector()
    let perception = perception(profile)
    let outcome = makeSkill(profile: profile, effector: effector, perception: perception)
      .execute(capability(permitSend: true, issuedAt: 1, validUntil: 2))
    #expect(outcome.failure == .capabilityExpired)
    #expect(outcome.stageReached == .precheck)
  }

  @Test("a window too small to be the chat surface aborts")
  func smallWindowAborts() {
    let profile = profile()
    let effector = FakeEffector()
    let perception = perception(profile)
    perception.frame = WindowFrame(x: 0, y: 0, width: 320, height: 420)
    let outcome = makeSkill(profile: profile, effector: effector, perception: perception)
      .execute(capability(permitSend: true))
    #expect(outcome.failure == .windowNotFound)
    #expect(outcome.stageReached == .calibrate)
  }

  @Test("a tool outside the bounded manifest is refused")
  func manifestToolRefused() {
    let profile = profile()
    let effector = FakeEffector()
    let perception = perception(profile)
    let manifest = BoundedCapabilityManifest(
      allowedBundleIdentifiers: ["com.tencent.xinWeChat"],
      allowedTools: [.click, .hotkey, .pressKey, .windowRead, .windowCapture]
    )
    let outcome = makeSkill(
      profile: profile,
      effector: effector,
      perception: perception,
      manifest: manifest
    ).execute(capability(permitSend: true))
    #expect(outcome.failure == .manifestViolation)
    #expect(outcome.stageReached == .precheck)
  }

  @Test("a manifest for another application is refused")
  func manifestApplicationRefused() {
    let profile = profile()
    let manifest = BoundedCapabilityManifest(
      allowedBundleIdentifiers: ["com.example.other"],
      allowedTools: Set(SendTool.allCases)
    )
    let outcome = makeSkill(
      profile: profile,
      effector: FakeEffector(),
      perception: perception(profile),
      manifest: manifest
    ).execute(capability(permitSend: false))
    #expect(outcome.failure == .manifestViolation)
  }

  @Test("the calibration self-test never sends and reports drift")
  func calibrationSelfTest() {
    let profile = profile()
    let effector = FakeEffector()
    let passing = makeSkill(
      profile: profile,
      effector: effector,
      perception: perception(profile)
    ).runCalibrationSelfTest()
    #expect(passing.passed)
    #expect(passing.searchBoxFocused)
    #expect(passing.driftReport.isEmpty)
    #expect(!(effector.actions.contains("press(returnKey)")))

    let drifted = makeSkill(
      profile: profile,
      effector: FakeEffector(),
      perception: perception(profile, titleText: "", titleConfidence: 100_000)
    ).runCalibrationSelfTest()
    #expect(!(drifted.passed))
    #expect(drifted.failure == .calibrationDrift)
    #expect(drifted.driftReport.count == 2)
  }

  @Test("window geometry maps parts-per-million onto the live frame")
  func windowGeometry() {
    let frame = WindowFrame(x: 100, y: 80, width: 1_000, height: 500)
    let point = WindowGeometry.point(
      WindowRelativePoint(xPartsPerMillion: 250_000, yPartsPerMillion: 500_000),
      in: frame
    )
    #expect(abs(point.x - 350) < 0.001)
    #expect(abs(point.y - 330) < 0.001)
    let rect = WindowGeometry.rect(
      WindowRelativeRect(
        xPartsPerMillion: 100_000,
        yPartsPerMillion: 200_000,
        widthPartsPerMillion: 300_000,
        heightPartsPerMillion: 400_000
      ),
      in: frame
    )
    #expect(abs(rect.origin.x - 200) < 0.001)
    #expect(abs(rect.origin.y - 180) < 0.001)
    #expect(abs(rect.size.width - 300) < 0.001)
    #expect(abs(rect.size.height - 200) < 0.001)
  }
}
