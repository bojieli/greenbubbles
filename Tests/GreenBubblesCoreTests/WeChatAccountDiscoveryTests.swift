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
  }
}
