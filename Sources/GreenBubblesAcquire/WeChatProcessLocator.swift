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
  /// When set (under sudo), app bundle metadata is read with this user's
  /// permissions rather than root's.
  public let readingAs: InvokingUser?

  public init(
    userID: uid_t = WeChatProcessLocator.invokingUserID(),
    readingAs: InvokingUser? = nil
  ) {
    self.userID = userID
    self.readingAs = readingAs
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
    Self.candidates(
      processIDs: try processIDs(),
      describe: { Self.candidate(forProcessID: $0, expectedUserID: userID, readingAs: readingAs) },
      isGone: Self.processIsGone
    )
  }

  /// Every listed PID stays a candidate. One that can't be described is kept
  /// with no details (unclassified), so selection treats it as unresolved,
  /// unless the process is confirmed gone.
  static func candidates(
    processIDs: [pid_t],
    describe: (pid_t) -> WeChatProcessCandidate?,
    isGone: (pid_t) -> Bool
  ) -> [WeChatProcessCandidate] {
    processIDs.compactMap { processID in
      if let candidate = describe(processID) { return candidate }
      if isGone(processID) { return nil }
      return WeChatProcessCandidate(
        processID: processID,
        userID: nil,
        executablePath: nil,
        applicationPath: nil,
        bundleIdentifier: nil
      )
    }
  }

  /// True only when the kernel reports that no such process exists.
  static func processIsGone(_ processID: pid_t) -> Bool {
    kill(processID, 0) != 0 && errno == ESRCH
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
    let data = output.fileHandleForReading.readDataToEndOfFile()
    process.waitUntilExit()
    guard process.terminationReason == .exit else {
      throw ProcessLocatorError.commandFailed("pgrep")
    }
    return try Self.parsePgrep(status: process.terminationStatus, output: data)
  }

  /// pgrep exits 0 with matches and 1 when nothing matched (an empty result);
  /// any other status is a failed listing, never "no process".
  static func parsePgrep(status: Int32, output: Data) throws -> [pid_t] {
    switch status {
    case 0:
      guard let text = String(data: output, encoding: .utf8) else {
        throw ProcessLocatorError.commandFailed("pgrep")
      }
      return text.split(whereSeparator: \.isNewline).compactMap {
        pid_t($0.trimmingCharacters(in: .whitespaces))
      }
    case 1:
      return []
    default:
      throw ProcessLocatorError.commandFailed("pgrep")
    }
  }

  /// The PID's real user ID and start time as the kernel reports them now
  /// (`proc_pidinfo`), or nil when they can't be read (for example because
  /// no such process exists).
  public static func attachIdentity(ofProcessID processID: pid_t) -> AttachIdentity? {
    guard processID > 0 else { return nil }
    var info = proc_bsdinfo()
    let size = Int32(MemoryLayout<proc_bsdinfo>.size)
    guard proc_pidinfo(processID, PROC_PIDTBSDINFO, 0, &info, size) == size,
      info.pbi_pid == UInt32(processID)
    else { return nil }
    return AttachIdentity(
      processID: processID,
      userID: info.pbi_ruid,
      startMicroseconds: info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec
    )
  }

  /// The PID's executable path (`proc_pidpath`); reads no file.
  static func executablePath(ofProcessID processID: pid_t) -> String? {
    var buffer = [CChar](repeating: 0, count: 4 * Int(MAXPATHLEN))
    let length = proc_pidpath(processID, &buffer, UInt32(buffer.count))
    return length > 0
      ? String(decoding: buffer.prefix(Int(length)).map { UInt8(bitPattern: $0) }, as: UTF8.self)
      : nil
  }

  /// Describes a live process: its real UID and start time (`proc_pidinfo`),
  /// executable (`proc_pidpath`), enclosing app bundle and, only when the
  /// process belongs to `expectedUserID`, that bundle's identifier. The
  /// Info.plist is read with `readingAs`'s permissions when given. Returns nil
  /// when the kernel describes nothing about the process.
  public static func candidate(
    forProcessID processID: pid_t,
    expectedUserID: uid_t,
    readingAs: InvokingUser? = nil
  ) -> WeChatProcessCandidate? {
    guard processID > 0 else { return nil }
    var info = proc_bsdinfo()
    let size = Int32(MemoryLayout<proc_bsdinfo>.size)
    let haveInfo = proc_pidinfo(processID, PROC_PIDTBSDINFO, 0, &info, size) == size
    let userID: uid_t? = haveInfo ? info.pbi_ruid : nil
    let startMicroseconds: UInt64? =
      haveInfo ? info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec : nil

    let executablePath = Self.executablePath(ofProcessID: processID)
    guard userID != nil || executablePath != nil else { return nil }

    let applicationPath = executablePath.flatMap(
      WeChatProcessSelector.applicationPath(forExecutablePath:))
    // Read the bundle only for the invoking user's own process; another
    // user's (or an unverifiable) process is left unclassified.
    var bundleID: String?
    if userID == expectedUserID, let applicationPath, let executablePath {
      let executableName = (executablePath as NSString).lastPathComponent
      let read = {
        Self.bundleIdentifier(applicationPath: applicationPath, executableName: executableName)
      }
      if let readingAs {
        bundleID = (try? readingAs.perform(read)) ?? nil
      } else {
        bundleID = read()
      }
    }
    return WeChatProcessCandidate(
      processID: processID,
      userID: userID,
      executablePath: executablePath,
      applicationPath: applicationPath,
      bundleIdentifier: bundleID,
      startMicroseconds: startMicroseconds
    )
  }

  /// `CFBundleIdentifier` from the bundle's Info.plist, only when its
  /// `CFBundleExecutable` names the running executable.
  static func bundleIdentifier(applicationPath: String, executableName: String) -> String? {
    guard
      let data = readInfoPlist(applicationPath: applicationPath),
      let value = try? PropertyListSerialization.propertyList(from: data, format: nil),
      let info = value as? [String: Any],
      let identifier = info["CFBundleIdentifier"] as? String,
      !identifier.isEmpty,
      info["CFBundleExecutable"] as? String == executableName
    else { return nil }
    return identifier
  }

  static let maximumInfoPlistBytes = 1 << 20

  /// Reads `<app>/Contents/Info.plist` without following a link at the final
  /// component and without blocking on a FIFO or device, and only when the
  /// opened file is a regular file that really lies inside the resolved
  /// app's `Contents` folder.
  static func readInfoPlist(applicationPath: String) -> Data? {
    guard let resolvedApplication = realpath(applicationPath, nil) else { return nil }
    let applicationRoot = String(cString: resolvedApplication)
    free(resolvedApplication)
    let descriptor = open(
      applicationRoot + "/Contents/Info.plist", O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC)
    guard descriptor >= 0 else { return nil }
    let handle = FileHandle(fileDescriptor: descriptor, closeOnDealloc: true)
    var metadata = stat()
    guard fstat(descriptor, &metadata) == 0, metadata.st_mode & S_IFMT == S_IFREG,
      metadata.st_size <= maximumInfoPlistBytes
    else { return nil }
    var buffer = [CChar](repeating: 0, count: Int(MAXPATHLEN))
    guard fcntl(descriptor, F_GETPATH, &buffer) != -1 else { return nil }
    let openedPath = String(
      decoding: buffer.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }, as: UTF8.self)
    guard openedPath == applicationRoot + "/Contents/Info.plist" else { return nil }
    guard let data = try? handle.read(upToCount: maximumInfoPlistBytes + 1),
      data.count <= maximumInfoPlistBytes
    else { return nil }
    return data
  }
}
