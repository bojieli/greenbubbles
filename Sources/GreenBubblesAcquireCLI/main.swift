// Derived from wcdb-key-tool (https://github.com/TANGandXUE/wcdb-key-tool), MIT license.

import Darwin
import Foundation
import GreenBubblesAcquire
import GreenBubblesCore

enum CLIError: Error, CustomStringConvertible {
  case invalidOption(String)
  case missingValue(String)
  case missingRequiredOption(String)
  case invalidInteger(option: String, value: String)
  case verificationFailed(String)
  case rootConflictsWithTarget(String)
  case parentComponentInRoot

  var description: String {
    switch self {
    case .invalidOption(let option):
      return "Unknown option: \(option)"
    case .missingValue(let option):
      return "Missing value for \(option)"
    case .missingRequiredOption(let option):
      return "Missing required option: \(option)"
    case .invalidInteger(let option, let value):
      return "Expected a positive integer for \(option), got: \(value)"
    case .verificationFailed(let reason):
      return "Passphrase verification failed: \(reason)"
    case .parentComponentInRoot:
      return "--db-root must not contain '..'; give the folder's plain path"
    case .rootConflictsWithTarget(let origin):
      return
        "--db-root may name a different account than the root \(origin), whose key file "
        + "capture would write; pass --output as well, or select that account's profile "
        + "with --profile"
    }
  }
}

struct Arguments {
  enum Command: String {
    case preflight
    case capture
    case verify
    case help
  }

  var command: Command = .help
  var output: URL?
  var timeoutSeconds = LLDBPassphraseCapture.defaultTimeoutSeconds
  var dbRoot: URL?
  var profile: String?
  var processID: pid_t?
  var overwrite = false
  var passphraseStdin = false
  var includePaths = false
  var json = false
  var verbose = false

  init(_ rawArguments: [String]) throws {
    var arguments = rawArguments
    if let first = arguments.first, !first.hasPrefix("-") {
      command = Command(rawValue: first) ?? .help
      if Command(rawValue: first) == nil {
        throw CLIError.invalidOption(first)
      }
      arguments.removeFirst()
    }

    var index = 0
    while index < arguments.count {
      let option = arguments[index]
      switch option {
      case "--output":
        index += 1
        guard index < arguments.count else { throw CLIError.missingValue(option) }
        output = URL(fileURLWithPath: arguments[index])
      case "--owner-authorized":
        // Accepted for backward compatibility; owner consent is implicit in
        // running this deliberately gated helper. Ignored.
        break
      case "--timeout-seconds":
        index += 1
        guard index < arguments.count else { throw CLIError.missingValue(option) }
        let value = arguments[index]
        guard let number = Int(value), number > 0 else {
          throw CLIError.invalidInteger(option: option, value: value)
        }
        timeoutSeconds = number
      case "--db-root":
        index += 1
        guard index < arguments.count else { throw CLIError.missingValue(option) }
        dbRoot = URL(fileURLWithPath: arguments[index])
      case "--profile":
        index += 1
        guard index < arguments.count else { throw CLIError.missingValue(option) }
        profile = arguments[index]
      case "--pid":
        index += 1
        guard index < arguments.count else { throw CLIError.missingValue(option) }
        let value = arguments[index]
        guard let number = pid_t(value), number > 0 else {
          throw CLIError.invalidInteger(option: option, value: value)
        }
        processID = number
      case "--overwrite":
        overwrite = true
      case "--passphrase-stdin":
        passphraseStdin = true
      case "--include-paths":
        includePaths = true
      case "--json":
        json = true
      case "--verbose":
        verbose = true
      case "-h", "--help":
        command = .help
      default:
        throw CLIError.invalidOption(option)
      }
      index += 1
    }
  }
}

private let resignRemediation = "sudo codesign --force --deep --sign - /Applications/WeChat.app"

/// The re-sign command for the selected app copy; the default copy keeps the
/// documented command. Another copy's path can name the user's account (for
/// example `~/Applications`), so it is shown only with `--include-paths`.
private func resignRemediation(applicationPath: String?, includePaths: Bool) -> String {
  guard let applicationPath, applicationPath != "/Applications/WeChat.app" else {
    return resignRemediation
  }
  guard includePaths else {
    return "sudo codesign --force --deep --sign - <selected WeChat app> "
      + "(re-run preflight with --include-paths to show its path)"
  }
  return "sudo codesign --force --deep --sign - \(shellQuoted(applicationPath))"
}

private let usage = """
  Usage: greenbubbles-acquire <command> [options]

  Commands:
    preflight              Check capture readiness; exits non-zero when blocked
    capture                Attach lldb to WeChat and capture the database passphrase
    verify                 Re-validate a stored passphrase read from standard input
    help                   Show this help

  Options:
    --output <path>        Where capture writes the passphrase (default: the
                           query profile's credentialFile, else config.toml's
                           source.passphrase_file, else
                           ~/.greenbubbles-acquire/passphrase.txt; mode 0600)
    --timeout-seconds <n>  Capture window in seconds (default: 300)
    --db-root <path>       Select a db_storage root (overrides the profile's
                           sourceRoot; required if several roots are found)
    --profile <name>       Use this live query profile's sourceRoot and
                           credentialFile (default: the file's defaultProfile,
                           or config.toml when no profile file exists)
    --pid <n>              Select the WeChat process (required when the root is
                           not in an app container or several copies match)
    --overwrite            Replace an existing passphrase output file
    --passphrase-stdin     Read the 32-byte passphrase from standard input
    --include-paths        Include sensitive filesystem paths in local output
    --json                 Emit machine-readable JSON instead of human-readable text
    --verbose              Mirror lldb output during capture (for diagnosing stalls)
    -h, --help             Show this help

  Capture requires root, lldb, a running WeChat, and an owner re-signed client
  without Hardened Runtime. When re-signing is required, preflight prints the
  exact command for the owner to run manually; this tool never automates it.
  Preflight checks prerequisites. Only successful database verification
  permits an output file to be written.
  """

private struct PreflightReport: Encodable {
  let formatVersion: Int
  let ready: Bool
  let blockers: [String]
  let remediation: String?
  let lldbAvailable: Bool
  let runningAsRoot: Bool
  let wechatProcessRunning: Bool
  let processHardeningStatus: ProcessHardeningStatus?
  let clientInstalled: Bool
  let clientReSigned: Bool
  let clientMarketingVersion: String?
  let clientBuildVersion: String?
  let selectedProcessID: pid_t?
  let selectedApplicationBundleIdentifier: String?
  let databaseRootApplicationBundleIdentifier: String?
  let databaseRoot: PathReference?
  let databaseRootAutoDiscovered: Bool
  let databaseCount: Int?
  let distinctSaltCount: Int?
}

private struct VerificationReport: Encodable {
  let formatVersion: Int
  let databaseCount: Int
  let distinctSaltCount: Int
  let verifiedSaltCount: Int
  let allVerified: Bool
}

private struct PreflightCheck {
  let ok: Bool
  let label: String
  let hint: String?
}

private struct PreflightResult {
  let report: PreflightReport
  let checks: [PreflightCheck]
  /// The WeChat app copy capture attaches to; set whenever `report.ready`.
  let target: WeChatProcessTarget?
  /// The user-checked database root, resolved once for this run; set
  /// whenever `report.ready`.
  let databaseRoot: URL?
}

private func printJSON<T: Encodable>(_ value: T) throws {
  let encoder = JSONEncoder()
  encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
  encoder.dateEncodingStrategy = .iso8601
  let data = try encoder.encode(value)
  FileHandle.standardOutput.write(data)
  FileHandle.standardOutput.write(Data("\n".utf8))
}

private func printError(_ message: String) {
  FileHandle.standardError.write(Data("error: \(message)\n".utf8))
}

private func printNote(_ message: String) {
  FileHandle.standardError.write(Data("note: \(message)\n".utf8))
}

private let stdoutIsTerminal = isatty(FileHandle.standardOutput.fileDescriptor) != 0

private func statusMark(_ ok: Bool) -> String {
  if stdoutIsTerminal {
    return ok ? "\u{1B}[32m✓\u{1B}[0m" : "\u{1B}[31m✗\u{1B}[0m"
  }
  return ok ? "✓" : "✗"
}

// Human-readable rendering: one line per check, then a plain verdict.
private func printPreflight(
  _ result: PreflightResult,
  captureCommand: [String] = ["sudo", "greenbubbles-acquire", "capture"]
) {
  for check in result.checks {
    print("\(statusMark(check.ok)) \(check.label)")
    if !check.ok, let hint = check.hint {
      print("    \(hint)")
    }
  }
  print("")
  let failed = result.checks.filter { !$0.ok }.count
  if result.report.ready {
    print("Ready to capture.")
    print("Next: \(captureCommand.joined(separator: " "))")
  } else {
    print("Not ready — \(failed) \(failed == 1 ? "problem" : "problems") to fix (see above).")
  }
}

// Under sudo, the user who ran it (SUDO_USER checked against SUDO_UID);
// otherwise the current user. User-controlled files are opened as this user.
private let invokingUser: InvokingUser = {
  do {
    return try InvokingUser.current()
  } catch {
    printError(String(describing: error))
    exit(2)
  }
}()

// Under sudo the effective home is /var/root, but the WeChat data lives in the
// invoking user's home.
private func invokingUserHomeDirectory() -> URL {
  invokingUser.homeDirectory
}

// Explicit --db-root and --output win; a query profile or config.toml fills in
// what they leave out, as for the Rust CLI's queries.
private func resolveTarget(_ arguments: Arguments, needsOutput: Bool) throws -> AcquireTarget {
  if arguments.profile == nil, arguments.dbRoot != nil, !needsOutput || arguments.output != nil {
    return .none
  }
  let locations = try QueryProfileTarget.locations(
    environment: ProcessInfo.processInfo.environment,
    home: invokingUser.homeDirectory
  )
  let target = try invokingUser.perform {
    try QueryProfileTarget.resolve(
      requestedProfile: arguments.profile,
      locations: locations,
      ownerUID: invokingUser.uid
    )
  }
  if let name = target.profileName { printNote("using query profile \(name)") }
  if needsOutput, arguments.output == nil,
    let origin = try invokingUser.perform({
      target.originConflicting(withDatabaseRoot: arguments.dbRoot)
    })
  {
    throw CLIError.rootConflictsWithTarget(origin)
  }
  return target
}

// Where capture writes the key for this target.
private func captureOutput(_ arguments: Arguments, _ target: AcquireTarget) -> URL {
  arguments.output
    ?? target.credentialFile
    ?? invokingUserHomeDirectory()
    .appending(path: ".greenbubbles-acquire", directoryHint: .isDirectory)
    .appending(path: "passphrase.txt")
}

// The database root, found and resolved through the filesystem (realpath) as
// the invoking user, so the folder inventoried is the one their queries open.
private func userDatabaseRoot(_ option: URL?) throws -> URL {
  if let option, QueryProfileTarget.hasParentComponent(option.path) {
    throw CLIError.parentComponentInRoot
  }
  return try invokingUser.perform {
    let root = try resolveDatabaseRoot(option)
    // Rust's LiveQuerySource::open checks the path as given, then resolves it.
    try QueryProfileTarget.checkLiveSourceRoot(root, ownerUID: invokingUser.uid)
    return QueryProfileTarget.canonicalPath(root) ?? root
  }
}

private func resolveDatabaseRoot(_ option: URL?) throws -> URL {
  if let option {
    return option.standardizedFileURL
  }
  return try WeChatAccountDiscovery(homeDirectory: invokingUserHomeDirectory())
    .singleDatabaseRoot()
}

// A path as one shell word: plain when safe, otherwise single-quoted.
private func shellQuoted(_ text: String) -> String {
  let plain =
    !text.isEmpty
    && text.unicodeScalars.allSatisfy {
      CharacterSet.alphanumerics.contains($0) && $0.isASCII
        || "/._-+,:@".unicodeScalars.contains($0)
    }
  return plain ? text : "'" + text.replacingOccurrences(of: "'", with: "'\\''") + "'"
}

// The capture command that reproduces this preflight's target. Private paths
// become placeholders unless --include-paths was given.
private func captureCommand(_ arguments: Arguments) -> [String] {
  func path(_ url: URL, placeholder: String) -> String {
    arguments.includePaths ? shellQuoted(url.path) : placeholder
  }
  // sudo drops these unless told to keep them (names only, never values).
  let preserved = [
    QueryProfileTarget.profilesEnvironmentVariable, QueryProfileTarget.settingsEnvironmentVariable,
  ].filter { ProcessInfo.processInfo.environment[$0] != nil }
  var command = ["sudo"]
  if !preserved.isEmpty { command.append("--preserve-env=" + preserved.joined(separator: ",")) }
  command += ["greenbubbles-acquire", "capture"]
  if let profile = arguments.profile { command += ["--profile", profile] }
  if let dbRoot = arguments.dbRoot {
    command += ["--db-root", path(dbRoot, placeholder: "<db_storage-path>")]
  }
  if let output = arguments.output {
    command += ["--output", path(output, placeholder: "<key-file-path>")]
  }
  if let processID = arguments.processID { command += ["--pid", String(processID)] }
  return command
}

private struct TargetResolutionFailure: Error, CustomStringConvertible {
  let description: String
}

private let listProcessesHint =
  "list WeChat processes and their apps with: pgrep -lf /Contents/MacOS/WeChat"

private struct ProcessSelection {
  var target: WeChatProcessTarget?
  var processID: pid_t?
  var applicationPath: String?
  var running = false
  var failure: (label: String, hint: String?)?
}

private func processLocator() -> WeChatProcessLocator {
  WeChatProcessLocator(userID: invokingUser.uid, readingAs: invokingUser)
}

/// Selects the WeChat process whose app owns the database root (or validates
/// `--pid`). Without a database root it only reports whether one WeChat runs.
private func selectProcess(
  processIDOption: pid_t?,
  owner: DatabaseRootOwner?,
  ownerLookupError: String? = nil
) -> ProcessSelection {
  // The invoking user the privilege switch uses (SUDO_USER checked against
  // SUDO_UID); only their WeChat processes count, and app metadata is read
  // with their permissions.
  let locator = processLocator()
  var selection = ProcessSelection()
  if let ownerLookupError {
    // Not knowing the owner is different from a root outside any container:
    // block instead of skipping the ownership requirement.
    selection.running = (try? locator.processIDs().isEmpty == false) ?? false
    selection.failure = (
      "could not determine which app owns the database root: \(ownerLookupError)", nil
    )
    return selection
  }
  do {
    if let processIDOption {
      let target = try WeChatProcessSelector.validate(
        requestedProcessID: processIDOption,
        candidate: WeChatProcessLocator.candidate(
          forProcessID: processIDOption, expectedUserID: invokingUser.uid,
          readingAs: invokingUser),
        userID: locator.userID,
        owner: owner ?? .unknown,
        runningCandidates: try locator.candidates()
      )
      selection.target = target
    } else if let owner {
      selection.target = try WeChatProcessSelector.select(
        candidates: locator.candidates(),
        userID: locator.userID,
        owner: owner
      )
    } else if let processID = try locator.uniqueProcessID() {
      // No database root to match against; the database check blocks capture.
      selection.processID = processID
      if let candidate = WeChatProcessLocator.candidate(
        forProcessID: processID, expectedUserID: invokingUser.uid, readingAs: invokingUser),
        candidate.userID == invokingUser.uid
      {
        selection.applicationPath = candidate.applicationPath
      }
      selection.running = true
    }
    if let target = selection.target {
      selection.processID = target.processID
      selection.applicationPath = target.applicationPath
      selection.running = true
    }
  } catch let error as ProcessSelectionError {
    selection.running = (try? locator.processIDs().isEmpty == false) ?? false
    let hint: String?
    switch error {
    case .noProcessForDatabaseRoot:
      hint = "start the WeChat copy that owns this data and log in"
    case .unclassifiedProcesses:
      hint = "choose the WeChat process with --pid <n>; \(listProcessesHint)"
    case .multipleProcessesForDatabaseRoot, .multipleProcessesForTarget:
      hint = "quit the extra instances, or choose one with --pid"
    case .databaseRootNotInAppContainer:
      hint = "choose the WeChat process that uses this data with --pid <n>; \(listProcessesHint)"
    case .processDoesNotOwnDatabaseRoot:
      hint = "choose the process of the WeChat copy that owns this data, or a matching --db-root"
    case .processNotFound, .processOwnedByAnotherUser, .processNotWeChat,
      .processIdentityUnavailable:
      hint = listProcessesHint
    }
    selection.failure = (error.description, hint)
  } catch ProcessLocatorError.multipleProcesses {
    selection.running = true
    selection.failure = (
      ProcessLocatorError.multipleProcesses.description,
      "quit the other copies, or choose one with --pid"
    )
  } catch {
    selection.failure = (String(describing: error), nil)
  }
  return selection
}

private func runPreflight(
  dbRootOption: URL?,
  rootOrigin: String = "from --db-root",
  processIDOption: pid_t? = nil,
  includePaths: Bool,
  targetError: String? = nil
) -> PreflightResult {
  var checks: [PreflightCheck] = []
  var remediation: String?
  let homeDirectory = invokingUserHomeDirectory()

  let lldbAvailable = LLDBPassphraseCapture().lldbAvailable
  checks.append(
    PreflightCheck(
      ok: lldbAvailable,
      label: lldbAvailable ? "lldb is available" : "lldb is not available",
      hint: lldbAvailable ? nil : "install the Xcode Command Line Tools: xcode-select --install"
    ))

  let runningAsRoot = geteuid() == 0
  checks.append(
    PreflightCheck(
      ok: runningAsRoot,
      label: runningAsRoot ? "running as root" : "not running as root",
      hint: runningAsRoot ? nil : "capture needs to attach to WeChat; re-run with sudo"
    ))

  // Resolve the database root once, as the invoking user (a root their own
  // queries can't read fails here): process selection, the inventory and the
  // capture all use this same root, or its error.
  let resolvedRoot = Result<URL, Error> {
    // A profile or config.toml problem fails the database check instead of the run.
    if let targetError { throw TargetResolutionFailure(description: targetError) }
    return try userDatabaseRoot(dbRootOption)
  }

  // Choose the process first: version and signing are read from the app
  // bundle of the selected process, not from a default install location.
  var databaseRootOwner: DatabaseRootOwner?
  var ownerLookupError: String?
  if let root = try? resolvedRoot.get() {
    do {
      databaseRootOwner = try invokingUser.perform {
        DatabaseRootOwner.resolve(databaseRoot: root, homeDirectory: homeDirectory)
      }
    } catch {
      ownerLookupError = String(describing: error)
    }
  }
  let selection = selectProcess(
    processIDOption: processIDOption, owner: databaseRootOwner,
    ownerLookupError: ownerLookupError)

  // The capture mechanism breakpoints a system CommonCrypto symbol, so it is
  // build-agnostic: client version and signing state are reported for the
  // owner's information only and never gate the capture.
  var clientInstalled = false
  var clientReSigned = false
  var clientMarketingVersion: String?
  var clientBuildVersion: String?
  let buildInspector = WeChatClientBuildInspector(homeDirectory: homeDirectory)
  if let selectedApplication = selection.applicationPath.map({ URL(fileURLWithPath: $0) }) {
    // The selected process's app may sit in a folder the user controls, so
    // inspect it with the invoking user's permissions.
    clientInstalled = true
    if let build = try? invokingUser.perform({
      try buildInspector.inspect(application: selectedApplication)
    }) {
      clientMarketingVersion = build.marketingVersion
      clientBuildVersion = build.buildVersion
      clientReSigned = !build.hardenedRuntime
    }
  } else if let application = buildInspector.defaultApplicationURL() {
    clientInstalled = true
    if let build = try? buildInspector.inspect(application: application) {
      clientMarketingVersion = build.marketingVersion
      clientBuildVersion = build.buildVersion
      clientReSigned = !build.hardenedRuntime
    }
  }
  checks.append(
    PreflightCheck(
      ok: clientInstalled,
      label: clientInstalled ? "WeChat is installed" : "no WeChat installation was found",
      hint: clientInstalled ? nil : "install WeChat for macOS first"
    ))

  let wechatProcessRunning = selection.running
  var processHardeningStatus: ProcessHardeningStatus? = nil
  var processLabel = "WeChat is not running"
  var processHint: String? = "start WeChat and log in before capturing"
  if let failure = selection.failure {
    processLabel = failure.label
    processHint = failure.hint
  } else if let processID = selection.processID {
    let status = RuntimeHardeningProbe.status(forProcessID: processID)
    processHardeningStatus = status
    let version = [clientMarketingVersion, clientBuildVersion.map { "build \($0)" }]
      .compactMap { $0 }.joined(separator: ", ")
    let app = selection.target.map { ", \($0.bundleIdentifier)" } ?? ""
    let suffix = " (pid \(processID)\(app)\(version.isEmpty ? "" : ", \(version)"))"
    switch status {
    case .hardened:
      let command = resignRemediation(
        applicationPath: selection.applicationPath, includePaths: includePaths)
      processLabel = "WeChat is running\(suffix), but Hardened Runtime is still active"
      processHint = "re-sign the app, then restart WeChat:\n    \(command)"
      remediation = command
    case .notHardened:
      processLabel = "WeChat is running\(suffix), Hardened Runtime removed"
      processHint = nil
    case .unknown:
      processLabel = "WeChat is running\(suffix), hardening status unknown (proceeding)"
      processHint = nil
    }
  }
  checks.append(
    PreflightCheck(
      ok: selection.failure == nil && processHardeningStatus != nil
        && processHardeningStatus != .hardened,
      label: processLabel,
      hint: processHint
    ))

  var databaseRootReference: PathReference? = nil
  var databaseCount: Int? = nil
  var distinctSaltCount: Int? = nil
  var databaseLabel = "no WeChat database root was discovered"
  var databaseHint: String? = "log into WeChat once so it creates its databases"
  var databasesOK = false
  do {
    let root = try resolvedRoot.get()
    databaseRootReference = PathPrivacy(includePaths: includePaths).reference(for: root)
    let inventory = try invokingUser.perform { try DatabaseSaltInventory(root: root) }
    databaseCount = inventory.entries.count
    distinctSaltCount = inventory.distinctSalts.count
    let origin = dbRootOption == nil ? "auto-discovered" : rootOrigin
    if inventory.entries.isEmpty {
      databaseLabel = "database root found (\(origin)) but it contains no databases"
    } else {
      databasesOK = true
      databaseLabel =
        "database root \(origin) — \(inventory.entries.count) databases, "
        + "\(inventory.distinctSalts.count) distinct salts"
      databaseHint = nil
    }
  } catch {
    databaseLabel = String(describing: error)
    databaseHint = nil
  }
  checks.append(PreflightCheck(ok: databasesOK, label: databaseLabel, hint: databaseHint))

  let blockers = checks.filter { !$0.ok }.map { check in
    check.hint.map { "\(check.label) (\($0))" } ?? check.label
  }
  let report = PreflightReport(
    formatVersion: 1,
    ready: blockers.isEmpty,
    blockers: blockers,
    remediation: remediation,
    lldbAvailable: lldbAvailable,
    runningAsRoot: runningAsRoot,
    wechatProcessRunning: wechatProcessRunning,
    processHardeningStatus: processHardeningStatus,
    clientInstalled: clientInstalled,
    clientReSigned: clientReSigned,
    clientMarketingVersion: clientMarketingVersion,
    clientBuildVersion: clientBuildVersion,
    selectedProcessID: selection.target?.processID,
    selectedApplicationBundleIdentifier: selection.target?.bundleIdentifier,
    databaseRootApplicationBundleIdentifier: {
      if case .application(let bundleIdentifier) = databaseRootOwner { return bundleIdentifier }
      return nil
    }(),
    databaseRoot: databaseRootReference,
    databaseRootAutoDiscovered: dbRootOption == nil,
    databaseCount: databaseCount,
    distinctSaltCount: distinctSaltCount
  )
  return PreflightResult(
    report: report,
    checks: checks,
    target: selection.target,
    databaseRoot: try? resolvedRoot.get()
  )
}

private func rootOrigin(_ arguments: Arguments, _ target: AcquireTarget) -> String {
  if arguments.dbRoot == nil, let origin = target.sourceRootOrigin { return origin }
  return "from --db-root"
}

do {
  let arguments = try Arguments(Array(CommandLine.arguments.dropFirst()))
  switch arguments.command {
  case .help:
    print(usage)
  case .preflight:
    var target = AcquireTarget.none
    var targetError: String?
    do {
      // Readiness means capture would accept this target and destination.
      target = try resolveTarget(arguments, needsOutput: true)
      try SecretOutputWriter.checkDestination(captureOutput(arguments, target))
    } catch {
      targetError = String(describing: error)
    }
    let result = runPreflight(
      dbRootOption: arguments.dbRoot ?? target.sourceRoot,
      rootOrigin: rootOrigin(arguments, target),
      processIDOption: arguments.processID,
      includePaths: arguments.includePaths,
      targetError: targetError
    )
    if arguments.json {
      try printJSON(result.report)
    } else {
      printPreflight(result, captureCommand: captureCommand(arguments))
    }
    if !result.report.ready { exit(1) }
  case .capture:
    let target = try resolveTarget(arguments, needsOutput: true)
    let dbRoot = arguments.dbRoot ?? target.sourceRoot
    let output = captureOutput(arguments, target)
    try SecretOutputWriter.checkDestination(output)
    let preflight = runPreflight(
      dbRootOption: dbRoot,
      rootOrigin: rootOrigin(arguments, target),
      processIDOption: arguments.processID,
      includePaths: arguments.includePaths
    )
    guard preflight.report.ready, let processTarget = preflight.target,
      let root = preflight.databaseRoot
    else {
      if arguments.json {
        try printJSON(preflight.report)
      } else {
        printPreflight(preflight)
      }
      exit(1)
    }
    // Only the debugger attach needs root; read the databases as the user.
    // `root` is the user-checked root preflight resolved and selected for.
    let inventory = try invokingUser.perform { try DatabaseSaltInventory(root: root) }
    printNote(
      "why this step: the passphrase only crosses the system key-derivation "
        + "function while WeChat opens its databases at login, so a fresh "
        + "login is required"
    )
    printNote("in WeChat, log out of the account (not just quit the app), then log back in")
    printNote("waiting up to \(arguments.timeoutSeconds) seconds for the login")
    printNote(
      "a captured value that does not open your databases is ignored; capture keeps waiting"
    )
    var capture = LLDBPassphraseCapture()
    capture.target = processTarget
    capture.processLocator = processLocator()
    capture.verbose = arguments.verbose
    capture.outputHandler = { message in
      FileHandle.standardError.write(Data("note: \(message)\n".utf8))
    }
    let started = Date()
    // Matches the capture's own deadline. Verification stops between salts
    // once it passes, so a candidate that arrives late cannot keep lldb
    // attached for a full pass over every salt.
    let deadline = started.addingTimeInterval(TimeInterval(arguments.timeoutSeconds))
    var verifiedSaltCount = 0
    let passphrase: PassphraseSecret
    do {
      passphrase = try capture.capture(timeoutSeconds: arguments.timeoutSeconds) { secret in
        verifiedSaltCount = try inventory.verifiedSaltCount(for: secret) {
          Date() >= deadline
        }
        return verifiedSaltCount > 0
      }
    } catch CaptureError.noAcceptableCandidate {
      throw CLIError.verificationFailed(
        CaptureError.verificationFailureDiagnostic(
          distinctSaltCount: inventory.distinctSalts.count,
          databaseCount: inventory.entries.count
        )
      )
    }
    let captureDurationSeconds = Date().timeIntervalSince(started)
    // `capture` returns only a candidate the closure accepted.
    assert(verifiedSaltCount > 0)
    let written = try SecretOutputWriter().write(
      passphrase,
      to: output,
      overwrite: arguments.overwrite,
      as: invokingUser
    )
    if !written.parentIsPrivate {
      printNote(
        "the folder holding the key is not owner-only; greenbubbles reads a key only "
          + "from a folder you own with mode 700")
    }
    if arguments.json {
      try printJSON(
        AcquisitionReport(
          capturedAt: Date(),
          captureDurationSeconds: captureDurationSeconds,
          databaseCount: inventory.entries.count,
          distinctSaltCount: inventory.distinctSalts.count,
          verifiedSaltCount: verifiedSaltCount,
          clientMarketingVersion: preflight.report.clientMarketingVersion,
          clientBuildVersion: preflight.report.clientBuildVersion,
          clientReSigned: preflight.report.clientReSigned,
          databaseRoot: preflight.report.databaseRoot,
          outputWritten: true
        ))
    } else {
      let seconds = Int(captureDurationSeconds.rounded())
      print("")
      print(
        "\(statusMark(true)) passphrase captured in \(seconds)s and verified against "
          + "\(verifiedSaltCount)/\(inventory.distinctSalts.count) databases"
      )
      print("  written to \(output.path) (mode 0600, owner-only)")
      print("")
      // An explicit --output is not the file the profile reads.
      if let name = target.profileName, arguments.output == nil {
        print("Use it with:  greenbubbles <args> --profile \(name)")
      } else {
        print(
          "Use it with:  cat \(shellQuoted(output.path)) | greenbubbles <args> --passphrase-stdin")
      }
    }
  case .verify:
    guard arguments.passphraseStdin else {
      throw CLIError.missingRequiredOption("--passphrase-stdin")
    }
    let passphrase = try PassphraseSecret.readFromStandardInput()
    let target = try resolveTarget(arguments, needsOutput: false)
    let root = try userDatabaseRoot(arguments.dbRoot ?? target.sourceRoot)
    let inventory = try invokingUser.perform { try DatabaseSaltInventory(root: root) }
    let verifiedSaltCount = try inventory.verifiedSaltCount(for: passphrase)
    let distinctSaltCount = inventory.distinctSalts.count
    let allVerified = distinctSaltCount > 0 && verifiedSaltCount == distinctSaltCount
    if arguments.json {
      try printJSON(
        VerificationReport(
          formatVersion: 1,
          databaseCount: inventory.entries.count,
          distinctSaltCount: distinctSaltCount,
          verifiedSaltCount: verifiedSaltCount,
          allVerified: allVerified
        ))
    } else {
      if allVerified {
        print(
          "\(statusMark(true)) passphrase verified against all "
            + "\(verifiedSaltCount) databases (\(distinctSaltCount) distinct salts)"
        )
      } else {
        print(
          "\(statusMark(false)) passphrase verified against only "
            + "\(verifiedSaltCount)/\(distinctSaltCount) distinct salts"
        )
      }
    }
    if !allVerified { exit(1) }
  }
} catch {
  printError(String(describing: error))
  printError("Run greenbubbles-acquire help for usage.")
  exit(2)
}
