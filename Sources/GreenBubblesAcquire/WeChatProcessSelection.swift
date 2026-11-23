import Darwin
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
  /// Process start time in microseconds since 1970 (`pbi_start_tvsec` and
  /// `pbi_start_tvusec`). Together with the PID it identifies one process,
  /// which guards against PID reuse.
  public let startMicroseconds: UInt64?

  public init(
    processID: pid_t,
    userID: uid_t?,
    executablePath: String?,
    applicationPath: String?,
    bundleIdentifier: String?,
    startMicroseconds: UInt64? = nil
  ) {
    self.processID = processID
    self.userID = userID
    self.executablePath = executablePath
    self.applicationPath = applicationPath
    self.bundleIdentifier = bundleIdentifier
    self.startMicroseconds = startMicroseconds
  }
}

/// One process: its PID together with its start time, so a later process
/// that reuses the PID is a different identity.
public struct ProcessIdentity: Hashable, Sendable {
  public let processID: pid_t
  public let startMicroseconds: UInt64

  public init(processID: pid_t, startMicroseconds: UInt64) {
    self.processID = processID
    self.startMicroseconds = startMicroseconds
  }
}

/// The WeChat process chosen for a capture. The capture follows this app
/// bundle (path and bundle ID) when WeChat restarts during logout and login.
public struct WeChatProcessTarget: Equatable, Sendable {
  public let processID: pid_t
  public let userID: uid_t
  public let applicationPath: String
  public let bundleIdentifier: String
  /// Start time of the selected process; a process is only selectable when
  /// it can be read.
  public let startMicroseconds: UInt64
  /// Other processes of the same app that were already running when this
  /// one was selected (PID and start time). They are never taken as its
  /// replacement; a later process that reuses one of those PIDs is not
  /// excluded.
  public let excludedProcesses: Set<ProcessIdentity>
  /// When the capture first saw this process gone (microseconds since 1970).
  /// A replacement must have started after this moment.
  public var exitObservedMicroseconds: UInt64?

  public init(
    processID: pid_t,
    userID: uid_t,
    applicationPath: String,
    bundleIdentifier: String,
    startMicroseconds: UInt64,
    excludedProcesses: Set<ProcessIdentity> = [],
    exitObservedMicroseconds: UInt64? = nil
  ) {
    self.processID = processID
    self.userID = userID
    self.applicationPath = applicationPath
    self.bundleIdentifier = bundleIdentifier
    self.startMicroseconds = startMicroseconds
    self.excludedProcesses = excludedProcesses.subtracting([
      ProcessIdentity(processID: processID, startMicroseconds: startMicroseconds)
    ])
    self.exitObservedMicroseconds = exitObservedMicroseconds
  }

  func replaced(by replacement: WeChatProcessTarget) -> WeChatProcessTarget {
    WeChatProcessTarget(
      processID: replacement.processID,
      userID: userID,
      applicationPath: applicationPath,
      bundleIdentifier: bundleIdentifier,
      startMicroseconds: replacement.startMicroseconds,
      excludedProcesses: excludedProcesses
    )
  }

  var identity: ProcessIdentity {
    ProcessIdentity(processID: processID, startMicroseconds: startMicroseconds)
  }

  func runsSameApp(as other: WeChatProcessTarget) -> Bool {
    userID == other.userID && applicationPath == other.applicationPath
      && bundleIdentifier == other.bundleIdentifier
  }

  /// Same PID and the same start time.
  func isSameProcess(as other: WeChatProcessTarget) -> Bool {
    processID == other.processID && startMicroseconds == other.startMicroseconds
  }

  /// Microseconds since 1970 for `date`, the clock process start times use.
  public static func microseconds(_ date: Date) -> UInt64 {
    UInt64(max(0, date.timeIntervalSince1970 * 1_000_000))
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

  /// Both paths are first canonicalized, so a root given in another letter
  /// case on a case-insensitive volume (or through a link) still maps to its
  /// container, using the container's on-disk name.
  public static func resolve(databaseRoot: URL, homeDirectory: URL) -> DatabaseRootOwner {
    let root = canonicalURL(databaseRoot).pathComponents
    let home = canonicalURL(homeDirectory).pathComponents
    let prefix = home + ["Library", "Containers"]
    guard root.count >= prefix.count + 2, Array(root.prefix(prefix.count)) == prefix else {
      return .unknown
    }
    let bundleIdentifier = root[prefix.count]
    guard !bundleIdentifier.isEmpty, root[prefix.count + 1] == "Data" else { return .unknown }
    return .application(bundleIdentifier: bundleIdentifier)
  }

  /// The path the file system reports for an existing directory (`F_GETPATH`
  /// on a descriptor opened only for its path: links resolved, on-disk
  /// letter case). Paths that cannot be opened fall back to a lexical form.
  static func canonicalURL(_ url: URL) -> URL {
    let standardized = url.standardizedFileURL
    let descriptor = open(standardized.path, O_EVTONLY | O_CLOEXEC)
    guard descriptor >= 0 else { return standardized.resolvingSymlinksInPath() }
    defer { close(descriptor) }
    var buffer = [CChar](repeating: 0, count: Int(MAXPATHLEN))
    guard fcntl(descriptor, F_GETPATH, &buffer) != -1 else {
      return standardized.resolvingSymlinksInPath()
    }
    let path = String(
      decoding: buffer.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }, as: UTF8.self)
    return URL(fileURLWithPath: path).standardizedFileURL
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
  case unclassifiedProcesses(count: Int)
  case processIdentityUnavailable(pid_t)

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
    case .processIdentityUnavailable(let processID):
      return "The user and start time of process \(processID) could not be read"
    case .processNotFound(let processID):
      return "Process \(processID) was not found"
    case .processOwnedByAnotherUser(let processID):
      return "Process \(processID) belongs to another user"
    case .processNotWeChat(let processID):
      return "Process \(processID) is not a WeChat app"
    case .processDoesNotOwnDatabaseRoot(let processID, let processBundle, let ownerBundle):
      return
        "Process \(processID) runs \(processBundle), but the database root belongs to \(ownerBundle)"
    case .unclassifiedProcesses(let count):
      return
        "\(count) WeChat \(count == 1 ? "process" : "processes") could not be identified "
        + "(user, app bundle or start time unreadable); cannot choose a process automatically"
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
    // A listed process whose user is unknown may still be the user's own.
    let own = candidates.filter { $0.userID == userID || $0.userID == nil }
    guard !own.isEmpty else { return nil }
    guard case .application(let ownerBundle) = owner else {
      throw ProcessSelectionError.databaseRootNotInAppContainer(processCount: own.count)
    }
    // A live process that can't be classified could be the one that owns the
    // root, so choosing among the others would be a guess.
    let unclassified = own.filter { !isClassified($0, userID: userID) }
    guard unclassified.isEmpty else {
      throw ProcessSelectionError.unclassifiedProcesses(count: unclassified.count)
    }
    let matches = own.compactMap { target(for: $0, userID: userID) }
      .filter { sameBundleIdentifier($0.bundleIdentifier, ownerBundle) }
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
  /// `runningCandidates` are the user's WeChat processes at selection time;
  /// other instances of the same app among them are recorded so a capture
  /// never mistakes one for the chosen process's replacement.
  public static func validate(
    requestedProcessID: pid_t,
    candidate: WeChatProcessCandidate?,
    userID: uid_t,
    owner: DatabaseRootOwner,
    runningCandidates: [WeChatProcessCandidate] = []
  ) throws -> WeChatProcessTarget {
    guard let candidate, candidate.processID == requestedProcessID else {
      throw ProcessSelectionError.processNotFound(requestedProcessID)
    }
    guard let candidateUser = candidate.userID, candidate.startMicroseconds != nil else {
      throw ProcessSelectionError.processIdentityUnavailable(requestedProcessID)
    }
    guard candidateUser == userID else {
      throw ProcessSelectionError.processOwnedByAnotherUser(requestedProcessID)
    }
    guard
      let executablePath = candidate.executablePath,
      (executablePath as NSString).lastPathComponent == WeChatProcessLocator.executableName,
      let target = target(for: candidate, userID: userID)
    else { throw ProcessSelectionError.processNotWeChat(requestedProcessID) }
    if case .application(let ownerBundle) = owner,
      !sameBundleIdentifier(target.bundleIdentifier, ownerBundle)
    {
      throw ProcessSelectionError.processDoesNotOwnDatabaseRoot(
        processID: requestedProcessID,
        processBundleIdentifier: target.bundleIdentifier,
        ownerBundleIdentifier: ownerBundle
      )
    }
    let siblings = runningCandidates.compactMap { Self.target(for: $0, userID: userID) }
      .filter { $0.runsSameApp(as: target) }
      .map(\.identity)
    return WeChatProcessTarget(
      processID: target.processID,
      userID: userID,
      applicationPath: target.applicationPath,
      bundleIdentifier: target.bundleIdentifier,
      startMicroseconds: target.startMicroseconds,
      excludedProcesses: Set(siblings)
    )
  }

  /// Finds the process to attach to for the selected app, for the first
  /// attach and after WeChat restarts.
  ///
  /// The selected process is kept while it still runs (same PID and start
  /// time), even if other instances of the same app run too. Once it is
  /// gone, the first call records `now` as the moment its exit was observed.
  /// A replacement must run the same app, must have started strictly after
  /// that moment, and must not be an instance recorded at selection; exactly
  /// one may qualify. An instance that started while the selected process
  /// was still running, a reused PID, a process whose start time is unknown,
  /// or another app copy never qualifies. Returns nil while none qualifies,
  /// so the capture keeps waiting until its timeout (fail closed).
  public static func reselect(
    candidates: [WeChatProcessCandidate],
    following selected: inout WeChatProcessTarget,
    nowMicroseconds: UInt64
  ) throws -> WeChatProcessTarget? {
    let matches = candidates.compactMap { target(for: $0, userID: selected.userID) }
      .filter { $0.runsSameApp(as: selected) }
    if selected.exitObservedMicroseconds == nil {
      if matches.contains(where: { $0.isSameProcess(as: selected) }) {
        return selected
      }
      // The selected PID is still listed but its identity or app could not
      // be verified this time (unreadable user, start time or Info.plist):
      // don't attach now, and don't take this as evidence that it exited.
      if candidates.contains(where: { mayBeSelected($0, selected) }) { return nil }
      selected.exitObservedMicroseconds = nowMicroseconds
    }
    // Choose a replacement only from a conclusive process list: a live
    // process of this user that can't be classified might be the real one.
    let unclassified = candidates.filter {
      ($0.userID == nil || $0.userID == selected.userID)
        && (!isClassified($0, userID: selected.userID) || $0.startMicroseconds == nil)
    }
    if !unclassified.isEmpty { return nil }
    let exitObserved = selected.exitObservedMicroseconds ?? nowMicroseconds
    let replacements = matches.filter { match in
      match.startMicroseconds > exitObserved
        && !selected.excludedProcesses.contains(match.identity)
    }
    guard replacements.count <= 1 else {
      throw ProcessSelectionError.multipleProcessesForTarget(
        bundleIdentifier: selected.bundleIdentifier, count: replacements.count)
    }
    guard let replacement = replacements.first else { return nil }
    selected = selected.replaced(by: replacement)
    return selected
  }

  /// `reselect` for one poll's process listing. When the listing itself
  /// failed (for example pgrep exiting with an error), the poll is skipped:
  /// nothing is attached and the exit state is left unchanged.
  public static func reselect(
    listing: Result<[WeChatProcessCandidate], Error>,
    following selected: inout WeChatProcessTarget,
    nowMicroseconds: UInt64
  ) throws -> WeChatProcessTarget? {
    guard case .success(let candidates) = listing else { return nil }
    return try reselect(
      candidates: candidates, following: &selected, nowMicroseconds: nowMicroseconds)
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

  /// Bundle IDs are case-insensitive, and a container's name may differ in
  /// case from the app's `CFBundleIdentifier`.
  static func sameBundleIdentifier(_ lhs: String, _ rhs: String) -> Bool {
    lhs.caseInsensitiveCompare(rhs) == .orderedSame
  }

  /// User, start time, app bundle and bundle ID are all known.
  private static func isClassified(_ candidate: WeChatProcessCandidate, userID: uid_t) -> Bool {
    candidate.userID == userID && candidate.startMicroseconds != nil
      && candidate.applicationPath != nil && candidate.bundleIdentifier?.isEmpty == false
  }

  /// The candidate has the selected PID and nothing read about it proves it
  /// is a different process (another user, start time or app).
  private static func mayBeSelected(
    _ candidate: WeChatProcessCandidate,
    _ selected: WeChatProcessTarget
  ) -> Bool {
    guard candidate.processID == selected.processID else { return false }
    if let user = candidate.userID, user != selected.userID { return false }
    if let start = candidate.startMicroseconds, start != selected.startMicroseconds {
      return false
    }
    if let path = candidate.applicationPath, path != selected.applicationPath { return false }
    if let bundle = candidate.bundleIdentifier, bundle != selected.bundleIdentifier {
      return false
    }
    return true
  }

  private static func target(
    for candidate: WeChatProcessCandidate,
    userID: uid_t
  ) -> WeChatProcessTarget? {
    // An incomplete candidate (no start time) is never a target.
    guard
      candidate.userID == userID,
      let startMicroseconds = candidate.startMicroseconds,
      let applicationPath = candidate.applicationPath,
      let bundleIdentifier = candidate.bundleIdentifier,
      !bundleIdentifier.isEmpty
    else { return nil }
    return WeChatProcessTarget(
      processID: candidate.processID,
      userID: userID,
      applicationPath: applicationPath,
      bundleIdentifier: bundleIdentifier,
      startMicroseconds: startMicroseconds
    )
  }
}
