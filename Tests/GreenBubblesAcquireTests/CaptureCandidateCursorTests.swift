import Foundation
import Testing

@testable import GreenBubblesAcquire

struct CaptureCandidateCursorTests {
  private let unrelated = [UInt8](repeating: 0x11, count: 32)
  private let account = [UInt8](repeating: 0x22, count: 32)

  private let farFuture = Date.distantFuture

  @Test func rejectedCandidateDoesNotHideTheNextOne() throws {
    var candidates = [unrelated, account]
    var seen: [[UInt8]] = []
    let decision = try CaptureCandidateCursor.decide(
      among: &candidates,
      deadline: farFuture
    ) { secret in
      let bytes = secret.withUnsafeBytes { Array($0) }
      seen.append(bytes)
      return bytes == account
    }
    #expect(decision.newlyRejected == 1)
    #expect(!decision.deadlineReached)
    let accepted = try #require(decision.secret)
    #expect(accepted.withUnsafeBytes { Array($0) } == account)
    #expect(seen == [unrelated, account])
    // Offered candidates are zeroized in place.
    #expect(candidates.allSatisfy { $0.allSatisfy { $0 == 0 } })
  }

  @Test func allRejectedLeavesNothingAccepted() throws {
    var candidates = [unrelated, unrelated]
    let decision = try CaptureCandidateCursor.decide(
      among: &candidates,
      deadline: farFuture
    ) { _ in false }
    #expect(decision.secret == nil)
    #expect(decision.newlyRejected == 2)
    #expect(!decision.deadlineReached)
  }

  @Test func expiredDeadlineSkipsVerification() throws {
    var candidates = [account]
    var offered = 0
    let decision = try CaptureCandidateCursor.decide(
      among: &candidates,
      deadline: Date(timeIntervalSinceReferenceDate: 100),
      now: { Date(timeIntervalSinceReferenceDate: 100) },
      accept: { _ in
        offered += 1
        return true
      }
    )
    #expect(offered == 0)
    #expect(decision.secret == nil)
    #expect(decision.newlyRejected == 0)
    #expect(decision.deadlineReached)
  }

  @Test func deadlineIsCheckedBeforeEachVerificationInABacklog() throws {
    // Each verification takes "10 seconds" on the injected clock; the
    // deadline falls after the second one.
    var clock = 0.0
    var candidates = [unrelated, unrelated, account]
    var offered = 0
    let decision = try CaptureCandidateCursor.decide(
      among: &candidates,
      deadline: Date(timeIntervalSinceReferenceDate: 15),
      now: { Date(timeIntervalSinceReferenceDate: clock) },
      accept: { _ in
        offered += 1
        clock += 10
        return false
      }
    )
    #expect(offered == 2)
    #expect(decision.secret == nil)
    #expect(decision.newlyRejected == 2)
    #expect(decision.deadlineReached)
  }

  @Test func verificationStopsBetweenSaltsOnceTheDeadlinePasses() {
    // Each salt takes "10 seconds" on the injected clock; the deadline is 15.
    var clock = 0.0
    var checked: [Int] = []
    let count = DatabaseSaltInventory.countVerified(
      [1, 2, 3],
      shouldStop: { clock >= 15 },
      verify: { salt in
        checked.append(salt)
        clock += 10
        return true
      }
    )
    #expect(checked == [1, 2])
    #expect(count == 2)
  }

  @Test func accumulatorCloseWipesUntakenOutputAndIgnoresLateAppends() {
    let accumulator = LLDBPassphraseCapture.OutputAccumulator()
    accumulator.append(Data(Self.hexdump(of: account, at: 0x1000).utf8))
    #expect(!accumulator.isEmpty)
    accumulator.close()
    #expect(accumulator.isEmpty)
    accumulator.append(Data(Self.hexdump(of: account, at: 0x2000).utf8))
    #expect(accumulator.isEmpty)
    #expect(accumulator.take().isEmpty)
  }

  @Test func rejectedCandidateIsNotOfferedAgain() throws {
    var stream = LLDBMemoryReadStream()
    var offered: [[UInt8]] = []
    func poll(_ text: String) throws -> CaptureCandidateCursor.Decision {
      var chunk = Array(text.utf8)
      var drained = stream.consume(&chunk)
      return try CaptureCandidateCursor.decide(
        among: &drained.candidates,
        deadline: farFuture
      ) { secret in
        let bytes = secret.withUnsafeBytes { Array($0) }
        offered.append(bytes)
        return bytes == account
      }
    }
    let first = try poll(Self.hexdump(of: unrelated, at: 0x1000) + "(lldb) continue\n")
    #expect(first.newlyRejected == 1)
    #expect(first.secret == nil)
    // A later poll with no new output offers nothing.
    let idle = try poll("Process 1 resuming\n")
    #expect(idle.newlyRejected == 0)
    #expect(idle.secret == nil)
    let second = try poll(Self.hexdump(of: account, at: 0x2000))
    #expect(second.newlyRejected == 0)
    #expect(second.secret != nil)
    #expect(offered == [unrelated, account])
  }

  static func hexdump(of bytes: [UInt8], at address: UInt64) -> String {
    stride(from: 0, to: bytes.count, by: 8).map { offset in
      let group = bytes[offset..<min(offset + 8, bytes.count)]
        .map { String(format: "0x%02x", $0) }
        .joined(separator: " ")
      return String(format: "0x%llx: ", address + UInt64(offset)) + group + "\n"
    }.joined()
  }

  @Test func noAcceptableCandidateWritesNothing() {
    #expect(
      CaptureError.noAcceptableCandidate.description.contains(
        "No output was created or replaced.")
    )
  }

  @Test func verificationFailureDiagnosticReportsInventoryCounts() {
    #expect(
      CaptureError.verificationFailureDiagnostic(distinctSaltCount: 3, databaseCount: 7)
        == "no match among 3 distinct database salts in 7 inventoried files. "
        + "No output was created or replaced."
    )
  }
}
