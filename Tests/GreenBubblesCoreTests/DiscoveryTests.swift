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
}
