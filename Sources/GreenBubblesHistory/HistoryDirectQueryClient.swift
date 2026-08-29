import Darwin
import Foundation

public enum HistoryDirectAccessMode: String, CaseIterable, Equatable, Sendable {
  case liveEncrypted
  case snapshotKeychain
  case snapshotLocalCredential
  case snapshotPassphrase
  case snapshotRecoveryKit
  case snapshotEncrypted
  case decrypted

  fileprivate var responseMode: String {
    switch self {
    case .liveEncrypted: "liveEncrypted"
    case .snapshotKeychain, .snapshotLocalCredential, .snapshotPassphrase, .snapshotRecoveryKit,
      .snapshotEncrypted:
      "snapshotEncrypted"
    case .decrypted: "decrypted"
    }
  }

  public var displayName: String {
    switch self {
    case .liveEncrypted: "Live WeChat (read-only)"
    case .snapshotKeychain: "Snapshot unlock in macOS Keychain"
    case .snapshotLocalCredential: "Snapshot hidden-file unlock"
    case .snapshotPassphrase: "Snapshot passphrase (Argon2id)"
    case .snapshotRecoveryKit: "Snapshot recovery words (portable)"
    case .snapshotEncrypted: "Legacy snapshot raw key"
    case .decrypted: "Plaintext SQLite (explicit)"
    }
  }
}

public struct HistoryDirectConfiguration: Equatable, Sendable {
  public let executableURL: URL
  public let sourceURL: URL
  public let accessMode: HistoryDirectAccessMode
  public let recoveryKitURL: URL?
  public let localCredentialURL: URL?

  public init(
    executableURL: URL,
    sourceURL: URL,
    accessMode: HistoryDirectAccessMode,
    recoveryKitURL: URL? = nil,
    localCredentialURL: URL? = nil
  ) {
    self.executableURL = executableURL.standardizedFileURL
    self.sourceURL = sourceURL.standardizedFileURL
    self.accessMode = accessMode
    self.recoveryKitURL = recoveryKitURL?.standardizedFileURL
    self.localCredentialURL = localCredentialURL?.standardizedFileURL
  }

  fileprivate var accessArguments: [String] {
    switch accessMode {
    case .liveEncrypted: ["--passphrase-stdin"]
    case .snapshotKeychain, .snapshotLocalCredential:
      localCredentialURL.map { ["--snapshot-local-credential", $0.path] } ?? []
    case .snapshotPassphrase: ["--snapshot-passphrase-stdin"]
    case .snapshotRecoveryKit:
      recoveryKitURL.map { ["--snapshot-recovery-kit", $0.path] } ?? []
    case .snapshotEncrypted: ["--snapshot-key-stdin"]
    case .decrypted: ["--decrypted"]
    }
  }
}

public struct HistoryDirectSource: Codable, Equatable, Sendable {
  public let mode: String
  public let identity: String
}

public struct HistoryDirectConsistency: Codable, Equatable, Sendable {
  public let guarantee: String
  public let databaseCount: Int
  public let crossDatabaseAtomic: Bool
  public let coverageComplete: Bool
  public let observedAtUnixMilliseconds: UInt64
}

public struct HistoryDirectWarning: Codable, Equatable, Identifiable, Sendable {
  public let code: String
  public let message: String
  public let shardID: UInt32?
  public let count: Int?

  public var id: String { "\(code):\(shardID.map(String.init) ?? "all")" }

  enum CodingKeys: String, CodingKey {
    case code
    case message
    case shardID = "shardId"
    case count
  }
}

public struct HistoryDirectPage: Codable, Equatable, Sendable {
  public let limit: Int
  public let returned: Int
  public let hasMore: Bool
  public let nextCursor: String?
}

public struct HistoryDirectConversation: Codable, Equatable, Identifiable, Sendable {
  public let id: String
  public let contactDisplayName: String?
  public let summary: String?
  public let summaryDecodeState: String
  public let summaryTruncated: Bool
  public let sortTimestamp: Int64
  public let lastMessageType: UInt32?
  public let lastMessageSender: String?
  public let lastSenderDisplayName: String?

  public var displayName: String {
    if let contactDisplayName, !contactDisplayName.isEmpty { return contactDisplayName }
    guard !id.isEmpty else { return "Unknown conversation" }
    return id
  }

  public var sortDate: Date? {
    guard sortTimestamp > 0 else { return nil }
    // Observed WeChat session schemas may use seconds or milliseconds.
    let seconds =
      sortTimestamp > 10_000_000_000 ? Double(sortTimestamp) / 1_000 : Double(sortTimestamp)
    return Date(timeIntervalSince1970: seconds)
  }

  enum CodingKeys: String, CodingKey {
    case id
    case contactDisplayName = "displayName"
    case summary
    case summaryDecodeState
    case summaryTruncated
    case sortTimestamp
    case lastMessageType
    case lastMessageSender
    case lastSenderDisplayName
  }
}

public enum HistoryDirectJSONValue: Codable, Equatable, Sendable {
  case null
  case bool(Bool)
  case number(Double)
  case string(String)
  case array([HistoryDirectJSONValue])
  case object([String: HistoryDirectJSONValue])

  public init(from decoder: Decoder) throws {
    let value = try decoder.singleValueContainer()
    if value.decodeNil() {
      self = .null
    } else if let decoded = try? value.decode(Bool.self) {
      self = .bool(decoded)
    } else if let decoded = try? value.decode(Double.self) {
      self = .number(decoded)
    } else if let decoded = try? value.decode(String.self) {
      self = .string(decoded)
    } else if let decoded = try? value.decode([HistoryDirectJSONValue].self) {
      self = .array(decoded)
    } else {
      self = .object(try value.decode([String: HistoryDirectJSONValue].self))
    }
  }

  public func encode(to encoder: Encoder) throws {
    var value = encoder.singleValueContainer()
    switch self {
    case .null: try value.encodeNil()
    case .bool(let decoded): try value.encode(decoded)
    case .number(let decoded): try value.encode(decoded)
    case .string(let decoded): try value.encode(decoded)
    case .array(let decoded): try value.encode(decoded)
    case .object(let decoded): try value.encode(decoded)
    }
  }

  public var displayText: String {
    switch self {
    case .null: return "[No content]"
    case .bool(let value): return value ? "true" : "false"
    case .number(let value): return value.formatted()
    case .string(let value): return value
    case .array(let values):
      return values.first?.displayText ?? "[Empty content]"
    case .object(let values):
      if case .string(let text)? = values["Text"] { return text }
      if case .string(let reason)? = values["unavailable"] {
        return "[Content unavailable: \(reason)]"
      }
      if let first = values.sorted(by: { $0.key < $1.key }).first {
        switch first.value {
        case .string(let text) where !text.isEmpty: return text
        case .object(let nested):
          for key in ["title", "description", "content", "url"] {
            if case .string(let text)? = nested[key], !text.isEmpty { return text }
          }
          return "[\(first.key)]"
        default: return "[\(first.key)]"
        }
      }
      return "[Message content unavailable]"
    }
  }
}

public struct HistoryDirectMessage: Codable, Equatable, Identifiable, Sendable {
  public let id: String
  public let conversationID: String
  public let sortSequence: Int64
  public let serverID: Int64
  public let messageType: UInt32
  public let messageTypeLabel: String
  public let messageSubtype: UInt32
  public let messageSubtypeLabel: String
  public let sender: String
  public let senderDisplayName: String?
  public let createdAtUnix: Int64
  public let status: Int32
  public let content: HistoryDirectJSONValue
  public let contentDecodeState: String
  public let contentTruncated: Bool

  public var createdAt: Date { Date(timeIntervalSince1970: TimeInterval(createdAtUnix)) }
  public var displayText: String { content.displayText }
  public var senderLabel: String {
    if let senderDisplayName, !senderDisplayName.isEmpty { return senderDisplayName }
    return sender.isEmpty ? "Unknown sender" : sender
  }

  enum CodingKeys: String, CodingKey {
    case id
    case conversationID = "conversationId"
    case sortSequence
    case serverID = "serverId"
    case messageType
    case messageTypeLabel
    case messageSubtype
    case messageSubtypeLabel
    case sender
    case senderDisplayName
    case createdAtUnix
    case status
    case content
    case contentDecodeState
    case contentTruncated
  }
}

public struct HistoryDirectSearchHit: Codable, Equatable, Identifiable, Sendable {
  public let id: String
  public let conversationID: String
  public let sender: String
  public let senderDisplayName: String?
  public let createdAtUnix: Int64
  public let sortSequence: Int64
  public let messageLocalID: Int64
  public let messageType: UInt32
  public let messageTypeLabel: String
  public let messageSubtype: UInt32
  public let messageSubtypeLabel: String
  public let snippet: String
  public let snippetTruncated: Bool

  public var createdAt: Date { Date(timeIntervalSince1970: TimeInterval(createdAtUnix)) }
  public var senderLabel: String {
    if let senderDisplayName, !senderDisplayName.isEmpty { return senderDisplayName }
    return sender
  }

  enum CodingKeys: String, CodingKey {
    case id
    case conversationID = "conversationId"
    case sender
    case senderDisplayName
    case createdAtUnix
    case sortSequence
    case messageLocalID = "messageLocalId"
    case messageType
    case messageTypeLabel
    case messageSubtype
    case messageSubtypeLabel
    case snippet
    case snippetTruncated
  }
}

public struct HistoryDirectConversationPage: Codable, Equatable, Sendable {
  public let schema: String
  public let formatVersion: Int
  public let operation: String
  public let ok: Bool
  public let source: HistoryDirectSource
  public let consistency: HistoryDirectConsistency
  public let page: HistoryDirectPage
  public let warnings: [HistoryDirectWarning]
  public let items: [HistoryDirectConversation]
}

public struct HistoryDirectMessagePage: Codable, Equatable, Sendable {
  public let schema: String
  public let formatVersion: Int
  public let operation: String
  public let ok: Bool
  public let source: HistoryDirectSource
  public let consistency: HistoryDirectConsistency
  public let page: HistoryDirectPage
  public let warnings: [HistoryDirectWarning]
  public let items: [HistoryDirectMessage]
}

public struct HistoryDirectSearchPage: Codable, Equatable, Sendable {
  public let schema: String
  public let formatVersion: Int
  public let operation: String
  public let ok: Bool
  public let source: HistoryDirectSource
  public let consistency: HistoryDirectConsistency
  public let page: HistoryDirectPage
  public let warnings: [HistoryDirectWarning]
  public let items: [HistoryDirectSearchHit]
}

public struct HistoryDirectMessageResource: Codable, Equatable, Sendable {
  public let schema: String
  public let formatVersion: Int
  public let operation: String
  public let ok: Bool
  public let source: HistoryDirectSource
  public let consistency: HistoryDirectConsistency
  public let warnings: [HistoryDirectWarning]
  public let item: HistoryDirectMessage
}

public struct HistoryDirectDatabaseSize: Codable, Equatable, Identifiable, Sendable {
  public let relativePath: String
  public let databaseBytes: UInt64
  public let writeAheadLogBytes: UInt64
  public let sharedMemoryBytes: UInt64
  public let rollbackJournalBytes: UInt64

  public var id: String { relativePath }
}

public struct HistoryDirectSourceStatus: Codable, Equatable, Sendable {
  public let schema: String
  public let formatVersion: Int
  public let operation: String
  public let ok: Bool
  public let source: HistoryDirectSource
  public let observedAtUnixMilliseconds: UInt64
  public let databaseCount: Int
  public let databaseBytes: UInt64
  public let writeAheadLogCount: Int
  public let writeAheadLogBytes: UInt64
  public let sharedMemoryCount: Int
  public let sharedMemoryBytes: UInt64
  public let rollbackJournalCount: Int
  public let rollbackJournalBytes: UInt64
  public let totalSqliteStorageBytes: UInt64
  public let entries: [HistoryDirectDatabaseSize]
}

public enum HistoryDirectQueryError: Error, Equatable, CustomStringConvertible, Sendable {
  case invalidConfiguration(String)
  case invalidKey
  case invalidQuery
  case timedOut
  case responseTooLarge
  case commandFailed(code: String, message: String, retryable: Bool)
  case invalidResponse

  public var description: String {
    switch self {
    case .invalidConfiguration(let detail): "Direct history configuration is invalid: \(detail)"
    case .invalidKey: "The selected encrypted source requires a 32-byte or 64-hex key."
    case .invalidQuery: "The search query is empty or outside the 16 KiB limit."
    case .timedOut: "The bounded local query timed out."
    case .responseTooLarge: "The local query exceeded its fixed response bound."
    case .commandFailed(_, let message, _): message
    case .invalidResponse: "GreenBubbles returned an invalid bounded-query response."
    }
  }
}

public struct HistoryDirectQueryClient: Sendable {
  private static let schema = "greenbubbles.query.v1"
  private static let maximumResponseBytes = 8 * 1_024 * 1_024
  private static let maximumErrorBytes = 256 * 1_024
  private let timeoutMilliseconds: Int

  public init() {
    timeoutMilliseconds = 20_000
  }

  init(timeoutMillisecondsForTesting: Int) {
    timeoutMilliseconds = max(1, timeoutMillisecondsForTesting)
  }

  public func status(
    configuration: HistoryDirectConfiguration,
    keyUTF8: [UInt8]
  ) async throws -> HistoryDirectSourceStatus {
    try await request(
      configuration: configuration,
      keyUTF8: keyUTF8,
      operation: "source.status",
      arguments: ["source", "status", configuration.sourceURL.path],
      query: nil,
      type: HistoryDirectSourceStatus.self
    )
  }

  public func conversations(
    configuration: HistoryDirectConfiguration,
    keyUTF8: [UInt8],
    limit: Int = 100,
    cursor: String? = nil
  ) async throws -> HistoryDirectConversationPage {
    guard (1...500).contains(limit) else {
      throw HistoryDirectQueryError.invalidConfiguration("conversation limit must be 1...500")
    }
    var arguments = [
      "conversations", "list", configuration.sourceURL.path, "--limit", String(limit),
    ]
    if let cursor { arguments += ["--cursor", cursor] }
    return try await request(
      configuration: configuration,
      keyUTF8: keyUTF8,
      operation: "conversations.list",
      arguments: arguments,
      query: nil,
      type: HistoryDirectConversationPage.self
    )
  }

  public func messages(
    configuration: HistoryDirectConfiguration,
    keyUTF8: [UInt8],
    conversationID: String,
    limit: Int = 100,
    cursor: String? = nil
  ) async throws -> HistoryDirectMessagePage {
    guard !conversationID.isEmpty, (1...500).contains(limit) else {
      throw HistoryDirectQueryError.invalidConfiguration("message request is outside safe bounds")
    }
    var arguments = [
      "messages", "list", configuration.sourceURL.path,
      "--conversation", conversationID, "--limit", String(limit),
    ]
    if let cursor { arguments += ["--cursor", cursor] }
    return try await request(
      configuration: configuration,
      keyUTF8: keyUTF8,
      operation: "messages.list",
      arguments: arguments,
      query: nil,
      type: HistoryDirectMessagePage.self
    )
  }

  public func message(
    configuration: HistoryDirectConfiguration,
    keyUTF8: [UInt8],
    conversationID: String,
    messageID: String
  ) async throws -> HistoryDirectMessageResource {
    guard !conversationID.isEmpty, !messageID.isEmpty else {
      throw HistoryDirectQueryError.invalidConfiguration("message identity is empty")
    }
    return try await request(
      configuration: configuration,
      keyUTF8: keyUTF8,
      operation: "message.get",
      arguments: [
        "message", "get", configuration.sourceURL.path,
        "--conversation", conversationID, "--message", messageID,
      ],
      query: nil,
      type: HistoryDirectMessageResource.self
    )
  }

  public func search(
    configuration: HistoryDirectConfiguration,
    keyUTF8: [UInt8],
    query: String,
    conversationID: String? = nil,
    limit: Int = 50,
    cursor: String? = nil
  ) async throws -> HistoryDirectSearchPage {
    let queryBytes = Array(query.utf8)
    guard !query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
      queryBytes.count <= 16 * 1_024, !queryBytes.contains(0), (1...200).contains(limit)
    else { throw HistoryDirectQueryError.invalidQuery }
    var arguments = [
      "messages", "search", configuration.sourceURL.path,
      "--query-stdin", "--limit", String(limit),
    ]
    if let conversationID {
      guard !conversationID.isEmpty else { throw HistoryDirectQueryError.invalidQuery }
      arguments += ["--conversation", conversationID]
    }
    if let cursor { arguments += ["--cursor", cursor] }
    return try await request(
      configuration: configuration,
      keyUTF8: keyUTF8,
      operation: "messages.search",
      arguments: arguments,
      query: queryBytes,
      type: HistoryDirectSearchPage.self
    )
  }

  private func request<Response: Decodable & Sendable>(
    configuration: HistoryDirectConfiguration,
    keyUTF8: [UInt8],
    operation: String,
    arguments: [String],
    query: [UInt8]?,
    type: Response.Type
  ) async throws -> Response {
    let key = DirectSecureBytes(copying: keyUTF8)
    let queryBytes = query.map(DirectSecureBytes.init(copying:))
    let worker = Task.detached(priority: .userInitiated) {
      defer {
        key.clear()
        queryBytes?.clear()
      }
      return try requestSynchronously(
        configuration: configuration,
        key: key,
        operation: operation,
        arguments: arguments,
        query: queryBytes,
        type: type
      )
    }
    return try await withTaskCancellationHandler {
      try await worker.value
    } onCancel: {
      worker.cancel()
    }
  }

  private func requestSynchronously<Response: Decodable>(
    configuration: HistoryDirectConfiguration,
    key: DirectSecureBytes,
    operation: String,
    arguments: [String],
    query: DirectSecureBytes?,
    type: Response.Type
  ) throws -> Response {
    try validateDirectExecutable(configuration.executableURL)
    try validateDirectSource(configuration.sourceURL)
    if configuration.accessMode == .decrypted
      || configuration.accessMode == .snapshotRecoveryKit
      || configuration.accessMode == .snapshotKeychain
      || configuration.accessMode == .snapshotLocalCredential
    {
      guard key.isEmpty else {
        throw HistoryDirectQueryError.invalidConfiguration(
          "the selected file-backed mode must not receive key material")
      }
    } else if configuration.accessMode == .snapshotPassphrase {
      guard (12...1_024).contains(key.count), !key.contains(0), !key.contains(10),
        !key.contains(13), key.isValidUTF8
      else { throw HistoryDirectQueryError.invalidKey }
    } else {
      let validRaw = key.count == 32
      let validHex = key.count == 64 && key.allSatisfy(isDirectHexadecimal)
      guard validRaw || validHex else { throw HistoryDirectQueryError.invalidKey }
    }
    if configuration.accessMode == .snapshotRecoveryKit {
      guard let recoveryKitURL = configuration.recoveryKitURL else {
        throw HistoryDirectQueryError.invalidConfiguration("the snapshot recovery kit is missing")
      }
      try validateDirectPrivateCredential(recoveryKitURL, description: "recovery kit")
      guard configuration.localCredentialURL == nil else {
        throw HistoryDirectQueryError.invalidConfiguration(
          "a local credential is valid only in snapshot local-unlock mode")
      }
    } else if configuration.accessMode == .snapshotLocalCredential
      || configuration.accessMode == .snapshotKeychain
    {
      guard let localCredentialURL = configuration.localCredentialURL else {
        throw HistoryDirectQueryError.invalidConfiguration(
          "the snapshot local credential is missing")
      }
      try validateDirectPrivateCredential(
        localCredentialURL,
        description: "local credential"
      )
      guard configuration.recoveryKitURL == nil else {
        throw HistoryDirectQueryError.invalidConfiguration(
          "a recovery kit is valid only in snapshot recovery-word mode")
      }
    } else if configuration.recoveryKitURL != nil || configuration.localCredentialURL != nil {
      throw HistoryDirectQueryError.invalidConfiguration(
        "a protector file is valid only in its matching snapshot access mode")
    }

    let input = Pipe()
    let output = DirectBoundedProcessStream(maximumBytes: Self.maximumResponseBytes)
    let errors = DirectBoundedProcessStream(maximumBytes: Self.maximumErrorBytes)
    let process = Process()
    process.executableURL = configuration.executableURL
    process.arguments = arguments + configuration.accessArguments
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
      var inputData = Data()
      if configuration.accessMode == .liveEncrypted
        || configuration.accessMode == .snapshotPassphrase
        || configuration.accessMode == .snapshotEncrypted
      {
        key.append(to: &inputData)
        inputData.append(0x0A)
      }
      query?.append(to: &inputData)
      defer { inputData.resetBytes(in: 0..<inputData.count) }
      try input.fileHandleForWriting.write(contentsOf: inputData)
      try input.fileHandleForWriting.close()
    } catch {
      if process.isRunning { process.terminate() }
      output.stop()
      errors.stop()
      throw HistoryDirectQueryError.invalidConfiguration("the local CLI could not be started")
    }

    let deadline = DispatchTime.now() + .milliseconds(timeoutMilliseconds)
    while completion.wait(timeout: .now() + .milliseconds(50)) == .timedOut {
      if Task.isCancelled {
        terminateDirectProcess(process, completion: completion)
        output.stop()
        errors.stop()
        throw CancellationError()
      }
      if output.overflowed || errors.overflowed {
        terminateDirectProcess(process, completion: completion)
        output.stop()
        errors.stop()
        throw HistoryDirectQueryError.responseTooLarge
      }
      if DispatchTime.now() >= deadline {
        terminateDirectProcess(process, completion: completion)
        output.stop()
        errors.stop()
        throw HistoryDirectQueryError.timedOut
      }
    }
    let stdout = try output.finish()
    _ = try errors.finish()
    guard !output.overflowed, !errors.overflowed else {
      throw HistoryDirectQueryError.responseTooLarge
    }
    if process.terminationStatus != 0 {
      if let envelope = try? JSONDecoder().decode(DirectErrorEnvelope.self, from: stdout),
        envelope.schema == Self.schema, envelope.formatVersion == 1,
        envelope.operation == operation, envelope.ok == false
      {
        throw HistoryDirectQueryError.commandFailed(
          code: envelope.error.code,
          message: envelope.error.message,
          retryable: envelope.error.retryable
        )
      }
      throw HistoryDirectQueryError.invalidResponse
    }
    guard let envelope = try? JSONDecoder().decode(DirectSuccessIdentity.self, from: stdout),
      envelope.schema == Self.schema, envelope.formatVersion == 1,
      envelope.operation == operation, envelope.ok,
      envelope.source.mode == configuration.accessMode.responseMode,
      !envelope.source.identity.isEmpty
    else { throw HistoryDirectQueryError.invalidResponse }
    guard let response = try? JSONDecoder().decode(type, from: stdout) else {
      throw HistoryDirectQueryError.invalidResponse
    }
    return response
  }
}

private final class DirectSecureBytes: @unchecked Sendable {
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

  var isEmpty: Bool { count == 0 }

  func allSatisfy(_ predicate: (UInt8) -> Bool) -> Bool {
    guard let storage else { return true }
    let bytes = storage.assumingMemoryBound(to: UInt8.self)
    for index in 0..<count where !predicate(bytes[index]) { return false }
    return true
  }

  func contains(_ value: UInt8) -> Bool {
    guard let storage else { return false }
    let bytes = storage.assumingMemoryBound(to: UInt8.self)
    for index in 0..<count where bytes[index] == value { return true }
    return false
  }

  var isValidUTF8: Bool {
    guard let storage else { return true }
    let bytes = storage.assumingMemoryBound(to: UInt8.self)
    let continuation: (UInt8) -> Bool = { (0x80...0xBF).contains($0) }
    var index = 0
    while index < count {
      switch bytes[index] {
      case 0x00...0x7F:
        index += 1
      case 0xC2...0xDF:
        guard index + 1 < count, continuation(bytes[index + 1]) else { return false }
        index += 2
      case 0xE0:
        guard index + 2 < count, (0xA0...0xBF).contains(bytes[index + 1]),
          continuation(bytes[index + 2])
        else { return false }
        index += 3
      case 0xE1...0xEC, 0xEE...0xEF:
        guard index + 2 < count, continuation(bytes[index + 1]),
          continuation(bytes[index + 2])
        else { return false }
        index += 3
      case 0xED:
        guard index + 2 < count, (0x80...0x9F).contains(bytes[index + 1]),
          continuation(bytes[index + 2])
        else { return false }
        index += 3
      case 0xF0:
        guard index + 3 < count, (0x90...0xBF).contains(bytes[index + 1]),
          continuation(bytes[index + 2]), continuation(bytes[index + 3])
        else { return false }
        index += 4
      case 0xF1...0xF3:
        guard index + 3 < count, continuation(bytes[index + 1]),
          continuation(bytes[index + 2]), continuation(bytes[index + 3])
        else { return false }
        index += 4
      case 0xF4:
        guard index + 3 < count, (0x80...0x8F).contains(bytes[index + 1]),
          continuation(bytes[index + 2]), continuation(bytes[index + 3])
        else { return false }
        index += 4
      default:
        return false
      }
    }
    return true
  }

  func append(to data: inout Data) {
    guard let storage else { return }
    data.append(storage.assumingMemoryBound(to: UInt8.self), count: count)
  }

  func clear() {
    guard let storage else { return }
    memset_s(storage, count, 0, count)
  }
}

private struct DirectSuccessIdentity: Decodable {
  let schema: String
  let formatVersion: Int
  let operation: String
  let ok: Bool
  let source: HistoryDirectSource
}

private struct DirectErrorEnvelope: Decodable {
  struct Body: Decodable {
    let code: String
    let message: String
    let retryable: Bool
  }

  let schema: String
  let formatVersion: Int
  let operation: String
  let ok: Bool
  let error: Body
}

private func validateDirectExecutable(_ url: URL) throws {
  var metadata = stat()
  guard lstat(url.path, &metadata) == 0, metadata.st_mode & S_IFMT == S_IFREG,
    metadata.st_mode & S_IXUSR != 0, metadata.st_uid == getuid() || metadata.st_uid == 0
  else {
    throw HistoryDirectQueryError.invalidConfiguration(
      "the CLI must be a real executable owned by the current user or root")
  }
}

private func validateDirectSource(_ url: URL) throws {
  var metadata = stat()
  guard lstat(url.path, &metadata) == 0, metadata.st_mode & S_IFMT == S_IFDIR,
    metadata.st_uid == getuid()
  else {
    throw HistoryDirectQueryError.invalidConfiguration(
      "the source must be a current-user-owned real directory")
  }
}

private func validateDirectPrivateCredential(_ url: URL, description: String) throws {
  var metadata = stat()
  var parentMetadata = stat()
  let parent = url.deletingLastPathComponent()
  guard lstat(url.path, &metadata) == 0, metadata.st_mode & S_IFMT == S_IFREG,
    metadata.st_uid == getuid(), metadata.st_nlink == 1, metadata.st_mode & 0o077 == 0,
    lstat(parent.path, &parentMetadata) == 0, parentMetadata.st_mode & S_IFMT == S_IFDIR,
    parentMetadata.st_uid == getuid(), parentMetadata.st_mode & 0o077 == 0
  else {
    throw HistoryDirectQueryError.invalidConfiguration(
      "the \(description) and its directory must be current-user-owned and owner-only")
  }
}

private func isDirectHexadecimal(_ value: UInt8) -> Bool {
  (value >= 48 && value <= 57) || (value >= 65 && value <= 70) || (value >= 97 && value <= 102)
}

private final class DirectBoundedProcessStream: @unchecked Sendable {
  let pipe = Pipe()
  private let maximumBytes: Int
  private let queue = DispatchQueue(label: "greenbubbles.history.direct-stream")
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
          let shouldStop = lock.withLock { () -> Bool in
            if data.count > maximumBytes - min(maximumBytes, chunk.count) {
              didOverflow = true
              return true
            }
            data.append(chunk)
            return false
          }
          if shouldStop { break }
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

private func terminateDirectProcess(_ process: Process, completion: DispatchSemaphore) {
  if process.isRunning { process.terminate() }
  if completion.wait(timeout: .now() + .seconds(1)) == .timedOut, process.isRunning {
    kill(process.processIdentifier, SIGKILL)
    _ = completion.wait(timeout: .now() + .seconds(1))
  }
}
