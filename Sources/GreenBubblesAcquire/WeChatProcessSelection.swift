import Foundation

/// A running process named `WeChat`, described by the app bundle it was
/// launched from. Fields are nil when they could not be read.
public struct WeChatProcessCandidate: Equatable, Sendable {
  public let processID: pid_t
  /// Real user ID of the process.
  public let userID: uid_t?
  public let executablePath: String?
  /// The enclosing `.app` bundle of `executablePath`.
  public let applicationPath: String?
  /// `CFBundleIdentifier` of that bundle, set only when the bundle's
  /// `CFBundleExecutable` is the running executable.
  public let bundleIdentifier: String?

  public init(
    processID: pid_t,
    userID: uid_t?,
    executablePath: String?,
    applicationPath: String?,
    bundleIdentifier: String?
  ) {
    self.processID = processID
    self.userID = userID
    self.executablePath = executablePath
    self.applicationPath = applicationPath
    self.bundleIdentifier = bundleIdentifier
  }
}

/// The WeChat process chosen for a capture. The capture follows this app
/// bundle (path and bundle ID) when WeChat restarts during logout and login.
public struct WeChatProcessTarget: Equatable, Sendable {
  public let processID: pid_t
  public let userID: uid_t
  public let applicationPath: String
  public let bundleIdentifier: String

  public init(processID: pid_t, userID: uid_t, applicationPath: String, bundleIdentifier: String) {
    self.processID = processID
    self.userID = userID
    self.applicationPath = applicationPath
    self.bundleIdentifier = bundleIdentifier
  }
}

/// Which app owns a database root, judged from its path alone.
public enum DatabaseRootOwner: Equatable, Sendable {
  /// The root is inside `~/Library/Containers/<bundle id>/Data`; macOS names
  /// an app's sandbox container after its bundle ID.
  case application(bundleIdentifier: String)
  /// A group container, a copy elsewhere, or any other path. The owning app
  /// cannot be told from the path, so the process must be chosen with `--pid`.
  case unknown

  public static func resolve(databaseRoot: URL, homeDirectory: URL) -> DatabaseRootOwner {
    let root = databaseRoot.standardizedFileURL.resolvingSymlinksInPath().pathComponents
    let home = homeDirectory.standardizedFileURL.resolvingSymlinksInPath().pathComponents
    let prefix = home + ["Library", "Containers"]
    guard root.count >= prefix.count + 2, Array(root.prefix(prefix.count)) == prefix else {
      return .unknown
    }
    let bundleIdentifier = root[prefix.count]
    guard !bundleIdentifier.isEmpty, root[prefix.count + 1] == "Data" else { return .unknown }
    return .application(bundleIdentifier: bundleIdentifier)
  }
}

public enum ProcessSelectionError: Error, Equatable, CustomStringConvertible {
  case noProcessForDatabaseRoot(bundleIdentifier: String, otherProcessCount: Int)
  case multipleProcessesForDatabaseRoot(bundleIdentifier: String, count: Int)
  case databaseRootNotInAppContainer(processCount: Int)
  case processNotFound(pid_t)
  case processOwnedByAnotherUser(pid_t)
  case processNotWeChat(pid_t)
  case processDoesNotOwnDatabaseRoot(
    processID: pid_t, processBundleIdentifier: String, ownerBundleIdentifier: String)
  case multipleProcessesForTarget(bundleIdentifier: String, count: Int)

  public var description: String {
    switch self {
    case .noProcessForDatabaseRoot(let bundleIdentifier, let others):
      let suffix =
        others == 0
        ? "" : "; \(others) other WeChat \(others == 1 ? "copy is" : "copies are") running"
      return
        "The WeChat app that owns the database root (\(bundleIdentifier)) is not running\(suffix)"
    case .multipleProcessesForDatabaseRoot(let bundleIdentifier, let count):
      return
        "\(count) WeChat processes belong to \(bundleIdentifier); cannot identify a unique process"
    case .databaseRootNotInAppContainer(let count):
      return
        "The database root is not inside a WeChat app container, so the app that owns it is unknown "
        + "(\(count) WeChat \(count == 1 ? "process is" : "processes are") running)"
    case .processNotFound(let processID):
      return "Process \(processID) was not found"
    case .processOwnedByAnotherUser(let processID):
      return "Process \(processID) belongs to another user"
    case .processNotWeChat(let processID):
      return "Process \(processID) is not a WeChat app"
    case .processDoesNotOwnDatabaseRoot(let processID, let processBundle, let ownerBundle):
      return
        "Process \(processID) runs \(processBundle), but the database root belongs to \(ownerBundle)"
    case .multipleProcessesForTarget(let bundleIdentifier, let count):
      return
        "\(count) WeChat processes run the selected app (\(bundleIdentifier)); cannot follow one"
    }
  }
}

/// Pure selection rules. They take candidate lists, never live processes, so
/// every case can be tested without running WeChat.
public enum WeChatProcessSelector {
  /// Chooses the candidate whose app owns the database root. Returns nil when
  /// none of the user's WeChat processes is running. Zero or several matching
  /// processes are errors; the selector never guesses.
  public static func select(
    candidates: [WeChatProcessCandidate],
    userID: uid_t,
    owner: DatabaseRootOwner
  ) throws -> WeChatProcessTarget? {
    let own = candidates.filter { $0.userID == userID }
    guard !own.isEmpty else { return nil }
    guard case .application(let ownerBundle) = owner else {
      throw ProcessSelectionError.databaseRootNotInAppContainer(processCount: own.count)
    }
    let matches = own.compactMap { target(for: $0, userID: userID) }
      .filter { $0.bundleIdentifier == ownerBundle }
    guard matches.count <= 1 else {
      throw ProcessSelectionError.multipleProcessesForDatabaseRoot(
        bundleIdentifier: ownerBundle, count: matches.count)
    }
    guard let match = matches.first else {
      throw ProcessSelectionError.noProcessForDatabaseRoot(
        bundleIdentifier: ownerBundle, otherProcessCount: own.count)
    }
    return match
  }

  /// Validates a process the owner chose with `--pid`: it must belong to the
  /// invoking user, run a WeChat app bundle, and that app must own the
  /// database root when the owner can be told from the path.
  public static func validate(
    requestedProcessID: pid_t,
    candidate: WeChatProcessCandidate?,
    userID: uid_t,
    owner: DatabaseRootOwner
  ) throws -> WeChatProcessTarget {
    guard let candidate, candidate.processID == requestedProcessID else {
      throw ProcessSelectionError.processNotFound(requestedProcessID)
    }
    guard candidate.userID == userID else {
      throw ProcessSelectionError.processOwnedByAnotherUser(requestedProcessID)
    }
    guard
      let executablePath = candidate.executablePath,
      (executablePath as NSString).lastPathComponent == WeChatProcessLocator.executableName,
      let target = target(for: candidate, userID: userID)
    else { throw ProcessSelectionError.processNotWeChat(requestedProcessID) }
    if case .application(let ownerBundle) = owner, target.bundleIdentifier != ownerBundle {
      throw ProcessSelectionError.processDoesNotOwnDatabaseRoot(
        processID: requestedProcessID,
        processBundleIdentifier: target.bundleIdentifier,
        ownerBundleIdentifier: ownerBundle
      )
    }
    return target
  }

  /// Finds the process now running the selected app, for re-attaching after
  /// WeChat restarts. Other app copies never match. Returns nil while the app
  /// is not running.
  public static func reselect(
    candidates: [WeChatProcessCandidate],
    following selected: WeChatProcessTarget
  ) throws -> WeChatProcessTarget? {
    let matches = candidates.compactMap { target(for: $0, userID: selected.userID) }
      .filter {
        $0.applicationPath == selected.applicationPath
          && $0.bundleIdentifier == selected.bundleIdentifier
      }
    guard matches.count <= 1 else {
      throw ProcessSelectionError.multipleProcessesForTarget(
        bundleIdentifier: selected.bundleIdentifier, count: matches.count)
    }
    return matches.first
  }

  /// The `.app` bundle that contains a main executable at
  /// `<name>.app/Contents/MacOS/<executable>`. Helper executables nested
  /// deeper are not main executables and return nil.
  public static func applicationPath(forExecutablePath path: String) -> String? {
    let components = URL(fileURLWithPath: path).standardizedFileURL.pathComponents
    guard components.count >= 5 else { return nil }
    let count = components.count
    guard
      components[count - 2] == "MacOS",
      components[count - 3] == "Contents",
      components[count - 4].hasSuffix(".app"),
      components[count - 4].count > ".app".count
    else { return nil }
    return NSString.path(withComponents: Array(components.prefix(count - 3)))
  }

  private static func target(
    for candidate: WeChatProcessCandidate,
    userID: uid_t
  ) -> WeChatProcessTarget? {
    guard
      candidate.userID == userID,
      let applicationPath = candidate.applicationPath,
      let bundleIdentifier = candidate.bundleIdentifier,
      !bundleIdentifier.isEmpty
    else { return nil }
    return WeChatProcessTarget(
      processID: candidate.processID,
      userID: userID,
      applicationPath: applicationPath,
      bundleIdentifier: bundleIdentifier
    )
  }
}
