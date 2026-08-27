import Foundation
import GreenBubblesCore

enum CLIError: Error, CustomStringConvertible {
  case invalidOption(String)
  case missingValue(String)
  case invalidInteger(option: String, value: String)

  var description: String {
    switch self {
    case .invalidOption(let option):
      return "Unknown option: \(option)"
    case .missingValue(let option):
      return "Missing value for \(option)"
    case .invalidInteger(let option, let value):
      return "Expected a positive integer for \(option), got: \(value)"
    }
  }
}

struct Arguments {
  enum Command: String {
    case accounts
    case discover
    case inventory
    case notificationHints = "notification-hints"
    case snapshot
    case help
  }

  var command: Command = .discover
  var includePaths = false
  var roots: [URL] = []
  var accountID: String?
  var snapshotBase: URL?
  var maxDepth = 10
  var maxArtifacts = 10_000

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
      case "--include-paths":
        includePaths = true
      case "--root":
        index += 1
        guard index < arguments.count else { throw CLIError.missingValue(option) }
        roots.append(URL(fileURLWithPath: arguments[index]))
      case "--account":
        index += 1
        guard index < arguments.count else { throw CLIError.missingValue(option) }
        accountID = arguments[index]
      case "--snapshot-base":
        index += 1
        guard index < arguments.count else { throw CLIError.missingValue(option) }
        snapshotBase = URL(fileURLWithPath: arguments[index])
      case "--max-depth", "--max-artifacts":
        index += 1
        guard index < arguments.count else { throw CLIError.missingValue(option) }
        let value = arguments[index]
        guard let number = Int(value), number > 0 else {
          throw CLIError.invalidInteger(option: option, value: value)
        }
        if option == "--max-depth" { maxDepth = number }
        if option == "--max-artifacts" { maxArtifacts = number }
      case "-h", "--help":
        command = .help
      default:
        throw CLIError.invalidOption(option)
      }
      index += 1
    }
  }
}

private let usage = """
  Usage: greenbubbles <command> [options]

  Commands:
    accounts             Find account-scoped database and attachment roots
    discover             Find known WeChat installations and data roots (default)
    inventory            Classify candidate artifacts without opening their contents
    notification-hints   Assess optional notification wake-up feasibility without prompting
    snapshot             Verify a consistent, temporary read-only database snapshot
    help                 Show this help

  Options:
    --root <path>        Inventory a supplied root instead of discovered roots; repeatable
    --account <id>       Scope snapshot discovery to one opaque account ID
    --snapshot-base <p>  Preserve a snapshot under this owner-only base directory
    --include-paths      Include sensitive filesystem paths in local output
    --max-depth <n>      Limit recursive traversal depth (default: 10)
    --max-artifacts <n>  Stop after this many classified artifacts (default: 10000)
    -h, --help           Show this help
  """

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

do {
  let arguments = try Arguments(Array(CommandLine.arguments.dropFirst()))
  switch arguments.command {
  case .help:
    print(usage)
  case .accounts:
    try printJSON(WeChatAccountDiscovery(includePaths: arguments.includePaths).discover())
  case .discover:
    let discovery = WeChatDiscovery(includePaths: arguments.includePaths)
    try printJSON(discovery.discover())
  case .inventory:
    let discovery = WeChatDiscovery(includePaths: arguments.includePaths)
    let roots: [(url: URL, kind: DataRootKind)]
    if arguments.roots.isEmpty {
      roots = discovery.accessibleDataRoots()
    } else {
      roots = arguments.roots.map { ($0, .supplied) }
    }

    let inventory = ArtifactInventory(
      options: InventoryOptions(
        maxDepth: arguments.maxDepth,
        maxArtifacts: arguments.maxArtifacts,
        includePaths: arguments.includePaths
      ))
    try printJSON(inventory.inventory(roots: roots))
  case .notificationHints:
    try printJSON(NotificationHintAssessor().assess())
  case .snapshot:
    let roots: [URL]
    if arguments.roots.isEmpty {
      roots = WeChatAccountDiscovery(includePaths: arguments.includePaths)
        .databaseRoots(accountID: arguments.accountID)
    } else {
      roots = arguments.roots
    }
    let sets = DatabaseSetPlanner().findDatabaseSets(in: roots, maxDepth: arguments.maxDepth)
    let clientBuild = try WeChatClientBuildInspector().inspectDefaultInstallation()
    let lease = try ReadOnlySnapshotter(
      baseDirectory: arguments.snapshotBase
        ?? FileManager.default.temporaryDirectory
        .appending(path: "greenbubbles-snapshots", directoryHint: .isDirectory),
      includeSourcePaths: arguments.includePaths,
      clientBuild: clientBuild
    ).createSnapshot(of: sets, cleanUpOnDeinit: arguments.snapshotBase == nil)
    try printJSON(
      SnapshotCommandReport(
        databaseSetCount: sets.count,
        manifest: lease.manifest,
        automaticallyCleanedUp: arguments.snapshotBase == nil,
        snapshotDirectory: arguments.snapshotBase == nil ? nil : lease.directory.path
      ))
  }
} catch {
  printError(String(describing: error))
  printError("Run greenbubbles help for usage.")
  exit(2)
}

private struct SnapshotCommandReport: Encodable {
  let databaseSetCount: Int
  let manifest: SnapshotManifest
  let automaticallyCleanedUp: Bool
  let snapshotDirectory: String?
}
