import Foundation
import Testing

@testable import GreenBubblesSendKit

/// The onboarding plan and the status projection are what the user and support
/// actually see, so both are held to the same fail-closed standard as the send
/// path itself.
struct OnboardingAndStatusTests {
  private func status(
    accessibility: Bool = true,
    screenRecording: Bool = true,
    running: Bool = true,
    loggedIn: Bool = true,
    windowFound: Bool = true,
    healthy: Bool = true,
    manifest: [String] = ["com.tencent.xinWeChat:click+clipboardWrite"]
  ) -> HelperCapabilityStatus {
    HelperCapabilityStatus(
      helperVersion: "1.0.0",
      engineVersion: "1.0.0",
      accessibilityGranted: accessibility,
      screenRecordingGranted: screenRecording,
      wechatRunning: running,
      wechatLoggedIn: loggedIn,
      wechatBundleIdentifier: "com.tencent.xinWeChat",
      wechatMarketingVersion: "4.1.13",
      wechatBuild: "4.1.13.269579",
      macosBuild: "25G83",
      macosMajor: 26,
      mainWindowFound: windowFound,
      activeCalibrationProfileID: "wechat-4.1.13.269579-macos-26",
      engineHealthy: healthy,
      boundedManifestScope: manifest,
      observedAtUnixNanoseconds: 1
    )
  }

  @Test("a ready environment has no blocking failure")
  func readyEnvironment() {
    #expect(status().blockingFailure == nil)
    #expect(OnboardingPlan.make(from: status()).complete)
  }

  @Test("each missing prerequisite is reported in repair order")
  func missingPrerequisites() {
    let cases: [(HelperCapabilityStatus, SendFailureCode)] = [
      (status(accessibility: false), .grantsMissing),
      (status(screenRecording: false), .grantsMissing),
      (status(healthy: false), .engineUnavailable),
      (status(running: false), .wechatNotRunning),
      (status(loggedIn: false), .notLoggedIn),
      (status(windowFound: false), .windowNotFound),
      (status(manifest: []), .manifestViolation),
    ]
    for (status, expected) in cases {
      #expect(status.blockingFailure == expected)
    }
  }

  @Test("the onboarding plan names the exact pane for each missing grant")
  func onboardingPlan() {
    let plan = OnboardingPlan.make(from: status(accessibility: false, screenRecording: false))
    #expect(!plan.complete)
    #expect(plan.sendPathBlockedBy == .grantsMissing)
    #expect(plan.steps.count == 2)
    for step in plan.steps {
      #expect(!step.granted)
      #expect(step.settingsURL.absoluteString.hasPrefix("x-apple.systempreferences:"))
      #expect(!step.instructions.isEmpty)
      #expect(step.title.contains("GreenBubblesInputHelper"))
    }
    #expect(plan.steps[0].settingsURL.absoluteString.contains("Privacy_Accessibility"))
    #expect(plan.steps[1].settingsURL.absoluteString.contains("Privacy_ScreenCapture"))
  }

  @Test("every failure code names one operator action")
  func failureActions() {
    for code in SendFailureCode.allCases {
      #expect(code.operatorAction.count > 16, "\(code) has no actionable guidance")
    }
  }

  @Test("the bounded manifest describes its scope and refuses everything else")
  func boundedManifest() throws {
    let manifest = BoundedCapabilityManifest.weChatOnly
    #expect(manifest.scopeDescription.count == 1)
    #expect(manifest.scopeDescription[0].hasPrefix("com.tencent.xinWeChat:"))
    try manifest.authorize(.click, bundleIdentifier: "com.tencent.xinWeChat")
    #expect(throws: SendFailure.self) {
      try manifest.authorize(.click, bundleIdentifier: "com.apple.Safari")
    }
    let refusal = #expect(throws: SendFailure.self) {
      try manifest.authorize(.clipboardWrite, bundleIdentifier: "com.apple.Safari")
    }
    #expect(refusal?.code == .manifestViolation)
  }

  @Test("the code-signing requirement refuses every peer when the team is unset")
  func codeSigningRequirement() {
    let unconfigured = SendHelperIdentity.codeSigningRequirement(teamIdentifier: "")
    #expect(unconfigured.contains("never.matches"))
    let configured = SendHelperIdentity.codeSigningRequirement(teamIdentifier: "5A4RE8SF68")
    #expect(configured.contains("anchor apple generic"))
    #expect(configured.contains("5A4RE8SF68"))
  }

  @Test("a capability that forbids sending cannot claim a send-permitting stage")
  func dryRunCannotClaimSend() throws {
    let body = "x"
    func build(stage: SendRolloutStage, permitSend: Bool, binding: String)
      -> ActionCapabilityEnvelope
    {
      ActionCapabilityEnvelope(
        formatVersion: SendContract.version,
        capabilityID: String(repeating: "1", count: 64),
        actionID: String(repeating: "2", count: 64),
        draftID: String(repeating: "3", count: 64),
        approvalID: String(repeating: "4", count: 64),
        idempotencyKey: String(repeating: "5", count: 64),
        accountID: "account",
        conversationID: "filehelper",
        capability: .textSend,
        addressingMode: .search,
        searchKey: "File Transfer",
        expectedTitle: "File Transfer",
        body: body,
        bodySHA256: SendDigest.sha256Hex(Data(body.utf8)),
        normalizedBodySHA256: SendText.normalizedSHA256(body),
        clientBuildProfileID: "wechat-macos-4.1.13-269579",
        calibrationProfileID: "profile",
        calibrationProfileSHA256: String(repeating: "6", count: 64),
        attachment: nil,
        rolloutStage: stage,
        permitSend: permitSend,
        issuedAtUnixNanoseconds: 1_000,
        validUntilUnixNanoseconds: 9_000,
        bindingSHA256: binding
      )
    }
    let draft = build(stage: .dryRun, permitSend: true, binding: "")
    let capability = build(
      stage: .dryRun,
      permitSend: true,
      binding: draft.computedBindingSHA256 ?? ""
    )
    let refusal = #expect(throws: SendFailure.self) {
      try capability.validate(nowUnixNanoseconds: 2_000)
    }
    #expect(refusal?.code == .capabilityMismatch)
  }
}

/// The human-collision policy decides when an in-flight skill may touch the
/// client at all. Getting it wrong in the lax direction types over a person
/// mid-sentence, which is exactly what a live run did before this was
/// tightened, so the tests pin both directions.
struct HumanCollisionPolicyTests {
  @Test("recent input anywhere blocks the skill, even when the target is not frontmost")
  func recentInputAnywhereBlocks() {
    // The lesson from the incident: a background click moves keyboard focus
    // inside the target, so input elsewhere is not proof of safety.
    #expect(
      HumanCollisionPolicy.mustYield(
        targetIsFrontmost: false,
        idleSecondsByEventType: [0.4, 30, 30]
      ))
  }

  @Test("the machine must be idle for the full window before the skill acts")
  func idleWindowIsRequired() {
    #expect(
      HumanCollisionPolicy.mustYield(
        targetIsFrontmost: false,
        idleSecondsByEventType: [4.9]
      ))
    #expect(
      !HumanCollisionPolicy.mustYield(
        targetIsFrontmost: false,
        idleSecondsByEventType: [5.1]
      ))
  }

  @Test("a frontmost target raises the bar rather than lowering it")
  func frontmostOnlyRaisesTheBar() {
    // Idle long enough for a quiet machine, but not for one the person is
    // looking at right now.
    #expect(
      HumanCollisionPolicy.mustYield(
        targetIsFrontmost: true,
        idleSecondsByEventType: [8]
      ))
    #expect(
      !HumanCollisionPolicy.mustYield(
        targetIsFrontmost: true,
        idleSecondsByEventType: [16]
      ))
    // Frontmost can never make an otherwise-blocked run permissible.
    for idle in [0.1, 1.0, 4.9] {
      #expect(
        HumanCollisionPolicy.mustYield(targetIsFrontmost: true, idleSecondsByEventType: [idle]))
      #expect(
        HumanCollisionPolicy.mustYield(targetIsFrontmost: false, idleSecondsByEventType: [idle]))
    }
  }

  @Test("no sampled events is never a collision")
  func noSamplesIsNotACollision() {
    #expect(!HumanCollisionPolicy.mustYield(targetIsFrontmost: true, idleSecondsByEventType: []))
  }
}
