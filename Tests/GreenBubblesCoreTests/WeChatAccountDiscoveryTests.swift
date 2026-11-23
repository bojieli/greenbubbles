import Foundation
import Testing

@testable import GreenBubblesCore

struct WeChatAccountDiscoveryTests {
  @Test func discoversOnlyDirectoriesWithDatabaseStorageAndRedactsThem() throws {
    let home = FileManager.default.temporaryDirectory.appending(
      path: "greenbubbles-account-tests-\(UUID().uuidString)",
      directoryHint: .isDirectory
    )
    defer { try? FileManager.default.removeItem(at: home) }
    let xwechat = home.appending(
      path: "Library/Containers/com.tencent.xinWeChat/Data/Documents/xwechat_files",
      directoryHint: .isDirectory
    )
    try FileManager.default.createDirectory(
      at: xwechat.appending(path: "account-one/db_storage"),
      withIntermediateDirectories: true
    )
    try FileManager.default.createDirectory(
      at: xwechat.appending(path: "account-one/msg/attach"),
      withIntermediateDirectories: true
    )
    try FileManager.default.createDirectory(
      at: xwechat.appending(path: "not-an-account"),
      withIntermediateDirectories: true
    )

    let discovery = WeChatAccountDiscovery(homeDirectory: home)
    let accounts = discovery.discover()

    #expect(accounts.count == 1)
    #expect(accounts[0].root.path == nil)
    #expect(accounts[0].databaseRoot.path == nil)
    #expect(accounts[0].attachmentRoot?.path == nil)
    #expect(discovery.databaseRoots(accountID: accounts[0].accountID).count == 1)

    #expect(try discovery.singleDatabaseRoot() == discovery.databaseRoots()[0])

    try FileManager.default.createDirectory(
      at: xwechat.appending(path: "account-two/db_storage"), withIntermediateDirectories: true
    )
    #expect(throws: DatabaseRootSelectionError.ambiguous) {
      try discovery.singleDatabaseRoot()
    }
  }

  @Test func discoversAccountsInDuplicatedAppContainers() throws {
    let home = FileManager.default.temporaryDirectory.appending(
      path: "greenbubbles-account-tests-\(UUID().uuidString)",
      directoryHint: .isDirectory
    )
    defer { try? FileManager.default.removeItem(at: home) }
    let fileManager = FileManager.default
    func makeAccount(_ relative: String) throws {
      try fileManager.createDirectory(
        at: home.appending(path: relative).appending(path: "db_storage"),
        withIntermediateDirectories: true
      )
    }
    try makeAccount(
      "Library/Containers/com.tencent.xinWeChat/Data/Documents/xwechat_files/account-one")
    try makeAccount(
      "Library/Containers/com.example.WeChatCopy/Data/Documents/xwechat_files/account-two")
    // An unrelated app container without WeChat data is skipped.
    try fileManager.createDirectory(
      at: home.appending(path: "Library/Containers/com.example.Other/Data/Documents"),
      withIntermediateDirectories: true
    )

    let discovery = WeChatAccountDiscovery(homeDirectory: home)
    let owners = Set(discovery.discover().map(\.containerBundleIdentifier))
    #expect(owners == ["com.tencent.xinWeChat", "com.example.WeChatCopy"])
    #expect(discovery.databaseRoots().count == 2)
    #expect(throws: DatabaseRootSelectionError.ambiguous) {
      try discovery.singleDatabaseRoot()
    }
    #expect(
      discovery.resolvedAccounts().contains {
        $0.containerBundleIdentifier == "com.example.WeChatCopy"
          && $0.databaseRoot.path.contains("/com.example.WeChatCopy/Data/Documents/")
      })
  }

  @Test func onlyDuplicatedAppContainerIsStillUnique() throws {
    let home = FileManager.default.temporaryDirectory.appending(
      path: "greenbubbles-account-tests-\(UUID().uuidString)",
      directoryHint: .isDirectory
    )
    defer { try? FileManager.default.removeItem(at: home) }
    let databaseRoot = home.appending(
      path: "Library/Containers/com.example.WeChatCopy/Data/Documents/xwechat_files/a/db_storage"
    )
    try FileManager.default.createDirectory(at: databaseRoot, withIntermediateDirectories: true)

    let discovery = WeChatAccountDiscovery(homeDirectory: home)
    #expect(try discovery.singleDatabaseRoot().lastPathComponent == "db_storage")
    #expect(discovery.discover().map(\.containerBundleIdentifier) == ["com.example.WeChatCopy"])
  }

  @Test func rejectsSymlinkedDuplicateContainersAndKeepsGroupContainers() throws {
    let home = FileManager.default.temporaryDirectory.appending(
      path: "greenbubbles-account-tests-\(UUID().uuidString)",
      directoryHint: .isDirectory
    )
    defer { try? FileManager.default.removeItem(at: home) }
    let fileManager = FileManager.default
    let elsewhere = home.appending(path: "elsewhere/xwechat_files")
    try fileManager.createDirectory(
      at: elsewhere.appending(path: "account/db_storage"), withIntermediateDirectories: true)
    // A container whose xwechat_files is a symbolic link is ignored.
    let linkedDocuments = home.appending(
      path: "Library/Containers/com.example.Linked/Data/Documents")
    try fileManager.createDirectory(at: linkedDocuments, withIntermediateDirectories: true)
    try fileManager.createSymbolicLink(
      at: linkedDocuments.appending(path: "xwechat_files"), withDestinationURL: elsewhere)
    // A container directory that is itself a symbolic link is ignored.
    try fileManager.createSymbolicLink(
      at: home.appending(path: "Library/Containers/com.example.LinkedRoot"),
      withDestinationURL: home.appending(path: "Library/Containers/com.example.Linked"))
    // Group containers are still scanned and carry no owning bundle ID.
    try fileManager.createDirectory(
      at: home.appending(
        path: "Library/Group Containers/ABC.com.tencent.xinWeChat/xwechat_files/g/db_storage"),
      withIntermediateDirectories: true
    )

    let accounts = WeChatAccountDiscovery(homeDirectory: home).discover()
    #expect(accounts.count == 1)
    #expect(accounts.first?.containerBundleIdentifier == nil)
  }
}
