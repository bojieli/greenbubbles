#!/usr/bin/env swift

import Darwin
import Foundation

private let reportSchema = "greenbubbles.live-database-check.v1"
private let querySchema = "greenbubbles.query.v1"
private let conversationPageLimit = 10
private let messagePageLimit = 5
private let continuationPageLimit = 3
private let searchPageLimit = 5
private let maximumConversationScan = 20
private let maximumSearchPages = 4
private let maximumPrivateFileBytes = 16 * 1024

private enum CheckError: Error, CustomStringConvertible {
  case usage(String)
  case prerequisite(String)
  case commandFailed(String, Int32)
  case invalidResponse(String)
  case failed(String)

  var description: String {
    switch self {
    case .usage(let detail):
      return detail
    case .prerequisite(let detail):
      return "prerequisite failed: \(detail)"
    case .commandFailed(let stage, let status):
      return "\(stage) failed with status \(status)"
    case .invalidResponse(let stage):
      return "\(stage) returned an invalid or mismatched response"
    case .failed(let detail):
      return "sanity check failed: \(detail)"
    }
  }
}

private struct Options {
  var keyFile = FileManager.default.homeDirectoryForCurrentUser
    .appending(path: ".greenbubbles-acquire/passphrase.txt")
  var searchQueryFile: URL?
  var skipBuild = false
  var showHelp = false

  static func parse(_ arguments: [String]) throws -> Options {
    var options = Options()
    var index = 0
    while index < arguments.count {
      let argument = arguments[index]
      switch argument {
      case "--key-file":
        index += 1
        guard index < arguments.count else {
          throw CheckError.usage("--key-file requires a path")
        }
        options.keyFile = privateFileURL(arguments[index])
      case "--search-query-file":
        index += 1
        guard index < arguments.count else {
          throw CheckError.usage("--search-query-file requires a path")
        }
        options.searchQueryFile = privateFileURL(arguments[index])
      case "--skip-build":
        options.skipBuild = true
      case "-h", "--help":
        options.showHelp = true
      default:
        throw CheckError.usage("unknown option: \(argument)")
      }
      index += 1
    }
    return options
  }
}

private struct CapturedProcess {
  let status: Int32
  let standardOutput: Data
}

private struct DiscoveredSource {
  let root: URL
}

private struct QueryPageSnapshot {
  let returned: Int
  let hasMore: Bool
  let nextCursor: String?
  let coverageComplete: Bool
  let guarantee: String
  let warningCodes: [String]
  let items: [[String: Any]]
}

private struct MessageCandidate {
  let conversationID: String
  let page: QueryPageSnapshot
}

private struct SearchSeed {
  let conversationID: String?
  let query: String
  let kind: String
}

private struct StatusSummary: Encodable {
  let databaseCount: Int
  let databaseBytes: UInt64
  let writeAheadLogCount: Int
  let writeAheadLogBytes: UInt64
  let sharedMemoryCount: Int
  let sharedMemoryBytes: UInt64
  let rollbackJournalCount: Int
  let rollbackJournalBytes: UInt64
  let totalSqliteStorageBytes: UInt64
}

private struct PageSummary: Encodable {
  let returned: Int
  let pagesRead: Int
  let hasMore: Bool
  let cursorCheck: String
  let coverageComplete: Bool
  let guarantee: String
  let warningCodes: [String]
}

private struct ExactMessageSummary: Encodable {
  let passed: Bool
  let coverageComplete: Bool
  let warningCodes: [String]
}

private struct SearchSummary: Encodable {
  let probeKind: String
  let pagesRead: Int
  let returnedOnFirstPage: Int
  let positiveHitObserved: Bool
  let exactHitHydrationPassed: Bool
  let cursorCheck: String
  let coverageComplete: Bool
  let guarantee: String
  let warningCodes: [String]
}

private struct SourceSummary: Encodable {
  let ordinal: Int
  let status: StatusSummary
  let conversations: PageSummary
  let scannedConversationCount: Int
  let messages: PageSummary
  let exactMessage: ExactMessageSummary
  let search: SearchSummary
}

private struct CheckReport: Encodable {
  let schema = reportSchema
  let formatVersion = 1
  let ok: Bool
  let sourceMode = "liveEncrypted"
  let discoveredSourceCount: Int
  let authenticatedSourceCount: Int
  let rejectedSourceCount: Int
  let sources: [SourceSummary]
}

private let usage = """
  Usage:
    swift scripts/check-live-database.swift [options]

  Runs a bounded, read-only developer sanity check against real encrypted
  WeChat databases discovered on this Mac. It cannot accept a fixture source,
  plaintext database, snapshot, archive, or replica.

  Options:
    --key-file <path>          Owner-only live WeChat key file
                               (default: ~/.greenbubbles-acquire/passphrase.txt)
    --search-query-file <path> Owner-only UTF-8 query expected to have a hit;
                               otherwise a query is derived privately from a
                               decoded text message in the live database
    --skip-build               Reuse existing release binaries
    -h, --help                 Show this help

  The command prints aggregate JSON only. It never prints database paths,
  account IDs, conversation IDs, message IDs, keys, queries, or content.
  """

private let scriptURL = URL(fileURLWithPath: #filePath).standardizedFileURL
private let repositoryRoot = scriptURL.deletingLastPathComponent().deletingLastPathComponent()
private let discoveryExecutable = repositoryRoot.appending(path: ".build/release/greenbubbles")
private let queryExecutable = repositoryRoot.appending(
  path: "Native/GreenBubblesRestore/target/release/greenbubbles-restore"
)

private func privateFileURL(_ value: String) -> URL {
  URL(
    fileURLWithPath: NSString(string: value).expandingTildeInPath
  ).standardizedFileURL
}

private func note(_ message: String) {
  FileHandle.standardError.write(Data("\(message)\n".utf8))
}

private func runBuild(_ command: String, _ arguments: [String], stage: String) throws {
  note(stage)
  let process = Process()
  process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
  process.arguments = [command] + arguments
  process.currentDirectoryURL = repositoryRoot
  process.standardOutput = FileHandle.standardError
  process.standardError = FileHandle.standardError
  do {
    try process.run()
  } catch {
    throw CheckError.prerequisite("\(stage) could not start")
  }
  process.waitUntilExit()
  guard process.terminationReason == .exit, process.terminationStatus == 0 else {
    throw CheckError.commandFailed(stage, process.terminationStatus)
  }
}

private func runCaptured(
  executable: URL,
  arguments: [String],
  standardInput: Data? = nil,
  stage: String
) throws -> CapturedProcess {
  let process = Process()
  let output = Pipe()
  let errorOutput = Pipe()
  let input = standardInput.map { _ in Pipe() }
  process.executableURL = executable
  process.arguments = arguments
  process.currentDirectoryURL = repositoryRoot
  process.standardOutput = output
  process.standardError = errorOutput
  process.standardInput = input
  do {
    try process.run()
  } catch {
    throw CheckError.prerequisite("\(stage) could not start")
  }
  if let standardInput, let input {
    input.fileHandleForWriting.write(standardInput)
    try? input.fileHandleForWriting.close()
  }
  let outputData = output.fileHandleForReading.readDataToEndOfFile()
  _ = errorOutput.fileHandleForReading.readDataToEndOfFile()
  process.waitUntilExit()
  return CapturedProcess(status: process.terminationStatus, standardOutput: outputData)
}

private func validateExecutable(_ url: URL, label: String) throws {
  var metadata = stat()
  guard lstat(url.path, &metadata) == 0,
    metadata.st_mode & mode_t(S_IFMT) == mode_t(S_IFREG),
    metadata.st_uid == geteuid(),
    access(url.path, X_OK) == 0
  else {
    throw CheckError.prerequisite("\(label) is not a current-user executable")
  }
}

private func readOwnerOnlyFile(
  _ url: URL,
  label: String,
  maximumBytes: Int = maximumPrivateFileBytes
) throws -> [UInt8] {
  var parentMetadata = stat()
  let parent = url.deletingLastPathComponent()
  guard lstat(parent.path, &parentMetadata) == 0,
    parentMetadata.st_mode & mode_t(S_IFMT) == mode_t(S_IFDIR),
    parentMetadata.st_uid == geteuid(),
    parentMetadata.st_mode & mode_t(0o077) == 0
  else {
    throw CheckError.prerequisite("\(label) parent must be a current-user owner-only directory")
  }

  var pathMetadata = stat()
  guard lstat(url.path, &pathMetadata) == 0,
    pathMetadata.st_mode & mode_t(S_IFMT) == mode_t(S_IFREG),
    pathMetadata.st_uid == geteuid(),
    pathMetadata.st_nlink == 1,
    pathMetadata.st_mode & mode_t(0o777) == mode_t(0o600)
  else {
    throw CheckError.prerequisite("\(label) must be a current-user mode-0600 single-link file")
  }

  let descriptor = open(url.path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW)
  guard descriptor >= 0 else {
    throw CheckError.prerequisite("\(label) could not be opened safely")
  }
  defer { close(descriptor) }

  var openedMetadata = stat()
  guard fstat(descriptor, &openedMetadata) == 0,
    openedMetadata.st_dev == pathMetadata.st_dev,
    openedMetadata.st_ino == pathMetadata.st_ino,
    openedMetadata.st_mode & mode_t(S_IFMT) == mode_t(S_IFREG),
    openedMetadata.st_uid == geteuid(),
    openedMetadata.st_nlink == 1,
    openedMetadata.st_mode & mode_t(0o777) == mode_t(0o600)
  else {
    throw CheckError.prerequisite("\(label) changed during secure open")
  }

  var result: [UInt8] = []
  var buffer = [UInt8](repeating: 0, count: min(4096, maximumBytes + 1))
  while result.count <= maximumBytes {
    let count = buffer.withUnsafeMutableBytes { bytes in
      Darwin.read(descriptor, bytes.baseAddress, bytes.count)
    }
    if count == 0 { break }
    guard count > 0 else {
      throw CheckError.prerequisite("\(label) could not be read safely")
    }
    result.append(contentsOf: buffer.prefix(count))
  }
  guard !result.isEmpty, result.count <= maximumBytes else {
    throw CheckError.prerequisite("\(label) is empty or exceeds its fixed size limit")
  }
  return result
}

private func normalizedKeyBytes(_ bytes: [UInt8]) throws -> [UInt8] {
  var key = bytes
  if key.last == 0x0A { key.removeLast() }
  if key.last == 0x0D { key.removeLast() }
  let isHex =
    key.count == 64
    && key.allSatisfy {
      (0x30...0x39).contains($0) || (0x41...0x46).contains($0)
        || (0x61...0x66).contains($0)
    }
  guard key.count == 32 || isHex, !key.contains(0x0A), !key.contains(0x0D) else {
    throw CheckError.prerequisite("key file does not contain one supported 32-byte or 64-hex key")
  }
  return key
}

private func liveInput(key: [UInt8], query: String? = nil) -> Data {
  var input = key
  input.append(0x0A)
  if let query {
    input.append(contentsOf: query.utf8)
  }
  return Data(input)
}

private func jsonValue(_ data: Data, stage: String) throws -> Any {
  do {
    return try JSONSerialization.jsonObject(with: data)
  } catch {
    throw CheckError.invalidResponse(stage)
  }
}

private func object(_ value: Any?, stage: String) throws -> [String: Any] {
  guard let value = value as? [String: Any] else {
    throw CheckError.invalidResponse(stage)
  }
  return value
}

private func array(_ value: Any?, stage: String) throws -> [Any] {
  guard let value = value as? [Any] else {
    throw CheckError.invalidResponse(stage)
  }
  return value
}

private func string(_ object: [String: Any], _ key: String, stage: String) throws -> String {
  guard let value = object[key] as? String else {
    throw CheckError.invalidResponse(stage)
  }
  return value
}

private func integer(_ object: [String: Any], _ key: String, stage: String) throws -> Int {
  guard let value = object[key] as? NSNumber else {
    throw CheckError.invalidResponse(stage)
  }
  return value.intValue
}

private func unsignedInteger(
  _ object: [String: Any],
  _ key: String,
  stage: String
) throws -> UInt64 {
  guard let value = object[key] as? NSNumber, value.int64Value >= 0 else {
    throw CheckError.invalidResponse(stage)
  }
  return value.uint64Value
}

private func boolean(_ object: [String: Any], _ key: String, stage: String) throws -> Bool {
  guard let value = object[key] as? Bool else {
    throw CheckError.invalidResponse(stage)
  }
  return value
}

private func validateSuccessEnvelope(
  _ value: [String: Any],
  operation: String,
  stage: String
) throws {
  guard try boolean(value, "ok", stage: stage),
    try string(value, "schema", stage: stage) == querySchema,
    try integer(value, "formatVersion", stage: stage) == 1,
    try string(value, "operation", stage: stage) == operation
  else {
    throw CheckError.invalidResponse(stage)
  }
  let source = try object(value["source"], stage: stage)
  guard try string(source, "mode", stage: stage) == "liveEncrypted" else {
    throw CheckError.invalidResponse(stage)
  }
}

private func warningCodes(_ value: [String: Any], stage: String) throws -> [String] {
  let warnings = try array(value["warnings"], stage: stage)
  return try Array(
    Set(
      warnings.map {
        try string(try object($0, stage: stage), "code", stage: stage)
      }
    )
  ).sorted()
}

private func parsePage(
  _ result: CapturedProcess,
  operation: String,
  expectedLimit: Int,
  stage: String
) throws -> QueryPageSnapshot {
  guard result.status == 0 else {
    throw CheckError.commandFailed(stage, result.status)
  }
  let envelope = try object(try jsonValue(result.standardOutput, stage: stage), stage: stage)
  try validateSuccessEnvelope(envelope, operation: operation, stage: stage)
  let page = try object(envelope["page"], stage: stage)
  let consistency = try object(envelope["consistency"], stage: stage)
  let items = try array(envelope["items"], stage: stage).map {
    try object($0, stage: stage)
  }
  let returned = try integer(page, "returned", stage: stage)
  let limit = try integer(page, "limit", stage: stage)
  let hasMore = try boolean(page, "hasMore", stage: stage)
  let nextCursor = page["nextCursor"] as? String
  guard returned == items.count, limit == expectedLimit, !hasMore || nextCursor != nil else {
    throw CheckError.invalidResponse(stage)
  }
  return QueryPageSnapshot(
    returned: returned,
    hasMore: hasMore,
    nextCursor: nextCursor,
    coverageComplete: try boolean(consistency, "coverageComplete", stage: stage),
    guarantee: try string(consistency, "guarantee", stage: stage),
    warningCodes: try warningCodes(envelope, stage: stage),
    items: items
  )
}

private func requireCompleteCoverage(_ page: QueryPageSnapshot, stage: String) throws {
  guard page.coverageComplete else {
    throw CheckError.failed("\(stage) reported incomplete source coverage")
  }
}

private func itemIDs(_ page: QueryPageSnapshot, stage: String) throws -> [String] {
  try page.items.map { try string($0, "id", stage: stage) }
}

private func assertNoOverlap(_ left: [String], _ right: [String], stage: String) throws {
  guard Set(left).isDisjoint(with: Set(right)) else {
    throw CheckError.failed("\(stage) returned an identity on both cursor pages")
  }
}

private func discoverSources() throws -> [DiscoveredSource] {
  let stage = "installed-account discovery"
  let result = try runCaptured(
    executable: discoveryExecutable,
    arguments: ["accounts", "--include-paths"],
    stage: stage
  )
  guard result.status == 0 else {
    throw CheckError.commandFailed(stage, result.status)
  }
  let records = try array(try jsonValue(result.standardOutput, stage: stage), stage: stage)
  guard records.count <= 64 else {
    throw CheckError.invalidResponse(stage)
  }
  var seen = Set<String>()
  var sources: [DiscoveredSource] = []
  for recordValue in records {
    let record = try object(recordValue, stage: stage)
    guard (record["isReadable"] as? Bool) == true,
      let databaseRoot = record["databaseRoot"] as? [String: Any],
      let path = databaseRoot["path"] as? String
    else {
      continue
    }
    let root = URL(fileURLWithPath: path).standardizedFileURL
    guard root.lastPathComponent == "db_storage", root.path.hasPrefix("/") else {
      throw CheckError.invalidResponse(stage)
    }
    if seen.insert(root.path).inserted {
      sources.append(DiscoveredSource(root: root))
    }
  }
  guard !sources.isEmpty else {
    throw CheckError.prerequisite("no readable installed account database was discovered")
  }
  return sources
}

private func runQuery(
  _ arguments: [String],
  key: [UInt8],
  query: String? = nil,
  stage: String
) throws -> CapturedProcess {
  try runCaptured(
    executable: queryExecutable,
    arguments: arguments,
    standardInput: liveInput(key: key, query: query),
    stage: stage
  )
}

private func authenticate(
  source: DiscoveredSource,
  key: [UInt8]
) throws -> (StatusSummary, [String: Any])? {
  let stage = "source status"
  let result = try runQuery(
    ["source", "status", source.root.path, "--passphrase-stdin"],
    key: key,
    stage: stage
  )
  let envelope = try object(try jsonValue(result.standardOutput, stage: stage), stage: stage)
  if result.status != 0 || (envelope["ok"] as? Bool) != true {
    let error = try? object(envelope["error"], stage: stage)
    let code = error?["code"] as? String
    if code == "databaseUnavailable" || code == "invalidAccessMaterial" {
      return nil
    }
    throw CheckError.commandFailed(stage, result.status)
  }
  try validateSuccessEnvelope(envelope, operation: "source.status", stage: stage)
  let databaseCount = try integer(envelope, "databaseCount", stage: stage)
  let entries = try array(envelope["entries"], stage: stage).map {
    try object($0, stage: stage)
  }
  let relativePaths = try Set(entries.map { try string($0, "relativePath", stage: stage) })
  guard databaseCount == entries.count,
    relativePaths.contains("contact/contact.db"),
    relativePaths.contains("session/session.db"),
    relativePaths.contains(where: { $0.hasPrefix("message/") && $0.hasSuffix(".db") })
  else {
    throw CheckError.failed("authenticated source is missing the required live database families")
  }
  return (
    StatusSummary(
      databaseCount: databaseCount,
      databaseBytes: try unsignedInteger(envelope, "databaseBytes", stage: stage),
      writeAheadLogCount: try integer(envelope, "writeAheadLogCount", stage: stage),
      writeAheadLogBytes: try unsignedInteger(envelope, "writeAheadLogBytes", stage: stage),
      sharedMemoryCount: try integer(envelope, "sharedMemoryCount", stage: stage),
      sharedMemoryBytes: try unsignedInteger(envelope, "sharedMemoryBytes", stage: stage),
      rollbackJournalCount: try integer(envelope, "rollbackJournalCount", stage: stage),
      rollbackJournalBytes: try unsignedInteger(envelope, "rollbackJournalBytes", stage: stage),
      totalSqliteStorageBytes: try unsignedInteger(
        envelope,
        "totalSqliteStorageBytes",
        stage: stage
      )
    ),
    envelope
  )
}

private func conversations(
  source: DiscoveredSource,
  key: [UInt8]
) throws -> (PageSummary, [String]) {
  var cursor: String?
  var pagesRead = 0
  var seenIDs: [String] = []
  var firstPage: QueryPageSnapshot?
  var allWarningCodes = Set<String>()
  var cursorCheck = "notApplicable"

  while seenIDs.count < maximumConversationScan {
    let stage = "conversation page"
    var arguments = [
      "conversations", "list", source.root.path, "--passphrase-stdin", "--limit",
      String(conversationPageLimit),
    ]
    if let cursor {
      arguments += ["--cursor", cursor]
    }
    let page = try parsePage(
      runQuery(arguments, key: key, stage: stage),
      operation: "conversations.list",
      expectedLimit: conversationPageLimit,
      stage: stage
    )
    try requireCompleteCoverage(page, stage: stage)
    pagesRead += 1
    if firstPage == nil { firstPage = page }
    allWarningCodes.formUnion(page.warningCodes)
    let ids = try itemIDs(page, stage: stage)
    try assertNoOverlap(seenIDs, ids, stage: "conversation pagination")
    if pagesRead == 2 { cursorCheck = "passed" }
    seenIDs.append(contentsOf: ids)
    guard page.hasMore, let nextCursor = page.nextCursor,
      seenIDs.count < maximumConversationScan
    else {
      break
    }
    cursor = nextCursor
  }

  guard let firstPage, !seenIDs.isEmpty else {
    throw CheckError.failed("live database returned no conversations")
  }
  if firstPage.hasMore && pagesRead < 2 {
    throw CheckError.failed("conversation continuation was not exercised")
  }
  return (
    PageSummary(
      returned: firstPage.returned,
      pagesRead: pagesRead,
      hasMore: firstPage.hasMore,
      cursorCheck: cursorCheck,
      coverageComplete: true,
      guarantee: firstPage.guarantee,
      warningCodes: allWarningCodes.sorted()
    ),
    Array(seenIDs.prefix(maximumConversationScan))
  )
}

private func derivedSearchQuery(from text: String) -> String? {
  var candidates: [String] = []
  var current = String.UnicodeScalarView()
  func appendCurrent() {
    guard !current.isEmpty else { return }
    candidates.append(String(current))
    current = String.UnicodeScalarView()
  }
  for scalar in text.unicodeScalars {
    if CharacterSet.alphanumerics.contains(scalar) {
      current.append(scalar)
    } else {
      appendCurrent()
    }
  }
  appendCurrent()

  for candidate in candidates {
    guard candidate.unicodeScalars.contains(where: { CharacterSet.letters.contains($0) }) else {
      continue
    }
    var query = ""
    for scalar in candidate.unicodeScalars {
      let proposed = query + String(scalar)
      if proposed.utf8.count > 64 { break }
      query = proposed
    }
    if query.unicodeScalars.count >= 3 {
      return query
    }
  }
  return nil
}

private func searchSeed(
  from page: QueryPageSnapshot,
  conversationID: String
) -> SearchSeed? {
  for item in page.items.reversed() {
    guard let content = item["content"] as? [String: Any],
      let text = content["Text"] as? String,
      let query = derivedSearchQuery(from: text)
    else {
      continue
    }
    return SearchSeed(conversationID: conversationID, query: query, kind: "sourceDerived")
  }
  return nil
}

private func messages(
  source: DiscoveredSource,
  key: [UInt8],
  conversationIDs: [String]
) throws -> (PageSummary, ExactMessageSummary, SearchSeed, Int) {
  var firstNonempty: MessageCandidate?
  var paged: MessageCandidate?
  var seed: SearchSeed?
  var scanned = 0

  for conversationID in conversationIDs.prefix(maximumConversationScan) {
    scanned += 1
    let stage = "message page"
    let page = try parsePage(
      runQuery(
        [
          "messages", "list", source.root.path, "--conversation", conversationID,
          "--passphrase-stdin", "--limit", String(messagePageLimit),
        ],
        key: key,
        stage: stage
      ),
      operation: "messages.list",
      expectedLimit: messagePageLimit,
      stage: stage
    )
    try requireCompleteCoverage(page, stage: stage)
    guard page.returned > 0 else { continue }
    let candidate = MessageCandidate(conversationID: conversationID, page: page)
    if firstNonempty == nil { firstNonempty = candidate }
    if seed == nil { seed = searchSeed(from: page, conversationID: conversationID) }
    if page.hasMore {
      paged = candidate
      if seed != nil { break }
    }
  }

  guard let candidate = paged ?? firstNonempty else {
    throw CheckError.failed(
      "no messages were found in the first \(scanned) discovered conversations"
    )
  }
  guard let seed else {
    throw CheckError.failed(
      "no searchable decoded text was found; supply --search-query-file for this account"
    )
  }

  let firstIDs = try itemIDs(candidate.page, stage: "message page")
  var pagesRead = 1
  var cursorCheck = "notApplicable"
  var allWarningCodes = Set(candidate.page.warningCodes)
  if candidate.page.hasMore {
    guard let cursor = candidate.page.nextCursor else {
      throw CheckError.invalidResponse("message page")
    }
    let stage = "message continuation page"
    let secondPage = try parsePage(
      runQuery(
        [
          "messages", "list", source.root.path, "--conversation", candidate.conversationID,
          "--passphrase-stdin", "--limit", String(continuationPageLimit), "--cursor", cursor,
        ],
        key: key,
        stage: stage
      ),
      operation: "messages.list",
      expectedLimit: continuationPageLimit,
      stage: stage
    )
    try requireCompleteCoverage(secondPage, stage: stage)
    try assertNoOverlap(
      firstIDs,
      try itemIDs(secondPage, stage: stage),
      stage: "message pagination"
    )
    pagesRead = 2
    cursorCheck = "passed"
    allWarningCodes.formUnion(secondPage.warningCodes)
  }

  guard let messageID = firstIDs.first else {
    throw CheckError.invalidResponse("message page")
  }
  let exactStage = "exact message hydration"
  let exactResult = try runQuery(
    [
      "message", "get", source.root.path, "--conversation", candidate.conversationID,
      "--message", messageID, "--passphrase-stdin",
    ],
    key: key,
    stage: exactStage
  )
  guard exactResult.status == 0 else {
    throw CheckError.commandFailed(exactStage, exactResult.status)
  }
  let exact = try object(
    try jsonValue(exactResult.standardOutput, stage: exactStage),
    stage: exactStage
  )
  try validateSuccessEnvelope(exact, operation: "message.get", stage: exactStage)
  let item = try object(exact["item"], stage: exactStage)
  guard try string(item, "id", stage: exactStage) == messageID,
    try string(item, "conversationId", stage: exactStage) == candidate.conversationID
  else {
    throw CheckError.failed("exact message hydration was not identity-bound")
  }
  let exactConsistency = try object(exact["consistency"], stage: exactStage)
  let exactCoverage = try boolean(exactConsistency, "coverageComplete", stage: exactStage)
  guard exactCoverage else {
    throw CheckError.failed("exact message hydration reported incomplete coverage")
  }

  return (
    PageSummary(
      returned: candidate.page.returned,
      pagesRead: pagesRead,
      hasMore: candidate.page.hasMore,
      cursorCheck: cursorCheck,
      coverageComplete: true,
      guarantee: candidate.page.guarantee,
      warningCodes: allWarningCodes.sorted()
    ),
    ExactMessageSummary(
      passed: true,
      coverageComplete: exactCoverage,
      warningCodes: try warningCodes(exact, stage: exactStage)
    ),
    seed,
    scanned
  )
}

private func loadOperatorSearchSeed(_ url: URL) throws -> SearchSeed {
  var bytes = try readOwnerOnlyFile(url, label: "search query file", maximumBytes: 4096)
  defer {
    for index in bytes.indices { bytes[index] = 0 }
  }
  guard var query = String(bytes: bytes, encoding: .utf8) else {
    throw CheckError.prerequisite("search query file is not UTF-8")
  }
  while query.last?.isNewline == true { query.removeLast() }
  guard !query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
    query.utf8.count <= 4096,
    !query.contains("\0")
  else {
    throw CheckError.prerequisite("search query file is empty or outside safe limits")
  }
  return SearchSeed(conversationID: nil, query: query, kind: "operatorProvided")
}

private func search(
  source: DiscoveredSource,
  key: [UInt8],
  seed: SearchSeed
) throws -> SearchSummary {
  var cursor: String?
  var pagesRead = 0
  var firstPage: QueryPageSnapshot?
  var firstHit: [String: Any]?
  var seenIDs: [String] = []
  var warningCodes = Set<String>()
  var cursorCheck = "notApplicable"

  while pagesRead < maximumSearchPages {
    let stage = "live message search"
    var arguments = [
      "messages", "search", source.root.path, "--passphrase-stdin", "--query-stdin",
      "--limit", String(searchPageLimit),
    ]
    if let conversationID = seed.conversationID {
      arguments += ["--conversation", conversationID]
    }
    if let cursor {
      arguments += ["--cursor", cursor]
    }
    let page = try parsePage(
      runQuery(arguments, key: key, query: seed.query, stage: stage),
      operation: "messages.search",
      expectedLimit: searchPageLimit,
      stage: stage
    )
    pagesRead += 1
    if firstPage == nil { firstPage = page }
    warningCodes.formUnion(page.warningCodes)
    let ids = try itemIDs(page, stage: stage)
    try assertNoOverlap(seenIDs, ids, stage: "search pagination")
    if pagesRead == 2 { cursorCheck = "passed" }
    seenIDs.append(contentsOf: ids)
    if firstHit == nil { firstHit = page.items.first }

    if firstHit != nil && (!page.hasMore || pagesRead >= 2) { break }
    guard page.hasMore, let nextCursor = page.nextCursor else { break }
    cursor = nextCursor
  }

  guard let firstPage else {
    throw CheckError.invalidResponse("live message search")
  }
  guard let firstHit else {
    throw CheckError.failed(
      "a real search query produced no hit; native FTS may be stale or incompatible"
    )
  }
  if firstPage.hasMore && pagesRead < 2 {
    throw CheckError.failed("search continuation was not exercised")
  }

  let hitID = try string(firstHit, "id", stage: "live message search")
  let conversationID = try string(firstHit, "conversationId", stage: "live message search")
  let exactStage = "search-hit hydration"
  let exactResult = try runQuery(
    [
      "message", "get", source.root.path, "--conversation", conversationID, "--message", hitID,
      "--passphrase-stdin",
    ],
    key: key,
    stage: exactStage
  )
  guard exactResult.status == 0 else {
    throw CheckError.commandFailed(exactStage, exactResult.status)
  }
  let exact = try object(
    try jsonValue(exactResult.standardOutput, stage: exactStage),
    stage: exactStage
  )
  try validateSuccessEnvelope(exact, operation: "message.get", stage: exactStage)
  let exactItem = try object(exact["item"], stage: exactStage)
  guard try string(exactItem, "id", stage: exactStage) == hitID,
    try string(exactItem, "conversationId", stage: exactStage) == conversationID
  else {
    throw CheckError.failed("search result could not be hydrated to its exact source message")
  }

  return SearchSummary(
    probeKind: seed.kind,
    pagesRead: pagesRead,
    returnedOnFirstPage: firstPage.returned,
    positiveHitObserved: true,
    exactHitHydrationPassed: true,
    cursorCheck: cursorCheck,
    coverageComplete: firstPage.coverageComplete,
    guarantee: firstPage.guarantee,
    warningCodes: warningCodes.sorted()
  )
}

private func checkSource(
  _ source: DiscoveredSource,
  ordinal: Int,
  status: StatusSummary,
  key: [UInt8],
  operatorSearchSeed: SearchSeed?
) throws -> SourceSummary {
  note("Checking authenticated live source \(ordinal)...")
  let (conversationSummary, conversationIDs) = try conversations(source: source, key: key)
  let (messageSummary, exactSummary, derivedSeed, scannedConversationCount) = try messages(
    source: source,
    key: key,
    conversationIDs: conversationIDs
  )
  let searchSummary = try search(
    source: source,
    key: key,
    seed: operatorSearchSeed ?? derivedSeed
  )
  return SourceSummary(
    ordinal: ordinal,
    status: status,
    conversations: conversationSummary,
    scannedConversationCount: scannedConversationCount,
    messages: messageSummary,
    exactMessage: exactSummary,
    search: searchSummary
  )
}

private func run(_ options: Options) throws -> CheckReport {
  if !options.skipBuild {
    try runBuild(
      "swift",
      ["build", "-c", "release", "--product", "greenbubbles"],
      stage: "Building the current Swift discovery CLI..."
    )
    try runBuild(
      "cargo",
      [
        "build", "--release", "--locked", "--manifest-path",
        "Native/GreenBubblesRestore/Cargo.toml",
      ],
      stage: "Building the current native query CLI..."
    )
  }
  try validateExecutable(discoveryExecutable, label: "release discovery CLI")
  try validateExecutable(queryExecutable, label: "release query CLI")

  var rawKey = try readOwnerOnlyFile(options.keyFile, label: "key file")
  defer {
    for index in rawKey.indices { rawKey[index] = 0 }
  }
  var key = try normalizedKeyBytes(rawKey)
  defer {
    for index in key.indices { key[index] = 0 }
  }
  let operatorSearchSeed = try options.searchQueryFile.map(loadOperatorSearchSeed)

  let discovered = try discoverSources()
  note("Discovered \(discovered.count) readable installed account source(s).")
  var authenticated: [(DiscoveredSource, StatusSummary)] = []
  var rejected = 0
  for source in discovered {
    if let (status, _) = try authenticate(source: source, key: key) {
      authenticated.append((source, status))
    } else {
      rejected += 1
    }
  }
  guard !authenticated.isEmpty else {
    throw CheckError.failed("the supplied key did not authenticate any discovered live source")
  }

  var summaries: [SourceSummary] = []
  for (index, entry) in authenticated.enumerated() {
    summaries.append(
      try checkSource(
        entry.0,
        ordinal: index + 1,
        status: entry.1,
        key: key,
        operatorSearchSeed: operatorSearchSeed
      )
    )
  }
  return CheckReport(
    ok: true,
    discoveredSourceCount: discovered.count,
    authenticatedSourceCount: authenticated.count,
    rejectedSourceCount: rejected,
    sources: summaries
  )
}

do {
  let options = try Options.parse(Array(CommandLine.arguments.dropFirst()))
  if options.showHelp {
    print(usage)
    exit(0)
  }
  let report = try run(options)
  let encoder = JSONEncoder()
  encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
  FileHandle.standardOutput.write(try encoder.encode(report))
  FileHandle.standardOutput.write(Data("\n".utf8))
} catch {
  FileHandle.standardError.write(Data("error: \(error)\n".utf8))
  exit(1)
}
