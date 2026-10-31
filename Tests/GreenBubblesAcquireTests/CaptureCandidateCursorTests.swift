import Testing

@testable import GreenBubblesAcquire

struct CaptureCandidateCursorTests {
  private let unrelated = [UInt8](repeating: 0x11, count: 32)
  private let account = [UInt8](repeating: 0x22, count: 32)

  @Test func rejectedCandidateDoesNotHideTheNextOne() throws {
    var considered = 0
    var seen: [[UInt8]] = []
    let decision = try CaptureCandidateCursor.decide(
      among: [unrelated, account],
      consideredCount: &considered
    ) { secret in
      let bytes = secret.withUnsafeBytes { Array($0) }
      seen.append(bytes)
      return bytes == account
    }
    #expect(considered == 2)
    #expect(decision.newlyRejected == 1)
    let accepted = try #require(decision.secret)
    #expect(accepted.withUnsafeBytes { Array($0) } == account)
    #expect(seen == [unrelated, account])
  }

  @Test func allRejectedLeavesNothingAccepted() throws {
    var considered = 0
    let decision = try CaptureCandidateCursor.decide(
      among: [unrelated, unrelated],
      consideredCount: &considered
    ) { _ in false }
    #expect(decision.secret == nil)
    #expect(decision.newlyRejected == 2)
    #expect(considered == 2)
  }

  @Test func alreadyConsideredCandidatesAreNotRevisited() throws {
    var considered = 0
    let first = try CaptureCandidateCursor.decide(
      among: [unrelated, account],
      consideredCount: &considered
    ) { _ in false }
    #expect(first.secret == nil)
    #expect(first.newlyRejected == 2)

    var seen = 0
    let second = try CaptureCandidateCursor.decide(
      among: [unrelated, account],
      consideredCount: &considered
    ) { _ in
      seen += 1
      return true
    }
    #expect(second.secret == nil)
    #expect(second.newlyRejected == 0)
    #expect(seen == 0)
    #expect(considered == 2)
  }

  @Test func noAcceptableCandidateWritesNothing() {
    #expect(
      CaptureError.noAcceptableCandidate.description.contains("nothing was written")
    )
  }
}
