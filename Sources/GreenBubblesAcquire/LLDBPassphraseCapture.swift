// Derived from wcdb-key-tool (https://github.com/TANGandXUE/wcdb-key-tool), MIT license.

import Darwin
import Foundation

public enum CaptureError: Error, Equatable, CustomStringConvertible {
  case lldbUnavailable
  case lldbLaunchFailed
  case timedOut
  case exitedWithoutCapture

  public var description: String {
    switch self {
    case .lldbUnavailable:
      return "lldb is not available; install the Xcode Command Line Tools"
    case .lldbLaunchFailed:
      return "lldb could not be launched"
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
public struct LLDBPassphraseCapture: Sendable {
  public static let defaultTimeoutSeconds = 300

  private let lldbPath: String

  public init(lldbPath: String = "/usr/bin/lldb") {
    self.lldbPath = lldbPath
  }

  public var lldbAvailable: Bool {
    FileManager.default.isExecutableFile(atPath: lldbPath)
  }

  public func capture(
    processID: pid_t,
    architecture: LLDBTargetArchitecture = .current,
    timeoutSeconds: Int = LLDBPassphraseCapture.defaultTimeoutSeconds
  ) throws -> PassphraseSecret {
    guard lldbAvailable else { throw CaptureError.lldbUnavailable }
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
      if !data.isEmpty { accumulator.append(data) }
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
      // script has already detached and quit; on timeout lldb may still be
      // attached, and terminating it detaches the target.
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

    let deadline = Date().addingTimeInterval(TimeInterval(timeoutSeconds))
    while Date() < deadline {
      if let captured = LLDBOutputParser.parsePassphrase(from: accumulator.current()) {
        return try Self.makeSecret(consuming: captured)
      }
      if !process.isRunning {
        // Give the readability handler a moment to flush the final output.
        Thread.sleep(forTimeInterval: 0.1)
        if let captured = LLDBOutputParser.parsePassphrase(from: accumulator.current()) {
          return try Self.makeSecret(consuming: captured)
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
