import CryptoKit
import Darwin
import Foundation

public enum HistorySnapshotSourceAccess: String, CaseIterable, Sendable {
  case encryptedWeChat
  case decryptedSQLite

  public var displayName: String {
    switch self {
    case .encryptedWeChat: "Encrypted WeChat SQLite"
    case .decryptedSQLite: "Plaintext SQLite (explicit)"
    }
  }
}

public struct HistorySnapshotRecoveryKit: Equatable, Sendable {
  public let url: URL
  public let words: [String]
  public let sha256: String

  public init(url: URL, words: [String], sha256: String) {
    self.url = url.standardizedFileURL
    self.words = words
    self.sha256 = sha256
  }
}

public struct HistorySnapshotWordChallenge: Equatable, Sendable {
  public let zeroBasedPositions: [Int]

  public init(zeroBasedPositions: [Int], wordCount: Int = 24) throws {
    guard (1...wordCount).contains(zeroBasedPositions.count),
      Set(zeroBasedPositions).count == zeroBasedPositions.count,
      zeroBasedPositions.allSatisfy({ (0..<wordCount).contains($0) })
    else { throw HistorySnapshotCreationError.invalidRecoveryKit }
    self.zeroBasedPositions = zeroBasedPositions.sorted()
  }

  public static func random(wordCount: Int = 24, confirmationCount: Int = 4) throws -> Self {
    guard confirmationCount > 0, confirmationCount <= wordCount else {
      throw HistorySnapshotCreationError.invalidRecoveryKit
    }
    var generator = SystemRandomNumberGenerator()
    let positions = Array(0..<wordCount).shuffled(using: &generator).prefix(confirmationCount)
    return try Self(zeroBasedPositions: Array(positions), wordCount: wordCount)
  }

  public func accepts(responses: [Int: String], words: [String]) -> Bool {
    guard words.count == 24 else { return false }
    return zeroBasedPositions.allSatisfy { position in
      responses[position]?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        == words[position]
    }
  }
}

public struct HistorySnapshotCreationRequest: Sendable {
  public let executableURL: URL
  public let sourceURL: URL
  public let outputURL: URL
  public let recoveryKit: HistorySnapshotRecoveryKit
  public let localCredentialURL: URL?
  public let sourceAccess: HistorySnapshotSourceAccess
  public let stableCapture: Bool

  public init(
    executableURL: URL,
    sourceURL: URL,
    outputURL: URL,
    recoveryKit: HistorySnapshotRecoveryKit,
    localCredentialURL: URL? = nil,
    sourceAccess: HistorySnapshotSourceAccess,
    stableCapture: Bool = false
  ) {
    self.executableURL = executableURL.standardizedFileURL
    self.sourceURL = sourceURL.standardizedFileURL
    self.outputURL = outputURL.standardizedFileURL
    self.recoveryKit = recoveryKit
    self.localCredentialURL = localCredentialURL?.standardizedFileURL
    self.sourceAccess = sourceAccess
    self.stableCapture = stableCapture
  }
}

public struct HistorySnapshotCreationResult: Equatable, Sendable {
  public let snapshotID: String
  public let outputURL: URL
  public let databaseCount: Int
  public let hasRecoveryWords: Bool
  public let hasLocalCredential: Bool
  public let hasPassphrase: Bool
  public let recoveryVerified: Bool
}

public enum HistorySnapshotCreationError: Error, Equatable, LocalizedError {
  case invalidExecutable
  case invalidSource
  case unsafeOutput
  case unsafeProtector
  case invalidRecoveryKit
  case invalidSourceKey
  case invalidPassphrase
  case responseTooLarge
  case timedOut
  case commandFailed
  case invalidResponse

  public var errorDescription: String? {
    switch self {
    case .invalidExecutable: "Choose a real local greenbubbles-restore executable."
    case .invalidSource: "Choose a current-user-owned real SQLite source directory."
    case .unsafeOutput:
      "Choose a new snapshot path beneath a current-user-owned owner-only directory."
    case .unsafeProtector:
      "Recovery and convenience files must be single-link owner-only files in owner-only directories."
    case .invalidRecoveryKit: "The recovery kit is not the confirmed 24-word kit."
    case .invalidSourceKey: "The encrypted source key must be 32 raw bytes or 64 hexadecimal characters."
    case .invalidPassphrase:
      "The optional snapshot passphrase must be 12–1024 UTF-8 bytes with no line break."
    case .responseTooLarge: "The local snapshot command exceeded its fixed response bound."
    case .timedOut: "The local snapshot command exceeded its two-hour safety deadline."
    case .commandFailed: "The local snapshot command failed. The portable recovery kit was not deleted."
    case .invalidResponse: "The local snapshot command returned an invalid bounded response."
    }
  }
}

public struct HistorySnapshotCreationClient: Sendable {
  private static let maximumOutputBytes = 8 * 1_024 * 1_024
  private static let maximumErrorBytes = 1 * 1_024 * 1_024
  private static let timeoutMilliseconds = 2 * 60 * 60 * 1_000

  private let runner: any HistorySnapshotCommandRunning

  public init() { runner = HistorySnapshotProcessRunner() }

  init(runner: any HistorySnapshotCommandRunning) { self.runner = runner }

  public func createRecoveryKit(
    executableURL: URL,
    outputURL: URL
  ) async throws -> HistorySnapshotRecoveryKit {
    try validateSnapshotExecutable(executableURL)
    try validateNewPrivateFile(outputURL, description: "recovery-kit output")
    let stdout = try await run(
      executableURL: executableURL,
      arguments: ["snapshot", "recovery-kit", "create", outputURL.path],
      input: []
    )
    let report = try decode(RecoveryKitReport.self, from: stdout)
    guard report.schema == "greenbubbles.recovery-kit.v1", report.formatVersion == 1,
      report.wordCount == 24, report.checksumValidated, report.portable, report.fileCreated
    else { throw HistorySnapshotCreationError.invalidResponse }
    return try readRecoveryKit(outputURL)
  }

  public func createLocalCredential(
    executableURL: URL,
    outputURL: URL
  ) async throws -> Data {
    try validateSnapshotExecutable(executableURL)
    try validateNewPrivateFile(outputURL, description: "local-credential output")
    let stdout = try await run(
      executableURL: executableURL,
      arguments: ["snapshot", "local-credential", "create", outputURL.path],
      input: []
    )
    let report = try decode(LocalCredentialReport.self, from: stdout)
    guard report.schema == "greenbubbles.local-unlock-credential.v1",
      report.formatVersion == 1, report.localConvenience, !report.portable, report.fileCreated
    else { throw HistorySnapshotCreationError.invalidResponse }
    try validatePrivateSnapshotFile(outputURL, maximumBytes: 1_024)
    return try Data(contentsOf: outputURL, options: [.mappedIfSafe])
  }

  public func createSnapshot(
    request: HistorySnapshotCreationRequest,
    sourceKeyUTF8: [UInt8],
    snapshotPassphraseUTF8: [UInt8] = []
  ) async throws -> HistorySnapshotCreationResult {
    try validateSnapshotExecutable(request.executableURL)
    try validateSnapshotSource(request.sourceURL)
    try validateNewSnapshotOutput(request.outputURL, protectedSource: request.sourceURL)
    try validatePrivateSnapshotFile(request.recoveryKit.url, maximumBytes: 2 * 1_024)
    let currentKit = try readRecoveryKit(request.recoveryKit.url)
    guard currentKit.sha256 == request.recoveryKit.sha256,
      currentKit.words == request.recoveryKit.words
    else { throw HistorySnapshotCreationError.invalidRecoveryKit }
    if let localCredentialURL = request.localCredentialURL {
      try validatePrivateSnapshotFile(localCredentialURL, maximumBytes: 1_024)
    }

    switch request.sourceAccess {
    case .encryptedWeChat:
      let raw = sourceKeyUTF8.count == 32
      let hex = sourceKeyUTF8.count == 64 && sourceKeyUTF8.allSatisfy(isSnapshotHexadecimal)
      guard raw || hex else { throw HistorySnapshotCreationError.invalidSourceKey }
    case .decryptedSQLite:
      guard sourceKeyUTF8.isEmpty else { throw HistorySnapshotCreationError.invalidSourceKey }
    }
    if !snapshotPassphraseUTF8.isEmpty {
      guard (12...1_024).contains(snapshotPassphraseUTF8.count),
        !snapshotPassphraseUTF8.contains(0), !snapshotPassphraseUTF8.contains(10),
        !snapshotPassphraseUTF8.contains(13),
        String(bytes: snapshotPassphraseUTF8, encoding: .utf8) != nil
      else { throw HistorySnapshotCreationError.invalidPassphrase }
    }

    _ = try await run(
      executableURL: request.executableURL,
      arguments: [
        "snapshot", "recovery-kit", "validate", request.recoveryKit.url.path,
      ],
      input: []
    )
    let revalidatedKit = try readRecoveryKit(request.recoveryKit.url)
    guard revalidatedKit.sha256 == request.recoveryKit.sha256,
      revalidatedKit.words == request.recoveryKit.words
    else { throw HistorySnapshotCreationError.invalidRecoveryKit }

    var arguments = [
      "snapshot", request.stableCapture ? "create-capture" : "create",
      request.sourceURL.path, request.outputURL.path,
      request.sourceAccess == .encryptedWeChat
        ? "--source-passphrase-stdin" : "--source-decrypted",
      "--snapshot-recovery-kit", request.recoveryKit.url.path,
    ]
    if let localCredentialURL = request.localCredentialURL {
      arguments += ["--snapshot-local-credential", localCredentialURL.path]
    }
    if !snapshotPassphraseUTF8.isEmpty { arguments.append("--snapshot-passphrase-stdin") }
    var input = [UInt8]()
    if request.sourceAccess == .encryptedWeChat {
      input.append(contentsOf: sourceKeyUTF8)
      input.append(10)
    }
    if !snapshotPassphraseUTF8.isEmpty {
      input.append(contentsOf: snapshotPassphraseUTF8)
      input.append(10)
    }
    defer { input.resetBytes(in: 0..<input.count) }
    let stdout = try await run(
      executableURL: request.executableURL,
      arguments: arguments,
      input: input
    )
    if containsSecret(stdout, secret: sourceKeyUTF8)
      || containsSecret(stdout, secret: snapshotPassphraseUTF8)
    {
      throw HistorySnapshotCreationError.invalidResponse
    }
    let manifest = try decode(SnapshotManifest.self, from: stdout)
    let kinds = Set(manifest.protection.protectors.map(\.kind))
    guard manifest.schema == "greenbubbles.recoverable-snapshot.v2",
      manifest.formatVersion == 2, manifest.snapshotID.count == 64,
      manifest.snapshotID.utf8.allSatisfy(isSnapshotHexadecimal), manifest.recoveryVerified,
      manifest.protection.independentOfWechatKey,
      !manifest.protection.plaintextDatabaseFiles,
      kinds.contains("bip39English24"), !manifest.databases.isEmpty,
      request.localCredentialURL == nil || kinds.contains("localCredentialV1"),
      snapshotPassphraseUTF8.isEmpty || kinds.contains("argon2idPassphraseV1")
    else { throw HistorySnapshotCreationError.invalidResponse }
    return HistorySnapshotCreationResult(
      snapshotID: manifest.snapshotID,
      outputURL: request.outputURL,
      databaseCount: manifest.databases.count,
      hasRecoveryWords: true,
      hasLocalCredential: kinds.contains("localCredentialV1"),
      hasPassphrase: kinds.contains("argon2idPassphraseV1"),
      recoveryVerified: manifest.recoveryVerified
    )
  }

  public func readRecoveryKit(_ url: URL) throws -> HistorySnapshotRecoveryKit {
    try validatePrivateSnapshotFile(url, maximumBytes: 2 * 1_024)
    let data = try Data(contentsOf: url, options: [.mappedIfSafe])
    guard let text = String(data: data, encoding: .utf8) else {
      throw HistorySnapshotCreationError.invalidRecoveryKit
    }
    let lines = text.split(whereSeparator: \Character.isNewline).map(String.init)
    guard lines.count == 4, lines[0] == "GREENBUBBLES RECOVERY KIT",
      lines[1] == "format: 1", lines[2] == "language: english",
      lines[3].hasPrefix("words: ")
    else { throw HistorySnapshotCreationError.invalidRecoveryKit }
    let words = lines[3].dropFirst("words: ".count).split(separator: " ").map(String.init)
    guard words.count == 24,
      words.allSatisfy({ word in
        !word.isEmpty && word.utf8.allSatisfy { $0 >= 97 && $0 <= 122 }
      })
    else { throw HistorySnapshotCreationError.invalidRecoveryKit }
    return HistorySnapshotRecoveryKit(
      url: url,
      words: words,
      sha256: SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    )
  }

  private func run(
    executableURL: URL,
    arguments: [String],
    input: [UInt8]
  ) async throws -> Data {
    do {
      return try await runner.run(
        executableURL: executableURL,
        arguments: arguments,
        standardInput: input,
        maximumOutputBytes: Self.maximumOutputBytes,
        maximumErrorBytes: Self.maximumErrorBytes,
        timeoutMilliseconds: Self.timeoutMilliseconds
      )
    } catch let error as HistorySnapshotCreationError {
      throw error
    } catch {
      throw HistorySnapshotCreationError.commandFailed
    }
  }

  private func decode<Value: Decodable>(_ type: Value.Type, from data: Data) throws -> Value {
    guard let value = try? JSONDecoder().decode(type, from: data) else {
      throw HistorySnapshotCreationError.invalidResponse
    }
    return value
  }
}

protocol HistorySnapshotCommandRunning: Sendable {
  func run(
    executableURL: URL,
    arguments: [String],
    standardInput: [UInt8],
    maximumOutputBytes: Int,
    maximumErrorBytes: Int,
    timeoutMilliseconds: Int
  ) async throws -> Data
}

private struct HistorySnapshotProcessRunner: HistorySnapshotCommandRunning {
  func run(
    executableURL: URL,
    arguments: [String],
    standardInput: [UInt8],
    maximumOutputBytes: Int,
    maximumErrorBytes: Int,
    timeoutMilliseconds: Int
  ) async throws -> Data {
    let secret = SnapshotSecureBytes(copying: standardInput)
    let worker = Task.detached(priority: .userInitiated) {
      defer { secret.clear() }
      return try runSnapshotProcessSynchronously(
        executableURL: executableURL,
        arguments: arguments,
        standardInput: secret,
        maximumOutputBytes: maximumOutputBytes,
        maximumErrorBytes: maximumErrorBytes,
        timeoutMilliseconds: timeoutMilliseconds
      )
    }
    return try await withTaskCancellationHandler {
      try await worker.value
    } onCancel: {
      worker.cancel()
    }
  }
}

private struct RecoveryKitReport: Decodable {
  let schema: String
  let formatVersion: Int
  let wordCount: Int
  let checksumValidated: Bool
  let portable: Bool
  let fileCreated: Bool
}

private struct LocalCredentialReport: Decodable {
  let schema: String
  let formatVersion: Int
  let localConvenience: Bool
  let portable: Bool
  let fileCreated: Bool
}

private struct SnapshotManifest: Decodable {
  struct Protection: Decodable {
    struct Protector: Decodable { let kind: String }
    let independentOfWechatKey: Bool
    let plaintextDatabaseFiles: Bool
    let protectors: [Protector]
  }

  struct Database: Decodable { let relativePath: String }

  let schema: String
  let formatVersion: Int
  let snapshotID: String
  let protection: Protection
  let recoveryVerified: Bool
  let databases: [Database]

  enum CodingKeys: String, CodingKey {
    case schema
    case formatVersion
    case snapshotID = "snapshotId"
    case protection
    case recoveryVerified
    case databases
  }
}

private func validateSnapshotExecutable(_ url: URL) throws {
  var metadata = stat()
  guard lstat(url.path, &metadata) == 0, metadata.st_mode & S_IFMT == S_IFREG,
    metadata.st_mode & S_IXUSR != 0, metadata.st_uid == getuid() || metadata.st_uid == 0
  else { throw HistorySnapshotCreationError.invalidExecutable }
}

private func validateSnapshotSource(_ url: URL) throws {
  var metadata = stat()
  guard lstat(url.path, &metadata) == 0, metadata.st_mode & S_IFMT == S_IFDIR,
    metadata.st_uid == getuid()
  else { throw HistorySnapshotCreationError.invalidSource }
}

private func validateNewSnapshotOutput(_ url: URL, protectedSource: URL) throws {
  var metadata = stat()
  guard lstat(url.path, &metadata) != 0, errno == ENOENT else {
    throw HistorySnapshotCreationError.unsafeOutput
  }
  let parent = url.deletingLastPathComponent().standardizedFileURL
  try validateOwnerOnlySnapshotDirectory(parent)
  let canonicalParent = parent.resolvingSymlinksInPath().standardizedFileURL
  let canonicalSource = protectedSource.resolvingSymlinksInPath().standardizedFileURL
  guard canonicalParent.path != canonicalSource.path,
    !canonicalParent.path.hasPrefix(canonicalSource.path + "/")
  else { throw HistorySnapshotCreationError.unsafeOutput }
}

private func validateNewPrivateFile(_ url: URL, description: String) throws {
  _ = description
  var metadata = stat()
  guard lstat(url.path, &metadata) != 0, errno == ENOENT else {
    throw HistorySnapshotCreationError.unsafeProtector
  }
  try validateOwnerOnlySnapshotDirectory(url.deletingLastPathComponent())
}

private func validateOwnerOnlySnapshotDirectory(_ url: URL) throws {
  var metadata = stat()
  guard lstat(url.path, &metadata) == 0, metadata.st_mode & S_IFMT == S_IFDIR,
    metadata.st_uid == getuid(), metadata.st_mode & 0o077 == 0
  else { throw HistorySnapshotCreationError.unsafeProtector }
}

private func validatePrivateSnapshotFile(_ url: URL, maximumBytes: Int64) throws {
  var metadata = stat()
  let parent = url.deletingLastPathComponent()
  guard lstat(url.path, &metadata) == 0, metadata.st_mode & S_IFMT == S_IFREG,
    metadata.st_uid == getuid(), metadata.st_nlink == 1, metadata.st_mode & 0o077 == 0,
    metadata.st_size > 0, metadata.st_size <= maximumBytes
  else { throw HistorySnapshotCreationError.unsafeProtector }
  try validateOwnerOnlySnapshotDirectory(parent)
}

private func isSnapshotHexadecimal(_ value: UInt8) -> Bool {
  (value >= 48 && value <= 57) || (value >= 65 && value <= 70) || (value >= 97 && value <= 102)
}

private func containsSecret(_ data: Data, secret: [UInt8]) -> Bool {
  guard secret.count >= 12 else { return false }
  return data.range(of: Data(secret)) != nil
}

private final class SnapshotSecureBytes: @unchecked Sendable {
  let count: Int
  private let storage: UnsafeMutableRawPointer?

  init(copying bytes: [UInt8]) {
    count = bytes.count
    guard !bytes.isEmpty else {
      storage = nil
      return
    }
    let allocated = UnsafeMutableRawPointer.allocate(
      byteCount: bytes.count,
      alignment: MemoryLayout<UInt8>.alignment
    )
    bytes.withUnsafeBytes { source in
      if let baseAddress = source.baseAddress {
        allocated.copyMemory(from: baseAddress, byteCount: bytes.count)
      }
    }
    storage = allocated
  }

  deinit {
    clear()
    storage?.deallocate()
  }

  func data() -> Data {
    guard let storage else { return Data() }
    return Data(bytes: storage, count: count)
  }

  func clear() {
    guard let storage else { return }
    memset_s(storage, count, 0, count)
  }
}

private func runSnapshotProcessSynchronously(
  executableURL: URL,
  arguments: [String],
  standardInput: SnapshotSecureBytes,
  maximumOutputBytes: Int,
  maximumErrorBytes: Int,
  timeoutMilliseconds: Int
) throws -> Data {
  let input = Pipe()
  let output = SnapshotBoundedProcessStream(maximumBytes: maximumOutputBytes)
  let errors = SnapshotBoundedProcessStream(maximumBytes: maximumErrorBytes)
  let process = Process()
  process.executableURL = executableURL
  process.arguments = arguments
  process.standardInput = input
  process.standardOutput = output.pipe
  process.standardError = errors.pipe
  let completion = DispatchSemaphore(value: 0)
  process.terminationHandler = { _ in completion.signal() }
  output.start()
  errors.start()
  do {
    try process.run()
    try output.closeParentWriter()
    try errors.closeParentWriter()
    var data = standardInput.data()
    defer { data.resetBytes(in: 0..<data.count) }
    try input.fileHandleForWriting.write(contentsOf: data)
    try input.fileHandleForWriting.close()
  } catch {
    if process.isRunning { process.terminate() }
    output.stop()
    errors.stop()
    throw HistorySnapshotCreationError.commandFailed
  }

  let deadline = DispatchTime.now() + .milliseconds(timeoutMilliseconds)
  while completion.wait(timeout: .now() + .milliseconds(50)) == .timedOut {
    if Task.isCancelled {
      terminateSnapshotProcess(process, completion: completion)
      output.stop()
      errors.stop()
      throw CancellationError()
    }
    if output.overflowed || errors.overflowed {
      terminateSnapshotProcess(process, completion: completion)
      output.stop()
      errors.stop()
      throw HistorySnapshotCreationError.responseTooLarge
    }
    if DispatchTime.now() >= deadline {
      terminateSnapshotProcess(process, completion: completion)
      output.stop()
      errors.stop()
      throw HistorySnapshotCreationError.timedOut
    }
  }
  let stdout = try output.finish()
  _ = try errors.finish()
  guard !output.overflowed, !errors.overflowed else {
    throw HistorySnapshotCreationError.responseTooLarge
  }
  guard process.terminationStatus == 0 else {
    throw HistorySnapshotCreationError.commandFailed
  }
  return stdout
}

private final class SnapshotBoundedProcessStream: @unchecked Sendable {
  let pipe = Pipe()
  private let maximumBytes: Int
  private let queue = DispatchQueue(label: "greenbubbles.history.snapshot-stream")
  private let lock = NSLock()
  private var data = Data()
  private var readError: Error?
  private var reachedEnd = false
  private var didOverflow = false
  private let finished = DispatchSemaphore(value: 0)

  init(maximumBytes: Int) { self.maximumBytes = maximumBytes }

  var overflowed: Bool { lock.withLock { didOverflow } }

  func start() {
    queue.async { [self] in
      defer {
        lock.withLock { reachedEnd = true }
        finished.signal()
      }
      do {
        while true {
          let chunk = try pipe.fileHandleForReading.read(upToCount: 64 * 1_024) ?? Data()
          if chunk.isEmpty { break }
          let stop = lock.withLock { () -> Bool in
            if data.count > maximumBytes - min(maximumBytes, chunk.count) {
              didOverflow = true
              return true
            }
            data.append(chunk)
            return false
          }
          if stop { break }
        }
      } catch {
        lock.withLock { readError = error }
      }
    }
  }

  func closeParentWriter() throws { try pipe.fileHandleForWriting.close() }

  func finish() throws -> Data {
    if !lock.withLock({ reachedEnd }) { finished.wait() }
    return try lock.withLock {
      if let readError { throw readError }
      return data
    }
  }

  func stop() { try? pipe.fileHandleForReading.close() }
}

private func terminateSnapshotProcess(_ process: Process, completion: DispatchSemaphore) {
  if process.isRunning { process.terminate() }
  if completion.wait(timeout: .now() + .seconds(1)) == .timedOut, process.isRunning {
    kill(process.processIdentifier, SIGKILL)
    _ = completion.wait(timeout: .now() + .seconds(1))
  }
}
