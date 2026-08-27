import Testing

@testable import GreenBubblesCore

@Suite("NotificationHintAssessmentTests")
struct NotificationHintAssessmentTests {
  @Test
  func neverClaimsCrossApplicationNotificationCompleteness() {
    let result = NotificationHintAssessor().assess()
    #expect(!result.publicCrossApplicationContentAPIAvailable)
    #expect(!result.canProvideCompleteness)
    #expect(result.recommendedRole.contains("snapshot reconciliation"))
  }
}
