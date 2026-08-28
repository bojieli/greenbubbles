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
  case multipleDatabaseRoots
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
    case .multipleDatabaseRoots:
      return "Multiple WeChat accounts were discovered; pass --db-root to select one"
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
  var ownerAuthorized = false
  var timeoutSeconds = LLDBPassphraseCapture.defaultTimeoutSeconds
  var dbRoot: URL?
  var overwrite = false
  var passphraseStdin = false
  var includePaths = false

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
        ownerAuthorized = true
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
    preflight              Report capture readiness as JSON; exits non-zero when blocked
    capture                Attach lldb to WeChat and capture the database passphrase
    verify                 Re-validate a stored passphrase read from standard input
    help                   Show this help

  Options:
    --output <path>        Where capture writes the passphrase (mode 0600, exclusive)
    --owner-authorized     Confirm the owner authorizes this capture; required by capture
    --timeout-seconds <n>  Capture window in seconds (default: 300)
    --db-root <path>       Use this db_storage root instead of the discovered one
    --overwrite            Replace an existing passphrase output file
    --passphrase-stdin     Read the 32-byte passphrase from standard input
    --include-paths        Include sensitive filesystem paths in local output
    -h, --help             Show this help

  Capture requires root, lldb, a running WeChat, and an owner re-signed client
  without Hardened Runtime. When re-signing is required, preflight reports the
  exact command for the owner to run manually; this tool never automates it.
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
  let clientFingerprintMatchesPin: Bool?
  let databaseRoot: PathReference?
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

private func resolveDatabaseRoot(_ option: URL?) throws -> URL {
  if let option {
    return option.standardizedFileURL
  }
  let roots = WeChatAccountDiscovery().databaseRoots()
  if roots.count == 1 { return roots[0] }
  if roots.isEmpty { throw CLIError.noDatabaseRootDiscovered }
  throw CLIError.multipleDatabaseRoots
}

private func runPreflight(dbRootOption: URL?, includePaths: Bool) -> PreflightReport {
  var blockers: [String] = []
  var remediation: String?

  let lldbAvailable = LLDBPassphraseCapture().lldbAvailable
  if !lldbAvailable {
    blockers.append("lldb is not available; install the Xcode Command Line Tools")
  }

  let runningAsRoot = geteuid() == 0
  if !runningAsRoot {
    blockers.append("capture requires root privileges for lldb attach; re-run with sudo")
  }

  var clientInstalled = false
  var clientReSigned = false
  var clientMarketingVersion: String?
  var clientBuildVersion: String?
  var clientFingerprintMatchesPin: Bool? = nil
  let buildInspector = WeChatClientBuildInspector()
  if let application = buildInspector.defaultApplicationURL() {
    clientInstalled = true
    do {
      let build = try buildInspector.inspect(application: application)
      clientMarketingVersion = build.marketingVersion
      clientBuildVersion = build.buildVersion
      let pinned = WeChatIntegrationSurfaceInspector.pinnedWeChat4113
      if build.hardenedRuntime, build.signatureValid {
        // Still fully signed: the complete fingerprint must match the pin.
        clientFingerprintMatchesPin = build == pinned
        if build == pinned {
          blockers.append(
            "WeChat still has Hardened Runtime; lldb attach will fail until the owner re-signs the app and restarts WeChat"
          )
          remediation = resignRemediation
        } else {
          blockers.append(
            "the installed WeChat build does not match the pinned 4.1.13 fingerprint; capture is disabled for unpinned builds"
          )
        }
      } else if build.hardenedRuntime {
        blockers.append(
          "WeChat still has Hardened Runtime; lldb attach will fail until the owner re-signs the app and restarts WeChat"
        )
        remediation = resignRemediation
      } else {
        // Owner re-signed: ad-hoc signing legitimately changes the executable
        // and code-directory hashes, so only the plist metadata is compared.
        clientReSigned = true
        let plistMatches =
          build.bundleIdentifier == pinned.bundleIdentifier
          && build.marketingVersion == pinned.marketingVersion
          && build.buildVersion == pinned.buildVersion
          && build.teamIdentifier == pinned.teamIdentifier
        if !plistMatches {
          blockers.append(
            "the re-signed WeChat version metadata does not match the pinned 4.1.13 profile; capture is disabled for unpinned builds"
          )
        }
      }
    } catch {
      blockers.append("the WeChat build could not be inspected: \(error)")
    }
  } else {
    blockers.append("no WeChat installation was found")
  }

  var wechatProcessRunning = false
  var processHardeningStatus: ProcessHardeningStatus? = nil
  if let processIDs = try? WeChatProcessLocator().processIDs(), let first = processIDs.first {
    wechatProcessRunning = true
    let status = RuntimeHardeningProbe.status(forProcessID: first)
    processHardeningStatus = status
    if status == .hardened {
      blockers.append(
        "the running WeChat process still has Hardened Runtime; re-sign the app and restart WeChat"
      )
      remediation = resignRemediation
    }
  } else {
    blockers.append("WeChat is not running; start it and log in before capturing")
  }

  var databaseRootReference: PathReference? = nil
  var databaseCount: Int? = nil
  var distinctSaltCount: Int? = nil
  do {
    let root = try resolveDatabaseRoot(dbRootOption)
    databaseRootReference = PathPrivacy(includePaths: includePaths).reference(for: root)
    let inventory = try DatabaseSaltInventory(root: root)
    databaseCount = inventory.entries.count
    distinctSaltCount = inventory.distinctSalts.count
    if inventory.entries.isEmpty {
      blockers.append("no WeChat databases were found under the database root")
    }
  } catch {
    blockers.append(String(describing: error))
  }

  return PreflightReport(
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
    clientFingerprintMatchesPin: clientFingerprintMatchesPin,
    databaseRoot: databaseRootReference,
    databaseCount: databaseCount,
    distinctSaltCount: distinctSaltCount
  )
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
    let report = runPreflight(
      dbRootOption: arguments.dbRoot,
      includePaths: arguments.includePaths
    )
    try printJSON(report)
    if !report.ready { exit(1) }
  case .capture:
    guard arguments.ownerAuthorized else {
      throw CLIError.missingRequiredOption("--owner-authorized")
    }
    guard let output = arguments.output else {
      throw CLIError.missingRequiredOption("--output")
    }
    let preflight = runPreflight(
      dbRootOption: arguments.dbRoot,
      includePaths: arguments.includePaths
    )
    guard preflight.ready else {
      try printJSON(preflight)
      exit(1)
    }
    let root = try resolveDatabaseRoot(arguments.dbRoot)
    let inventory = try DatabaseSaltInventory(root: root)
    guard let processID = try WeChatProcessLocator().processIDs().first else {
      throw CLIError.verificationFailed("WeChat stopped running after preflight")
    }
    printNote("capture armed on WeChat process \(processID)")
    printNote("in WeChat, log out of the account (not just quit the app), then log back in")
    printNote("waiting up to \(arguments.timeoutSeconds) seconds for the login")
    let started = Date()
    let passphrase = try LLDBPassphraseCapture().capture(
      processID: processID,
      timeoutSeconds: arguments.timeoutSeconds
    )
    let captureDurationSeconds = Date().timeIntervalSince(started)
    let verifiedSaltCount = try verifyPassphrase(passphrase, inventory: inventory)
    guard verifiedSaltCount > 0 else {
      throw CLIError.verificationFailed(
        "the captured bytes verified against no database; nothing was written"
      )
    }
    try SecretOutputWriter().write(passphrase, to: output, overwrite: arguments.overwrite)
    try printJSON(
      AcquisitionReport(
        capturedAt: Date(),
        captureDurationSeconds: captureDurationSeconds,
        databaseCount: inventory.entries.count,
        distinctSaltCount: inventory.distinctSalts.count,
        verifiedSaltCount: verifiedSaltCount,
        clientMarketingVersion: preflight.clientMarketingVersion,
        clientBuildVersion: preflight.clientBuildVersion,
        clientReSigned: preflight.clientReSigned,
        databaseRoot: preflight.databaseRoot,
        outputWritten: true
      ))
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
    try printJSON(
      VerificationReport(
        formatVersion: 1,
        databaseCount: inventory.entries.count,
        distinctSaltCount: distinctSaltCount,
        verifiedSaltCount: verifiedSaltCount,
        allVerified: allVerified
      ))
    if !allVerified { exit(1) }
  }
} catch {
  printError(String(describing: error))
  printError("Run greenbubbles-acquire help for usage.")
  exit(2)
}
