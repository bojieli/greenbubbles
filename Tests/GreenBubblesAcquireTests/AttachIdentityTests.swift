import Darwin
import Foundation
import Testing

@testable import GreenBubblesAcquire

/// The attach is checked against the selected process's identity before any
/// memory read or breakpoint. Process metadata here is synthetic and lldb is
/// a shell script that logs the commands it receives; no real process is
/// attached. The real PID-reuse race can't be reproduced in a test.
struct AttachIdentityTests {
  static let selected = AttachIdentity(processID: 4242, userID: 501, startMicroseconds: 1_000)

  // MARK: - Identity comparison

  @Test func identicalIdentityMatches() {
    #expect(AttachIdentity.mismatch(expected: Self.selected, observed: Self.selected) == nil)
  }

  @Test func reusedPIDWithAnotherStartTimeOrUserDoesNotMatch() throws {
    let restarted = AttachIdentity(processID: 4242, userID: 501, startMicroseconds: 9_000)
    let otherUser = AttachIdentity(processID: 4242, userID: 0, startMicroseconds: 1_000)
    let both = AttachIdentity(processID: 4242, userID: 502, startMicroseconds: 9_000)
    #expect(
      try #require(AttachIdentity.mismatch(expected: Self.selected, observed: restarted))
        .contains("start time 9000 instead of 1000"))
    #expect(
      try #require(AttachIdentity.mismatch(expected: Self.selected, observed: otherUser))
        .contains("uid 0 instead of 501"))
    let detail = try #require(AttachIdentity.mismatch(expected: Self.selected, observed: both))
    #expect(detail.contains("uid 502") && detail.contains("start time 9000"))
  }

  @Test func unreadableIdentityNeverMatches() throws {
    let detail = try #require(AttachIdentity.mismatch(expected: Self.selected, observed: nil))
    #expect(detail.contains("could not be read"))
  }

  @Test func targetCarriesItsPIDUserAndStartTime() {
    let target = WeChatProcessTarget(
      processID: 4242, userID: 501, applicationPath: "/Applications/WeChat.app",
      bundleIdentifier: "com.tencent.xinWeChat", startMicroseconds: 1_000)
    #expect(target.attachIdentity == Self.selected)
  }

  @Test func kernelIdentityOfThisProcessIsReadable() throws {
    let identity = try #require(WeChatProcessLocator.attachIdentity(ofProcessID: getpid()))
    #expect(identity.processID == getpid())
    #expect(identity.userID == getuid())
    #expect(identity.startMicroseconds > 0)
    #expect(WeChatProcessLocator.attachIdentity(ofProcessID: 0) == nil)
    #expect(WeChatProcessLocator.attachIdentity(ofProcessID: -1) == nil)
  }

  // MARK: - No selected app

  static func described(
    executable: String? = "/Applications/WeChat.app/Contents/MacOS/WeChat",
    user: uid_t? = 501,
    start: UInt64? = 1_000
  ) -> WeChatProcessCandidate {
    WeChatProcessCandidate(
      processID: 4242, userID: user, executablePath: executable,
      applicationPath: nil, bundleIdentifier: nil, startMicroseconds: start)
  }

  @Test func unselectedPIDIsAttachedOnlyAsAStableWeChatOfThisUser() {
    #expect(
      LLDBPassphraseCapture.unselectedAttachIdentity(
        candidate: Self.described(), userID: 501, current: Self.selected) == Self.selected)
    let reused = AttachIdentity(processID: 4242, userID: 501, startMicroseconds: 9_000)
    for (candidate, current) in [
      // The PID now runs another program of the same user.
      (Self.described(executable: "/usr/bin/caffeinate"), Self.selected),
      (Self.described(executable: nil), Self.selected),
      (Self.described(user: 502), Self.selected),
      (Self.described(user: nil), Self.selected),
      (Self.described(start: nil), Self.selected),
      // Reused between the description and the identity read.
      (Self.described(), reused),
      (Self.described(), nil),
    ] as [(WeChatProcessCandidate, AttachIdentity?)] {
      #expect(
        LLDBPassphraseCapture.unselectedAttachIdentity(
          candidate: candidate, userID: 501, current: current) == nil)
    }
    #expect(
      LLDBPassphraseCapture.unselectedAttachIdentity(
        candidate: nil, userID: 501, current: Self.selected) == nil)
  }

  // MARK: - lldb stop lines

  @Test func detectsOnlyProcessStoppedStatusLines() {
    #expect(LLDBOutputParser.detectProcessStop(in: "Process 4242 stopped") == 4242)
    #expect(LLDBOutputParser.detectProcessStop(in: "  Process 7 stopped  ") == 7)
    for line in [
      "Process 4242 exited with status = 0 (0x00000000)",
      "Process 4242 detached",
      "Process 4242 stopped and restarted: thread 1 received signal: SIGCHLD",
      "Process -1 stopped",
      "Process 0 stopped",
      "Process \u{FF14}\u{FF12} stopped",
      "* thread #1, stop reason = signal SIGSTOP",
      "error: attach failed: Process 4242 stopped",
    ] {
      #expect(LLDBOutputParser.detectProcessStop(in: line) == nil, "\(line)")
    }
  }

  @Test func streamReportsStoppedProcessIDsInOrder() {
    var stream = LLDBMemoryReadStream()
    var chunk = Array("Process 4242 stopped\n* thread #1\nProcess 4242 sto".utf8)
    #expect(stream.consume(&chunk).stoppedProcessIDs == [4242])
    var rest = Array("pped\n".utf8)
    #expect(stream.consume(&rest).stoppedProcessIDs == [4242])
  }

  // MARK: - Fake lldb session

  @Test func matchingIdentityIsVerifiedBeforeTheBreakpointIsSet() throws {
    let session = try FakeLLDB()
    let reader = RecordingReader(session: session, result: Self.selected)
    var capture = LLDBPassphraseCapture(lldbPath: session.executable.path)
    capture.processIdentityReader = { reader.read($0) }
    let secret = try session.run(capture, identity: Self.selected)
    #expect(secret.withUnsafeBytes { Array($0) } == (0..<32).map(UInt8.init))

    // The identity was read once, for the selected PID, while lldb had only
    // attached.
    let calls = reader.calls
    #expect(calls.count == 1)
    let call = try #require(calls.first)
    #expect(call.processID == 4242)
    #expect(call.logSoFar.contains("process attach -p 4242\n"))
    for command in ["breakpoint", "memory read", "process continue"] {
      #expect(!call.logSoFar.contains(command))
    }
    // Then the capture commands followed, in order.
    let log = try session.log()
    let attach = try #require(log.range(of: "process attach -p 4242"))
    let breakpoint = try #require(log.range(of: "breakpoint set -n CCKeyDerivationPBKDF"))
    let resume = try #require(log.range(of: "process continue"))
    #expect(attach.upperBound <= breakpoint.lowerBound)
    #expect(breakpoint.upperBound <= resume.lowerBound)
    #expect(!log.contains("process detach"))
  }

  @Test func mismatchedOrUnreadableIdentityDetachesWithoutReadingOrBreaking() throws {
    let reused = AttachIdentity(processID: 4242, userID: 501, startMicroseconds: 9_000)
    let foreign = AttachIdentity(processID: 4242, userID: 0, startMicroseconds: 1_000)
    for observed in [reused, foreign, nil] as [AttachIdentity?] {
      let session = try FakeLLDB()
      let reader = RecordingReader(session: session, result: observed)
      var capture = LLDBPassphraseCapture(lldbPath: session.executable.path)
      capture.processIdentityReader = { reader.read($0) }
      // Unreadable but still present: never treated as an exit.
      capture.processIsGone = { _ in false }
      var offered = 0
      do {
        _ = try session.run(capture, identity: Self.selected) { _ in
          offered += 1
          return true
        }
        Issue.record("capture succeeded with observed identity \(String(describing: observed))")
      } catch CaptureError.attachedProcessMismatch(let processID, let detail) {
        #expect(processID == 4242)
        #expect(
          detail == AttachIdentity.mismatch(expected: Self.selected, observed: observed))
      }
      #expect(offered == 0)
      #expect(reader.calls.count == 1)
      let log = try session.log()
      #expect(log.contains("process attach -p 4242\n"))
      #expect(log.contains("process detach\nquit\n"))
      for command in ["breakpoint", "memory read", "process continue"] {
        #expect(!log.contains(command), "\(command) was sent")
      }
    }
  }

  @Test func processGoneAtTheIdentityCheckIsAnExitNotAMismatch() throws {
    let session = try FakeLLDB()
    let reader = RecordingReader(session: session, result: nil)
    var capture = LLDBPassphraseCapture(lldbPath: session.executable.path)
    capture.processIdentityReader = { reader.read($0) }
    capture.processIsGone = { $0 == 4242 }
    var exitObservedAt: UInt64?
    #expect(throws: CaptureError.targetExited) {
      _ = try session.run(capture, identity: Self.selected, exitObservedAt: &exitObservedAt)
    }
    let log = try session.log()
    #expect(log.contains("process detach\nquit\n"))
    for command in ["breakpoint", "memory read", "process continue"] {
      #expect(!log.contains(command), "\(command) was sent")
    }
  }

  @Test func vanishedProcessExitIsRecordedBeforeTheDetachWait() throws {
    // This lldb ignores `quit`, so the detach waits its full grace period.
    let session = try FakeLLDB(ignoresQuit: true)
    var capture = LLDBPassphraseCapture(lldbPath: session.executable.path)
    capture.processIdentityReader = { _ in nil }
    capture.processIsGone = { $0 == 4242 }
    var exitObservedAt: UInt64?
    #expect(throws: CaptureError.targetExited) {
      _ = try session.run(capture, identity: Self.selected, exitObservedAt: &exitObservedAt)
    }
    let finished = WeChatProcessTarget.microseconds(Date())
    let recorded = try #require(exitObservedAt)
    // Recorded when the exit was confirmed, so the whole detach wait (3 s
    // grace period) came after it. Independent of how long lldb took to start.
    #expect(finished >= recorded + 2_500_000)
  }

  @Test func exitBeforeTheStopIsReportedWithoutReadingTheIdentity() throws {
    let session = try FakeLLDB(attachOutput: "Process 4242 exited with status = 0 (0x00000000)")
    let reader = RecordingReader(session: session, result: Self.selected)
    var capture = LLDBPassphraseCapture(lldbPath: session.executable.path)
    capture.processIdentityReader = { reader.read($0) }
    #expect(throws: CaptureError.targetExited) {
      _ = try session.run(capture, identity: Self.selected)
    }
    #expect(reader.calls.isEmpty)
    #expect(!(try session.log()).contains("breakpoint"))
  }

  @Test func mismatchErrorSaysNothingWasReadAndToRerunPreflight() {
    let message = CaptureError.attachedProcessMismatch(
      processID: 4242, detail: "uid 0 instead of 501"
    )
    .description
    #expect(message.contains("process 4242"))
    #expect(message.contains("before setting the capture breakpoint or reading a passphrase"))
    #expect(message.contains("Rerun preflight"))
  }

  // MARK: - Replacement started before the exit was recorded

  static let app = "/Applications/WeChat.app"
  static let bundle = "com.tencent.xinWeChat"

  static func candidate(_ processID: pid_t, start: UInt64, user: uid_t = 501)
    -> WeChatProcessCandidate
  {
    WeChatProcessCandidate(
      processID: processID, userID: user, executablePath: app + "/Contents/MacOS/WeChat",
      applicationPath: app, bundleIdentifier: bundle, startMicroseconds: start)
  }

  static func followed(exitRecordedAt exitRecorded: UInt64?, excluded: Set<ProcessIdentity> = [])
    -> WeChatProcessTarget
  {
    WeChatProcessTarget(
      processID: 4242, userID: 501, applicationPath: app, bundleIdentifier: bundle,
      startMicroseconds: 1_000, excludedProcesses: excluded,
      exitObservedMicroseconds: exitRecorded)
  }

  @Test func replacementThatStartedBeforeTheRecordedExitIsReportedNotAttached() throws {
    // WeChat restarted at 4_800, but lldb's exit report was read at 5_000.
    var selected = Self.followed(exitRecordedAt: 5_000)
    let early = Self.candidate(90, start: 4_800)
    #expect(
      try WeChatProcessSelector.reselect(
        candidates: [early], following: &selected, nowMicroseconds: 6_000) == nil)
    #expect(
      WeChatProcessSelector.replacementsStartedBeforeExitRecorded(
        candidates: [early], following: selected) == [90])
    // Started exactly at the recorded moment: still not after it.
    #expect(
      WeChatProcessSelector.replacementsStartedBeforeExitRecorded(
        candidates: [Self.candidate(91, start: 5_000)], following: selected) == [91])
  }

  @Test func onlyUnexplainedSameAppProcessesAreAmbiguous() {
    let sibling = ProcessIdentity(processID: 60, startMicroseconds: 500)
    let selected = Self.followed(exitRecordedAt: 5_000, excluded: [sibling])
    let candidates = [
      Self.candidate(60, start: 500),  // recorded at selection
      Self.candidate(4242, start: 1_000),  // the selected process itself
      Self.candidate(70, start: 6_000),  // started after the exit: a real replacement
      Self.candidate(80, start: 4_000, user: 502),  // another user
      WeChatProcessCandidate(
        processID: 81, userID: 501, executablePath: "/Applications/Other.app/Contents/MacOS/WeChat",
        applicationPath: "/Applications/Other.app", bundleIdentifier: Self.bundle,
        startMicroseconds: 4_000),  // another app copy
    ]
    #expect(
      WeChatProcessSelector.replacementsStartedBeforeExitRecorded(
        candidates: candidates, following: selected
      ).isEmpty)
  }

  @Test func inconclusiveListingIsNotReportedAsTimingAmbiguity() {
    let unreadable = WeChatProcessCandidate(
      processID: 99, userID: nil, executablePath: nil, applicationPath: nil,
      bundleIdentifier: nil)
    #expect(
      WeChatProcessSelector.replacementsStartedBeforeExitRecorded(
        candidates: [
          Self.candidate(90, start: 4_800), Self.candidate(70, start: 6_000), unreadable,
        ],
        following: Self.followed(exitRecordedAt: 5_000)
      ).isEmpty)
  }

  @Test func processOlderThanTheSelectedOneIsNotItsRestart() {
    // A sibling from before the current target started (for example from
    // an earlier restart) can't be this target's restart.
    #expect(
      WeChatProcessSelector.replacementsStartedBeforeExitRecorded(
        candidates: [Self.candidate(60, start: 900), Self.candidate(61, start: 1_000)],
        following: Self.followed(exitRecordedAt: 5_000)
      ).isEmpty)
  }

  @Test func nothingIsAmbiguousBeforeAnExitIsRecorded() {
    #expect(
      WeChatProcessSelector.replacementsStartedBeforeExitRecorded(
        candidates: [Self.candidate(90, start: 4_800)],
        following: Self.followed(exitRecordedAt: nil)
      ).isEmpty)
  }

  @Test func ambiguityErrorExplainsTheTimingAndAsksForPreflight() {
    let one = CaptureError.replacementTimingAmbiguous(
      exitedProcessID: 4242, candidateProcessIDs: [90]
    ).description
    #expect(one.contains("WeChat process 4242 exited"))
    #expect(one.contains("process 90 had already started when that exit was recorded"))
    #expect(one.contains("Rerun preflight"))
    let two = CaptureError.replacementTimingAmbiguous(
      exitedProcessID: 4242, candidateProcessIDs: [90, 91]
    ).description
    #expect(two.contains("processes 90, 91"))
  }
}

/// Records each identity read with a snapshot of what lldb had received at
/// that moment.
private final class RecordingReader: @unchecked Sendable {
  struct Call {
    var processID: pid_t
    var logSoFar: String
  }

  private let lock = NSLock()
  private var recorded: [Call] = []
  private let session: FakeLLDB
  private let result: AttachIdentity?

  init(session: FakeLLDB, result: AttachIdentity?) {
    self.session = session
    self.result = result
  }

  var calls: [Call] {
    lock.lock()
    defer { lock.unlock() }
    return recorded
  }

  func read(_ processID: pid_t) -> AttachIdentity? {
    let snapshot = (try? session.log()) ?? ""
    lock.lock()
    recorded.append(Call(processID: processID, logSoFar: snapshot))
    lock.unlock()
    return result
  }
}

/// A stand-in for lldb: logs every command line, answers `process attach`
/// with `attachOutput`, and answers `process continue` with one 32-byte
/// `memory read` dump (bytes 0 to 31).
private final class FakeLLDB: @unchecked Sendable {
  let directory: URL
  let executable: URL
  let logFile: URL

  init(attachOutput: String = "Process 4242 stopped", ignoresQuit: Bool = false) throws {
    directory = FileManager.default.temporaryDirectory
      .appending(path: "greenbubbles-fake-lldb-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    executable = directory.appending(path: "lldb")
    logFile = directory.appending(path: "commands.log")
    FileManager.default.createFile(atPath: logFile.path, contents: nil)
    let dump = (0..<4).map { line in
      let address = String(0x1000 + line * 8, radix: 16)
      let bytes = (0..<8).map { String(format: "0x%02x", line * 8 + $0) }
      return "echo '0x\(address): \(bytes.joined(separator: " "))'"
    }.joined(separator: "\n      ")
    let script = """
      #!/bin/bash
      log='\(logFile.path)'
      while IFS= read -r line; do
        printf '%s\\n' "$line" >> "$log"
        case "$line" in
          "process attach -p "*)
            echo '\(attachOutput)'
            echo '* thread #1, stop reason = signal SIGSTOP' ;;
          "process status")
            echo 'Process 4242 stopped' ;;
          "process continue")
            \(dump) ;;
          "process detach")
            echo 'Process 4242 detached' ;;
          "quit")
            \(ignoresQuit ? ":" : "exit 0") ;;
        esac
      done

      """
    try Data(script.utf8).write(to: executable)
    try FileManager.default.setAttributes(
      [.posixPermissions: 0o700], ofItemAtPath: executable.path)
  }

  deinit {
    try? FileManager.default.removeItem(at: directory)
  }

  func log() throws -> String {
    try String(contentsOf: logFile, encoding: .utf8)
  }

  func run(
    _ capture: LLDBPassphraseCapture,
    identity: AttachIdentity,
    accept: (PassphraseSecret) throws -> Bool = { _ in true }
  ) throws -> PassphraseSecret {
    var exitObservedAt: UInt64?
    return try run(capture, identity: identity, exitObservedAt: &exitObservedAt, accept: accept)
  }

  func run(
    _ capture: LLDBPassphraseCapture,
    identity: AttachIdentity,
    exitObservedAt: inout UInt64?,
    accept: (PassphraseSecret) throws -> Bool = { _ in true }
  ) throws -> PassphraseSecret {
    var rejected = 0
    return try capture.runSingleCapture(
      identity: identity,
      architecture: .arm64,
      deadline: Date().addingTimeInterval(20),
      accept: accept,
      rejectedCandidateCount: &rejected,
      exitObservedAt: &exitObservedAt
    )
  }
}
