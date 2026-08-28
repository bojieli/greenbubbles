import Foundation
import Testing

@testable import GreenBubblesCore

@Suite("SnapshotAccountBindingTests")
struct SnapshotAccountBindingTests {
  @Test
  func snapshotExportBindsWxidAccountDirectoryAutomatically() throws {
    let fixture = try AccountBindingFixture()
    defer { fixture.remove() }
    let database = try fixture.createDatabase(
      accountDirectory: "wxid_example123abc_ab12",
      relativePath: "message/message_0.db"
    )

    let lease = try ReadOnlySnapshotter(
      baseDirectory: fixture.snapshots,
      maxRetries: 0
    ).createSnapshot(
      of: [DatabaseFileSet(database: database)],
      cleanUpOnDeinit: false
    )
    defer { try? lease.cleanUp() }

    let binding = try #require(lease.manifest.accountBinding)
    #expect(lease.manifest.manifestFormatVersion == 4)
    #expect(binding.accountID.count == 64)
    #expect(decodedSelfIdentifier(binding) == "wxid_example123abc")
  }

  @Test
  func legacyAliasRequiresIndependentLoginDirectoryConfirmation() throws {
    let fixture = try AccountBindingFixture()
    defer { fixture.remove() }
    let database = try fixture.createDatabase(
      accountDirectory: "testuser001_1662",
      relativePath: "message/message_0.db"
    )
    let sets = [DatabaseFileSet(database: database)]

    let unconfirmed = try SnapshotAccountBinder().bind(sets: sets)
    #expect(decodedSelfIdentifier(unconfirmed) == "testuser001_1662")

    try FileManager.default.createDirectory(
      at: fixture.xwechatRoot.appending(
        path: "all_users/login/testuser001",
        directoryHint: .isDirectory
      ),
      withIntermediateDirectories: true
    )
    let confirmed = try SnapshotAccountBinder().bind(sets: sets)
    #expect(decodedSelfIdentifier(confirmed) == "testuser001")
    #expect(confirmed.accountID == unconfirmed.accountID)
  }

  @Test
  func refusesDatabaseSetsFromDifferentAccountRoots() throws {
    let fixture = try AccountBindingFixture()
    defer { fixture.remove() }
    let first = try fixture.createDatabase(
      accountDirectory: "wxid_first_ab12",
      relativePath: "message/message_0.db"
    )
    let second = try fixture.createDatabase(
      accountDirectory: "wxid_second_cd34",
      relativePath: "contact/contact.db"
    )

    #expect(throws: SnapshotAccountBindingError.ambiguousAccountRoots) {
      _ = try SnapshotAccountBinder().bind(
        sets: [DatabaseFileSet(database: first), DatabaseFileSet(database: second)]
      )
    }
  }

  @Test
  func refusesSidecarFromAnotherAccountRoot() throws {
    let fixture = try AccountBindingFixture()
    defer { fixture.remove() }
    let database = try fixture.createDatabase(
      accountDirectory: "wxid_first_ab12",
      relativePath: "message/message_0.db"
    )
    let foreignSidecar = try fixture.createDatabase(
      accountDirectory: "wxid_second_cd34",
      relativePath: "message/message_0.db-wal"
    )

    #expect(throws: SnapshotAccountBindingError.ambiguousAccountRoots) {
      _ = try SnapshotAccountBinder().bind(
        sets: [DatabaseFileSet(database: database, writeAheadLog: foreignSidecar)]
      )
    }
  }

  @Test
  func refusesDatabaseOutsideAccountDbStorageHierarchy() throws {
    let fixture = try AccountBindingFixture()
    defer { fixture.remove() }
    let database = fixture.root.appending(path: "unbound/message.db")
    try FileManager.default.createDirectory(
      at: database.deletingLastPathComponent(),
      withIntermediateDirectories: true
    )
    try Data([1]).write(to: database)

    #expect(throws: SnapshotAccountBindingError.self) {
      _ = try SnapshotAccountBinder().bind(sets: [DatabaseFileSet(database: database)])
    }
  }

  @Test
  func refusesSymbolicAccountDirectory() throws {
    let fixture = try AccountBindingFixture()
    defer { fixture.remove() }
    _ = try fixture.createDatabase(
      accountDirectory: "wxid_real_ab12",
      relativePath: "message/message_0.db"
    )
    let realAccount = fixture.xwechatRoot.appending(
      path: "wxid_real_ab12",
      directoryHint: .isDirectory
    )
    let symbolicAccount = fixture.xwechatRoot.appending(
      path: "wxid_alias_cd34",
      directoryHint: .isDirectory
    )
    try FileManager.default.createSymbolicLink(
      at: symbolicAccount,
      withDestinationURL: realAccount
    )
    let aliasedDatabase = symbolicAccount.appending(
      path: "db_storage/message/message_0.db"
    )

    #expect(throws: SnapshotAccountBindingError.unsafeAccountDirectory) {
      _ = try SnapshotAccountBinder().bind(
        sets: [DatabaseFileSet(database: aliasedDatabase)]
      )
    }
  }

  @Test
  func rejectsMalformedPrivateBindingPayloads() throws {
    let binder = SnapshotAccountBinder()
    let validIdentifier = Data("wxid_fixture".utf8).base64EncodedString()
    for binding in [
      SnapshotAccountBinding(
        accountID: String(repeating: "A", count: 64),
        selfSourceIdentifierBase64: validIdentifier
      ),
      SnapshotAccountBinding(
        accountID: String(repeating: "a", count: 64),
        selfSourceIdentifierBase64: Data().base64EncodedString()
      ),
      SnapshotAccountBinding(
        accountID: String(repeating: "a", count: 64),
        selfSourceIdentifierBase64: Data("../account".utf8).base64EncodedString()
      ),
      SnapshotAccountBinding(
        accountID: String(repeating: "a", count: 64),
        selfSourceIdentifierBase64: Data(String(repeating: "a", count: 256).utf8)
          .base64EncodedString()
      ),
    ] {
      #expect(throws: SnapshotAccountBindingError.malformedBinding) {
        try binder.validate(binding)
      }
    }
  }
}

private func decodedSelfIdentifier(_ binding: SnapshotAccountBinding) -> String? {
  Data(base64Encoded: binding.selfSourceIdentifierBase64)
    .flatMap { String(data: $0, encoding: .utf8) }
}

private struct AccountBindingFixture {
  let root: URL
  let xwechatRoot: URL
  let snapshots: URL

  init() throws {
    root = FileManager.default.temporaryDirectory.appending(
      path: "greenbubbles-account-binding-tests-\(UUID().uuidString)",
      directoryHint: .isDirectory
    )
    xwechatRoot = root.appending(path: "xwechat_files", directoryHint: .isDirectory)
    snapshots = root.appending(path: "snapshots", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: xwechatRoot, withIntermediateDirectories: true)
    try FileManager.default.createDirectory(at: snapshots, withIntermediateDirectories: true)
  }

  func createDatabase(accountDirectory: String, relativePath: String) throws -> URL {
    let database = xwechatRoot.appending(
      path: "\(accountDirectory)/db_storage/\(relativePath)"
    )
    try FileManager.default.createDirectory(
      at: database.deletingLastPathComponent(),
      withIntermediateDirectories: true
    )
    try Data([1, 2, 3]).write(to: database)
    return database
  }

  func remove() {
    try? FileManager.default.removeItem(at: root)
  }
}
