import CryptoKit
import Darwin
import Foundation
import Testing

@testable import GreenBubblesCore

@Suite("WeChatAcquisitionSurfaceInspectorTests")
struct WeChatAcquisitionSurfaceInspectorTests {
  @Test
  func reportsOfficialWorkflowEvidenceWithoutClaimingPortableCoverage() throws {
    let fixture = try makeFixture()
    defer { try? FileManager.default.removeItem(at: fixture.root) }

    let report = try WeChatAcquisitionSurfaceInspector(
      supportedBuilds: [fixture.build]
    ).inspect(application: fixture.application, clientBuild: fixture.build)

    #expect(report.inspectionMode == .staticPinnedBundleBytes)
    #expect(report.inspectedResourceRelativePath == "Contents/Resources/wechat.dylib")
    #expect(report.inspectedResourceByteCount == Int64(fixture.resource.count))
    #expect(
      report.inspectedResourceSHA256
        == SHA256.hash(data: fixture.resource).map { String(format: "%02x", $0) }.joined()
    )
    #expect(report.surfaces.count == OfficialAcquisitionSurface.allCases.count)
    #expect(report.surfaces.allSatisfy { $0.state == .observed })
    #expect(report.conclusions.officialBackupAndRestoreUIObserved)
    #expect(report.conclusions.officialChatHistoryMigrationUIObserved)
    #expect(report.conclusions.officialDeviceTransferUIObserved)
    #expect(report.conclusions.genericFileExportUIObserved)
    #expect(!report.conclusions.portablePlaintextConversationExportProven)
    #expect(!report.conclusions.officialBackupFormatCompatibilityProven)
    #expect(!report.conclusions.completeConversationAndAttachmentCoverageProven)
    #expect(!report.conclusions.reusableCredentialExportPerformed)
    #expect(!report.conclusions.liveClientInteractionPerformed)

    let json = try #require(
      String(data: JSONEncoder().encode(report), encoding: .utf8)
    )
    #expect(!json.contains(fixture.root.path))
  }

  @Test
  func missingMarkerPreventsAnObservedSurfaceVerdict() throws {
    let fixture = try makeFixture(omitting: "RemoteImportMode")
    defer { try? FileManager.default.removeItem(at: fixture.root) }

    let report = try WeChatAcquisitionSurfaceInspector(
      supportedBuilds: [fixture.build]
    ).inspect(application: fixture.application, clientBuild: fixture.build)
    let transfer = try #require(
      report.surfaces.first { $0.surface == .deviceTransfer }
    )
    #expect(transfer.state == .notObserved)
    #expect(transfer.observedMarkerCount == transfer.requiredMarkerCount - 1)
    #expect(!report.conclusions.officialDeviceTransferUIObserved)
  }

  @Test
  func findsMarkersThatCrossAReadChunkBoundary() throws {
    let fixture = try makeFixture(prefixByteCount: 1_048_576 - 5)
    defer { try? FileManager.default.removeItem(at: fixture.root) }

    let report = try WeChatAcquisitionSurfaceInspector(
      supportedBuilds: [fixture.build]
    ).inspect(application: fixture.application, clientBuild: fixture.build)

    #expect(report.surfaces.allSatisfy { $0.state == .observed })
  }

  @Test
  func rejectsUnpinnedBuildAndSymlinkedFeatureResource() throws {
    let fixture = try makeFixture()
    defer { try? FileManager.default.removeItem(at: fixture.root) }
    let inspector = WeChatAcquisitionSurfaceInspector(supportedBuilds: [fixture.build])
    let unpinned = WeChatClientBuildFingerprint(
      bundleIdentifier: fixture.build.bundleIdentifier,
      marketingVersion: "99",
      buildVersion: fixture.build.buildVersion,
      executableSHA256: fixture.build.executableSHA256,
      signingIdentifier: fixture.build.signingIdentifier,
      teamIdentifier: fixture.build.teamIdentifier,
      codeDirectorySHA256: fixture.build.codeDirectorySHA256,
      architectures: fixture.build.architectures,
      hardenedRuntime: fixture.build.hardenedRuntime,
      signatureValid: true
    )
    #expect(throws: AcquisitionSurfaceInspectionError.unsupportedClientBuild) {
      try inspector.inspect(application: fixture.application, clientBuild: unpinned)
    }

    let resource = fixture.application.appending(path: "Contents/Resources/wechat.dylib")
    try FileManager.default.removeItem(at: resource)
    let outside = fixture.root.appending(path: "outside.dylib")
    try fixture.resource.write(to: outside)
    try FileManager.default.createSymbolicLink(at: resource, withDestinationURL: outside)
    #expect(
      throws: AcquisitionSurfaceInspectionError.posix(
        operation: "open acquisition resource", code: ELOOP)
    ) {
      try inspector.inspect(application: fixture.application, clientBuild: fixture.build)
    }
  }

  private func makeFixture(
    omitting omittedMarker: String? = nil,
    prefixByteCount: Int = 0
  ) throws -> (
    root: URL,
    application: URL,
    build: WeChatClientBuildFingerprint,
    resource: Data
  ) {
    let root = FileManager.default.temporaryDirectory.appending(
      path: "greenbubbles-acquisition-surfaces-\(UUID().uuidString)",
      directoryHint: .isDirectory
    )
    let application = root.appending(path: "WeChat.app", directoryHint: .isDirectory)
    let contents = application.appending(path: "Contents", directoryHint: .isDirectory)
    let resources = contents.appending(path: "Resources", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: resources, withIntermediateDirectories: true)
    let build = WeChatClientBuildFingerprint(
      bundleIdentifier: "com.example.synthetic-wechat",
      marketingVersion: "1.2.3",
      buildVersion: "123",
      executableSHA256: String(repeating: "a", count: 64),
      signingIdentifier: "com.example.synthetic-wechat",
      teamIdentifier: "TEAM",
      codeDirectorySHA256: String(repeating: "b", count: 64),
      architectures: ["arm64"],
      hardenedRuntime: true,
      signatureValid: true
    )
    let info: [String: String] = [
      "CFBundleIdentifier": build.bundleIdentifier,
      "CFBundleShortVersionString": build.marketingVersion,
      "CFBundleVersion": build.buildVersion,
    ]
    try PropertyListSerialization.data(fromPropertyList: info, format: .xml, options: 0)
      .write(to: contents.appending(path: "Info.plist"))
    let markers: [String] = [
      "backup_and_resto", "BackupRestoreVie", "BackupFlowViewMo", "RestoreFlowView",
      "ChatlogMigrateVi", "LocalMigrateView", "RemoteMigrateVie", "startExportToPC",
      "StartExportToPho", "RemoteImportMode", "BatchExportWindo", "StartExportFile",
      "GetExportDestPat",
    ].filter { marker in
      omittedMarker.map { marker != $0 } ?? true
    }
    var resource = Data(repeating: 0x78, count: prefixByteCount)
    resource.append(Data(markers.joined(separator: "\0synthetic-padding\0").utf8))
    try resource.write(to: resources.appending(path: "wechat.dylib"))
    return (root, application, build, resource)
  }
}
