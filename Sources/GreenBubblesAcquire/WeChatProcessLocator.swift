// Derived from wcdb-key-tool (https://github.com/TANGandXUE/wcdb-key-tool), MIT license.

import Darwin
import Foundation

public enum ProcessLocatorError: Error, Equatable, CustomStringConvertible {
  case commandFailed(String)
  case multipleProcesses

  public var description: String {
    switch self {
    case .multipleProcesses:
      return "Multiple WeChat processes are running; cannot identify a unique process."
    case .commandFailed(let command):
      return "Process locator command failed: \(command)"
    }
  }
}

/// Finds the invoking user's running WeChat processes via
/// `/usr/bin/pgrep -x -U <uid>`, matching the subprocess style of
/// `WeChatClientBuildInspector`. Other macOS users' WeChat never counts.
public struct WeChatProcessLocator: Sendable {
  /// Every WeChat app copy, including a duplicate with another bundle ID,
  /// runs a main executable with this name.
  public static let executableName = "WeChat"

  public let userID: uid_t

  public init(userID: uid_t = WeChatProcessLocator.invokingUserID()) {
    self.userID = userID
  }

  /// The user who ran the command: `SUDO_UID` under sudo, else the real UID.
  public static func invokingUserID(
    effectiveUserID: uid_t = geteuid(),
    realUserID: uid_t = getuid(),
    environment: [String: String] = ProcessInfo.processInfo.environment
  ) -> uid_t {
    if effectiveUserID == 0, let value = environment["SUDO_UID"], let uid = uid_t(value) {
      return uid
    }
    return realUserID
  }

  public func uniqueProcessID() throws -> pid_t? {
    try Self.uniqueProcessID(in: processIDs())
  }

  static func uniqueProcessID(in matches: [pid_t]) throws -> pid_t? {
    guard matches.count <= 1 else { throw ProcessLocatorError.multipleProcesses }
    return matches.first
  }

  /// The user's WeChat processes, each described by its app bundle.
  public func candidates() throws -> [WeChatProcessCandidate] {
    try processIDs().compactMap(Self.candidate(forProcessID:))
  }

  public func processIDs() throws -> [pid_t] {
    let process = Process()
    let output = Pipe()
    process.executableURL = URL(fileURLWithPath: "/usr/bin/pgrep")
    process.arguments = ["-x", "-U", String(userID), Self.executableName]
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

  /// Describes a live process: its real UID and start time (`proc_pidinfo`), executable
  /// (`proc_pidpath`), enclosing app bundle, and that bundle's identifier.
  /// Returns nil when the process does not exist.
  public static func candidate(forProcessID processID: pid_t) -> WeChatProcessCandidate? {
    guard processID > 0 else { return nil }
    var info = proc_bsdinfo()
    let size = Int32(MemoryLayout<proc_bsdinfo>.size)
    let haveInfo = proc_pidinfo(processID, PROC_PIDTBSDINFO, 0, &info, size) == size
    let userID: uid_t? = haveInfo ? info.pbi_ruid : nil
    let startMicroseconds: UInt64? =
      haveInfo ? info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec : nil

    var buffer = [CChar](repeating: 0, count: 4 * Int(MAXPATHLEN))
    let length = proc_pidpath(processID, &buffer, UInt32(buffer.count))
    let executablePath: String? =
      length > 0
      ? String(decoding: buffer.prefix(Int(length)).map { UInt8(bitPattern: $0) }, as: UTF8.self)
      : nil
    guard userID != nil || executablePath != nil else { return nil }

    let applicationPath = executablePath.flatMap(
      WeChatProcessSelector.applicationPath(forExecutablePath:))
    let bundleIdentifier = applicationPath.flatMap { application in
      executablePath.flatMap {
        bundleIdentifier(
          applicationPath: application,
          executableName: ($0 as NSString).lastPathComponent
        )
      }
    }
    return WeChatProcessCandidate(
      processID: processID,
      userID: userID,
      executablePath: executablePath,
      applicationPath: applicationPath,
      bundleIdentifier: bundleIdentifier,
      startMicroseconds: startMicroseconds
    )
  }

  /// `CFBundleIdentifier` from the bundle's Info.plist, only when its
  /// `CFBundleExecutable` names the running executable.
  static func bundleIdentifier(applicationPath: String, executableName: String) -> String? {
    let infoURL = URL(fileURLWithPath: applicationPath).appending(path: "Contents/Info.plist")
    guard
      let data = try? Data(contentsOf: infoURL),
      let value = try? PropertyListSerialization.propertyList(from: data, format: nil),
      let info = value as? [String: Any],
      let identifier = info["CFBundleIdentifier"] as? String,
      !identifier.isEmpty,
      info["CFBundleExecutable"] as? String == executableName
    else { return nil }
    return identifier
  }
}
