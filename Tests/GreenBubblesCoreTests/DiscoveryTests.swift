import Foundation
import Testing

@testable import GreenBubblesCore

struct DiscoveryTests {
  @Test func discoversSyntheticApplicationAndRedactsLocations() throws {
    let root = FileManager.default.temporaryDirectory
      .appending(path: "greenbubbles-discovery-\(UUID().uuidString)", directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: root) }

    let application = root.appending(path: "Applications/WeChat.app", directoryHint: .isDirectory)
    let contents = application.appending(path: "Contents", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: contents, withIntermediateDirectories: true)
    let info: [String: String] = [
      "CFBundleIdentifier": "com.example.synthetic-wechat",
      "CFBundleShortVersionString": "1.2.3",
      "CFBundleVersion": "123",
    ]
    let data = try PropertyListSerialization.data(fromPropertyList: info, format: .xml, options: 0)
    try data.write(to: contents.appending(path: "Info.plist"))

    let report = WeChatDiscovery(
      homeDirectory: root,
      includePaths: false,
      additionalApplicationURLs: [application]
    ).discover()

    let installation = try #require(
      report.installations.first {
        $0.bundleIdentifier == "com.example.synthetic-wechat"
      })
    #expect(installation.version == "1.2.3")
    #expect(installation.build == "123")
    #expect(installation.location.path == nil)
    #expect(installation.location.opaqueID.count == 24)
  }

  @Test func pathIdentifiersAreStableButPathDisclosureIsOptional() {
    let url = URL(fileURLWithPath: "/private/example/messages.db")
    let redacted = PathPrivacy().reference(for: url)
    let visible = PathPrivacy(includePaths: true).reference(for: url)

    #expect(redacted.opaqueID == visible.opaqueID)
    #expect(redacted.path == nil)
    #expect(visible.path == url.standardizedFileURL.path)
  }

  @Test func reportsDuplicatedAppContainersThatHoldWeChatData() throws {
    let home = FileManager.default.temporaryDirectory
      .appending(path: "greenbubbles-discovery-\(UUID().uuidString)", directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: home) }
    let containers = home.appending(path: "Library/Containers", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(
      at: containers.appending(path: "com.example.WeChatCopy/Data/Documents/xwechat_files"),
      withIntermediateDirectories: true
    )
    try FileManager.default.createDirectory(
      at: containers.appending(path: "com.example.Other/Data/Documents"),
      withIntermediateDirectories: true
    )

    // Unrelated data beside xwechat_files in the copy's container.
    try FileManager.default.createDirectory(
      at: containers.appending(path: "com.example.WeChatCopy/Data/Library/Other"),
      withIntermediateDirectories: true
    )

    let discovery = WeChatDiscovery(homeDirectory: home, includePaths: true)
    let report = discovery.discover()
    #expect(report.dataRoots.count == 1)
    let root = try #require(report.dataRoots.first)
    // Only the matched xwechat_files directory is a root, never the container.
    #expect(
      root.location.path?.hasSuffix("/com.example.WeChatCopy/Data/Documents/xwechat_files") == true)
    #expect(root.containerBundleIdentifier == "com.example.WeChatCopy")
    #expect(root.kind == .applicationContainer)
    let accessible = discovery.accessibleDataRoots().map(\.url.lastPathComponent)
    #expect(accessible == ["xwechat_files"])
    // Automatic inventory keeps the owner; supplied roots have none.
    let owned = discovery.accessibleDataRootsWithOwners()
    #expect(owned.map(\.containerBundleIdentifier) == ["com.example.WeChatCopy"])
    let inventory = ArtifactInventory().inventory(
      ownedRoots: owned + [(home, .supplied, nil)])
    #expect(
      inventory.roots.map(\.containerBundleIdentifier) == ["com.example.WeChatCopy", nil])
  }

  @Test func differentlyCasedStandardContainerIsStillReported() throws {
    let home = FileManager.default.temporaryDirectory
      .appending(path: "greenbubbles-discovery-\(UUID().uuidString)", directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: home) }
    let containers = home.appending(path: "Library/Containers", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(
      at: containers.appending(path: "com.tencent.xinwechat/Data/Documents/xwechat_files"),
      withIntermediateDirectories: true
    )

    let discovery = WeChatDiscovery(homeDirectory: home, includePaths: true)
    let roots = discovery.discover().dataRoots
    // Case-insensitive volume: the standard container path is this directory
    // and is reported once. Case-sensitive volume: the hard-coded spelling
    // does not exist, so the scanned xwechat_files root is reported.
    #expect(roots.count == 1)
    #expect(roots.first?.containerBundleIdentifier == "com.tencent.xinwechat")
    #expect(discovery.accessibleDataRoots().count == 1)
  }

  @Test func linkedStandardFilesDoNotHideTheRealDuplicateRoot() throws {
    let fileManager = FileManager.default
    let home = fileManager.temporaryDirectory
      .appending(path: "greenbubbles-discovery-\(UUID().uuidString)", directoryHint: .isDirectory)
    defer { try? fileManager.removeItem(at: home) }
    let containers = home.appending(path: "Library/Containers", directoryHint: .isDirectory)
    let duplicateFiles = containers.appending(
      path: "com.example.WeChatCopy/Data/Documents/xwechat_files")
    try fileManager.createDirectory(at: duplicateFiles, withIntermediateDirectories: true)
    let standardDocuments = containers.appending(path: "com.tencent.xinWeChat/Data/Documents")
    try fileManager.createDirectory(at: standardDocuments, withIntermediateDirectories: true)
    try fileManager.createSymbolicLink(
      at: standardDocuments.appending(path: "xwechat_files"), withDestinationURL: duplicateFiles)

    // Inventory skips symbolic links, so the real duplicate directory must
    // remain a root of its own.
    let discovery = WeChatDiscovery(homeDirectory: home, includePaths: true)
    let roots = discovery.discover().dataRoots
    #expect(
      roots.contains {
        $0.containerBundleIdentifier == "com.example.WeChatCopy"
          && $0.location.path?.hasSuffix("/com.example.WeChatCopy/Data/Documents/xwechat_files")
            == true
      })
    #expect(
      discovery.accessibleDataRoots().contains {
        $0.url.standardizedFileURL.path == duplicateFiles.standardizedFileURL.path
      })
  }
}
