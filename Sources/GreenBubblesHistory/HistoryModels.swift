import Foundation

public let greenBubblesAIContextSchema = "greenbubbles.ai-context.v2"
public let greenBubblesLegacyAIContextSchema = "greenbubbles.ai-context.v1"

public struct HistoryManifestFile: Codable, Equatable, Sendable {
  public let role: String
  public let relativePath: String
  public let recordCount: UInt64
  public let byteCount: UInt64
  public let sha256: String
}

public struct HistoryContextHealth: Codable, Equatable, Sendable {
  public let accountID: String
  public let selfParticipantID: String?
  public let replicaID: String
  public let sourceFingerprint: String
  public let checkpointRevision: String
  public let health: String
  public let clientBuildCompatibility: String?
  public let archiveScope: String?
  public let authoritativeDatabaseCoverage: Bool?
  public let totalDatabaseCount: Int?
  public let freshDatabaseCount: Int?
  public let unavailableDatabaseCount: Int?
  public let preservedStaleDatabaseCount: Int?
  public let conversationCount: UInt64
  public let participantCount: UInt64
  public let messageCount: UInt64
  public let artifactCount: UInt64
  public let semanticGapCount: UInt64?
  public let messageCandidateGapCount: UInt64?
  public let unavailableArtifactCount: UInt64?
  public let artifactDecodeGapCount: UInt64?
  public let entityDecodeGapCount: UInt64?
  public let checkpointAgeSeconds: UInt64?
  public let sourceCoverageComplete: Bool
  public let limitationCodes: [String]
  public let coverageNote: String

  enum CodingKeys: String, CodingKey {
    case accountID = "accountId"
    case selfParticipantID = "selfParticipantId"
    case replicaID = "replicaId"
    case sourceFingerprint
    case checkpointRevision
    case health
    case clientBuildCompatibility
    case archiveScope
    case authoritativeDatabaseCoverage
    case totalDatabaseCount
    case freshDatabaseCount
    case unavailableDatabaseCount
    case preservedStaleDatabaseCount
    case conversationCount
    case participantCount
    case messageCount
    case artifactCount
    case semanticGapCount
    case messageCandidateGapCount
    case unavailableArtifactCount
    case artifactDecodeGapCount
    case entityDecodeGapCount
    case checkpointAgeSeconds
    case sourceCoverageComplete
    case limitationCodes
    case coverageNote
  }
}

public struct HistoryBundleManifest: Codable, Equatable, Sendable {
  public let formatVersion: Int
  public let schema: String
  public let bundleID: String
  public let createdAtUnixNanoseconds: UInt64
  public let destination: String
  public let requesterID: String
  public let policySHA256: String
  public let policySourceFingerprint: String
  public let context: HistoryContextHealth
  public let enabledConversationCount: Int
  public let exportedContactCount: UInt64
  public let exportedMessageCount: UInt64
  public let exportedArtifactCount: UInt64
  public let artifactResolutionErrorCount: UInt64
  public let exportComplete: Bool
  public let files: [HistoryManifestFile]

  public var createdAt: Date {
    Date(timeIntervalSince1970: TimeInterval(createdAtUnixNanoseconds) / 1_000_000_000)
  }

  enum CodingKeys: String, CodingKey {
    case formatVersion
    case schema
    case bundleID = "bundleId"
    case createdAtUnixNanoseconds
    case destination
    case requesterID = "requesterId"
    case policySHA256
    case policySourceFingerprint
    case context
    case enabledConversationCount
    case exportedContactCount
    case exportedMessageCount
    case exportedArtifactCount
    case artifactResolutionErrorCount
    case exportComplete
    case files
  }
}

public struct HistoryParticipant: Codable, Equatable, Identifiable, Sendable {
  public let participantID: String
  public let displayName: String
  public let role: String

  public var id: String { participantID }

  enum CodingKeys: String, CodingKey {
    case participantID = "participantId"
    case displayName
    case role
  }
}

public struct HistoryConversation: Codable, Equatable, Identifiable, Sendable {
  public let formatVersion: Int
  public let conversationID: String
  public let humanLabel: String
  public let kind: String
  public let participantCount: Int
  public let participants: [HistoryParticipant]
  public let groupOwnerParticipantID: String?
  public let entityDecodeState: String
  public let sourceDatabaseFreshness: String
  public let capabilities: [String]
  public let messageFields: [String]
  public let notBeforeUnix: Int64?
  public let notAfterUnix: Int64?

  public var id: String { conversationID }

  enum CodingKeys: String, CodingKey {
    case formatVersion
    case conversationID = "conversationId"
    case humanLabel
    case kind
    case participantCount
    case participants
    case groupOwnerParticipantID = "groupOwnerParticipantId"
    case legacyOwnerParticipantID = "ownerParticipantId"
    case entityDecodeState
    case sourceDatabaseFreshness
    case capabilities
    case messageFields
    case notBeforeUnix
    case notAfterUnix
  }

  public init(from decoder: Decoder) throws {
    let values = try decoder.container(keyedBy: CodingKeys.self)
    formatVersion = try values.decode(Int.self, forKey: .formatVersion)
    conversationID = try values.decode(String.self, forKey: .conversationID)
    humanLabel = try values.decode(String.self, forKey: .humanLabel)
    kind = try values.decode(String.self, forKey: .kind)
    participantCount = try values.decode(Int.self, forKey: .participantCount)
    participants = try values.decode([HistoryParticipant].self, forKey: .participants)
    groupOwnerParticipantID =
      try values.decodeIfPresent(String.self, forKey: .groupOwnerParticipantID)
      ?? values.decodeIfPresent(String.self, forKey: .legacyOwnerParticipantID)
    entityDecodeState = try values.decode(String.self, forKey: .entityDecodeState)
    sourceDatabaseFreshness = try values.decode(
      String.self, forKey: .sourceDatabaseFreshness)
    capabilities = try values.decode([String].self, forKey: .capabilities)
    messageFields = try values.decode([String].self, forKey: .messageFields)
    notBeforeUnix = try values.decodeIfPresent(Int64.self, forKey: .notBeforeUnix)
    notAfterUnix = try values.decodeIfPresent(Int64.self, forKey: .notAfterUnix)
  }

  public func encode(to encoder: Encoder) throws {
    var values = encoder.container(keyedBy: CodingKeys.self)
    try values.encode(formatVersion, forKey: .formatVersion)
    try values.encode(conversationID, forKey: .conversationID)
    try values.encode(humanLabel, forKey: .humanLabel)
    try values.encode(kind, forKey: .kind)
    try values.encode(participantCount, forKey: .participantCount)
    try values.encode(participants, forKey: .participants)
    if formatVersion == 1 {
      try values.encodeIfPresent(groupOwnerParticipantID, forKey: .legacyOwnerParticipantID)
    } else {
      try values.encodeIfPresent(groupOwnerParticipantID, forKey: .groupOwnerParticipantID)
    }
    try values.encode(entityDecodeState, forKey: .entityDecodeState)
    try values.encode(sourceDatabaseFreshness, forKey: .sourceDatabaseFreshness)
    try values.encode(capabilities, forKey: .capabilities)
    try values.encode(messageFields, forKey: .messageFields)
    try values.encodeIfPresent(notBeforeUnix, forKey: .notBeforeUnix)
    try values.encodeIfPresent(notAfterUnix, forKey: .notAfterUnix)
  }

  @available(*, deprecated, renamed: "groupOwnerParticipantID")
  public var ownerParticipantID: String? { groupOwnerParticipantID }
}

public struct HistoryContactConversationProfile: Codable, Equatable, Identifiable, Sendable {
  public let conversationID: String
  public let conversationLabel: String
  public let displayName: String
  public let role: String

  public var id: String { "\(conversationID):\(role)" }

  enum CodingKeys: String, CodingKey {
    case conversationID = "conversationId"
    case conversationLabel
    case displayName
    case role
  }
}

public struct HistoryContact: Codable, Equatable, Identifiable, Sendable {
  public let formatVersion: Int
  public let participantID: String
  public let displayName: String
  public let localProfileAvailable: Bool
  public let sourceDatabaseFreshness: String
  public let enabledConversationIDs: [String]
  public let conversationProfiles: [HistoryContactConversationProfile]
  public let resolutionErrorCode: String?

  public var id: String { participantID }

  enum CodingKeys: String, CodingKey {
    case formatVersion
    case participantID = "participantId"
    case displayName
    case localProfileAvailable
    case sourceDatabaseFreshness
    case enabledConversationIDs = "enabledConversationIds"
    case conversationProfiles
    case resolutionErrorCode
  }
}

public struct HistoryArtifactReference: Codable, Equatable, Identifiable, Sendable {
  public let artifactID: String
  public let role: String
  public let preferred: Bool

  public var id: String { "\(artifactID):\(role)" }

  enum CodingKeys: String, CodingKey {
    case artifactID = "artifactId"
    case role
    case preferred
  }
}

public struct HistoryRelationshipReference: Codable, Equatable, Identifiable, Sendable {
  public let kind: String
  public let targetCanonicalID: String?
  public let resolved: Bool

  public var id: String { "\(kind):\(targetCanonicalID ?? "unresolved")" }

  enum CodingKeys: String, CodingKey {
    case kind
    case targetCanonicalID = "targetCanonicalId"
    case resolved
  }
}

public struct HistoryMessage: Codable, Equatable, Identifiable, Sendable {
  public let formatVersion: Int
  public let conversationLabel: String
  public let senderDisplayName: String?
  public let canonicalID: String
  public let conversationID: String
  public let sourceDatabaseFreshness: String
  public let senderID: String?
  public let createdAtUnix: Int64?
  public let conversationOrdinal: UInt64
  public let direction: String?
  public let logicalType: UInt32?
  public let subType: UInt32?
  public let payloadKind: String?
  public let payloadSummary: String?
  public let payloadSummaryTruncated: Bool?
  public let artifactReferences: [HistoryArtifactReference]
  public let relationships: [HistoryRelationshipReference]

  public var id: String { canonicalID }

  public var createdAt: Date? {
    createdAtUnix.map { Date(timeIntervalSince1970: TimeInterval($0)) }
  }

  public var displayText: String {
    if let payloadSummary, !payloadSummary.isEmpty { return payloadSummary }
    if let payloadKind, !payloadKind.isEmpty { return "[\(payloadKind)]" }
    return "[Message content unavailable]"
  }

  public func resolvedDirection(selfParticipantID: String?) -> String? {
    guard let senderID, let selfParticipantID else { return direction }
    return senderID == selfParticipantID ? "outgoing" : "incoming"
  }

  enum CodingKeys: String, CodingKey {
    case formatVersion
    case conversationLabel
    case senderDisplayName
    case canonicalID = "canonicalId"
    case conversationID = "conversationId"
    case sourceDatabaseFreshness
    case senderID = "senderId"
    case createdAtUnix
    case conversationOrdinal
    case direction
    case logicalType
    case subType
    case payloadKind
    case payloadSummary
    case payloadSummaryTruncated
    case artifactReferences
    case relationships
  }

  public init(from decoder: Decoder) throws {
    let values = try decoder.container(keyedBy: CodingKeys.self)
    formatVersion = try values.decode(Int.self, forKey: .formatVersion)
    conversationLabel = try values.decode(String.self, forKey: .conversationLabel)
    senderDisplayName = try values.decodeIfPresent(String.self, forKey: .senderDisplayName)
    canonicalID = try values.decode(String.self, forKey: .canonicalID)
    conversationID = try values.decode(String.self, forKey: .conversationID)
    sourceDatabaseFreshness = try values.decode(String.self, forKey: .sourceDatabaseFreshness)
    senderID = try values.decodeIfPresent(String.self, forKey: .senderID)
    createdAtUnix = try values.decodeIfPresent(Int64.self, forKey: .createdAtUnix)
    conversationOrdinal = try values.decode(UInt64.self, forKey: .conversationOrdinal)
    direction = try values.decodeIfPresent(String.self, forKey: .direction)
    logicalType = try values.decodeIfPresent(UInt32.self, forKey: .logicalType)
    subType = try values.decodeIfPresent(UInt32.self, forKey: .subType)
    payloadKind = try values.decodeIfPresent(String.self, forKey: .payloadKind)
    payloadSummary = try values.decodeIfPresent(String.self, forKey: .payloadSummary)
    payloadSummaryTruncated = try values.decodeIfPresent(
      Bool.self, forKey: .payloadSummaryTruncated)
    artifactReferences =
      try values.decodeIfPresent(
        [HistoryArtifactReference].self, forKey: .artifactReferences) ?? []
    relationships =
      try values.decodeIfPresent(
        [HistoryRelationshipReference].self, forKey: .relationships) ?? []
  }
}

public struct HistoryArtifactFile: Codable, Equatable, Sendable {
  public let origin: String
  public let accountRelativePath: String?
  public let byteCount: UInt64
  public let sha256: String
  public let format: String
}

public struct HistoryArtifactDetail: Codable, Equatable, Sendable {
  public let kind: String
  public let role: String
  public let availability: String
  public let decodeState: String
  public let source: HistoryArtifactFile?
  public let decoded: HistoryArtifactFile?
  public let verificationState: String
}

public struct HistoryArtifactError: Codable, Equatable, Sendable {
  public let code: String
  public let message: String
  public let retryable: Bool
}

public struct HistoryArtifact: Codable, Equatable, Identifiable, Sendable {
  public let formatVersion: Int
  public let artifactID: String
  public let conversationIDs: [String]
  public let detail: HistoryArtifactDetail?
  public let error: HistoryArtifactError?

  public var id: String { artifactID }

  enum CodingKeys: String, CodingKey {
    case formatVersion
    case artifactID = "artifactId"
    case conversationIDs = "conversationIds"
    case detail
    case error
  }
}

public struct HistoryConversationStatistics: Equatable, Sendable {
  public let messageCount: UInt64
  public let latestMessageUnix: Int64?

  public init(messageCount: UInt64, latestMessageUnix: Int64?) {
    self.messageCount = messageCount
    self.latestMessageUnix = latestMessageUnix
  }

  public var latestMessageDate: Date? {
    latestMessageUnix.map { Date(timeIntervalSince1970: TimeInterval($0)) }
  }
}

public struct HistoryMessageCursor: Equatable, Sendable {
  public let ordinal: UInt64
  public let rowID: Int64

  public init(ordinal: UInt64, rowID: Int64) {
    self.ordinal = ordinal
    self.rowID = rowID
  }
}

public struct HistoryMessagePage: Equatable, Sendable {
  public let messages: [HistoryMessage]
  public let nextCursor: HistoryMessageCursor?

  public init(messages: [HistoryMessage], nextCursor: HistoryMessageCursor?) {
    self.messages = messages
    self.nextCursor = nextCursor
  }
}

public struct HistoryBundleSession: Sendable {
  public let manifest: HistoryBundleManifest
  public let conversations: [HistoryConversation]
  public let contacts: [HistoryContact]
  public let conversationStatistics: [String: HistoryConversationStatistics]
  public let indexURL: URL
  public let messagesURL: URL
  public let artifactsURL: URL
  public let reusedIndex: Bool
  let validatedSources: HistoryValidatedSources
}

public enum HistoryLoadPhase: String, Codable, Equatable, Sendable {
  case validatingManifest
  case verifyingConversations
  case verifyingContacts
  case indexingMessages
  case indexingArtifacts
  case finalizingIndex
  case ready
}

public struct HistoryLoadProgress: Equatable, Sendable {
  public let phase: HistoryLoadPhase
  public let fileRole: String?
  public let completedBytes: UInt64
  public let totalBytes: UInt64
  public let completedRecords: UInt64
  public let totalRecords: UInt64
  public let bundleByteCount: UInt64
  public let bundleRecordCount: UInt64
  public let phaseFraction: Double
  public let overallFraction: Double
  public let usingCachedIndex: Bool

  public init(
    phase: HistoryLoadPhase,
    fileRole: String?,
    completedBytes: UInt64,
    totalBytes: UInt64,
    completedRecords: UInt64,
    totalRecords: UInt64,
    bundleByteCount: UInt64 = 0,
    bundleRecordCount: UInt64 = 0,
    phaseFraction: Double,
    overallFraction: Double,
    usingCachedIndex: Bool
  ) {
    self.phase = phase
    self.fileRole = fileRole
    self.completedBytes = completedBytes
    self.totalBytes = totalBytes
    self.completedRecords = completedRecords
    self.totalRecords = totalRecords
    self.bundleByteCount = bundleByteCount
    self.bundleRecordCount = bundleRecordCount
    self.phaseFraction = phaseFraction
    self.overallFraction = overallFraction
    self.usingCachedIndex = usingCachedIndex
  }
}

public enum HistoryBundleError: Error, Equatable, CustomStringConvertible, Sendable {
  case invalidBundle(String)
  case unsafePermissions(String)
  case integrityFailure(String)
  case unsupportedSchema(String)
  case indexFailure(String)

  public var description: String {
    switch self {
    case .invalidBundle(let detail):
      return "Invalid AI context bundle: \(detail)"
    case .unsafePermissions(let detail):
      return "Private bundle safety check failed: \(detail)"
    case .integrityFailure(let detail):
      return "AI context bundle integrity check failed: \(detail)"
    case .unsupportedSchema(let schema):
      return "Unsupported AI context schema: \(schema)"
    case .indexFailure(let detail):
      return "Private history index failed: \(detail)"
    }
  }
}
