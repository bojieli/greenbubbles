import Darwin
import Foundation
import Testing

@testable import GreenBubblesCore

struct ReadOnlySnapshotTests {
  @Test func copiesDatabaseSetWithManifestAndOwnerOnlyPermissions() throws {
    let fixture = try SnapshotFixture()
    defer { fixture.remove() }
    let database = try fixture.createFile("source/messages.db", bytes: [1, 2, 3, 4])
    let wal = try fixture.createFile("source/messages.db-wal", bytes: [5, 6])
    let shm = try fixture.createFile("source/messages.db-shm", bytes: [7])
    try FileManager.default.setAttributes([.posixPermissions: 0o400], ofItemAtPath: database.path)
    try FileManager.default.setAttributes([.posixPermissions: 0o400], ofItemAtPath: wal.path)
    try FileManager.default.setAttributes([.posixPermissions: 0o400], ofItemAtPath: shm.path)

    let sourceBefore = try attributes(database)
    let lease = try ReadOnlySnapshotter(
      baseDirectory: fixture.snapshots,
      maxRetries: 0
    ).createSnapshot(
      of: [DatabaseFileSet(database: database, writeAheadLog: wal, sharedMemory: shm)]
    )

    #expect(lease.manifest.entries.count == 3)
    #expect(
      lease.manifest.entries.map(\.logicalPath) == [
        "messages.db",
        "messages.db-shm",
        "messages.db-wal",
      ])
    #expect(lease.manifest.sourceFingerprint.count == 64)
    #expect(lease.manifest.entries.allSatisfy { $0.source.path == nil })
    #expect(
      try Data(contentsOf: lease.directory.appending(path: "sets/0000/database.db"))
        == Data([1, 2, 3, 4]))
    #expect(try permissions(lease.directory) == 0o700)
    #expect(try permissions(lease.directory.appending(path: "sets/0000/database.db")) == 0o600)
    #expect(try attributes(database) == sourceBefore)
    #expect(
      FileManager.default.fileExists(atPath: lease.directory.appending(path: "manifest.json").path))

    try lease.cleanUp()
    #expect(!FileManager.default.fileExists(atPath: lease.directory.path))
  }

  @Test func rejectsASetThatMutatesDuringCopy() throws {
    let fixture = try SnapshotFixture()
    defer { fixture.remove() }
    let database = try fixture.createFile("source/messages.db", bytes: [1, 2, 3])
    let snapshotter = ReadOnlySnapshotter(
      baseDirectory: fixture.snapshots,
      maxRetries: 0,
      includeSourcePaths: false,
      hooks: SnapshotHooks(afterCopy: { source in
        if let handle = try? FileHandle(forWritingTo: source) {
          _ = try? handle.seekToEnd()
          try? handle.write(contentsOf: Data([4]))
          try? handle.close()
        }
      })
    )

    #expect(throws: SnapshotError.self) {
      _ = try snapshotter.createSnapshot(of: [DatabaseFileSet(database: database)])
    }
    let remaining = try FileManager.default.contentsOfDirectory(atPath: fixture.snapshots.path)
    #expect(remaining.isEmpty)
  }

  @Test func plannerGroupsSQLiteSidecars() throws {
    let fixture = try SnapshotFixture()
    defer { fixture.remove() }
    let database = try fixture.createFile("source/messages.sqlite", bytes: [1])
    let wal = try fixture.createFile("source/messages.sqlite-wal", bytes: [2])
    _ = try fixture.createFile("source/unrelated.txt", bytes: [3])

    let sets = DatabaseSetPlanner().findDatabaseSets(in: [fixture.source])

    #expect(
      sets == [
        DatabaseFileSet(
          database: database,
          writeAheadLog: wal,
          logicalPath: "source/messages.sqlite"
        )
      ])
  }

  @Test func janitorOnlyRemovesExpiredSnapshotDirectories() throws {
    let fixture = try SnapshotFixture()
    defer { fixture.remove() }
    let expired = fixture.snapshots.appending(path: "snapshot-expired")
    let unrelated = fixture.snapshots.appending(path: "keep-me")
    try FileManager.default.createDirectory(at: expired, withIntermediateDirectories: true)
    try FileManager.default.createDirectory(at: unrelated, withIntermediateDirectories: true)
    let future = Date().addingTimeInterval(60)

    let removed = try SnapshotJanitor().removeExpiredSnapshots(
      in: fixture.snapshots,
      olderThan: 0,
      now: future
    )

    #expect(removed == 1)
    #expect(!FileManager.default.fileExists(atPath: expired.path))
    #expect(FileManager.default.fileExists(atPath: unrelated.path))
  }
}

private struct SnapshotFixture {
  let root: URL
  let source: URL
  let snapshots: URL

  init() throws {
    root = FileManager.default.temporaryDirectory.appending(
      path: "greenbubbles-snapshot-tests-\(UUID().uuidString)",
      directoryHint: .isDirectory
    )
    source = root.appending(path: "source", directoryHint: .isDirectory)
    snapshots = root.appending(path: "snapshots", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: source, withIntermediateDirectories: true)
    try FileManager.default.createDirectory(at: snapshots, withIntermediateDirectories: true)
  }

  func createFile(_ relativePath: String, bytes: [UInt8]) throws -> URL {
    let url = root.appending(path: relativePath)
    try FileManager.default.createDirectory(
      at: url.deletingLastPathComponent(),
      withIntermediateDirectories: true
    )
    try Data(bytes).write(to: url)
    return url
  }

  func remove() {
    try? FileManager.default.removeItem(at: root)
  }
}

private func permissions(_ url: URL) throws -> Int {
  let value = try FileManager.default.attributesOfItem(atPath: url.path)[.posixPermissions]
  return try #require(value as? Int)
}

private func attributes(_ url: URL) throws -> [FileAttributeKey: AnyHashable] {
  let raw = try FileManager.default.attributesOfItem(atPath: url.path)
  var result: [FileAttributeKey: AnyHashable] = [:]
  for key in [
    FileAttributeKey.posixPermissions,
    .size,
    .modificationDate,
    .systemFileNumber,
    .systemNumber,
  ] {
    if let value = raw[key] as? AnyHashable {
      result[key] = value
    }
  }
  return result
}
