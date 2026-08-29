import CryptoKit
import Darwin
import Foundation

public struct HistoryLiveMediaConfiguration: Equatable, Sendable {
  public let executableURL: URL
  public let replicaURL: URL
  public let policyURL: URL
  public let auditURL: URL
  public let sessionDirectory: URL
  public let scratchDirectory: URL
  public let previewDirectory: URL
  public let expectedAccountID: String
  public let expectedReplicaID: String
  public let expectedSourceFingerprint: String

  public init(
    executableURL: URL,
    replicaURL: URL,
    policyURL: URL,
    auditURL: URL,
    sessionDirectory: URL,
    scratchDirectory: URL,
    previewDirectory: URL,
    expectedAccountID: String,
    expectedReplicaID: String,
    expectedSourceFingerprint: String
  ) {
    self.executableURL = executableURL
    self.replicaURL = replicaURL
    self.policyURL = policyURL
    self.auditURL = auditURL
    self.sessionDirectory = sessionDirectory
    self.scratchDirectory = scratchDirectory
    self.previewDirectory = previewDirectory
    self.expectedAccountID = expectedAccountID
    self.expectedReplicaID = expectedReplicaID
    self.expectedSourceFingerprint = expectedSourceFingerprint
  }
}

public struct HistoryMediaResolutionProgress: Equatable, Sendable {
  public enum Phase: String, Equatable, Sendable {
    case requestingAuthorization
    case verifyingAndCopying
    case ready
  }

  public let phase: Phase
  public let completedBytes: UInt64
  public let totalBytes: UInt64

  public init(phase: Phase, completedBytes: UInt64, totalBytes: UInt64) {
    self.phase = phase
    self.completedBytes = completedBytes
    self.totalBytes = totalBytes
  }

  public var fraction: Double {
    totalBytes == 0
      ? (phase == .ready ? 1 : 0) : min(1, Double(completedBytes) / Double(totalBytes))
  }
}

public struct HistoryVerifiedMedia: Equatable, Sendable {
  public let artifactID: String
  public let kind: String
  public let format: String
  public let byteCount: UInt64
  public let sha256: String
  public let previewURL: URL

  public init(
    artifactID: String,
    kind: String,
    format: String,
    byteCount: UInt64,
    sha256: String,
    previewURL: URL
  ) {
    self.artifactID = artifactID
    self.kind = kind
    self.format = format
    self.byteCount = byteCount
    self.sha256 = sha256
    self.previewURL = previewURL
  }
}

public enum HistoryLiveMediaError: Error, Equatable, CustomStringConvertible, Sendable {
  case invalidConfiguration(String)
  case invalidReplicaKey
  case requestFailed(String)
  case requestTimedOut
  case unauthorized(String)
  case invalidResponse
  case artifactMismatch
  case sourceChanged
  case insufficientSpace
  case previewFailed(String)

  public var description: String {
    switch self {
    case .invalidConfiguration(let detail): "Live media configuration is invalid: \(detail)"
    case .invalidReplicaKey: "The replica key must contain exactly 64 hexadecimal characters."
    case .requestFailed(let detail): "GreenBubbles media request failed: \(detail)"
    case .requestTimedOut: "GreenBubbles media verification timed out."
    case .unauthorized(let detail): "Media access was denied: \(detail)"
    case .invalidResponse: "GreenBubbles returned an invalid media response."
    case .artifactMismatch: "The verified response does not match the requested artifact."
    case .sourceChanged: "The media file changed while it was being verified."
    case .insufficientSpace: "There is not enough free space for a private preview copy."
    case .previewFailed(let detail): "The private media preview could not be created: \(detail)"
    }
  }
}

public struct HistoryLiveMediaResolver: Sendable {
  public typealias ProgressHandler = @Sendable (HistoryMediaResolutionProgress) -> Void
  private static let maximumResponseBytes = 4 * 1_024 * 1_024

  public init() {}

  public func resolve(
    conversationID: String,
    artifactID: String,
    configuration: HistoryLiveMediaConfiguration,
    replicaKeyUTF8: [UInt8],
    progress: @escaping ProgressHandler = { _ in }
  ) async throws -> HistoryVerifiedMedia {
    try Task.checkCancellation()
    let worker = Task.detached(priority: .userInitiated) {
      var key = replicaKeyUTF8
      defer { key.resetBytes(in: 0..<key.count) }
      guard key.count == 64, key.allSatisfy(isHexadecimal) else {
        throw HistoryLiveMediaError.invalidReplicaKey
      }
      return try resolveSynchronously(
        conversationID: conversationID,
        artifactID: artifactID,
        configuration: configuration,
        key: key,
        progress: progress
      )
    }
    return try await withTaskCancellationHandler {
      try await worker.value
    } onCancel: {
      worker.cancel()
    }
  }

  private func resolveSynchronously(
    conversationID: String,
    artifactID: String,
    configuration: HistoryLiveMediaConfiguration,
    key: [UInt8],
    progress: ProgressHandler
  ) throws -> HistoryVerifiedMedia {
    guard !conversationID.isEmpty, !artifactID.isEmpty else {
      throw HistoryLiveMediaError.invalidConfiguration("artifact identity is empty")
    }
    guard !configuration.expectedAccountID.isEmpty, !configuration.expectedReplicaID.isEmpty,
      !configuration.expectedSourceFingerprint.isEmpty
    else {
      throw HistoryLiveMediaError.invalidConfiguration("expected replica identity is empty")
    }
    try validateExecutable(configuration.executableURL)
    guard
      configuration.scratchDirectory.deletingLastPathComponent().standardizedFileURL
        == configuration.sessionDirectory.standardizedFileURL,
      configuration.previewDirectory.deletingLastPathComponent().standardizedFileURL
        == configuration.sessionDirectory.standardizedFileURL,
      configuration.scratchDirectory.standardizedFileURL
        != configuration.previewDirectory.standardizedFileURL
    else {
      throw HistoryLiveMediaError.invalidConfiguration(
        "request and preview directories must be separate children of the private session")
    }
    try ensurePrivateMediaDirectory(configuration.sessionDirectory)
    try ensurePrivateMediaDirectory(configuration.scratchDirectory)
    try ensurePrivateMediaDirectory(configuration.previewDirectory)
    progress(
      HistoryMediaResolutionProgress(
        phase: .requestingAuthorization, completedBytes: 0, totalBytes: 0))

    let identifier = UUID().uuidString
    let requestID = "history-preview-\(identifier)"
    let requestURL = configuration.scratchDirectory.appending(path: "\(identifier)-request.json")
    defer {
      try? FileManager.default.removeItem(at: requestURL)
    }
    let request: [String: Any] = [
      "formatVersion": 1,
      "requestId": requestID,
      "requesterId": "greenbubbles-history",
      "destination": "local",
      "operation": [
        "kind": "getArtifact",
        "conversationId": conversationID,
        "artifactId": artifactID,
      ],
    ]
    let requestData = try JSONSerialization.data(
      withJSONObject: request, options: [.sortedKeys, .withoutEscapingSlashes])
    try createPrivateMediaFile(requestURL, data: requestData)
    let input = Pipe()
    let output = BoundedProcessStream(maximumBytes: Self.maximumResponseBytes)
    let errors = BoundedProcessStream(maximumBytes: 256 * 1_024)
    let process = Process()
    process.executableURL = configuration.executableURL
    process.arguments = [
      "ai-query",
      configuration.replicaURL.path,
      configuration.policyURL.path,
      configuration.auditURL.path,
      requestURL.path,
      "--replica-key-stdin",
    ]
    process.currentDirectoryURL = configuration.scratchDirectory
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
      var inputData = Data(key)
      inputData.append(0x0A)
      defer { inputData.resetBytes(in: 0..<inputData.count) }
      try input.fileHandleForWriting.write(contentsOf: inputData)
      try input.fileHandleForWriting.close()
    } catch {
      if process.isRunning { process.terminate() }
      output.stop()
      errors.stop()
      throw HistoryLiveMediaError.requestFailed("the local CLI could not be started")
    }
    let deadline = DispatchTime.now() + 60
    while completion.wait(timeout: .now() + .milliseconds(50)) == .timedOut {
      if Task.isCancelled {
        terminateMediaProcess(process, completion: completion)
        output.stop()
        errors.stop()
        throw CancellationError()
      }
      if output.overflowed || errors.overflowed {
        terminateMediaProcess(process, completion: completion)
        output.stop()
        errors.stop()
        throw HistoryLiveMediaError.invalidResponse
      }
      if DispatchTime.now() >= deadline {
        terminateMediaProcess(process, completion: completion)
        output.stop()
        errors.stop()
        throw HistoryLiveMediaError.requestTimedOut
      }
    }
    let responseData = output.finish()
    _ = errors.finish()
    try Task.checkCancellation()
    guard !output.overflowed, !errors.overflowed else {
      throw HistoryLiveMediaError.invalidResponse
    }
    guard process.terminationReason == .exit, process.terminationStatus == 0 else {
      throw HistoryLiveMediaError.requestFailed(
        "the policy-scoped local query was not completed")
    }
    let response: LiveQueryResponse
    do {
      response = try JSONDecoder().decode(LiveQueryResponse.self, from: responseData)
    } catch {
      throw HistoryLiveMediaError.invalidResponse
    }
    guard response.formatVersion == 1, response.schema == "greenbubbles.ai-query.v1",
      response.apiVersion == "greenbubbles.connector.v1", response.requestID == requestID,
      response.context.accountID == configuration.expectedAccountID,
      response.context.replicaID == configuration.expectedReplicaID,
      response.context.sourceFingerprint == configuration.expectedSourceFingerprint,
      !response.context.checkpointRevision.isEmpty
    else {
      throw HistoryLiveMediaError.invalidResponse
    }
    guard response.ok else {
      throw HistoryLiveMediaError.unauthorized(
        response.error?.message ?? "the current policy does not allow this artifact")
    }
    guard response.result?.kind == "artifact", let artifact = response.result?.value else {
      throw HistoryLiveMediaError.invalidResponse
    }
    guard artifact.artifactID == artifactID else {
      throw HistoryLiveMediaError.artifactMismatch
    }
    let selected = artifact.decoded ?? artifact.source
    guard let selected else {
      throw HistoryLiveMediaError.unauthorized(
        "no verified local file is available for this artifact")
    }
    return try createPreview(
      artifactID: artifactID,
      kind: artifact.kind,
      file: selected,
      previewDirectory: configuration.previewDirectory,
      progress: progress
    )
  }

  private func createPreview(
    artifactID: String,
    kind: String,
    file: LiveArtifactFile,
    previewDirectory: URL,
    progress: ProgressHandler
  ) throws -> HistoryVerifiedMedia {
    guard file.absolutePath.hasPrefix("/"), !file.absolutePath.contains("\0"),
      isMediaSHA256(file.sha256), file.byteCount <= UInt64(Int64.max)
    else {
      throw HistoryLiveMediaError.invalidResponse
    }
    let source = open(file.absolutePath, O_RDONLY | O_CLOEXEC | O_NOFOLLOW)
    guard source >= 0 else {
      throw HistoryLiveMediaError.sourceChanged
    }
    defer { close(source) }
    var before = stat()
    guard fstat(source, &before) == 0, before.st_mode & S_IFMT == S_IFREG,
      before.st_uid == getuid(), before.st_size >= 0,
      UInt64(before.st_size) == file.byteCount
    else {
      throw HistoryLiveMediaError.sourceChanged
    }
    if let values = try? previewDirectory.resourceValues(forKeys: [
      .volumeAvailableCapacityForImportantUsageKey
    ]),
      let capacity = values.volumeAvailableCapacityForImportantUsage,
      capacity < Int64(file.byteCount) + 64 * 1_024 * 1_024
    {
      throw HistoryLiveMediaError.insufficientSpace
    }

    let fileExtension = safeMediaExtension(file.format)
    let name = UUID().uuidString + (fileExtension.isEmpty ? "" : ".\(fileExtension)")
    let temporaryURL = previewDirectory.appending(path: ".\(name).tmp")
    let finalURL = previewDirectory.appending(path: name)
    let destination = open(
      temporaryURL.path, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW, 0o600)
    guard destination >= 0 else {
      throw HistoryLiveMediaError.previewFailed("a private output file could not be created")
    }
    var published = false
    defer {
      close(destination)
      if !published { try? FileManager.default.removeItem(at: temporaryURL) }
    }
    var hasher = SHA256()
    var buffer = [UInt8](repeating: 0, count: 1_024 * 1_024)
    var completed: UInt64 = 0
    var lastReported: UInt64 = 0
    progress(
      HistoryMediaResolutionProgress(
        phase: .verifyingAndCopying, completedBytes: 0, totalBytes: file.byteCount))
    while true {
      try Task.checkCancellation()
      let count = Darwin.read(source, &buffer, buffer.count)
      guard count >= 0 else { throw HistoryLiveMediaError.sourceChanged }
      if count == 0 { break }
      let data = Data(buffer[0..<count])
      hasher.update(data: data)
      try writeAllMediaBytes(destination, buffer: buffer, count: count)
      completed += UInt64(count)
      guard completed <= file.byteCount else { throw HistoryLiveMediaError.sourceChanged }
      if completed - lastReported >= 8 * 1_024 * 1_024 {
        progress(
          HistoryMediaResolutionProgress(
            phase: .verifyingAndCopying,
            completedBytes: completed,
            totalBytes: file.byteCount
          ))
        lastReported = completed
      }
    }
    var after = stat()
    guard completed == file.byteCount, fstat(source, &after) == 0,
      sameMediaIdentity(before, after),
      hasher.finalize().map({ String(format: "%02x", $0) }).joined() == file.sha256,
      fsync(destination) == 0
    else {
      throw HistoryLiveMediaError.sourceChanged
    }
    try Task.checkCancellation()
    guard rename(temporaryURL.path, finalURL.path) == 0 else {
      throw HistoryLiveMediaError.previewFailed("the verified copy could not be published")
    }
    published = true
    progress(
      HistoryMediaResolutionProgress(
        phase: .ready, completedBytes: completed, totalBytes: file.byteCount))
    return HistoryVerifiedMedia(
      artifactID: artifactID,
      kind: kind,
      format: file.format,
      byteCount: file.byteCount,
      sha256: file.sha256,
      previewURL: finalURL
    )
  }
}

private struct LiveQueryResponse: Decodable {
  let formatVersion: Int
  let schema: String
  let apiVersion: String
  let requestID: String
  let ok: Bool
  let context: LiveQueryContext
  let result: LiveQueryResult?
  let error: LiveQueryError?

  enum CodingKeys: String, CodingKey {
    case formatVersion
    case schema
    case apiVersion
    case requestID = "requestId"
    case ok
    case context
    case result
    case error
  }
}

private struct LiveQueryContext: Decodable {
  let accountID: String
  let replicaID: String
  let sourceFingerprint: String
  let checkpointRevision: String

  enum CodingKeys: String, CodingKey {
    case accountID = "accountId"
    case replicaID = "replicaId"
    case sourceFingerprint
    case checkpointRevision
  }
}

private struct LiveQueryResult: Decodable {
  let kind: String
  let value: LiveArtifactView?
}

private struct LiveQueryError: Decodable {
  let message: String
}

private struct LiveArtifactView: Decodable {
  let artifactID: String
  let kind: String
  let source: LiveArtifactFile?
  let decoded: LiveArtifactFile?

  enum CodingKeys: String, CodingKey {
    case artifactID = "artifactId"
    case kind
    case source
    case decoded
  }
}

private struct LiveArtifactFile: Decodable {
  let absolutePath: String
  let byteCount: UInt64
  let sha256: String
  let format: String
}

private func isHexadecimal(_ byte: UInt8) -> Bool {
  (48...57).contains(byte) || (65...70).contains(byte) || (97...102).contains(byte)
}

private func validateExecutable(_ url: URL) throws {
  var metadata = stat()
  guard url.isFileURL, url.path.hasPrefix("/"), lstat(url.path, &metadata) == 0,
    metadata.st_mode & S_IFMT == S_IFREG,
    metadata.st_uid == getuid() || metadata.st_uid == 0,
    metadata.st_mode & 0o022 == 0,
    access(url.path, X_OK) == 0
  else {
    throw HistoryLiveMediaError.invalidConfiguration(
      "choose the local greenbubbles executable")
  }
}

private func ensurePrivateMediaDirectory(_ url: URL) throws {
  if !FileManager.default.fileExists(atPath: url.path) {
    try FileManager.default.createDirectory(
      at: url, withIntermediateDirectories: true,
      attributes: [.posixPermissions: 0o700])
  }
  var metadata = stat()
  guard lstat(url.path, &metadata) == 0, metadata.st_mode & S_IFMT == S_IFDIR,
    metadata.st_uid == getuid(), metadata.st_mode & 0o077 == 0
  else {
    throw HistoryLiveMediaError.invalidConfiguration(
      "scratch and preview directories must be current-user-owned mode 0700")
  }
}

private func createPrivateMediaFile(_ url: URL, data: Data) throws {
  let descriptor = open(
    url.path, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW, 0o600)
  guard descriptor >= 0 else {
    throw HistoryLiveMediaError.requestFailed("a private request file could not be created")
  }
  defer { close(descriptor) }
  try data.withUnsafeBytes { bytes in
    var completed = 0
    while completed < bytes.count {
      let written = Darwin.write(
        descriptor, bytes.baseAddress!.advanced(by: completed), bytes.count - completed)
      guard written > 0 else {
        throw HistoryLiveMediaError.requestFailed("a private request file could not be written")
      }
      completed += written
    }
  }
  guard fsync(descriptor) == 0 else {
    throw HistoryLiveMediaError.requestFailed("a private request file could not be synchronized")
  }
}

private func writeAllMediaBytes(_ descriptor: Int32, buffer: [UInt8], count: Int) throws {
  var completed = 0
  while completed < count {
    let written = buffer.withUnsafeBytes { bytes in
      Darwin.write(descriptor, bytes.baseAddress!.advanced(by: completed), count - completed)
    }
    guard written > 0 else {
      throw HistoryLiveMediaError.previewFailed("the verified copy could not be written")
    }
    completed += written
  }
}

private func isMediaSHA256(_ value: String) -> Bool {
  value.utf8.count == 64
    && value.utf8.allSatisfy {
      (48...57).contains($0) || (97...102).contains($0)
    }
}

private func safeMediaExtension(_ format: String) -> String {
  let value = format.lowercased()
  guard value.count <= 12, !value.isEmpty,
    value.utf8.allSatisfy({ (48...57).contains($0) || (97...122).contains($0) })
  else { return "bin" }
  return value == "jpeg" ? "jpg" : value
}

private func sameMediaIdentity(_ left: stat, _ right: stat) -> Bool {
  left.st_dev == right.st_dev && left.st_ino == right.st_ino
    && left.st_size == right.st_size
    && left.st_mtimespec.tv_sec == right.st_mtimespec.tv_sec
    && left.st_mtimespec.tv_nsec == right.st_mtimespec.tv_nsec
}

private final class BoundedProcessStream: @unchecked Sendable {
  let pipe = Pipe()
  private let maximumBytes: Int
  private let lock = NSLock()
  private let completed = DispatchSemaphore(value: 0)
  private var storage = Data()
  private var didOverflow = false

  init(maximumBytes: Int) {
    self.maximumBytes = maximumBytes
  }

  var overflowed: Bool { lock.withLock { didOverflow } }

  func start() {
    DispatchQueue.global(qos: .userInitiated).async { [self] in
      defer { completed.signal() }
      while true {
        let chunk: Data
        do {
          guard let value = try pipe.fileHandleForReading.read(upToCount: 64 * 1_024),
            !value.isEmpty
          else { return }
          chunk = value
        } catch {
          return
        }
        let shouldStop = lock.withLock {
          if chunk.count > maximumBytes - storage.count {
            didOverflow = true
            return true
          }
          storage.append(chunk)
          return false
        }
        if shouldStop { return }
      }
    }
  }

  func closeParentWriter() throws {
    try pipe.fileHandleForWriting.close()
  }

  func finish() -> Data {
    if completed.wait(timeout: .now() + 2) == .timedOut {
      stop()
      _ = completed.wait(timeout: .now() + 1)
    }
    return lock.withLock { storage }
  }

  func stop() {
    try? pipe.fileHandleForReading.close()
    try? pipe.fileHandleForWriting.close()
  }
}

private func terminateMediaProcess(_ process: Process, completion: DispatchSemaphore) {
  if process.isRunning { process.terminate() }
  if completion.wait(timeout: .now() + 2) == .timedOut {
    kill(process.processIdentifier, SIGKILL)
    _ = completion.wait(timeout: .now() + 2)
  }
}
