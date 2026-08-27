// Derived from wcdb-key-tool (https://github.com/TANGandXUE/wcdb-key-tool), MIT license.

import Foundation

public enum ProcessLocatorError: Error, Equatable, CustomStringConvertible {
  case commandFailed(String)

  public var description: String {
    switch self {
    case .commandFailed(let command):
      return "Process locator command failed: \(command)"
    }
  }
}

/// Finds running WeChat process IDs via `/usr/bin/pgrep -x`, matching the
/// subprocess style of `WeChatClientBuildInspector`.
public struct WeChatProcessLocator: Sendable {
  public init() {}

  public func processIDs() throws -> [pid_t] {
    let process = Process()
    let output = Pipe()
    process.executableURL = URL(fileURLWithPath: "/usr/bin/pgrep")
    process.arguments = ["-x", "WeChat"]
    process.standardOutput = output
    process.standardError = Pipe()
    do {
      try process.run()
    } catch {
      throw ProcessLocatorError.commandFailed("pgrep")
    }
    process.waitUntilExit()
    let data = output.fileHandleForReading.readDataToEndOfFile()
    guard process.terminationReason == .exit else {
      throw ProcessLocatorError.commandFailed("pgrep")
    }
    // pgrep exits 1 when no process matched; that is an empty result, not an error.
    guard process.terminationStatus == 0 else { return [] }
    guard let text = String(data: data, encoding: .utf8) else {
      throw ProcessLocatorError.commandFailed("pgrep")
    }
    return text.split(whereSeparator: \.isNewline).compactMap {
      pid_t($0.trimmingCharacters(in: .whitespaces))
    }
  }
}
