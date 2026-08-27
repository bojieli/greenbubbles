import ApplicationServices
import Foundation

public struct NotificationHintAssessment: Codable, Equatable, Sendable {
  public let publicCrossApplicationContentAPIAvailable: Bool
  public let accessibilityTrusted: Bool
  public let canProvideCompleteness: Bool
  public let recommendedRole: String

  public init(
    publicCrossApplicationContentAPIAvailable: Bool,
    accessibilityTrusted: Bool,
    canProvideCompleteness: Bool,
    recommendedRole: String
  ) {
    self.publicCrossApplicationContentAPIAvailable = publicCrossApplicationContentAPIAvailable
    self.accessibilityTrusted = accessibilityTrusted
    self.canProvideCompleteness = canProvideCompleteness
    self.recommendedRole = recommendedRole
  }
}

public struct NotificationHintAssessor: Sendable {
  public init() {}

  /// Performs no prompt and reads no notification contents.
  public func assess() -> NotificationHintAssessment {
    NotificationHintAssessment(
      publicCrossApplicationContentAPIAvailable: false,
      accessibilityTrusted: AXIsProcessTrusted(),
      canProvideCompleteness: false,
      recommendedRole:
        "Optional user-enabled latency hint only; authoritative state must come from snapshot reconciliation."
    )
  }
}
