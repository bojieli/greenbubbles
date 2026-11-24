// Derived from wcdb-key-tool (https://github.com/TANGandXUE/wcdb-key-tool), MIT license.

import Darwin
import Foundation

public enum CaptureError: Error, Equatable, CustomStringConvertible {
  case lldbUnavailable
  case lldbLaunchFailed
  case attachFailed(String)
  case noWeChatProcess
  case targetExited
  case timedOut
  case noAcceptableCandidate
  /// The timeout passed with captured candidates still waiting to be offered
  /// to `accept`; they were neither accepted nor rejected.
  case timedOutWithUncheckedCandidates(uncheckedCandidateCount: Int, rejectedCandidateCount: Int)
  case exitedWithoutCapture
  /// lldb stopped the PID, but its user or start time no longer matched the
  /// selected process. lldb was told to detach before the capture breakpoint
  /// was set or the passphrase register read.
  case attachedProcessMismatch(processID: pid_t, detail: String)
  /// The selected process exited, and every process of the same app that
  /// could be its restart had already started when the exit was recorded,
  /// so none can be confirmed as the replacement.
  case replacementTimingAmbiguous(exitedProcessID: pid_t, candidateProcessIDs: [pid_t])
  /// lldb exited on its own before the deadline after `accept` rejected
  /// earlier candidates. The exit is the cause; the rejections are context.
  case exitedAfterRejecting(rejectedCandidateCount: Int)

  public var description: String {
    switch self {
    case .lldbUnavailable:
      return "lldb is not available; install the Xcode Command Line Tools"
    case .lldbLaunchFailed:
      return "lldb could not be launched"
    case .attachFailed(let detail):
      return "lldb could not attach to WeChat: \(detail)"
    case .noWeChatProcess:
      return "WeChat is not running; start it and log in before capturing"
    case .targetExited:
      return "the WeChat process exited before a passphrase was captured"
    case .timedOut:
      return
        "No passphrase was captured before the timeout; log out of WeChat and log back in while the capture is armed"
    case .noAcceptableCandidate:
      return
        "every captured candidate was rejected before the timeout. No output was created or replaced."
    case .timedOutWithUncheckedCandidates(let uncheckedCandidateCount, _):
      return
        "the timeout passed before \(Self.capturedCandidates(uncheckedCandidateCount)) could be "
        + "checked. No output was created or replaced."
    case .exitedWithoutCapture:
      return "lldb exited before a passphrase was captured"
    case .attachedProcessMismatch(let processID, let detail):
      return
        "process \(processID) is no longer the selected WeChat process (\(detail)); "
        + "lldb detached before setting the capture breakpoint or reading a passphrase. "
        + "No output was created or replaced. Rerun preflight, then capture again"
    case .replacementTimingAmbiguous(let exitedProcessID, let candidates):
      let list = candidates.map(String.init).joined(separator: ", ")
      let noun = candidates.count == 1 ? "process" : "processes"
      return
        "WeChat process \(exitedProcessID) exited, but the same app's \(noun) \(list) "
        + "had already started when that exit was recorded, so the capture can't confirm "
        + "which one is the restarted WeChat and attached to none. "
        + "No output was created or replaced. Rerun preflight to select the running "
        + "process, then capture again"
    case .exitedAfterRejecting(let rejectedCandidateCount):
      return
        "lldb exited before a passphrase was captured (earlier, "
        + "\(Self.capturedCandidates(rejectedCandidateCount)) "
        + "\(rejectedCandidateCount == 1 ? "was" : "were") rejected). "
        + "No output was created or replaced."
    }
  }

  private static func capturedCandidates(_ count: Int) -> String {
    count == 1 ? "1 captured candidate" : "\(count) captured candidates"
  }

  /// Diagnostic for a capture whose candidates all failed verification.
  ///
  /// `capture` reports `noAcceptableCandidate` with the counts of distinct
  /// salts and database files that were inventoried. The counts describe what
  /// was examined; they do not identify the cause.
  public static func verificationFailureDiagnostic(
    distinctSaltCount: Int,
    databaseCount: Int
  ) -> String {
    "no match among \(distinctSaltCount) distinct database salts "
      + "in \(databaseCount) inventoried files. No output was created or replaced."
  }
}

/// Attaches lldb to a running WeChat process and captures the 32-byte
/// SQLCipher passphrase passed to the system `CCKeyDerivationPBKDF` function
/// when the owner logs out and back in.
///
/// The lldb script is written to a stdin pipe that stays open for the whole
/// capture window (lldb must not see EOF while it waits for the breakpoint).
/// stdout and stderr are parsed incrementally; consumed output is zeroized
/// and dropped, so each read is offered once. The loop stops at the first
/// candidate `accept` keeps, or at the timeout. A rejected candidate does not
/// end the session: the breakpoint stays armed for a later call in the same
/// window. Only the child process spawned here is ever terminated — never
/// `pkill`.
///
/// Diagnostics the original Python tool lacks: an attach failure fails fast
/// instead of stalling until the timeout, and if WeChat's logout flow restarts
/// its main process, the capture re-attaches to the new process within the
/// same overall timeout.
public struct LLDBPassphraseCapture: Sendable {
  public static let defaultTimeoutSeconds = 300

  private let lldbPath: String

  public init(lldbPath: String = "/usr/bin/lldb") {
    self.lldbPath = lldbPath
  }

  public var lldbAvailable: Bool {
    FileManager.default.isExecutableFile(atPath: lldbPath)
  }

  /// Called with human-readable capture events (attach, re-attach, failure
  /// hints) and, when `verbose` is set, lldb's own output as it arrives.
  public var outputHandler: (@Sendable (String) -> Void)?

  /// Mirrors lldb's raw output through `outputHandler` (lines may be split
  /// arbitrarily; intended for diagnosing a stalled capture).
  public var verbose = false

  /// The WeChat app copy to capture from. When set, every attach and
  /// re-attach uses the process now running this app bundle (same path and
  /// bundle ID, same user), so a restart during logout never switches to
  /// another copy. When nil, the single running WeChat process is used.
  public var target: WeChatProcessTarget?

  /// Lists processes while following `target`; by default, that target's
  /// user's processes with metadata read by the current process.
  public var processLocator: WeChatProcessLocator?

  /// Reads a PID's current user and start time. The capture calls it after
  /// lldb has stopped the process, before any memory read or breakpoint, and
  /// detaches unless the result matches the selected process. Replaceable
  /// for tests.
  public var processIdentityReader: @Sendable (pid_t) -> AttachIdentity? = {
    WeChatProcessLocator.attachIdentity(ofProcessID: $0)
  }

  /// Whether the kernel confirms a PID no longer exists. Replaceable for
  /// tests.
  var processIsGone: @Sendable (pid_t) -> Bool = { WeChatProcessLocator.processIsGone($0) }

  private func emit(_ message: String) {
    outputHandler?(message)
  }

  /// Why the last poll found a WeChat process but attached to none.
  enum AttachRefusal: Equatable {
    /// Every possible restart of `exited` had already started when its exit
    /// was recorded.
    case timingAmbiguous(exited: pid_t, candidates: [pid_t])
    /// Without a selected app, the listed process could not be verified.
    case unverifiable(ProcessSelectionError)
  }

  /// The process to attach to next, with the identity the attach must
  /// still find. `followed` starts as `target`; it keeps the selected
  /// process while it runs and moves only to a process of the same app that
  /// started after the previous one was seen to exit. When a conclusive poll
  /// attaches to nothing although a process was found, `refusal` says why;
  /// it is cleared by a poll that finds nothing to explain.
  private func currentAttachIdentity(
    following followed: inout WeChatProcessTarget?,
    refusal: inout AttachRefusal?
  ) throws -> AttachIdentity? {
    guard var current = followed else {
      // No selected app: the single running WeChat of this user, identified
      // now so the attach can be checked against it. Only kernel metadata
      // is read here, never the app's files.
      let locator = processLocator ?? WeChatProcessLocator()
      refusal = nil
      guard let processID = try locator.uniqueProcessID() else { return nil }
      let before = processIdentityReader(processID)
      let executablePath = WeChatProcessLocator.executablePath(ofProcessID: processID)
      let identity = Self.unselectedAttachIdentity(
        candidate: before.map {
          WeChatProcessCandidate(
            processID: processID, userID: $0.userID, executablePath: executablePath,
            applicationPath: nil, bundleIdentifier: nil,
            startMicroseconds: $0.startMicroseconds)
        },
        userID: locator.userID,
        current: processIdentityReader(processID)
      )
      if identity == nil {
        let notWeChat =
          executablePath.map {
            ($0 as NSString).lastPathComponent != WeChatProcessLocator.executableName
          } ?? false
        refusal = .unverifiable(
          notWeChat
            ? .processNotWeChat(processID) : .processIdentityUnavailable(processID))
      }
      return identity
    }
    let locator = processLocator ?? WeChatProcessLocator(userID: current.userID)
    let listing = Result { try locator.candidates() }
    let next = try WeChatProcessSelector.reselect(
      listing: listing,
      following: &current,
      nowMicroseconds: WeChatProcessTarget.microseconds(Date())
    )
    followed = current
    if let next {
      refusal = nil
      return next.attachIdentity
    }
    if case .success(let candidates) = listing {
      let ambiguous = WeChatProcessSelector.replacementsStartedBeforeExitRecorded(
        candidates: candidates, following: current)
      refusal =
        ambiguous.isEmpty
        ? nil : .timingAmbiguous(exited: current.processID, candidates: ambiguous)
    }
    return nil
  }

  /// The identity to attach to when no app was selected: the listed PID
  /// must, when described, run an executable named `WeChat` for `userID`
  /// with a known start time, and its identity read afterwards (`current`)
  /// must be the same process. Otherwise nil, so nothing is attached on
  /// this poll (the PID may have been reused after the listing).
  static func unselectedAttachIdentity(
    candidate: WeChatProcessCandidate?,
    userID: uid_t,
    current: AttachIdentity?
  ) -> AttachIdentity? {
    guard let candidate, candidate.userID == userID,
      let start = candidate.startMicroseconds,
      let executablePath = candidate.executablePath,
      (executablePath as NSString).lastPathComponent == WeChatProcessLocator.executableName
    else { return nil }
    let described = AttachIdentity(
      processID: candidate.processID, userID: userID, startMicroseconds: start)
    return current == described ? described : nil
  }

  /// Resolves the WeChat process itself and re-attaches if the target exits
  /// mid-capture (WeChat can restart its main process during logout/login).
  /// Returns the first SQLCipher-shaped read.
  public func capture(
    architecture: LLDBTargetArchitecture = .current,
    timeoutSeconds: Int = LLDBPassphraseCapture.defaultTimeoutSeconds
  ) throws -> PassphraseSecret {
    try capture(architecture: architecture, timeoutSeconds: timeoutSeconds) { _ in true }
  }

  /// Same as the capture above, but `accept` decides whether a read is the
  /// account passphrase. Return false to keep waiting for a later call.
  /// Nothing is written here.
  public func capture(
    architecture: LLDBTargetArchitecture = .current,
    timeoutSeconds: Int = LLDBPassphraseCapture.defaultTimeoutSeconds,
    accept: (PassphraseSecret) throws -> Bool
  ) throws -> PassphraseSecret {
    guard lldbAvailable else { throw CaptureError.lldbUnavailable }
    let deadline = Date().addingTimeInterval(TimeInterval(timeoutSeconds))
    var followed = target
    var rejectedCandidateCount = 0
    var exitObservedAt: UInt64?
    var refusal: AttachRefusal?
    var reportedRefusal: AttachRefusal?
    while true {
      guard let identity = try currentAttachIdentity(following: &followed, refusal: &refusal)
      else {
        if case .timingAmbiguous(let exited, let ambiguous) = refusal, refusal != reportedRefusal {
          // Still fail closed: never attach to such a process. Say so now
          // rather than only at the timeout.
          let list = ambiguous.map(String.init).joined(separator: ", ")
          emit(
            "WeChat process \(exited) exited, but the same app's process \(list) had already "
              + "started when that exit was recorded, so it can't be confirmed as the restarted "
              + "WeChat and will not be attached. Still waiting for a process that starts "
              + "later; if none does, rerun preflight and capture again")
          reportedRefusal = refusal
        }
        if Date() < deadline {
          Thread.sleep(forTimeInterval: 1)
          continue
        }
        // The restart ambiguity comes first: a candidate rejected before the
        // restart says nothing about the process that was never attached.
        // So does a process that was found but could not be verified.
        if case .timingAmbiguous(let exited, let ambiguous) = refusal {
          throw CaptureError.replacementTimingAmbiguous(
            exitedProcessID: exited, candidateProcessIDs: ambiguous)
        }
        if case .unverifiable(let error) = refusal { throw error }
        if rejectedCandidateCount > 0 { throw CaptureError.noAcceptableCandidate }
        throw CaptureError.noWeChatProcess
      }
      let processID = identity.processID
      emit("attaching to WeChat process \(processID)")
      do {
        return try runSingleCapture(
          identity: identity,
          architecture: architecture,
          deadline: deadline,
          accept: accept,
          rejectedCandidateCount: &rejectedCandidateCount,
          exitObservedAt: &exitObservedAt
        )
      } catch CaptureError.targetExited {
        // The time lldb's exit report was read, not the time cleanup ended:
        // a replacement may start while lldb is being shut down.
        if followed?.processID == processID, followed?.exitObservedMicroseconds == nil {
          followed?.exitObservedMicroseconds =
            exitObservedAt ?? WeChatProcessTarget.microseconds(Date())
        }
        exitObservedAt = nil
        guard Date() < deadline else { throw deadlineError(rejectedCandidateCount) }
        emit("WeChat process \(processID) exited; waiting to re-attach to the new process")
        Thread.sleep(forTimeInterval: 1)
      }
    }
  }

  private func deadlineError(_ rejectedCandidateCount: Int) -> CaptureError {
    rejectedCandidateCount > 0 ? .noAcceptableCandidate : .timedOut
  }

  /// One lldb session against one process. lldb first only attaches; the
  /// breakpoint and its memory read are sent after the stopped process is
  /// confirmed to be `identity`. Internal (not private) so tests can drive
  /// it with a stand-in lldb.
  func runSingleCapture(
    identity: AttachIdentity,
    architecture: LLDBTargetArchitecture,
    deadline: Date,
    accept: (PassphraseSecret) throws -> Bool,
    rejectedCandidateCount: inout Int,
    exitObservedAt: inout UInt64?
  ) throws -> PassphraseSecret {
    let processID = identity.processID

    let process = Process()
    let input = Pipe()
    let output = Pipe()
    var environment = ProcessInfo.processInfo.environment
    environment["TERM"] = "dumb"
    process.environment = environment
    process.executableURL = URL(fileURLWithPath: lldbPath)
    process.standardInput = input
    process.standardOutput = output
    process.standardError = output

    let accumulator = OutputAccumulator()
    // When lldb exited, recorded as it happens rather than at the next poll
    // (a poll can be delayed by a long verification).
    let exitClock = ExitClock()
    process.terminationHandler = { _ in exitClock.record() }
    let verboseRelay = verbose ? VerboseLineRelay(emit: { [self] in emit($0) }) : nil
    output.fileHandleForReading.readabilityHandler = { handle in
      var data = handle.availableData
      if !data.isEmpty {
        accumulator.append(data)
        verboseRelay?.append(data)
        data.resetBytes(in: 0..<data.count)
      }
    }

    do {
      try process.run()
    } catch {
      output.fileHandleForReading.readabilityHandler = nil
      throw CaptureError.lldbLaunchFailed
    }
    defer {
      output.fileHandleForReading.readabilityHandler = nil
      // lldb keeps running while `accept` verifies, so output can arrive
      // after the last poll. Wipe it; later appends are dropped.
      accumulator.close()
      verboseRelay?.flush()
      // Only ever terminate the child spawned here. The breakpoint command
      // continues instead of quitting, so lldb is still attached when a
      // candidate is accepted; terminating it detaches WeChat.
      if process.isRunning {
        process.terminate()
        let graceDeadline = Date().addingTimeInterval(3)
        while process.isRunning, Date() < graceDeadline {
          Thread.sleep(forTimeInterval: 0.05)
        }
        if process.isRunning {
          kill(process.processIdentifier, SIGKILL)
        }
      }
      try? input.fileHandleForWriting.close()
    }

    // A write after lldb has exited must fail with EPIPE, not raise SIGPIPE.
    _ = fcntl(input.fileHandleForWriting.fileDescriptor, F_SETNOSIGPIPE, 1)
    // Keep stdin open after writing: lldb must not read EOF while it waits
    // for the identity check or for the breakpoint to hit. A failed write
    // means lldb has exited, which the loop below reports.
    try? input.fileHandleForWriting.write(
      contentsOf: Data(LLDBCaptureScript.attach(processID: processID).utf8))

    var stream = LLDBMemoryReadStream()
    defer { stream.discard() }
    var identityVerified = false
    while true {
      let running = process.isRunning
      // lldb had exited by this moment if `running` is false.
      let polledAt = Date()
      if !running {
        // Give the readability handler a moment to flush the final output.
        Thread.sleep(forTimeInterval: 0.1)
      }
      var chunk = accumulator.take()
      var drained = stream.consume(&chunk)
      if !running {
        let last = stream.finish()
        drained.candidates += last.candidates
        drained.attachFailure = drained.attachFailure ?? last.attachFailure
        drained.targetExited = drained.targetExited || last.targetExited
      }
      defer { Self.zeroizeCandidates(&drained.candidates) }
      // Time the exit as soon as it is known, before verifying any candidate
      // from the same batch (verification can take a while).
      if drained.targetExited, exitObservedAt == nil {
        exitObservedAt = WeChatProcessTarget.microseconds(Date())
      }
      if !identityVerified {
        // Nothing in this phase reads memory, so there are no real
        // candidates; anything shaped like one is dropped unoffered.
        if let detail = drained.attachFailure {
          emit("lldb reported: \(detail)")
          throw CaptureError.attachFailed(detail)
        }
        if drained.targetExited {
          throw CaptureError.targetExited
        }
        if drained.stoppedProcessIDs.contains(processID) {
          let observed = processIdentityReader(processID)
          // Gone, not replaced: lldb's exit report may not have arrived yet.
          // Handle it as an exit so the capture can follow a restart.
          if observed == nil, processIsGone(processID) {
            // Record the exit before detaching, which can wait up to 3 s: a
            // replacement started during that wait must count as later.
            if exitObservedAt == nil {
              exitObservedAt = WeChatProcessTarget.microseconds(Date())
            }
            Self.detach(process, input: input)
            throw CaptureError.targetExited
          }
          if let detail = AttachIdentity.mismatch(expected: identity, observed: observed) {
            emit("process \(processID) does not match the selected WeChat process; detaching")
            Self.detach(process, input: input)
            throw CaptureError.attachedProcessMismatch(processID: processID, detail: detail)
          }
          // Don't arm a capture whose window has already closed.
          guard Date() < deadline else {
            Self.detach(process, input: input)
            throw deadlineError(rejectedCandidateCount)
          }
          identityVerified = true
          try? input.fileHandleForWriting.write(
            contentsOf: Data(LLDBCaptureScript.capture(architecture: architecture).utf8))
          emit("attached to the selected WeChat process \(processID); waiting for the login")
        }
        if !running, polledAt < deadline { throw Self.exitError(rejectedCandidateCount) }
        guard Date() < deadline else { throw deadlineError(rejectedCandidateCount) }
        Thread.sleep(forTimeInterval: 0.05)
        continue
      }
      let decision = try CaptureCandidateCursor.decide(
        among: &drained.candidates,
        deadline: deadline,
        accept: accept
      )
      rejectedCandidateCount += decision.newlyRejected
      if let secret = decision.secret {
        return secret
      }
      if decision.deadlineReached {
        throw deadlineErrorAfterDraining(
          rejectedCandidateCount,
          uncheckedCandidateCount: decision.uncheckedCount,
          stream: &stream,
          accumulator: accumulator
        )
      }
      if decision.newlyRejected > 0 {
        emit("captured candidate was rejected; waiting for another derivation")
      }
      if let detail = drained.attachFailure {
        emit("lldb reported: \(detail)")
        throw CaptureError.attachFailed(detail)
      }
      if drained.targetExited {
        throw CaptureError.targetExited
      }
      if !running, polledAt < deadline {
        throw Self.exitError(rejectedCandidateCount)
      }
      guard Date() < deadline else {
        // lldb may have exited while the last candidate was being checked.
        if let exitedAt = exitClock.time, exitedAt < deadline {
          throw Self.exitError(rejectedCandidateCount)
        }
        throw deadlineErrorAfterDraining(
          rejectedCandidateCount,
          uncheckedCandidateCount: 0,
          stream: &stream,
          accumulator: accumulator
        )
      }
      Thread.sleep(forTimeInterval: 0.05)
    }
  }

  /// Asks lldb to detach and quit, waiting briefly. The caller's cleanup
  /// terminates lldb if it is still running, which also detaches.
  private static func detach(_ process: Process, input: Pipe) {
    try? input.fileHandleForWriting.write(contentsOf: Data(LLDBCaptureScript.detach.utf8))
    let graceDeadline = Date().addingTimeInterval(3)
    while process.isRunning, Date() < graceDeadline {
      Thread.sleep(forTimeInterval: 0.05)
    }
  }

  /// An exit before the deadline is the cause even after a rejection; the
  /// rejections are context, not the reason the capture stopped. An exit
  /// first seen after the deadline may have followed it, so the timeout is
  /// reported for that instead.
  private static func exitError(_ rejectedCandidateCount: Int) -> CaptureError {
    rejectedCandidateCount > 0
      ? .exitedAfterRejecting(rejectedCandidateCount: rejectedCandidateCount)
      : .exitedWithoutCapture
  }

  /// The error for a deadline reached inside a single capture. Complete reads
  /// that arrived while the last candidate was being checked were never
  /// offered, so they count as unchecked rather than letting the rejections
  /// read as a definitive "no acceptable candidate".
  private func deadlineErrorAfterDraining(
    _ rejectedCandidateCount: Int,
    uncheckedCandidateCount: Int,
    stream: inout LLDBMemoryReadStream,
    accumulator: OutputAccumulator
  ) -> CaptureError {
    if rejectedCandidateCount > 0 {
      // Before a definitive "no acceptable candidate", let output the reader
      // already has in hand reach the accumulator.
      Thread.sleep(forTimeInterval: 0.1)
    }
    var rest = accumulator.take()
    var late = stream.consume(&rest)
    defer { Self.zeroizeCandidates(&late.candidates) }
    let unchecked = uncheckedCandidateCount + late.candidates.count
    guard unchecked == 0 else {
      return .timedOutWithUncheckedCandidates(
        uncheckedCandidateCount: unchecked,
        rejectedCandidateCount: rejectedCandidateCount
      )
    }
    return deadlineError(rejectedCandidateCount)
  }

  private static func zeroizeCandidates(_ candidates: inout [[UInt8]]) {
    for index in candidates.indices {
      PassphraseSecret.zeroize(&candidates[index])
    }
  }

  /// The moment lldb exited, set from its termination handler.
  private final class ExitClock: @unchecked Sendable {
    private let lock = NSLock()
    private var recorded: Date?

    func record() {
      lock.lock()
      defer { lock.unlock() }
      if recorded == nil { recorded = Date() }
    }

    var time: Date? {
      lock.lock()
      defer { lock.unlock() }
      return recorded
    }
  }

  /// Bytes received from lldb and not yet handed to the parser. `take`
  /// moves them out, so output is held here only until the next poll.
  final class OutputAccumulator: @unchecked Sendable {
    private let lock = NSLock()
    private var buffer: [UInt8] = []
    private var closed = false

    init() {
      buffer.reserveCapacity(64 * 1024)
    }

    deinit {
      PassphraseSecret.zeroize(&buffer)
    }

    func append(_ data: Data) {
      lock.lock()
      defer { lock.unlock() }
      guard !closed else { return }
      buffer.append(contentsOf: data)
    }

    /// Wipes output that was never taken. A readability handler still in
    /// flight may call `append` afterwards; that output is ignored.
    func close() {
      lock.lock()
      defer { lock.unlock() }
      closed = true
      PassphraseSecret.zeroize(&buffer)
      buffer.removeAll()
    }

    /// Whether nothing is buffered (for tests).
    var isEmpty: Bool {
      lock.lock()
      defer { lock.unlock() }
      return buffer.isEmpty
    }

    func take() -> [UInt8] {
      lock.lock()
      defer { lock.unlock() }
      // Move the storage out rather than copying it; the caller zeroizes it.
      let taken = buffer
      buffer = []
      buffer.reserveCapacity(64 * 1024)
      return taken
    }
  }

  /// Line-buffered verbose relay. Chunks from the pipe can split lines
  /// anywhere, so redaction must wait for complete lines: a `memory read`
  /// hexdump line carries the captured passphrase and is never emitted.
  private final class VerboseLineRelay: @unchecked Sendable {
    private let lock = NSLock()
    private var pending = ""
    private let emitLine: (String) -> Void

    init(emit: @escaping (String) -> Void) {
      self.emitLine = emit
    }

    func append(_ data: Data) {
      lock.lock()
      pending.append(String(decoding: data, as: UTF8.self))
      var lines: [String] = []
      while let newline = pending.firstIndex(of: "\n") {
        lines.append(String(pending[pending.startIndex..<newline]))
        pending.removeSubrange(pending.startIndex...newline)
      }
      lock.unlock()
      for line in lines {
        emitLine("lldb| \(redact(line))")
      }
    }

    func flush() {
      lock.lock()
      let rest = pending
      pending = ""
      lock.unlock()
      guard !rest.trimmingCharacters(in: .whitespaces).isEmpty else { return }
      emitLine("lldb| \(redact(rest))")
    }

    private func redact(_ line: String) -> String {
      let trimmed = line.trimmingCharacters(in: .whitespaces)
      if LLDBOutputParser.isByteLine(trimmed)
        || (trimmed.hasPrefix("0x") && trimmed.contains(": 0x"))
      {
        return "<hexdump redacted>"
      }
      return line
    }
  }
}

/// Offers newly parsed passphrase candidates to `accept`, in order. An
/// earlier rejection does not hide a later one. Each candidate is offered at
/// most once, because the stream consumes the output it came from.
///
/// Every `accept` call can run a 256,000-round PBKDF per distinct salt, so
/// the deadline is checked before each one. A backlog of reads cannot keep
/// the debugger attached past the timeout.
enum CaptureCandidateCursor {
  struct Decision {
    var secret: PassphraseSecret?
    var newlyRejected: Int
    /// Candidates not offered because the deadline had passed.
    var uncheckedCount = 0
    /// The deadline passed before every candidate could be offered.
    var deadlineReached: Bool { uncheckedCount > 0 }
  }

  /// Each offered candidate is zeroized in place; the caller zeroizes any
  /// that were not offered.
  static func decide(
    among candidates: inout [[UInt8]],
    deadline: Date,
    now: () -> Date = Date.init,
    accept: (PassphraseSecret) throws -> Bool
  ) throws -> Decision {
    var newlyRejected = 0
    for index in candidates.indices {
      guard now() < deadline else {
        return Decision(
          secret: nil,
          newlyRejected: newlyRejected,
          uncheckedCount: candidates.count - index
        )
      }
      defer { PassphraseSecret.zeroize(&candidates[index]) }
      let secret = try PassphraseSecret(bytes: candidates[index])
      // `accept` may cross the deadline. The CLI's verifier stops between
      // salts once it does; a candidate it already verified is still a
      // verified key, so it is accepted rather than discarded.
      if try accept(secret) {
        return Decision(secret: secret, newlyRejected: newlyRejected)
      }
      newlyRejected += 1
    }
    return Decision(secret: nil, newlyRejected: newlyRejected)
  }
}
