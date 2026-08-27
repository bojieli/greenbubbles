import Foundation
import Testing

@testable import GreenBubblesCore

@Suite("WeChatAccountStorageAssessmentTests")
struct WeChatAccountStorageAssessmentTests {
  @Test
  func aggregatesDatabaseAndAttachmentCandidatesWithoutNamesOrPaths() throws {
    let fixture = try StorageAssessmentFixture()
    defer { fixture.remove() }
    try fixture.createFile("db_storage/message/message_0.db", byteCount: 8)
    try fixture.createFile("db_storage/message/message_12.db", byteCount: 8)
    try fixture.createFile("db_storage/message/biz_message_0.db", byteCount: 8)
    try fixture.createFile("db_storage/message/media_1.db", byteCount: 8)
    try fixture.createFile("db_storage/message/message_resource.db", byteCount: 8)
    try fixture.createFile("db_storage/contact/contact.db", byteCount: 8)
    try fixture.createFile("db_storage/session/session.db", byteCount: 8)
    try fixture.createFile("db_storage/sns/sns.db", byteCount: 8)
    try fixture.createFile("msg/attach/private-name.jpg", byteCount: 11)
    try fixture.createFile("msg/attach/another-private-name.pdf", byteCount: 13)
    try fixture.createFile("msg/attach/encrypted-private-name.dat", byteCount: 17)
    let outside = fixture.root.appending(path: "outside.dat")
    try Data([1]).write(to: outside)
    try FileManager.default.createSymbolicLink(
      at: fixture.accountRoot.appending(path: "msg/attach/unsafe-link.dat"),
      withDestinationURL: outside
    )

    let report = WeChatAccountStorageAssessor(homeDirectory: fixture.home).assess()
    let account = try #require(report.accounts.first)
    #expect(report.accounts.count == 1)
    #expect(account.databaseSetCount == 8)
    #expect(account.ordinaryMessageShardCandidateCount == 2)
    #expect(account.businessMessageShardCandidateCount == 1)
    #expect(account.mediaDatabaseCandidateCount == 1)
    #expect(account.messageResourceStoreCandidatePresent)
    #expect(account.contactStoreCandidatePresent)
    #expect(account.sessionStoreCandidatePresent)
    #expect(account.cachedMomentsStoreCandidatePresent)
    #expect(account.attachmentRootPresent)
    #expect(account.attachmentCandidateFileCount == 3)
    #expect(account.attachmentCandidateByteCount == 41)
    #expect(account.skippedSymbolicLinkCount == 1)
    #expect(!account.reachedAttachmentLimit)
    #expect(account.attachmentEnumerationCompleteWithinRoot)
    #expect(account.attachmentCandidates.first { $0.kind == .image }?.fileCount == 1)
    #expect(account.attachmentCandidates.first { $0.kind == .document }?.fileCount == 1)
    #expect(account.attachmentCandidates.first { $0.kind == .unclassified }?.fileCount == 1)

    let json = try #require(String(data: JSONEncoder().encode(report), encoding: .utf8))
    #expect(!json.contains(fixture.root.path))
    #expect(!json.contains("private-name"))
    #expect(!json.contains(".jpg"))
    #expect(!json.contains(".pdf"))
    #expect(!json.contains(".dat"))
    #expect(report.conclusions.locallyPersistedAttachmentCandidatesObserved)
    #expect(!report.conclusions.databaseOrAttachmentContentRead)
    #expect(!report.conclusions.messageAttachmentLinkageProven)
    #expect(!report.conclusions.completeConversationAndAttachmentCoverageProven)
  }

  @Test
  func capsAttachmentTraversalAndScopesOneOpaqueAccount() throws {
    let first = try StorageAssessmentFixture(accountName: "first")
    defer { first.remove() }
    let second = try StorageAssessmentFixture(home: first.home, accountName: "second")
    try second.createFile("msg/attach/one.jpg", byteCount: 1)
    try second.createFile("msg/attach/two.jpg", byteCount: 1)

    let secondID = PathPrivacy().reference(for: second.accountRoot).opaqueID
    let report = WeChatAccountStorageAssessor(
      homeDirectory: first.home,
      maxAttachmentFiles: 1
    ).assess(accountID: secondID)
    let account = try #require(report.accounts.first)
    #expect(report.accounts.count == 1)
    #expect(account.accountID == secondID)
    #expect(account.attachmentCandidateFileCount == 1)
    #expect(account.reachedAttachmentLimit)
    #expect(!account.attachmentEnumerationCompleteWithinRoot)
  }

  @Test
  func refusesASymlinkedAttachmentRoot() throws {
    let fixture = try StorageAssessmentFixture()
    defer { fixture.remove() }
    let attachmentRoot = fixture.accountRoot.appending(path: "msg/attach")
    try FileManager.default.removeItem(at: attachmentRoot)
    let outside = fixture.root.appending(path: "outside", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: outside, withIntermediateDirectories: true)
    try Data([1, 2, 3]).write(to: outside.appending(path: "private.jpg"))
    try FileManager.default.createSymbolicLink(at: attachmentRoot, withDestinationURL: outside)

    let report = WeChatAccountStorageAssessor(homeDirectory: fixture.home).assess()
    let account = try #require(report.accounts.first)
    #expect(!account.attachmentRootPresent)
    #expect(account.attachmentCandidateFileCount == 0)
    #expect(!report.conclusions.locallyPersistedAttachmentCandidatesObserved)
  }
}

private struct StorageAssessmentFixture {
  let root: URL
  let home: URL
  let accountRoot: URL
  private let ownsRoot: Bool

  init(home suppliedHome: URL? = nil, accountName: String = "account") throws {
    if let suppliedHome {
      root = suppliedHome.deletingLastPathComponent()
      home = suppliedHome
      ownsRoot = false
    } else {
      root = FileManager.default.temporaryDirectory.appending(
        path: "greenbubbles-storage-assessment-\(UUID().uuidString)",
        directoryHint: .isDirectory
      )
      home = root.appending(path: "home", directoryHint: .isDirectory)
      ownsRoot = true
    }
    accountRoot = home.appending(
      path:
        "Library/Containers/com.tencent.xinWeChat/Data/Documents/xwechat_files/\(accountName)",
      directoryHint: .isDirectory
    )
    try FileManager.default.createDirectory(
      at: accountRoot.appending(path: "db_storage", directoryHint: .isDirectory),
      withIntermediateDirectories: true
    )
    try FileManager.default.createDirectory(
      at: accountRoot.appending(path: "msg/attach", directoryHint: .isDirectory),
      withIntermediateDirectories: true
    )
  }

  func createFile(_ relativePath: String, byteCount: Int) throws {
    let url = accountRoot.appending(path: relativePath)
    try FileManager.default.createDirectory(
      at: url.deletingLastPathComponent(),
      withIntermediateDirectories: true
    )
    try Data(repeating: 0x41, count: byteCount).write(to: url)
  }

  func remove() {
    if ownsRoot { try? FileManager.default.removeItem(at: root) }
  }
}
