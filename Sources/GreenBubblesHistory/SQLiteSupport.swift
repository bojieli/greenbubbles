import Foundation
import SQLite3

final class SQLiteConnection {
  private var handle: OpaquePointer?

  init(path: String, flags: Int32) throws {
    var database: OpaquePointer?
    let result = sqlite3_open_v2(path, &database, flags, nil)
    guard result == SQLITE_OK, let database else {
      let detail = database.map { String(cString: sqlite3_errmsg($0)) } ?? "open failed"
      if let database { sqlite3_close(database) }
      throw HistoryBundleError.indexFailure(detail)
    }
    handle = database
    sqlite3_progress_handler(database, 20_000, sqliteCancellationCallback, nil)
  }

  deinit {
    if let handle { sqlite3_close(handle) }
  }

  func execute(_ sql: String) throws {
    guard let handle else { throw HistoryBundleError.indexFailure("database is closed") }
    var error: UnsafeMutablePointer<CChar>?
    let result = sqlite3_exec(handle, sql, nil, nil, &error)
    guard result == SQLITE_OK else {
      if result == SQLITE_INTERRUPT, Task.isCancelled {
        sqlite3_free(error)
        throw CancellationError()
      }
      let detail = error.map { String(cString: $0) } ?? String(cString: sqlite3_errmsg(handle))
      sqlite3_free(error)
      throw HistoryBundleError.indexFailure(detail)
    }
  }

  func prepare(_ sql: String) throws -> SQLiteStatement {
    guard let handle else { throw HistoryBundleError.indexFailure("database is closed") }
    var statement: OpaquePointer?
    let result = sqlite3_prepare_v2(handle, sql, -1, &statement, nil)
    guard result == SQLITE_OK, let statement
    else {
      if result == SQLITE_INTERRUPT, Task.isCancelled { throw CancellationError() }
      throw HistoryBundleError.indexFailure(String(cString: sqlite3_errmsg(handle)))
    }
    return SQLiteStatement(connection: handle, handle: statement)
  }

  var lastInsertedRowID: Int64 {
    handle.map(sqlite3_last_insert_rowid) ?? 0
  }
}

final class SQLiteStatement {
  private let connection: OpaquePointer
  private var handle: OpaquePointer?

  init(connection: OpaquePointer, handle: OpaquePointer) {
    self.connection = connection
    self.handle = handle
  }

  deinit {
    if let handle { sqlite3_finalize(handle) }
  }

  func reset() {
    guard let handle else { return }
    sqlite3_reset(handle)
    sqlite3_clear_bindings(handle)
  }

  func bind(_ value: String?, at index: Int32) throws {
    guard let handle else { throw HistoryBundleError.indexFailure("statement is closed") }
    let result: Int32
    if let value {
      guard value.utf8.count <= Int(Int32.max) else {
        throw HistoryBundleError.indexFailure("text exceeds SQLite range")
      }
      result = value.withCString { pointer in
        sqlite3_bind_text(handle, index, pointer, Int32(value.utf8.count), sqliteTransient)
      }
    } else {
      result = sqlite3_bind_null(handle, index)
    }
    try require(result)
  }

  func bind(_ value: Int64?, at index: Int32) throws {
    guard let handle else { throw HistoryBundleError.indexFailure("statement is closed") }
    try require(
      value.map { sqlite3_bind_int64(handle, index, $0) } ?? sqlite3_bind_null(handle, index))
  }

  func bind(_ value: UInt64, at index: Int32) throws {
    guard value <= UInt64(Int64.max) else {
      throw HistoryBundleError.indexFailure("integer exceeds SQLite range")
    }
    try bind(Int64(value), at: index)
  }

  func bind(_ value: Int, at index: Int32) throws {
    try bind(Int64(value), at: index)
  }

  @discardableResult
  func step() throws -> Int32 {
    guard let handle else { throw HistoryBundleError.indexFailure("statement is closed") }
    let result = sqlite3_step(handle)
    guard result == SQLITE_ROW || result == SQLITE_DONE else {
      if result == SQLITE_INTERRUPT, Task.isCancelled { throw CancellationError() }
      throw HistoryBundleError.indexFailure(String(cString: sqlite3_errmsg(connection)))
    }
    return result
  }

  func text(at index: Int32) -> String? {
    guard let handle, let pointer = sqlite3_column_text(handle, index) else { return nil }
    let count = Int(sqlite3_column_bytes(handle, index))
    return String(decoding: UnsafeBufferPointer(start: pointer, count: count), as: UTF8.self)
  }

  func int64(at index: Int32) -> Int64 {
    guard let handle else { return 0 }
    return sqlite3_column_int64(handle, index)
  }

  private func require(_ result: Int32) throws {
    guard result == SQLITE_OK else {
      if result == SQLITE_INTERRUPT, Task.isCancelled { throw CancellationError() }
      throw HistoryBundleError.indexFailure(String(cString: sqlite3_errmsg(connection)))
    }
  }
}

private let sqliteTransient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)

private let sqliteCancellationCallback: @convention(c) (UnsafeMutableRawPointer?) -> Int32 = {
  _ in
  Task.isCancelled ? 1 : 0
}
