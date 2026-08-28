import CryptoKit
import Darwin
import Foundation

public enum AcquisitionSurfaceInspectionMode: String, Codable, Sendable {
  case staticPinnedBundleBytes
}

public enum OfficialAcquisitionSurface: String, Codable, CaseIterable, Sendable {
  case backupAndRestore
  case chatHistoryMigration
  case deviceTransfer
  case fileExport
}

public enum OfficialAcquisitionSurfaceClassification: String, Codable, Sendable {
  case userMediatedArchiveWorkflow
  case userMediatedMigrationWorkflow
  case userMediatedDeviceTransferWorkflow
  case genericFileExportWorkflow
}

public enum StaticFeatureEvidenceState: String, Codable, Sendable {
  case observed
  case notObserved
}

public struct OfficialAcquisitionSurfaceEvidence: Codable, Equatable, Sendable {
  public let surface: OfficialAcquisitionSurface
  public let state: StaticFeatureEvidenceState
  public let classification: OfficialAcquisitionSurfaceClassification
  public let requiredMarkerCount: Int
  public let observedMarkerCount: Int

  public init(
    surface: OfficialAcquisitionSurface,
    state: StaticFeatureEvidenceState,
    classification: OfficialAcquisitionSurfaceClassification,
    requiredMarkerCount: Int,
    observedMarkerCount: Int
  ) {
    self.surface = surface
    self.state = state
    self.classification = classification
    self.requiredMarkerCount = requiredMarkerCount
    self.observedMarkerCount = observedMarkerCount
  }
}

public struct AcquisitionSurfaceConclusions: Codable, Equatable, Sendable {
  public let officialBackupAndRestoreUIObserved: Bool
  public let officialChatHistoryMigrationUIObserved: Bool
  public let officialDeviceTransferUIObserved: Bool
  public let genericFileExportUIObserved: Bool
  public let portablePlaintextConversationExportProven: Bool
  public let officialBackupFormatCompatibilityProven: Bool
  public let completeConversationAndAttachmentCoverageProven: Bool
  public let reusableCredentialExportPerformed: Bool
  public let liveClientInteractionPerformed: Bool

  public init(
    officialBackupAndRestoreUIObserved: Bool,
    officialChatHistoryMigrationUIObserved: Bool,
    officialDeviceTransferUIObserved: Bool,
    genericFileExportUIObserved: Bool,
    portablePlaintextConversationExportProven: Bool = false,
    officialBackupFormatCompatibilityProven: Bool = false,
    completeConversationAndAttachmentCoverageProven: Bool = false,
    reusableCredentialExportPerformed: Bool = false,
    liveClientInteractionPerformed: Bool = false
  ) {
    self.officialBackupAndRestoreUIObserved = officialBackupAndRestoreUIObserved
    self.officialChatHistoryMigrationUIObserved = officialChatHistoryMigrationUIObserved
    self.officialDeviceTransferUIObserved = officialDeviceTransferUIObserved
    self.genericFileExportUIObserved = genericFileExportUIObserved
    self.portablePlaintextConversationExportProven = portablePlaintextConversationExportProven
    self.officialBackupFormatCompatibilityProven = officialBackupFormatCompatibilityProven
    self.completeConversationAndAttachmentCoverageProven =
      completeConversationAndAttachmentCoverageProven
    self.reusableCredentialExportPerformed = reusableCredentialExportPerformed
    self.liveClientInteractionPerformed = liveClientInteractionPerformed
  }
}

public struct WeChatAcquisitionSurfaceReport: Codable, Equatable, Sendable {
  public let reportFormatVersion: Int
  public let generatedAt: Date
  public let inspectionMode: AcquisitionSurfaceInspectionMode
  public let clientBuild: WeChatClientBuildFingerprint
  public let inspectedResourceRelativePath: String
  public let inspectedResourceByteCount: Int64
  public let inspectedResourceSHA256: String
  public let surfaces: [OfficialAcquisitionSurfaceEvidence]
  public let conclusions: AcquisitionSurfaceConclusions

  public init(
    reportFormatVersion: Int = 1,
    generatedAt: Date,
    inspectionMode: AcquisitionSurfaceInspectionMode = .staticPinnedBundleBytes,
    clientBuild: WeChatClientBuildFingerprint,
    inspectedResourceRelativePath: String,
    inspectedResourceByteCount: Int64,
    inspectedResourceSHA256: String,
    surfaces: [OfficialAcquisitionSurfaceEvidence],
    conclusions: AcquisitionSurfaceConclusions
  ) {
    self.reportFormatVersion = reportFormatVersion
    self.generatedAt = generatedAt
    self.inspectionMode = inspectionMode
    self.clientBuild = clientBuild
    self.inspectedResourceRelativePath = inspectedResourceRelativePath
    self.inspectedResourceByteCount = inspectedResourceByteCount
    self.inspectedResourceSHA256 = inspectedResourceSHA256
    self.surfaces = surfaces
    self.conclusions = conclusions
  }
}

public enum AcquisitionSurfaceInspectionError: Error, Equatable, CustomStringConvertible {
  case unsupportedClientBuild
  case invalidApplicationBundle
  case unsafeResource
  case resourceTooLarge
  case malformedMetadata
  case posix(operation: String, code: Int32)

  public var description: String {
    switch self {
    case .unsupportedClientBuild:
      return "Static acquisition inspection is unavailable for this unpinned WeChat build"
    case .invalidApplicationBundle:
      return "The WeChat application bundle is missing or unsafe"
    case .unsafeResource:
      return "The pinned acquisition feature resource is missing or unsafe"
    case .resourceTooLarge:
      return "The pinned acquisition feature resource exceeds the inspection limit"
    case .malformedMetadata:
      return "The WeChat application bundle metadata does not match the inspected build"
    case .posix(let operation, let code):
      return "\(operation) failed with POSIX error \(code)"
    }
  }
}

public struct WeChatAcquisitionSurfaceInspector: Sendable {
  private static let resourceRelativePath = "Contents/Resources/wechat.dylib"
  private static let maximumResourceBytes: Int64 = 512 * 1_024 * 1_024
  private static let maximumMetadataBytes: Int64 = 1_048_576

  private struct FeatureDefinition: Sendable {
    let surface: OfficialAcquisitionSurface
    let classification: OfficialAcquisitionSurfaceClassification
    let markers: [String]
  }

  private static let features = [
    FeatureDefinition(
      surface: .backupAndRestore,
      classification: .userMediatedArchiveWorkflow,
      markers: ["backup_and_resto", "BackupRestoreVie", "BackupFlowViewMo", "RestoreFlowView"]
    ),
    FeatureDefinition(
      surface: .chatHistoryMigration,
      classification: .userMediatedMigrationWorkflow,
      markers: ["ChatlogMigrateVi", "LocalMigrateView", "RemoteMigrateVie"]
    ),
    FeatureDefinition(
      surface: .deviceTransfer,
      classification: .userMediatedDeviceTransferWorkflow,
      markers: ["startExportToPC", "StartExportToPho", "RemoteImportMode"]
    ),
    FeatureDefinition(
      surface: .fileExport,
      classification: .genericFileExportWorkflow,
      markers: ["BatchExportWindo", "StartExportFile", "GetExportDestPat"]
    ),
  ]

  private let homeDirectory: URL
  private let supportedBuilds: [WeChatClientBuildFingerprint]

  public init(homeDirectory: URL = FileManager.default.homeDirectoryForCurrentUser) {
    self.homeDirectory = homeDirectory.standardizedFileURL
    self.supportedBuilds = [WeChatIntegrationSurfaceInspector.pinnedWeChat4113]
  }

  init(
    homeDirectory: URL = FileManager.default.homeDirectoryForCurrentUser,
    supportedBuilds: [WeChatClientBuildFingerprint]
  ) {
    self.homeDirectory = homeDirectory.standardizedFileURL
    self.supportedBuilds = supportedBuilds
  }

  public func inspectDefaultInstallation() throws -> WeChatAcquisitionSurfaceReport? {
    let buildInspector = WeChatClientBuildInspector(homeDirectory: homeDirectory)
    guard let application = buildInspector.defaultApplicationURL() else { return nil }
    let build = try buildInspector.inspect(application: application)
    return try inspect(application: application, clientBuild: build)
  }

  public func inspect(
    application: URL,
    clientBuild: WeChatClientBuildFingerprint
  ) throws -> WeChatAcquisitionSurfaceReport {
    guard supportedBuilds.contains(clientBuild), clientBuild.signatureValid else {
      throw AcquisitionSurfaceInspectionError.unsupportedClientBuild
    }
    let application = application.standardizedFileURL
    try validateDirectory(application)
    try validateMetadata(application: application, clientBuild: clientBuild)
    let resource = application.appending(path: Self.resourceRelativePath).standardizedFileURL
    guard resource.path.hasPrefix(application.path + "/") else {
      throw AcquisitionSurfaceInspectionError.unsafeResource
    }

    let markerData = Self.features.flatMap(\.markers).map { Data($0.utf8) }
    let scan = try scanResource(resource, patterns: markerData)
    var markerIndex = 0
    var surfaces: [OfficialAcquisitionSurfaceEvidence] = []
    var observed: [OfficialAcquisitionSurface: Bool] = [:]
    for feature in Self.features {
      let matches = scan.matches[markerIndex..<(markerIndex + feature.markers.count)]
      markerIndex += feature.markers.count
      let observedMarkerCount = matches.filter { $0 }.count
      let isObserved = observedMarkerCount == feature.markers.count
      observed[feature.surface] = isObserved
      surfaces.append(
        OfficialAcquisitionSurfaceEvidence(
          surface: feature.surface,
          state: isObserved ? .observed : .notObserved,
          classification: feature.classification,
          requiredMarkerCount: feature.markers.count,
          observedMarkerCount: observedMarkerCount
        ))
    }

    return WeChatAcquisitionSurfaceReport(
      generatedAt: Date(),
      clientBuild: clientBuild,
      inspectedResourceRelativePath: Self.resourceRelativePath,
      inspectedResourceByteCount: scan.byteCount,
      inspectedResourceSHA256: scan.sha256,
      surfaces: surfaces,
      conclusions: AcquisitionSurfaceConclusions(
        officialBackupAndRestoreUIObserved: observed[.backupAndRestore] == true,
        officialChatHistoryMigrationUIObserved: observed[.chatHistoryMigration] == true,
        officialDeviceTransferUIObserved: observed[.deviceTransfer] == true,
        genericFileExportUIObserved: observed[.fileExport] == true
      )
    )
  }

  private struct ResourceScan {
    let byteCount: Int64
    let sha256: String
    let matches: [Bool]
  }

  private func scanResource(_ url: URL, patterns: [Data]) throws -> ResourceScan {
    let descriptor = Darwin.open(url.path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW)
    guard descriptor >= 0 else {
      throw AcquisitionSurfaceInspectionError.posix(
        operation: "open acquisition resource", code: errno)
    }
    defer { Darwin.close(descriptor) }
    var metadata = stat()
    guard Darwin.fstat(descriptor, &metadata) == 0 else {
      throw AcquisitionSurfaceInspectionError.posix(
        operation: "inspect acquisition resource",
        code: errno
      )
    }
    guard metadata.st_mode & S_IFMT == S_IFREG, metadata.st_nlink == 1 else {
      throw AcquisitionSurfaceInspectionError.unsafeResource
    }
    guard metadata.st_size >= 0, metadata.st_size <= Self.maximumResourceBytes else {
      throw AcquisitionSurfaceInspectionError.resourceTooLarge
    }
    let before = fileIdentity(metadata)
    let maximumPatternBytes = patterns.map(\.count).max() ?? 0
    var matches = [Bool](repeating: false, count: patterns.count)
    var hasher = SHA256()
    var buffer = [UInt8](repeating: 0, count: 1_024 * 1_024)
    var tail = Data()
    var total: Int64 = 0
    while true {
      let count = Darwin.read(descriptor, &buffer, buffer.count)
      if count == 0 { break }
      guard count > 0 else {
        throw AcquisitionSurfaceInspectionError.posix(
          operation: "read acquisition resource", code: errno)
      }
      let chunk = Data(buffer[0..<count])
      hasher.update(data: chunk)
      total += Int64(count)
      let searchable = tail + chunk
      for index in patterns.indices where !matches[index] {
        matches[index] = searchable.range(of: patterns[index]) != nil
      }
      let tailCount = min(max(0, maximumPatternBytes - 1), searchable.count)
      tail = searchable.suffix(tailCount)
    }
    guard total == metadata.st_size,
      Darwin.fstat(descriptor, &metadata) == 0,
      fileIdentity(metadata) == before
    else { throw AcquisitionSurfaceInspectionError.unsafeResource }
    return ResourceScan(
      byteCount: total,
      sha256: hasher.finalize().map { String(format: "%02x", $0) }.joined(),
      matches: matches
    )
  }

  private func validateDirectory(_ url: URL) throws {
    var metadata = stat()
    guard Darwin.lstat(url.path, &metadata) == 0 else {
      throw AcquisitionSurfaceInspectionError.posix(
        operation: "inspect application bundle", code: errno)
    }
    guard metadata.st_mode & S_IFMT == S_IFDIR else {
      throw AcquisitionSurfaceInspectionError.invalidApplicationBundle
    }
  }

  private func validateMetadata(
    application: URL,
    clientBuild: WeChatClientBuildFingerprint
  ) throws {
    let info = application.appending(path: "Contents/Info.plist").standardizedFileURL
    guard info.path.hasPrefix(application.path + "/") else {
      throw AcquisitionSurfaceInspectionError.malformedMetadata
    }
    let descriptor = Darwin.open(info.path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW)
    guard descriptor >= 0 else {
      throw AcquisitionSurfaceInspectionError.posix(operation: "open bundle metadata", code: errno)
    }
    defer { Darwin.close(descriptor) }
    var metadata = stat()
    guard Darwin.fstat(descriptor, &metadata) == 0,
      metadata.st_mode & S_IFMT == S_IFREG,
      metadata.st_nlink == 1,
      metadata.st_size >= 0,
      metadata.st_size <= Self.maximumMetadataBytes
    else { throw AcquisitionSurfaceInspectionError.malformedMetadata }
    let data =
      try FileHandle(fileDescriptor: descriptor, closeOnDealloc: false).readToEnd()
      ?? Data()
    guard
      let value = try? PropertyListSerialization.propertyList(from: data, format: nil),
      let dictionary = value as? [String: Any],
      dictionary["CFBundleIdentifier"] as? String == clientBuild.bundleIdentifier,
      dictionary["CFBundleShortVersionString"] as? String == clientBuild.marketingVersion,
      dictionary["CFBundleVersion"] as? String == clientBuild.buildVersion
    else { throw AcquisitionSurfaceInspectionError.malformedMetadata }
  }

  private func fileIdentity(_ metadata: stat) -> [Int64] {
    [
      Int64(metadata.st_dev),
      Int64(metadata.st_ino),
      metadata.st_size,
      Int64(metadata.st_mtimespec.tv_sec),
      Int64(metadata.st_mtimespec.tv_nsec),
    ]
  }
}
