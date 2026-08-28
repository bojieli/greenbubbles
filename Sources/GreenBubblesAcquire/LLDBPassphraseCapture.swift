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
/// chunk; the loop stops at the first parseable passphrase or at the timeout.
/// Only the child process spawned here is ever terminated — never `pkill`.
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
  public func capture(
    architecture: LLDBTargetArchitecture = .current,
    timeoutSeconds: Int = LLDBPassphraseCapture.defaultTimeoutSeconds
  ) throws -> PassphraseSecret {
    guard lldbAvailable else { throw CaptureError.lldbUnavailable }
    let deadline = Date().addingTimeInterval(TimeInterval(timeoutSeconds))
    while true {
      guard let processID = try? WeChatProcessLocator().processIDs().first else {
        if Date() < deadline {
          Thread.sleep(forTimeInterval: 1)
          continue
        }
        throw CaptureError.noWeChatProcess
      }
      emit("attaching to WeChat process \(processID)")
      do {
        return try runSingleCapture(
          processID: processID,
          architecture: architecture,
          deadline: deadline
        )
      } catch CaptureError.targetExited {
        guard Date() < deadline else { throw CaptureError.timedOut }
        emit("WeChat process \(processID) exited; waiting to re-attach to the new process")
        Thread.sleep(forTimeInterval: 1)
      }
    }
  }

  private func runSingleCapture(
    processID: pid_t,
    architecture: LLDBTargetArchitecture,
    deadline: Date
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
    output.fileHandleForReading.readabilityHandler = { handle in
      let data = handle.availableData
      if !data.isEmpty {
        accumulator.append(data)
        if self.verbose {
          self.emit("lldb| " + String(decoding: data, as: UTF8.self))
        }
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
      // Only ever terminate the child spawned here. On a successful capture the
      // script has already detached and quit; otherwise terminating lldb
      // detaches the target.
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
    while Date() < deadline {
      let current = accumulator.current()
      if let captured = LLDBOutputParser.parsePassphrase(from: current) {
        return try Self.makeSecret(consuming: captured)
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
        if let captured = LLDBOutputParser.parsePassphrase(from: final) {
          return try Self.makeSecret(consuming: captured)
        }
        if let detail = LLDBOutputParser.detectAttachFailure(in: final) {
          throw CaptureError.attachFailed(detail)
        }
        if LLDBOutputParser.detectTargetExit(in: final) {
          throw CaptureError.targetExited
        }
        throw CaptureError.exitedWithoutCapture
      }
      Thread.sleep(forTimeInterval: 0.05)
    }
    throw CaptureError.timedOut
  }

  private static func makeSecret(consuming bytes: [UInt8]) throws -> PassphraseSecret {
    var bytes = bytes
    defer { PassphraseSecret.zeroize(&bytes) }
    return try PassphraseSecret(bytes: bytes)
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
}
