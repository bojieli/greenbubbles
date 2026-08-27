import Foundation

public struct PathReference: Codable, Equatable, Sendable {
  public let opaqueID: String
  public let path: String?

  public init(opaqueID: String, path: String?) {
    self.opaqueID = opaqueID
    self.path = path
  }
}

public struct WeChatInstallation: Codable, Equatable, Sendable {
  public let location: PathReference
  public let bundleIdentifier: String?
  public let version: String?
  public let build: String?

  public init(
    location: PathReference,
    bundleIdentifier: String?,
    version: String?,
    build: String?
  ) {
    self.location = location
    self.bundleIdentifier = bundleIdentifier
    self.version = version
    self.build = build
  }
}

public enum DataRootKind: String, Codable, Sendable {
  case applicationContainer
  case groupContainer
  case supplied
}

public struct CandidateDataRoot: Codable, Equatable, Sendable {
  public let location: PathReference
  public let kind: DataRootKind
  public let isReadable: Bool

  public init(location: PathReference, kind: DataRootKind, isReadable: Bool) {
    self.location = location
    self.kind = kind
    self.isReadable = isReadable
  }
}

public struct DiscoveryReport: Codable, Equatable, Sendable {
  public let reportFormatVersion: Int
  public let generatedAt: Date
  public let installations: [WeChatInstallation]
  public let dataRoots: [CandidateDataRoot]

  public init(
    reportFormatVersion: Int = 1,
    generatedAt: Date,
    installations: [WeChatInstallation],
    dataRoots: [CandidateDataRoot]
  ) {
    self.reportFormatVersion = reportFormatVersion
    self.generatedAt = generatedAt
    self.installations = installations
    self.dataRoots = dataRoots
  }
}

public enum ArtifactKind: String, Codable, CaseIterable, Sendable {
  case database
  case writeAheadLog
  case sharedMemory
  case index
  case serializedData
  case configuration
  case image
  case audio
  case video
  case document
}

public struct ArtifactMetadata: Codable, Equatable, Sendable {
  public let location: PathReference
  public let rootID: String
  public let kind: ArtifactKind
  public let byteCount: Int64?
  public let modifiedAt: Date?

  public init(
    location: PathReference,
    rootID: String,
    kind: ArtifactKind,
    byteCount: Int64?,
    modifiedAt: Date?
  ) {
    self.location = location
    self.rootID = rootID
    self.kind = kind
    self.byteCount = byteCount
    self.modifiedAt = modifiedAt
  }
}

public struct InventoryIssue: Codable, Equatable, Sendable {
  public let locationID: String
  public let errorDomain: String
  public let errorCode: Int

  public init(locationID: String, errorDomain: String, errorCode: Int) {
    self.locationID = locationID
    self.errorDomain = errorDomain
    self.errorCode = errorCode
  }
}

public struct InventoryReport: Codable, Equatable, Sendable {
  public let reportFormatVersion: Int
  public let generatedAt: Date
  public let roots: [CandidateDataRoot]
  public let artifacts: [ArtifactMetadata]
  public let issues: [InventoryIssue]
  public let reachedArtifactLimit: Bool

  public init(
    reportFormatVersion: Int = 1,
    generatedAt: Date,
    roots: [CandidateDataRoot],
    artifacts: [ArtifactMetadata],
    issues: [InventoryIssue],
    reachedArtifactLimit: Bool
  ) {
    self.reportFormatVersion = reportFormatVersion
    self.generatedAt = generatedAt
    self.roots = roots
    self.artifacts = artifacts
    self.issues = issues
    self.reachedArtifactLimit = reachedArtifactLimit
  }
}
