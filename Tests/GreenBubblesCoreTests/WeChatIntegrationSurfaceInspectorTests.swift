import Foundation
import Testing

@testable import GreenBubblesCore

@Suite("WeChatIntegrationSurfaceInspectorTests")
struct WeChatIntegrationSurfaceInspectorTests {
  @Test
  func inventoriesPinnedStaticSurfacesWithoutDisclosingApplicationPath() throws {
    let fixture = try makeFixture()
    defer {
      try? FileManager.default.removeItem(at: fixture.application.deletingLastPathComponent())
    }

    let report = try makeInspector(build: fixture.build).inspect(
      application: fixture.application,
      clientBuild: fixture.build
    )

    #expect(report.inspectionMode == .staticSignedBundleMetadata)
    #expect(report.components.count == 5)
    #expect(
      report.components.contains {
        $0.bundleIdentifier == "com.example.wechat.file-provider"
          && $0.extensionPointIdentifier == "com.apple.fileprovider-nonui"
          && $0.fileProviderSupportsEnumeration == true
          && $0.fileProviderDocumentGroup == "TEAM.com.example.wechat"
      })
    #expect(
      report.boundaries.contains {
        $0.kind == .machLookupException
          && $0.identifier == "com.example.wechat-internal"
          && $0.classification == .internalServiceReference
          && $0.authenticatedReadEvidence == .notProven
      })
    #expect(
      report.boundaries.contains {
        $0.kind == .extensionPoint
          && $0.identifier == "com.apple.share-services"
          && $0.classification == .inboundHandoff
      })
    #expect(
      report.boundaries.contains {
        $0.kind == .bundledFramework
          && $0.identifier == "PrivateTransport"
          && $0.classification == .bundledImplementationDetail
      })
    #expect(report.activeRead.state == .unavailable)
    #expect(!report.activeRead.highLevelAuthenticatedReadContractProven)
    #expect(!report.activeRead.credentialExtractionPerformed)
    #expect(!report.activeRead.liveProcessInteractionPerformed)

    let encoded = try JSONEncoder().encode(report)
    let json = try #require(String(data: encoded, encoding: .utf8))
    #expect(!json.contains(fixture.application.deletingLastPathComponent().path))
  }

  @Test
  func rejectsAnUnpinnedBuildBeforeInterpretingBundleSurfaces() throws {
    let fixture = try makeFixture()
    defer {
      try? FileManager.default.removeItem(at: fixture.application.deletingLastPathComponent())
    }
    let unpinned = WeChatClientBuildFingerprint(
      bundleIdentifier: fixture.build.bundleIdentifier,
      marketingVersion: "99.0",
      buildVersion: fixture.build.buildVersion,
      executableSHA256: fixture.build.executableSHA256,
      signingIdentifier: fixture.build.signingIdentifier,
      teamIdentifier: fixture.build.teamIdentifier,
      codeDirectorySHA256: fixture.build.codeDirectorySHA256,
      architectures: fixture.build.architectures,
      hardenedRuntime: fixture.build.hardenedRuntime,
      signatureValid: true
    )

    #expect(throws: IntegrationSurfaceInspectionError.unsupportedClientBuild) {
      try makeInspector(build: fixture.build).inspect(
        application: fixture.application,
        clientBuild: unpinned
      )
    }
  }

  @Test
  func rejectsMalformedMetadataOnAPinnedComponent() throws {
    let fixture = try makeFixture(malformedFileProvider: true)
    defer {
      try? FileManager.default.removeItem(at: fixture.application.deletingLastPathComponent())
    }

    #expect(
      throws: IntegrationSurfaceInspectionError.malformedMetadata(
        "Contents/PlugIns/WeChatFileProviderExtension.appex"
      )
    ) {
      try makeInspector(build: fixture.build).inspect(
        application: fixture.application,
        clientBuild: fixture.build
      )
    }
  }

  private func makeInspector(
    build: WeChatClientBuildFingerprint
  ) -> WeChatIntegrationSurfaceInspector {
    WeChatIntegrationSurfaceInspector(
      supportedBuilds: [build],
      entitlementsProvider: { _, label in
        switch label {
        case ".":
          return [
            "com.apple.security.app-sandbox": true,
            "com.apple.security.application-groups": ["TEAM.com.example.wechat"],
            "com.apple.security.network.client": true,
            "com.apple.security.network.server": true,
            "com.apple.security.temporary-exception.mach-lookup.global-name": [
              "com.example.wechat-internal"
            ],
          ]
        case "Contents/MacOS/WeChatHelper.app":
          return [
            "com.apple.security.app-sandbox": true,
            "com.apple.security.inherit": true,
          ]
        case "Contents/PlugIns/WeChatFileProviderExtension.appex",
          "Contents/PlugIns/WeChatMacShare.appex":
          return [
            "com.apple.security.app-sandbox": true,
            "com.apple.security.application-groups": ["TEAM.com.example.wechat"],
            "com.apple.security.network.client": true,
            "com.apple.security.network.server": true,
          ]
        default:
          return [:]
        }
      }
    )
  }

  private func makeFixture(
    malformedFileProvider: Bool = false
  ) throws -> (application: URL, build: WeChatClientBuildFingerprint) {
    let root = FileManager.default.temporaryDirectory.appending(
      path: "greenbubbles-integration-surfaces-\(UUID().uuidString)",
      directoryHint: .isDirectory
    )
    let application = root.appending(path: "WeChat.app", directoryHint: .isDirectory)
    let build = WeChatClientBuildFingerprint(
      bundleIdentifier: "com.example.wechat",
      marketingVersion: "1.2.3",
      buildVersion: "123",
      executableSHA256: String(repeating: "a", count: 64),
      signingIdentifier: "com.example.wechat",
      teamIdentifier: "TEAM",
      codeDirectorySHA256: String(repeating: "b", count: 64),
      architectures: ["arm64"],
      hardenedRuntime: true,
      signatureValid: true
    )

    try writeBundle(
      application,
      info: [
        "CFBundleIdentifier": build.bundleIdentifier,
        "CFBundleShortVersionString": build.marketingVersion,
        "CFBundleVersion": build.buildVersion,
        "CFBundlePackageType": "APPL",
        "CFBundleURLTypes": [
          ["CFBundleURLSchemes": ["wechat-test"]]
        ],
        "NSDataAccessSecurityPolicy": [
          "AllowProcesses": ["TEAM": ["com.example.input-method"]]
        ],
      ]
    )
    try writeBundle(
      application.appending(path: "Contents/MacOS/WeChatHelper.app"),
      info: [
        "CFBundleIdentifier": "com.example.wechat.helper",
        "CFBundlePackageType": "APPL",
      ]
    )
    let fileProviderExtension: Any =
      malformedFileProvider
      ? "not-a-dictionary"
      : [
        "NSExtensionPointIdentifier": "com.apple.fileprovider-nonui",
        "NSExtensionFileProviderSupportsEnumeration": true,
        "NSExtensionFileProviderDocumentGroup": "TEAM.com.example.wechat",
      ]
    try writeBundle(
      application.appending(path: "Contents/PlugIns/WeChatFileProviderExtension.appex"),
      info: [
        "CFBundleIdentifier": "com.example.wechat.file-provider",
        "CFBundlePackageType": "XPC!",
        "NSExtension": fileProviderExtension,
      ]
    )
    try writeBundle(
      application.appending(path: "Contents/PlugIns/WeChatMacShare.appex"),
      info: [
        "CFBundleIdentifier": "com.example.wechat.share",
        "CFBundlePackageType": "XPC!",
        "NSExtension": [
          "NSExtensionPointIdentifier": "com.apple.share-services"
        ],
      ]
    )
    try writeBundle(
      application.appending(path: "Contents/XPCServices/DebugHelper.xpc"),
      info: [
        "CFBundleIdentifier": "com.example.wechat.debug-helper",
        "CFBundlePackageType": "XPC!",
        "XPCService": ["ServiceType": "Application"],
      ]
    )
    try FileManager.default.createDirectory(
      at: application.appending(path: "Contents/Frameworks/PrivateTransport.framework"),
      withIntermediateDirectories: true
    )
    return (application, build)
  }

  private func writeBundle(_ bundle: URL, info: [String: Any]) throws {
    let contents = bundle.appending(path: "Contents", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: contents, withIntermediateDirectories: true)
    let data = try PropertyListSerialization.data(fromPropertyList: info, format: .xml, options: 0)
    try data.write(to: contents.appending(path: "Info.plist"))
  }
}
