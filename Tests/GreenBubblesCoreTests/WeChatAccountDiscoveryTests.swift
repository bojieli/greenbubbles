import Darwin
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

  @Test func standardContainerSymlinkedToADuplicateIsNotCountedTwice() throws {
    let fileManager = FileManager.default
    for linkWholeContainer in [true, false] {
      let home = fileManager.temporaryDirectory.appending(
        path: "greenbubbles-account-tests-\(UUID().uuidString)",
        directoryHint: .isDirectory
      )
      defer { try? fileManager.removeItem(at: home) }
      let containers = home.appending(path: "Library/Containers", directoryHint: .isDirectory)
      let duplicate = containers.appending(path: "com.example.WeChatCopy")
      try fileManager.createDirectory(
        at: duplicate.appending(path: "Data/Documents/xwechat_files/account/db_storage"),
        withIntermediateDirectories: true
      )
      let standard = containers.appending(path: "com.tencent.xinWeChat")
      if linkWholeContainer {
        try fileManager.createSymbolicLink(at: standard, withDestinationURL: duplicate)
      } else {
        let documents = standard.appending(path: "Data/Documents")
        try fileManager.createDirectory(at: documents, withIntermediateDirectories: true)
        try fileManager.createSymbolicLink(
          at: documents.appending(path: "xwechat_files"),
          withDestinationURL: duplicate.appending(path: "Data/Documents/xwechat_files"))
      }

      let discovery = WeChatAccountDiscovery(homeDirectory: home)
      #expect(discovery.discover().map(\.containerBundleIdentifier) == ["com.example.WeChatCopy"])
      #expect(try discovery.singleDatabaseRoot().lastPathComponent == "db_storage")
    }
  }

  @Test func differentlyCasedStandardContainerIsReportedOnceWithItsOnDiskName() throws {
    let home = FileManager.default.temporaryDirectory.appending(
      path: "greenbubbles-account-tests-\(UUID().uuidString)",
      directoryHint: .isDirectory
    )
    defer { try? FileManager.default.removeItem(at: home) }
    try FileManager.default.createDirectory(
      at: home.appending(
        path: "Library/Containers/com.tencent.xinwechat/Data/Documents/xwechat_files/a/db_storage"),
      withIntermediateDirectories: true
    )

    // On a case-insensitive volume the hard-coded standard spelling also
    // resolves to this directory; it must not add a second account.
    let discovery = WeChatAccountDiscovery(homeDirectory: home)
    let accounts = discovery.discover()
    #expect(accounts.map(\.containerBundleIdentifier) == ["com.tencent.xinwechat"])
    #expect(try discovery.singleDatabaseRoot().lastPathComponent == "db_storage")

    // The account ID is a hash of the account path. Earlier versions listed
    // the hard-coded standard xwechat_files path; derive the ID the same way
    // where that path resolves (case-insensitive volume), so a saved
    // --account-id keeps matching after the upgrade.
    let canonicalFiles = home.appending(
      path: "Library/Containers/com.tencent.xinWeChat/Data/Documents/xwechat_files")
    let onDisk = home.appending(
      path: "Library/Containers/com.tencent.xinwechat/Data/Documents/xwechat_files/a")
    let earlierRoot =
      FileManager.default.fileExists(atPath: canonicalFiles.path)
      ? try #require(
        try FileManager.default.contentsOfDirectory(
          at: canonicalFiles, includingPropertiesForKeys: [.isDirectoryKey],
          options: [.skipsHiddenFiles]
        ).first)
      : onDisk
    #expect(accounts.first?.accountID == PathPrivacy().reference(for: earlierRoot).opaqueID)
  }

  @Test func groupContainerLinkedToAnAppContainerIsOneAccount() throws {
    let fileManager = FileManager.default
    let home = fileManager.temporaryDirectory.appending(
      path: "greenbubbles-account-tests-\(UUID().uuidString)",
      directoryHint: .isDirectory
    )
    defer { try? fileManager.removeItem(at: home) }
    let appDocuments = home.appending(
      path: "Library/Containers/com.example.WeChatCopy/Data/Documents")
    try fileManager.createDirectory(
      at: appDocuments.appending(path: "xwechat_files/account/db_storage"),
      withIntermediateDirectories: true)
    let groups = home.appending(path: "Library/Group Containers")
    try fileManager.createDirectory(at: groups, withIntermediateDirectories: true)
    // The group container is a link to the app's Documents, so the account is
    // also listed as <group>/xwechat_files/account.
    let group = groups.appending(path: "ABC.com.tencent.xinWeChat")
    try fileManager.createSymbolicLink(at: group, withDestinationURL: appDocuments)
    // A second group whose xwechat_files itself is a link to the same data.
    let linkedFiles = groups.appending(path: "DEF.com.tencent.xinWeChat")
    try fileManager.createDirectory(at: linkedFiles, withIntermediateDirectories: true)
    try fileManager.createSymbolicLink(
      at: linkedFiles.appending(path: "xwechat_files"),
      withDestinationURL: appDocuments.appending(path: "xwechat_files"))

    let discovery = WeChatAccountDiscovery(homeDirectory: home)
    #expect(discovery.discover().map(\.containerBundleIdentifier) == ["com.example.WeChatCopy"])
    #expect(try discovery.singleDatabaseRoot().path.contains("/Library/Containers/"))

    // An ID saved from the group path (what earlier versions reported for
    // this account) still selects it. Current Foundation lists a linked
    // directory under its resolved path, so the two IDs may already agree;
    // aliases cover the case where they do not.
    let groupAccount = try #require(
      try fileManager.contentsOfDirectory(
        at: group.appending(path: "xwechat_files", directoryHint: .isDirectory),
        includingPropertiesForKeys: nil
      ).first)
    let savedID = PathPrivacy().reference(for: groupAccount).opaqueID
    #expect(discovery.databaseRoots(accountID: savedID) == discovery.databaseRoots())
    #expect(discovery.accountRoot(accountID: savedID) != nil)
    #expect(
      WeChatAccountStorageAssessor(homeDirectory: home).assess(accountID: savedID).accounts.count
        == 1)
  }

  @Test func aliasAccountIDsSelectTheSameAccount() {
    let path = URL(fileURLWithPath: "/synthetic/xwechat_files/account")
    let reference = PathPrivacy().reference(for: path)
    let account = WeChatAccountDiscovery.ResolvedAccount(
      report: WeChatAccountLocation(
        accountID: reference.opaqueID, root: reference, databaseRoot: reference,
        attachmentRoot: nil, isReadable: true),
      root: path,
      databaseRoot: path,
      attachmentRoot: nil,
      containerBundleIdentifier: "com.example.WeChatCopy",
      aliasAccountIDs: ["group-path-id"]
    )
    #expect(account.matches(accountID: reference.opaqueID))
    #expect(account.matches(accountID: "group-path-id"))
    #expect(!account.matches(accountID: "other"))
  }

  @Test func accountListingStaysAnchoredToTheVerifiedDirectory() throws {
    let fileManager = FileManager.default
    let base = fileManager.temporaryDirectory.appending(
      path: "greenbubbles-account-tests-\(UUID().uuidString)", directoryHint: .isDirectory)
    defer { try? fileManager.removeItem(at: base) }
    let files = base.appending(path: "xwechat_files", directoryHint: .isDirectory)
    try fileManager.createDirectory(
      at: files.appending(path: "real/db_storage"), withIntermediateDirectories: true)
    try fileManager.createDirectory(
      at: files.appending(path: "real/msg/attach"), withIntermediateDirectories: true)
    // An account whose db_storage is a link, and an account that is a link.
    let elsewhere = base.appending(path: "elsewhere/db_storage")
    try fileManager.createDirectory(at: elsewhere, withIntermediateDirectories: true)
    try fileManager.createDirectory(
      at: files.appending(path: "linked-db"), withIntermediateDirectories: true)
    try fileManager.createSymbolicLink(
      at: files.appending(path: "linked-db/db_storage"), withDestinationURL: elsewhere)
    try fileManager.createSymbolicLink(
      at: files.appending(path: "linked-account"),
      withDestinationURL: base.appending(path: "elsewhere"))

    let descriptor = open(files.path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
    #expect(descriptor >= 0)
    defer { close(descriptor) }
    // Swap the verified path for a link to other data after opening it.
    let decoy = base.appending(path: "decoy", directoryHint: .isDirectory)
    try fileManager.createDirectory(
      at: decoy.appending(path: "decoy-account/db_storage"), withIntermediateDirectories: true)
    try fileManager.moveItem(at: files, to: base.appending(path: "moved"))
    try fileManager.createSymbolicLink(at: files, withDestinationURL: decoy)

    let accounts = WeChatAppContainerScanner.accounts(inFiles: descriptor, at: files)
    #expect(accounts.map(\.root.lastPathComponent) == ["real"])
    #expect(accounts.first?.attachmentRoot != nil)
    #expect(accounts.first?.isReadable == true)
  }

  @Test func reportsTheOwnerOfSelectedAccounts() throws {
    let home = FileManager.default.temporaryDirectory.appending(
      path: "greenbubbles-account-tests-\(UUID().uuidString)", directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: home) }
    try FileManager.default.createDirectory(
      at: home.appending(
        path: "Library/Containers/com.example.WeChatCopy/Data/Documents/xwechat_files/a/db_storage"),
      withIntermediateDirectories: true)
    let discovery = WeChatAccountDiscovery(homeDirectory: home)
    let accountID = try #require(discovery.discover().first?.accountID)
    #expect(
      discovery.containerBundleIdentifiers(accountID: accountID) == ["com.example.WeChatCopy"])
    #expect(discovery.containerBundleIdentifiers(accountID: "missing").isEmpty)
  }
}
