import Foundation
import Testing

@testable import GreenBubblesAcquire

struct DatabaseSaltInventoryTests {
  @Test func collectsSaltsAndRejectsUnsafeFiles() throws {
    let root = FileManager.default.temporaryDirectory.appending(
      path: "greenbubbles-acquire-salt-tests-\(UUID().uuidString)",
      directoryHint: .isDirectory
    )
    defer { try? FileManager.default.removeItem(at: root) }
    let fileManager = FileManager.default

    let nested = root.appending(path: "message", directoryHint: .isDirectory)
    try fileManager.createDirectory(at: nested, withIntermediateDirectories: true)

    let saltA = [UInt8](repeating: 0x01, count: 16)
    let saltB = [UInt8](repeating: 0x02, count: 16)
    try makeDatabase(salt: saltA, pages: 1)
      .write(to: nested.appending(path: "message_0.db"))
    try makeDatabase(salt: saltB, pages: 2)
      .write(to: root.appending(path: "session.db"))

    // Journals, undersized files, non-database files, and symlinks are skipped.
    try makeDatabase(salt: saltB, pages: 1)
      .write(to: root.appending(path: "session.db-wal"))
    try makeDatabase(salt: saltB, pages: 1)
      .write(to: root.appending(path: "session.db-shm"))
    try Data(repeating: 0x03, count: 100).write(to: root.appending(path: "tiny.db"))
    try makeDatabase(salt: saltB, pages: 1)
      .write(to: root.appending(path: "notes.txt"))
    try fileManager.createSymbolicLink(
      at: root.appending(path: "link.db"),
      withDestinationURL: root.appending(path: "session.db")
    )

    let inventory = try DatabaseSaltInventory(root: root)

    #expect(inventory.entries.map(\.relativePath) == ["message/message_0.db", "session.db"])
    #expect(inventory.entries[0].salt == saltA)
    #expect(inventory.entries[1].salt == saltB)
    #expect(inventory.entries[0].page1.count == 4096)
    #expect(inventory.entries[1].page1.count == 4096)
    #expect(inventory.skippedFileCount == 1)  // tiny.db
    #expect(inventory.distinctSalts.count == 2)
    #expect(inventory.saltVerificationSamples.count == 2)
  }

  @Test func rejectsAMissingRoot() {
    let missing = FileManager.default.temporaryDirectory.appending(
      path: "greenbubbles-acquire-salt-tests-missing-\(UUID().uuidString)",
      directoryHint: .isDirectory
    )
    #expect(throws: SaltInventoryError.unreadableRoot) {
      try DatabaseSaltInventory(root: missing)
    }
  }

  private func makeDatabase(salt: [UInt8], pages: Int) -> Data {
    var page = Data(count: 4096)
    page.replaceSubrange(0..<16, with: salt)
    var database = Data()
    database.append(page)
    database.append(Data(count: 4096 * (pages - 1)))
    return database
  }
}
