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
    case .rootConflictsWithTarget(let origin):
      return
        "--db-root differs from the root \(origin), whose key file capture would write; "
        + "pass --output as well, or select that account's profile with --profile"
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
private func printPreflight(_ result: PreflightResult) {
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
    print("Next: sudo greenbubbles-acquire capture")
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
  // Don't write one account's key into the file another account's queries read.
  if needsOutput, arguments.output == nil, target.credentialFile != nil,
    let dbRoot = arguments.dbRoot, let targetRoot = target.sourceRoot,
    dbRoot.standardizedFileURL.path != targetRoot.standardizedFileURL.path
  {
    throw CLIError.rootConflictsWithTarget(target.sourceRootOrigin ?? "configured")
  }
  return target
}

private func resolveDatabaseRoot(_ option: URL?) throws -> URL {
  if let option {
    return option.standardizedFileURL
  }
  return try WeChatAccountDiscovery(homeDirectory: invokingUserHomeDirectory())
    .singleDatabaseRoot()
}

private func runPreflight(
  dbRootOption: URL?,
  rootOrigin: String = "from --db-root",
  includePaths: Bool
) -> PreflightResult {
  var checks: [PreflightCheck] = []
  var remediation: String?

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

  // The capture mechanism breakpoints a system CommonCrypto symbol, so it is
  // build-agnostic: client version and signing state are reported for the
  // owner's information only and never gate the capture.
  var clientInstalled = false
  var clientReSigned = false
  var clientMarketingVersion: String?
  var clientBuildVersion: String?
  let buildInspector = WeChatClientBuildInspector(homeDirectory: invokingUserHomeDirectory())
  if let application = buildInspector.defaultApplicationURL() {
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

  var wechatProcessRunning = false
  var processHardeningStatus: ProcessHardeningStatus? = nil
  var processLabel = "WeChat is not running"
  var processHint: String? = "start WeChat and log in before capturing"
  do {
    if let processID = try WeChatProcessLocator().uniqueProcessID() {
      wechatProcessRunning = true
      let status = RuntimeHardeningProbe.status(forProcessID: processID)
      processHardeningStatus = status
      let version = [clientMarketingVersion, clientBuildVersion.map { "build \($0)" }]
        .compactMap { $0 }.joined(separator: ", ")
      let suffix = version.isEmpty ? "" : " (\(version))"
      switch status {
      case .hardened:
        processLabel = "WeChat is running\(suffix), but Hardened Runtime is still active"
        processHint = "re-sign the app, then restart WeChat:\n    \(resignRemediation)"
        remediation = resignRemediation
      case .notHardened:
        processLabel = "WeChat is running\(suffix), Hardened Runtime removed"
        processHint = nil
      case .unknown:
        processLabel = "WeChat is running\(suffix), hardening status unknown (proceeding)"
        processHint = nil
      }
    }
  } catch ProcessLocatorError.multipleProcesses {
    wechatProcessRunning = true
    processLabel = ProcessLocatorError.multipleProcesses.description
    processHint = "quit the other copies before continuing"
  } catch {
    processLabel = String(describing: error)
    processHint = nil
  }
  checks.append(
    PreflightCheck(
      ok: processHardeningStatus != nil && processHardeningStatus != .hardened,
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
    let root = try resolveDatabaseRoot(dbRootOption)
    databaseRootReference = PathPrivacy(includePaths: includePaths).reference(for: root)
    let inventory = try DatabaseSaltInventory(root: root)
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
    databaseRoot: databaseRootReference,
    databaseRootAutoDiscovered: dbRootOption == nil,
    databaseCount: databaseCount,
    distinctSaltCount: distinctSaltCount
  )
  return PreflightResult(report: report, checks: checks)
}

private func rootOrigin(_ arguments: Arguments, _ target: AcquireTarget) -> String {
  if arguments.dbRoot == nil, let origin = target.sourceRootOrigin { return origin }
  return "from --db-root"
}

private func verifyPassphrase(
  _ passphrase: PassphraseSecret,
  inventory: DatabaseSaltInventory
) throws -> Int {
  var verifiedSaltCount = 0
  for sample in inventory.saltVerificationSamples {
    var encryptionKey = try SQLCipherKeyVerifier.deriveEncryptionKey(
      passphrase: passphrase,
      salt: sample.salt
    )
    let verified = try SQLCipherKeyVerifier.verify(
      encryptionKey: encryptionKey,
      page1: sample.page1
    )
    PassphraseSecret.zeroize(&encryptionKey)
    if verified { verifiedSaltCount += 1 }
  }
  return verifiedSaltCount
}

do {
  let arguments = try Arguments(Array(CommandLine.arguments.dropFirst()))
  switch arguments.command {
  case .help:
    print(usage)
  case .preflight:
    let target = try resolveTarget(arguments, needsOutput: false)
    let result = runPreflight(
      dbRootOption: arguments.dbRoot ?? target.sourceRoot,
      rootOrigin: rootOrigin(arguments, target),
      includePaths: arguments.includePaths
    )
    if arguments.json {
      try printJSON(result.report)
    } else {
      printPreflight(result)
    }
    if !result.report.ready { exit(1) }
  case .capture:
    let target = try resolveTarget(arguments, needsOutput: true)
    let dbRoot = arguments.dbRoot ?? target.sourceRoot
    let output =
      arguments.output
      ?? target.credentialFile
      ?? invokingUserHomeDirectory()
      .appending(path: ".greenbubbles-acquire", directoryHint: .isDirectory)
      .appending(path: "passphrase.txt")
    let preflight = runPreflight(
      dbRootOption: dbRoot,
      rootOrigin: rootOrigin(arguments, target),
      includePaths: arguments.includePaths
    )
    guard preflight.report.ready else {
      if arguments.json {
        try printJSON(preflight.report)
      } else {
        printPreflight(preflight)
      }
      exit(1)
    }
    let root = try resolveDatabaseRoot(dbRoot)
    let inventory = try DatabaseSaltInventory(root: root)
    printNote(
      "why this step: the passphrase only crosses the system key-derivation "
        + "function while WeChat opens its databases at login, so a fresh "
        + "login is required"
    )
    printNote("in WeChat, log out of the account (not just quit the app), then log back in")
    printNote("waiting up to \(arguments.timeoutSeconds) seconds for the login")
    var capture = LLDBPassphraseCapture()
    capture.verbose = arguments.verbose
    capture.outputHandler = { message in
      FileHandle.standardError.write(Data("note: \(message)\n".utf8))
    }
    let started = Date()
    let passphrase = try capture.capture(timeoutSeconds: arguments.timeoutSeconds)
    let captureDurationSeconds = Date().timeIntervalSince(started)
    let verifiedSaltCount = try verifyPassphrase(passphrase, inventory: inventory)
    guard verifiedSaltCount > 0 else {
      throw CLIError.verificationFailed(
        "no match among \(inventory.distinctSalts.count) distinct database salts "
          + "in \(inventory.entries.count) inventoried files. No output was created or replaced."
      )
    }
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
        print("Use it with:  cat \(output.path) | greenbubbles <args> --passphrase-stdin")
      }
    }
  case .verify:
    guard arguments.passphraseStdin else {
      throw CLIError.missingRequiredOption("--passphrase-stdin")
    }
    let passphrase = try PassphraseSecret.readFromStandardInput()
    let target = try resolveTarget(arguments, needsOutput: false)
    let root = try resolveDatabaseRoot(arguments.dbRoot ?? target.sourceRoot)
    let inventory = try DatabaseSaltInventory(root: root)
    let verifiedSaltCount = try verifyPassphrase(passphrase, inventory: inventory)
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
