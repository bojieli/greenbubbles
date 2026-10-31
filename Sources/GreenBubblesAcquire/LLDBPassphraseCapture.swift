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
  case exitedWithoutCapture

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
        "captured candidates were rejected before the timeout; nothing was written"
    case .exitedWithoutCapture:
      return "lldb exited before a passphrase was captured"
    }
  }
}

/// Attaches lldb to a running WeChat process and captures the 32-byte
/// SQLCipher passphrase passed to the system `CCKeyDerivationPBKDF` function
/// when the owner logs out and back in.
///
/// The lldb script is written to a stdin pipe that stays open for the whole
/// capture window (lldb must not see EOF while it waits for the breakpoint).
/// stdout and stderr are accumulated incrementally and re-parsed after every
/// chunk. The loop stops at the first candidate `accept` keeps, or at the
/// timeout. A rejected candidate does not end the session: the breakpoint
/// stays armed for a later call in the same window. Only the child process
/// spawned here is ever terminated — never `pkill`.
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

  private func emit(_ message: String) {
    outputHandler?(message)
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
    var rejectedCandidateCount = 0
    while true {
      guard let processID = try? WeChatProcessLocator().processIDs().first else {
        if Date() < deadline {
          Thread.sleep(forTimeInterval: 1)
          continue
        }
        if rejectedCandidateCount > 0 { throw CaptureError.noAcceptableCandidate }
        throw CaptureError.noWeChatProcess
      }
      emit("attaching to WeChat process \(processID)")
      do {
        return try runSingleCapture(
          processID: processID,
          architecture: architecture,
          deadline: deadline,
          accept: accept,
          rejectedCandidateCount: &rejectedCandidateCount
        )
      } catch CaptureError.targetExited {
        guard Date() < deadline else { throw deadlineError(rejectedCandidateCount) }
        emit("WeChat process \(processID) exited; waiting to re-attach to the new process")
        Thread.sleep(forTimeInterval: 1)
      }
    }
  }

  private func deadlineError(_ rejectedCandidateCount: Int) -> CaptureError {
    rejectedCandidateCount > 0 ? .noAcceptableCandidate : .timedOut
  }

  private func runSingleCapture(
    processID: pid_t,
    architecture: LLDBTargetArchitecture,
    deadline: Date,
    accept: (PassphraseSecret) throws -> Bool,
    rejectedCandidateCount: inout Int
  ) throws -> PassphraseSecret {
    let script = LLDBCaptureScript.script(processID: processID, architecture: architecture)

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
    let verboseRelay = verbose ? VerboseLineRelay(emit: { [self] in emit($0) }) : nil
    output.fileHandleForReading.readabilityHandler = { handle in
      let data = handle.availableData
      if !data.isEmpty {
        accumulator.append(data)
        verboseRelay?.append(data)
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

    // Keep stdin open after writing the script: lldb must not read EOF while
    // it waits for the breakpoint to hit.
    input.fileHandleForWriting.write(Data(script.utf8))

    var reportedAttachFailure = false
    var consideredCount = 0
    while Date() < deadline {
      let current = accumulator.current()
      if let accepted = try consider(
        current,
        consideredCount: &consideredCount,
        rejectedCandidateCount: &rejectedCandidateCount,
        accept: accept
      ) {
        return accepted
      }
      if !reportedAttachFailure, let detail = LLDBOutputParser.detectAttachFailure(in: current) {
        reportedAttachFailure = true
        emit("lldb reported: \(detail)")
        throw CaptureError.attachFailed(detail)
      }
      if LLDBOutputParser.detectTargetExit(in: current) {
        throw CaptureError.targetExited
      }
      if !process.isRunning {
        // Give the readability handler a moment to flush the final output.
        Thread.sleep(forTimeInterval: 0.1)
        let final = accumulator.current()
        if let accepted = try consider(
          final,
          consideredCount: &consideredCount,
          rejectedCandidateCount: &rejectedCandidateCount,
          accept: accept
        ) {
          return accepted
        }
        if let detail = LLDBOutputParser.detectAttachFailure(in: final) {
          throw CaptureError.attachFailed(detail)
        }
        if LLDBOutputParser.detectTargetExit(in: final) {
          throw CaptureError.targetExited
        }
        if rejectedCandidateCount > 0 { throw CaptureError.noAcceptableCandidate }
        throw CaptureError.exitedWithoutCapture
      }
      Thread.sleep(forTimeInterval: 0.05)
    }
    throw deadlineError(rejectedCandidateCount)
  }

  private func consider(
    _ output: String,
    consideredCount: inout Int,
    rejectedCandidateCount: inout Int,
    accept: (PassphraseSecret) throws -> Bool
  ) throws -> PassphraseSecret? {
    var parsed = LLDBOutputParser.parsePassphrases(from: output)
    defer { Self.zeroizeCandidates(&parsed) }
    let decision = try CaptureCandidateCursor.decide(
      among: parsed,
      consideredCount: &consideredCount,
      accept: accept
    )
    rejectedCandidateCount += decision.newlyRejected
    if let secret = decision.secret {
      return secret
    }
    if decision.newlyRejected > 0 {
      emit("captured candidate was rejected; waiting for another derivation")
    }
    return nil
  }

  private static func zeroizeCandidates(_ candidates: inout [[UInt8]]) {
    for index in candidates.indices {
      PassphraseSecret.zeroize(&candidates[index])
    }
  }

  private final class OutputAccumulator: @unchecked Sendable {
    private let lock = NSLock()
    private var buffer = String()

    func append(_ data: Data) {
      lock.lock()
      buffer.append(String(decoding: data, as: UTF8.self))
      lock.unlock()
    }

    func current() -> String {
      lock.lock()
      defer { lock.unlock() }
      return buffer
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

/// Walks newly parsed passphrase candidates. An earlier rejection does not
/// hide a later one, and candidates already considered are not presented again.
enum CaptureCandidateCursor {
  struct Decision {
    var secret: PassphraseSecret?
    var newlyRejected: Int
  }

  static func decide(
    among candidates: [[UInt8]],
    consideredCount: inout Int,
    accept: (PassphraseSecret) throws -> Bool
  ) throws -> Decision {
    var newlyRejected = 0
    while consideredCount < candidates.count {
      var bytes = candidates[consideredCount]
      consideredCount += 1
      defer { PassphraseSecret.zeroize(&bytes) }
      let secret = try PassphraseSecret(bytes: bytes)
      if try accept(secret) {
        return Decision(secret: secret, newlyRejected: newlyRejected)
      }
      newlyRejected += 1
    }
    return Decision(secret: nil, newlyRejected: newlyRejected)
  }
}
