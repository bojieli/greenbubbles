import Darwin
import Foundation
import SQLite3

public actor HistoryStore {
  private let database: SQLiteConnection
  private let messages: HistorySourceFile
  private let artifacts: HistorySourceFile

  public init(session: HistoryBundleSession) throws {
    database = try SQLiteConnection(
      path: session.indexURL.path,
      flags: SQLITE_OPEN_READONLY | SQLITE_OPEN_FULLMUTEX
    )
    messages = session.validatedSources.messages
    artifacts = session.validatedSources.artifacts
  }

  public func messages(
    conversationID: String,
    before cursor: HistoryMessageCursor? = nil,
    limit requestedLimit: Int = 100
  ) throws -> HistoryMessagePage {
    let limit = min(max(requestedLimit, 1), 250)
    let statement: SQLiteStatement
    if let cursor {
      statement = try database.prepare(
        """
        SELECT rowid, ordinal, byte_offset, byte_length
        FROM messages
        WHERE conversation_id = ?
          AND (ordinal < ? OR (ordinal = ? AND rowid < ?))
        ORDER BY ordinal DESC, rowid DESC
        LIMIT ?
        """)
      try statement.bind(conversationID, at: 1)
      try statement.bind(cursor.ordinal, at: 2)
      try statement.bind(cursor.ordinal, at: 3)
      try statement.bind(cursor.rowID, at: 4)
      try statement.bind(limit + 1, at: 5)
    } else {
      statement = try database.prepare(
        """
        SELECT rowid, ordinal, byte_offset, byte_length
        FROM messages
        WHERE conversation_id = ?
        ORDER BY ordinal DESC, rowid DESC
        LIMIT ?
        """)
      try statement.bind(conversationID, at: 1)
      try statement.bind(limit + 1, at: 2)
    }

    let records = try readMessageRecords(statement)
    let pageRecords = Array(records.prefix(limit))
    let nextCursor = records.count > limit ? pageRecords.last.map(\.cursor) : nil
    return HistoryMessagePage(
      messages: try pageRecords.map { try decodeMessage(at: $0.offset, length: $0.length) },
      nextCursor: nextCursor
    )
  }

  public func searchMessages(
    query: String,
    conversationID: String? = nil,
    limit requestedLimit: Int = 100
  ) throws -> [HistoryMessage] {
    let normalized = query.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !normalized.isEmpty else { return [] }
    let limit = min(max(requestedLimit, 1), 250)
    let records: [IndexedRecord]
    if normalized.count >= 3 {
      let statement = try database.prepare(
        """
        SELECT m.rowid, m.ordinal, m.byte_offset, m.byte_length
        FROM message_search
        JOIN messages m ON m.rowid = message_search.rowid
        WHERE message_search MATCH ?
          AND (? IS NULL OR m.conversation_id = ?)
        ORDER BY rank, m.ordinal DESC
        LIMIT ?
        """)
      let phrase = "\"\(normalized.replacingOccurrences(of: "\"", with: "\"\""))\""
      try statement.bind(phrase, at: 1)
      try statement.bind(conversationID, at: 2)
      try statement.bind(conversationID, at: 3)
      try statement.bind(limit, at: 4)
      records = try readMessageRecords(statement)
    } else {
      let statement = try database.prepare(
        """
        SELECT rowid, ordinal, byte_offset, byte_length
        FROM messages
        WHERE (? IS NULL OR conversation_id = ?)
          AND (
            payload_summary LIKE ? ESCAPE '\\' OR
            sender_display_name LIKE ? ESCAPE '\\' OR
            conversation_label LIKE ? ESCAPE '\\'
          )
        ORDER BY ordinal DESC
        LIMIT ?
        """)
      let pattern = "%\(escapeLike(normalized))%"
      try statement.bind(conversationID, at: 1)
      try statement.bind(conversationID, at: 2)
      try statement.bind(pattern, at: 3)
      try statement.bind(pattern, at: 4)
      try statement.bind(pattern, at: 5)
      try statement.bind(limit, at: 6)
      records = try readMessageRecords(statement)
    }
    return try records.map { try decodeMessage(at: $0.offset, length: $0.length) }
  }

  public func message(canonicalID: String) throws -> HistoryMessage? {
    let statement = try database.prepare(
      "SELECT byte_offset, byte_length FROM messages WHERE canonical_id = ?")
    try statement.bind(canonicalID, at: 1)
    guard try statement.step() == SQLITE_ROW else { return nil }
    return try decodeMessage(
      at: unsigned(statement.int64(at: 0)),
      length: unsigned(statement.int64(at: 1))
    )
  }

  public func messagesAround(
    canonicalID: String,
    radius requestedRadius: Int = 30
  ) throws -> [HistoryMessage] {
    let radius = min(max(requestedRadius, 1), 100)
    let target = try database.prepare(
      "SELECT conversation_id, ordinal FROM messages WHERE canonical_id = ?")
    try target.bind(canonicalID, at: 1)
    guard try target.step() == SQLITE_ROW,
      let conversationID = target.text(at: 0)
    else { return [] }
    let ordinal = target.int64(at: 1)
    let statement = try database.prepare(
      """
      SELECT rowid, ordinal, byte_offset, byte_length
      FROM messages
      WHERE conversation_id = ?
      ORDER BY ABS(ordinal - ?) ASC, ordinal DESC
      LIMIT ?
      """)
    try statement.bind(conversationID, at: 1)
    try statement.bind(ordinal, at: 2)
    try statement.bind(radius * 2 + 1, at: 3)
    let records = try readMessageRecords(statement).sorted {
      if $0.cursor.ordinal == $1.cursor.ordinal { return $0.cursor.rowID > $1.cursor.rowID }
      return $0.cursor.ordinal > $1.cursor.ordinal
    }
    return try records.map { try decodeMessage(at: $0.offset, length: $0.length) }
  }

  public func artifact(artifactID: String) throws -> HistoryArtifact? {
    let statement = try database.prepare(
      "SELECT byte_offset, byte_length FROM artifacts WHERE artifact_id = ?")
    try statement.bind(artifactID, at: 1)
    guard try statement.step() == SQLITE_ROW else { return nil }
    let data = try artifacts.read(
      offset: unsigned(statement.int64(at: 0)),
      length: unsigned(statement.int64(at: 1))
    )
    do {
      return try JSONDecoder().decode(HistoryArtifact.self, from: data)
    } catch {
      throw HistoryBundleError.integrityFailure("artifact index no longer resolves valid JSON")
    }
  }

  private func readMessageRecords(_ statement: SQLiteStatement) throws -> [IndexedRecord] {
    var records: [IndexedRecord] = []
    while try statement.step() == SQLITE_ROW {
      let rowID = statement.int64(at: 0)
      let ordinal = unsigned(statement.int64(at: 1))
      records.append(
        IndexedRecord(
          cursor: HistoryMessageCursor(ordinal: ordinal, rowID: rowID),
          offset: unsigned(statement.int64(at: 2)),
          length: unsigned(statement.int64(at: 3))
        ))
    }
    return records
  }

  private func decodeMessage(at offset: UInt64, length: UInt64) throws -> HistoryMessage {
    let data = try messages.read(offset: offset, length: length)
    do {
      return try JSONDecoder().decode(HistoryMessage.self, from: data)
    } catch {
      throw HistoryBundleError.integrityFailure("message index no longer resolves valid JSON")
    }
  }
}

private struct IndexedRecord {
  let cursor: HistoryMessageCursor
  let offset: UInt64
  let length: UInt64
}

final class HistoryValidatedSources: @unchecked Sendable {
  let messages: HistorySourceFile
  let artifacts: HistorySourceFile

  init(messages: HistorySourceFile, artifacts: HistorySourceFile) {
    self.messages = messages
    self.artifacts = artifacts
  }
}

final class HistorySourceFile: @unchecked Sendable {
  private struct Identity: Equatable {
    let device: UInt64
    let inode: UInt64
    let size: Int64
    let modifiedSeconds: Int
    let modifiedNanoseconds: Int
    let changedSeconds: Int
    let changedNanoseconds: Int
  }

  private let descriptor: Int32
  private let identity: Identity

  private init(descriptor: Int32, expectedIdentity: Identity) throws {
    var metadata = stat()
    guard fstat(descriptor, &metadata) == 0,
      metadata.st_mode & S_IFMT == S_IFREG,
      metadata.st_uid == getuid(),
      metadata.st_nlink == 1,
      metadata.st_mode & 0o077 == 0,
      metadata.st_size >= 0,
      Self.identity(for: metadata) == expectedIdentity
    else {
      close(descriptor)
      throw HistoryBundleError.integrityFailure("indexed source file identity is unsafe")
    }
    self.descriptor = descriptor
    identity = Self.identity(for: metadata)
  }

  static func retainingValidatedDescriptor(
    _ descriptor: Int32,
    unchangedFrom initialMetadata: stat
  ) throws -> HistorySourceFile {
    var current = stat()
    let expectedIdentity = identity(for: initialMetadata)
    guard fstat(descriptor, &current) == 0, identity(for: current) == expectedIdentity else {
      throw HistoryBundleError.integrityFailure(
        "AI context bundle changed during validation")
    }
    let retained = fcntl(descriptor, F_DUPFD_CLOEXEC, 0)
    guard retained >= 0 else {
      throw HistoryBundleError.integrityFailure(
        "validated source descriptor could not be retained")
    }
    return try HistorySourceFile(descriptor: retained, expectedIdentity: expectedIdentity)
  }

  deinit {
    close(descriptor)
  }

  func read(offset: UInt64, length: UInt64) throws -> Data {
    guard length > 0, length <= 16 * 1_024 * 1_024,
      offset <= UInt64(Int64.max), length <= UInt64(Int.max),
      offset.addingReportingOverflow(length).overflow == false,
      offset + length <= UInt64(identity.size)
    else {
      throw HistoryBundleError.integrityFailure("indexed source range is invalid")
    }
    var current = stat()
    guard fstat(descriptor, &current) == 0, Self.identity(for: current) == identity else {
      throw HistoryBundleError.integrityFailure("AI context bundle changed after validation")
    }
    var bytes = [UInt8](repeating: 0, count: Int(length))
    var completed = 0
    while completed < bytes.count {
      let remaining = bytes.count - completed
      let result = bytes.withUnsafeMutableBytes { buffer in
        pread(
          descriptor,
          buffer.baseAddress!.advanced(by: completed),
          remaining,
          off_t(offset) + off_t(completed)
        )
      }
      guard result > 0 else {
        throw HistoryBundleError.integrityFailure("indexed source range could not be read")
      }
      completed += result
    }
    var after = stat()
    guard fstat(descriptor, &after) == 0, Self.identity(for: after) == identity else {
      throw HistoryBundleError.integrityFailure("AI context bundle changed during indexed read")
    }
    return Data(bytes)
  }

  private static func identity(for metadata: stat) -> Identity {
    Identity(
      device: UInt64(metadata.st_dev),
      inode: UInt64(metadata.st_ino),
      size: metadata.st_size,
      modifiedSeconds: metadata.st_mtimespec.tv_sec,
      modifiedNanoseconds: metadata.st_mtimespec.tv_nsec,
      changedSeconds: metadata.st_ctimespec.tv_sec,
      changedNanoseconds: metadata.st_ctimespec.tv_nsec
    )
  }
}

private func unsigned(_ value: Int64) -> UInt64 {
  value < 0 ? 0 : UInt64(value)
}

private func escapeLike(_ value: String) -> String {
  value
    .replacingOccurrences(of: "\\", with: "\\\\")
    .replacingOccurrences(of: "%", with: "\\%")
    .replacingOccurrences(of: "_", with: "\\_")
}
