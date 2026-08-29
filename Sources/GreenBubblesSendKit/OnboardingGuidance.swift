import Foundation

/// The two grants the helper needs, and where the user turns them on.
///
/// macOS does not let any software grant itself Accessibility or Screen
/// Recording, so this is the one unavoidable manual step. Onboarding never
/// assumes a toggle state: it probes live, deep-links to the exact pane, and
/// keeps the send path closed until the probe passes.
public enum SendGrant: String, CaseIterable, Sendable {
  case accessibility
  case screenRecording

  /// The System Settings pane that grants this permission.
  public var settingsURL: URL {
    switch self {
    case .accessibility:
      URL(
        string:
          "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
      )!
    case .screenRecording:
      URL(
        string:
          "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
      )!
    }
  }

  /// The name shown in System Settings, so the instructions can be exact.
  public var displayName: String {
    switch self {
    case .accessibility: "Accessibility"
    case .screenRecording: "Screen Recording"
    }
  }

  /// Why the helper needs it, in one sentence the user can act on.
  public var rationale: String {
    switch self {
    case .accessibility:
      "GreenBubblesInputHelper synthesizes the clicks and keystrokes that focus WeChat's search and compose boxes."
    case .screenRecording:
      "GreenBubblesInputHelper captures WeChat's window on device so it can verify the recipient and the composed text before anything is sent."
    }
  }

  /// The steps to grant it, written for the current System Settings layout.
  public func instructions(helperName: String) -> [String] {
    [
      "Open System Settings › Privacy & Security › \(displayName).",
      "Turn on the switch next to \(helperName).",
      "Return to GreenBubbles; the permission probe re-runs automatically.",
    ]
  }
}

/// One step of the guided permissions onboarding.
public struct OnboardingStep: Equatable, Sendable {
  public let grant: SendGrant
  public let granted: Bool
  public let title: String
  public let rationale: String
  public let instructions: [String]
  public let settingsURL: URL
}

/// The onboarding plan derived from a live capability probe. It is always
/// derived from an observation, never from a remembered toggle state.
public struct OnboardingPlan: Equatable, Sendable {
  public let steps: [OnboardingStep]
  public let sendPathBlockedBy: SendFailureCode?

  /// Whether every grant the helper needs is present.
  public var complete: Bool { steps.allSatisfy(\.granted) }

  /// Builds the plan from a probe result.
  public static func make(
    from status: HelperCapabilityStatus,
    helperName: String = "GreenBubblesInputHelper"
  ) -> OnboardingPlan {
    let granted: [SendGrant: Bool] = [
      .accessibility: status.accessibilityGranted,
      .screenRecording: status.screenRecordingGranted,
    ]
    let steps = SendGrant.allCases.map { grant in
      OnboardingStep(
        grant: grant,
        granted: granted[grant] ?? false,
        title: "Allow \(grant.displayName) for \(helperName)",
        rationale: grant.rationale,
        instructions: grant.instructions(helperName: helperName),
        settingsURL: grant.settingsURL
      )
    }
    return OnboardingPlan(steps: steps, sendPathBlockedBy: status.blockingFailure)
  }
}
