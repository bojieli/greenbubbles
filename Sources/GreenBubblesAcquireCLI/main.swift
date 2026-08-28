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
  case noDatabaseRootDiscovered
  case verificationFailed(String)

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
    case .noDatabaseRootDiscovered:
      return "No WeChat database root was discovered; pass --db-root to select one"
    case .verificationFailed(let reason):
      return "Passphrase verification failed: \(reason)"
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
    --output <path>        Where capture writes the passphrase (default:
                           ~/.greenbubbles-acquire/passphrase.txt, mode 0600)
    --timeout-seconds <n>  Capture window in seconds (default: 300)
    --db-root <path>       Override the auto-discovered active db_storage root
    --overwrite            Replace an existing passphrase output file
    --passphrase-stdin     Read the 32-byte passphrase from standard input
    --include-paths        Include sensitive filesystem paths in local output
    --json                 Emit machine-readable JSON instead of human-readable text
    --verbose              Mirror lldb output during capture (for diagnosing stalls)
    -h, --help             Show this help

  Capture requires root, lldb, a running WeChat, and an owner re-signed client
  without Hardened Runtime. When re-signing is required, preflight prints the
  exact command for the owner to run manually; this tool never automates it.
  The capture mechanism breakpoints a system library function, so it works
  with any WeChat build; the active account's database root is discovered
  automatically.
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

// Under sudo the effective home is /var/root, but the WeChat data lives in the
// invoking user's home; SUDO_USER tells us who that is.
private func invokingUserHomeDirectory() -> URL {
  if geteuid() == 0, let sudoUser = ProcessInfo.processInfo.environment["SUDO_USER"],
    let entry = getpwnam(sudoUser), let directory = entry.pointee.pw_dir
  {
    return URL(fileURLWithPath: String(cString: directory)).standardizedFileURL
  }
  return FileManager.default.homeDirectoryForCurrentUser
}

private func resolveDatabaseRoot(_ option: URL?) throws -> URL {
  if let option {
    return option.standardizedFileURL
  }
  let roots = WeChatAccountDiscovery(homeDirectory: invokingUserHomeDirectory())
    .databaseRoots()
  if roots.count == 1 { return roots[0] }
  if roots.isEmpty { throw CLIError.noDatabaseRootDiscovered }
  // Several accounts are present: the running client is logged into exactly
  // one, and that account's databases are the ones being written to.
  guard
    let active = roots.max(by: { latestDatabaseModification($0) < latestDatabaseModification($1) })
  else {
    throw CLIError.noDatabaseRootDiscovered
  }
  return active
}

private func latestDatabaseModification(_ root: URL) -> Date {
  var latest = Date.distantPast
  guard
    let enumerator = FileManager.default.enumerator(
      at: root,
      includingPropertiesForKeys: [.contentModificationDateKey, .isRegularFileKey],
      options: [.skipsHiddenFiles]
    )
  else { return latest }
  for case let url as URL in enumerator {
    guard url.pathExtension == "db", !url.lastPathComponent.hasSuffix("-wal"),
      !url.lastPathComponent.hasSuffix("-shm")
    else { continue }
    guard
      let values = try? url.resourceValues(forKeys: [
        .contentModificationDateKey, .isRegularFileKey,
      ]),
      values.isRegularFile == true,
      let modified = values.contentModificationDate
    else { continue }
    if modified > latest { latest = modified }
  }
  return latest
}

private func runPreflight(dbRootOption: URL?, includePaths: Bool) -> PreflightResult {
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
  if let processIDs = try? WeChatProcessLocator().processIDs(), let first = processIDs.first {
    wechatProcessRunning = true
    let status = RuntimeHardeningProbe.status(forProcessID: first)
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
  checks.append(
    PreflightCheck(
      ok: wechatProcessRunning && processHardeningStatus != .hardened,
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
    let origin = dbRootOption == nil ? "auto-discovered" : "from --db-root"
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
    let result = runPreflight(
      dbRootOption: arguments.dbRoot,
      includePaths: arguments.includePaths
    )
    if arguments.json {
      try printJSON(result.report)
    } else {
      printPreflight(result)
    }
    if !result.report.ready { exit(1) }
  case .capture:
    let output =
      arguments.output
      ?? invokingUserHomeDirectory()
      .appending(path: ".greenbubbles-acquire", directoryHint: .isDirectory)
      .appending(path: "passphrase.txt")
    let preflight = runPreflight(
      dbRootOption: arguments.dbRoot,
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
    let root = try resolveDatabaseRoot(arguments.dbRoot)
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
        "the captured bytes verified against no database; nothing was written"
      )
    }
    try SecretOutputWriter().write(passphrase, to: output, overwrite: arguments.overwrite)
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
      print("Use it with:  cat \(output.path) | greenbubbles-restore <args> --passphrase-stdin")
    }
  case .verify:
    guard arguments.passphraseStdin else {
      throw CLIError.missingRequiredOption("--passphrase-stdin")
    }
    let passphrase = try PassphraseSecret.readFromStandardInput()
    let root = try resolveDatabaseRoot(arguments.dbRoot)
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
