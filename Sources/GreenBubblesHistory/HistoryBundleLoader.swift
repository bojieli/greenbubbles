import CryptoKit
import Darwin
import Foundation
import SQLite3

public struct HistoryBundleLoader: Sendable {
  public typealias ProgressHandler = @Sendable (HistoryLoadProgress) -> Void

  private static let expectedInventory: Set<String> = [
    "manifest.json", "conversations.jsonl", "contacts.jsonl", "messages.jsonl",
    "artifacts.jsonl",
  ]
  private static let expectedRoles: [String: String] = [
    "conversations": "conversations.jsonl",
    "contacts": "contacts.jsonl",
    "messages": "messages.jsonl",
    "artifacts": "artifacts.jsonl",
  ]
  private static let indexFormatVersion = "2"

  public init() {}

  public func load(
    bundleURL: URL,
    indexDirectory: URL,
    progress: @escaping ProgressHandler = { _ in }
  ) async throws -> HistoryBundleSession {
    try Task.checkCancellation()
    let worker = Task.detached(priority: .userInitiated) {
      try loadSynchronously(
        bundleURL: bundleURL.standardizedFileURL,
        indexDirectory: indexDirectory.standardizedFileURL,
        progress: progress
      )
    }
    return try await withTaskCancellationHandler {
      try await worker.value
    } onCancel: {
      worker.cancel()
    }
  }

  public func loadSynchronously(
    bundleURL: URL,
    indexDirectory: URL,
    progress: ProgressHandler = { _ in }
  ) throws -> HistoryBundleSession {
    try Task.checkCancellation()
    let bundle = bundleURL.standardizedFileURL
    try requirePrivateDirectory(bundle, createIfMissing: false)
    let inventory = try Set(
      FileManager.default.contentsOfDirectory(atPath: bundle.path)
    )
    guard inventory == Self.expectedInventory else {
      throw HistoryBundleError.invalidBundle("the five-file inventory is incomplete or unexpected")
    }

    progress(
      loadProgress(
        phase: .validatingManifest, role: "manifest", bytes: 0, totalBytes: 1,
        records: 0, totalRecords: 1, lowerBound: 0, upperBound: 0.05))
    let manifestData = try readPrivateFile(
      bundle.appending(path: "manifest.json"), maximumBytes: 4 * 1_024 * 1_024)
    let manifest = try decodeStrict(
      HistoryBundleManifest.self, from: manifestData, role: "manifest")
    try validate(manifest: manifest)
    let workload = try bundleWorkload(manifest.files)
    progress(
      loadProgress(
        phase: .validatingManifest, role: "manifest", bytes: 1, totalBytes: 1,
        records: 1, totalRecords: 1, lowerBound: 0, upperBound: 0.05,
        workload: workload))

    let evidence = Dictionary(uniqueKeysWithValues: manifest.files.map { ($0.role, $0) })
    try requirePrivateDirectory(indexDirectory, createIfMissing: true)
    let indexURL = indexDirectory.appending(path: "\(manifest.bundleID).sqlite")
    let canReuse = try reusableIndex(at: indexURL, manifest: manifest, evidence: evidence)

    var conversations: [HistoryConversation] = []
    let conversationIDs = LockedSet<String>()
    let requiredContactIDs = LockedSet<String>()
    let conversationEvidence = try requiredEvidence("conversations", from: evidence)
    _ = try scanNDJSON(
      bundle.appending(path: conversationEvidence.relativePath),
      evidence: conversationEvidence,
      phase: .verifyingConversations,
      lowerBound: 0.05,
      upperBound: 0.12,
      usingCachedIndex: canReuse,
      workload: workload,
      progress: progress
    ) { data, _, _ in
      let conversation = try decodeStrict(
        HistoryConversation.self, from: data, role: "conversation")
      try validate(conversation: conversation, context: manifest.context)
      guard conversationIDs.insert(conversation.conversationID) else {
        throw HistoryBundleError.integrityFailure("conversation identities are not unique")
      }
      for participant in conversation.participants {
        _ = requiredContactIDs.insert(participant.participantID)
      }
      conversations.append(conversation)
    }
    guard UInt64(conversations.count) == conversationEvidence.recordCount,
      conversations.count == manifest.enabledConversationCount
    else {
      throw HistoryBundleError.integrityFailure("conversation counts disagree with the manifest")
    }

    var contacts: [HistoryContact] = []
    let contactIDs = LockedSet<String>()
    let contactEvidence = try requiredEvidence("contacts", from: evidence)
    _ = try scanNDJSON(
      bundle.appending(path: contactEvidence.relativePath),
      evidence: contactEvidence,
      phase: .verifyingContacts,
      lowerBound: 0.12,
      upperBound: 0.20,
      usingCachedIndex: canReuse,
      workload: workload,
      progress: progress
    ) { data, _, _ in
      let contact = try decodeStrict(HistoryContact.self, from: data, role: "contact")
      try validate(
        contact: contact, conversationIDs: conversationIDs.values, context: manifest.context)
      guard contactIDs.insert(contact.participantID) else {
        throw HistoryBundleError.integrityFailure("contact identities are not unique")
      }
      contacts.append(contact)
    }
    guard UInt64(contacts.count) == manifest.exportedContactCount else {
      throw HistoryBundleError.integrityFailure("contact counts disagree with the manifest")
    }
    guard contactIDs.values == requiredContactIDs.values else {
      throw HistoryBundleError.integrityFailure(
        "contacts do not exactly cover conversation participants")
    }

    let messageEvidence = try requiredEvidence("messages", from: evidence)
    let artifactEvidence = try requiredEvidence("artifacts", from: evidence)
    let validatedSources: HistoryValidatedSources
    if canReuse {
      let messages = try verifyMessagesWithoutReindexing(
        at: bundle.appending(path: messageEvidence.relativePath),
        evidence: messageEvidence,
        conversationIDs: conversationIDs.values,
        contactIDs: contactIDs.values,
        context: manifest.context,
        workload: workload,
        progress: progress
      )
      let artifacts = try verifyArtifactsWithoutReindexing(
        at: bundle.appending(path: artifactEvidence.relativePath),
        evidence: artifactEvidence,
        conversationIDs: conversationIDs.values,
        expectedErrorCount: manifest.artifactResolutionErrorCount,
        workload: workload,
        progress: progress
      )
      validatedSources = HistoryValidatedSources(messages: messages, artifacts: artifacts)
    } else {
      validatedSources = try buildIndex(
        at: indexURL,
        bundle: bundle,
        manifest: manifest,
        messageEvidence: messageEvidence,
        artifactEvidence: artifactEvidence,
        conversationIDs: conversationIDs.values,
        contactIDs: contactIDs.values,
        workload: workload,
        progress: progress
      )
    }

    guard messageEvidence.recordCount == manifest.exportedMessageCount,
      artifactEvidence.recordCount == manifest.exportedArtifactCount
    else {
      throw HistoryBundleError.integrityFailure(
        "exported record counts disagree with file evidence")
    }
    let statistics = try loadConversationStatistics(from: indexURL)
    progress(
      HistoryLoadProgress(
        phase: .ready,
        fileRole: nil,
        completedBytes: messageEvidence.byteCount + artifactEvidence.byteCount,
        totalBytes: messageEvidence.byteCount + artifactEvidence.byteCount,
        completedRecords: messageEvidence.recordCount + artifactEvidence.recordCount,
        totalRecords: messageEvidence.recordCount + artifactEvidence.recordCount,
        bundleByteCount: workload.byteCount,
        bundleRecordCount: workload.recordCount,
        phaseFraction: 1,
        overallFraction: 1,
        usingCachedIndex: canReuse
      ))
    return HistoryBundleSession(
      manifest: manifest,
      conversations: conversations.sorted {
        let left = statistics[$0.conversationID]?.latestMessageUnix ?? Int64.min
        let right = statistics[$1.conversationID]?.latestMessageUnix ?? Int64.min
        return left == right ? $0.humanLabel < $1.humanLabel : left > right
      },
      contacts: contacts.sorted {
        $0.displayName.localizedStandardCompare($1.displayName) == .orderedAscending
      },
      conversationStatistics: statistics,
      indexURL: indexURL,
      messagesURL: bundle.appending(path: messageEvidence.relativePath),
      artifactsURL: bundle.appending(path: artifactEvidence.relativePath),
      reusedIndex: canReuse,
      validatedSources: validatedSources
    )
  }

  private func validate(manifest: HistoryBundleManifest) throws {
    guard manifest.formatVersion == 1, manifest.schema == greenBubblesAIContextSchema else {
      throw HistoryBundleError.unsupportedSchema(manifest.schema)
    }
    guard manifest.exportComplete, manifest.createdAtUnixNanoseconds > 0,
      isSHA256(manifest.bundleID), isSHA256(manifest.policySHA256),
      !manifest.policySourceFingerprint.isEmpty, !manifest.requesterID.isEmpty,
      manifest.requesterID.utf8.count <= 256, !manifest.context.accountID.isEmpty,
      !manifest.context.replicaID.isEmpty, !manifest.context.sourceFingerprint.isEmpty,
      !manifest.context.checkpointRevision.isEmpty
    else {
      throw HistoryBundleError.integrityFailure(
        "manifest identity or completion evidence is invalid")
    }
    guard manifest.files.count == Self.expectedRoles.count else {
      throw HistoryBundleError.integrityFailure("manifest file inventory is invalid")
    }
    var roles = Set<String>()
    var paths = Set<String>()
    for file in manifest.files {
      guard Self.expectedRoles[file.role] == file.relativePath,
        roles.insert(file.role).inserted, paths.insert(file.relativePath).inserted,
        isSHA256(file.sha256)
      else {
        throw HistoryBundleError.integrityFailure("manifest file evidence is invalid")
      }
    }
    let expectedBundleID = try bundleIdentity(for: manifest)
    guard expectedBundleID == manifest.bundleID else {
      throw HistoryBundleError.integrityFailure(
        "bundle identity does not bind the checkpoint and policy")
    }
  }

  private func validate(
    conversation: HistoryConversation,
    context: HistoryContextHealth
  ) throws {
    let validTimeRange: Bool
    if let start = conversation.notBeforeUnix, let end = conversation.notAfterUnix {
      validTimeRange = start <= end
    } else {
      validTimeRange = true
    }
    guard conversation.formatVersion == 1, !conversation.conversationID.isEmpty,
      !conversation.humanLabel.isEmpty,
      !conversation.kind.isEmpty, !conversation.entityDecodeState.isEmpty,
      conversation.participantCount == conversation.participants.count,
      Set(conversation.participants.map(\.participantID)).count == conversation.participants.count,
      conversation.participants.allSatisfy({
        !$0.participantID.isEmpty && !$0.displayName.isEmpty && !$0.role.isEmpty
      }),
      conversation.ownerParticipantID.map({ owner in
        conversation.participants.contains { $0.participantID == owner }
      }) ?? true,
      validTimeRange
    else {
      throw HistoryBundleError.integrityFailure("conversation record is inconsistent")
    }
    try validateFreshness(conversation.sourceDatabaseFreshness, context: context)
  }

  private func validate(
    contact: HistoryContact,
    conversationIDs: Set<String>,
    context: HistoryContextHealth
  ) throws {
    guard contact.formatVersion == 1, !contact.participantID.isEmpty,
      !contact.displayName.isEmpty,
      contact.resolutionErrorCode?.isEmpty != true,
      Set(contact.enabledConversationIDs).count == contact.enabledConversationIDs.count,
      Set(contact.enabledConversationIDs).isSubset(of: conversationIDs),
      contact.conversationProfiles.allSatisfy({
        conversationIDs.contains($0.conversationID) && !$0.displayName.isEmpty && !$0.role.isEmpty
      }),
      Set(contact.conversationProfiles.map(\.conversationID)).count
        == contact.conversationProfiles.count,
      Set(contact.conversationProfiles.map(\.conversationID))
        == Set(contact.enabledConversationIDs)
    else {
      throw HistoryBundleError.integrityFailure("contact record is inconsistent")
    }
    try validateFreshness(contact.sourceDatabaseFreshness, context: context)
  }

  private func validate(
    message: HistoryMessage,
    conversationIDs: Set<String>,
    contactIDs: Set<String>,
    context: HistoryContextHealth
  ) throws {
    guard message.formatVersion == 1, !message.canonicalID.isEmpty,
      conversationIDs.contains(message.conversationID),
      !message.conversationLabel.isEmpty,
      message.senderID.map(contactIDs.contains) ?? true,
      message.artifactReferences.allSatisfy({
        !$0.artifactID.isEmpty && !$0.role.isEmpty
      }),
      Set(message.artifactReferences.map { "\($0.artifactID)\u{0}\($0.role)" }).count
        == message.artifactReferences.count,
      message.artifactReferences.isEmpty
        || message.artifactReferences.filter(\.preferred).count == 1,
      message.relationships.allSatisfy({
        !$0.kind.isEmpty && $0.resolved == ($0.targetCanonicalID != nil)
      })
    else {
      throw HistoryBundleError.integrityFailure("message record is inconsistent")
    }
    guard
      message.sourceDatabaseFreshness == "fresh"
        || message.sourceDatabaseFreshness == "preservedStale"
    else {
      throw HistoryBundleError.integrityFailure("message freshness is invalid")
    }
    try validateFreshness(message.sourceDatabaseFreshness, context: context)
  }

  private func validate(
    artifact: HistoryArtifact,
    conversationIDs: Set<String>
  ) throws {
    guard artifact.formatVersion == 1, !artifact.artifactID.isEmpty,
      !artifact.conversationIDs.isEmpty,
      Set(artifact.conversationIDs).count == artifact.conversationIDs.count,
      Set(artifact.conversationIDs).isSubset(of: conversationIDs),
      (artifact.detail == nil) != (artifact.error == nil)
    else {
      throw HistoryBundleError.integrityFailure("artifact record is inconsistent")
    }
    if let detail = artifact.detail {
      guard !detail.kind.isEmpty, !detail.role.isEmpty, !detail.availability.isEmpty,
        !detail.decodeState.isEmpty, detail.verificationState == "connectorDigestVerified"
      else {
        throw HistoryBundleError.integrityFailure("artifact verification state is invalid")
      }
      for file in [detail.source, detail.decoded].compactMap({ $0 }) {
        guard !file.origin.isEmpty, isSHA256(file.sha256), !file.format.isEmpty else {
          throw HistoryBundleError.integrityFailure("artifact file evidence is invalid")
        }
        if let relativePath = file.accountRelativePath,
          !isSafeRelativePath(relativePath)
        {
          throw HistoryBundleError.integrityFailure("artifact contains an unsafe relative path")
        }
      }
      if ["downloaded", "materializedFromDatabase"].contains(detail.availability),
        detail.source == nil, detail.decoded == nil
      {
        throw HistoryBundleError.integrityFailure(
          "available artifact has no verified file evidence")
      }
    } else if let error = artifact.error {
      guard !error.message.isEmpty, canonicalArtifactErrorMessage(error.code) == error.message
      else {
        throw HistoryBundleError.integrityFailure("artifact error is not canonical")
      }
    }
  }

  private func validateFreshness(
    _ freshness: String,
    context: HistoryContextHealth
  ) throws {
    let accepted = ["fresh", "preservedStale", "mixed", "derived"]
    guard accepted.contains(freshness) else {
      throw HistoryBundleError.integrityFailure("source database freshness is invalid")
    }
    if ["preservedStale", "mixed"].contains(freshness),
      context.preservedStaleDatabaseCount ?? 0 == 0
    {
      throw HistoryBundleError.integrityFailure(
        "record claims stale provenance without stale database coverage")
    }
  }

  private func verifyMessagesWithoutReindexing(
    at url: URL,
    evidence: HistoryManifestFile,
    conversationIDs: Set<String>,
    contactIDs: Set<String>,
    context: HistoryContextHealth,
    workload: HistoryBundleWorkload,
    progress: ProgressHandler
  ) throws -> HistorySourceFile {
    var count: UInt64 = 0
    let source = try scanNDJSON(
      url,
      evidence: evidence,
      phase: .indexingMessages,
      lowerBound: 0.20,
      upperBound: 0.90,
      usingCachedIndex: true,
      workload: workload,
      progress: progress
    ) { data, _, _ in
      let message = try decodeStrict(HistoryMessage.self, from: data, role: "message")
      try validate(
        message: message, conversationIDs: conversationIDs, contactIDs: contactIDs,
        context: context)
      count += 1
    }
    guard count == evidence.recordCount else {
      throw HistoryBundleError.integrityFailure("message count does not match its manifest")
    }
    return source
  }

  private func verifyArtifactsWithoutReindexing(
    at url: URL,
    evidence: HistoryManifestFile,
    conversationIDs: Set<String>,
    expectedErrorCount: UInt64,
    workload: HistoryBundleWorkload,
    progress: ProgressHandler
  ) throws -> HistorySourceFile {
    var count: UInt64 = 0
    var errorCount: UInt64 = 0
    let source = try scanNDJSON(
      url,
      evidence: evidence,
      phase: .indexingArtifacts,
      lowerBound: 0.90,
      upperBound: 0.98,
      usingCachedIndex: true,
      workload: workload,
      progress: progress
    ) { data, _, _ in
      let artifact = try decodeStrict(HistoryArtifact.self, from: data, role: "artifact")
      try validate(artifact: artifact, conversationIDs: conversationIDs)
      count += 1
      if artifact.error != nil { errorCount += 1 }
    }
    guard count == evidence.recordCount, errorCount == expectedErrorCount else {
      throw HistoryBundleError.integrityFailure(
        "artifact or artifact-error count does not match its manifest")
    }
    return source
  }

  private func buildIndex(
    at indexURL: URL,
    bundle: URL,
    manifest: HistoryBundleManifest,
    messageEvidence: HistoryManifestFile,
    artifactEvidence: HistoryManifestFile,
    conversationIDs: Set<String>,
    contactIDs: Set<String>,
    workload: HistoryBundleWorkload,
    progress: ProgressHandler
  ) throws -> HistoryValidatedSources {
    let temporaryURL = indexURL.deletingLastPathComponent().appending(
      path: ".\(manifest.bundleID).\(UUID().uuidString).tmp")
    guard !FileManager.default.fileExists(atPath: temporaryURL.path) else {
      throw HistoryBundleError.indexFailure("temporary index path already exists")
    }
    defer { try? FileManager.default.removeItem(at: temporaryURL) }

    let validatedSources: HistoryValidatedSources
    do {
      let database = try SQLiteConnection(
        path: temporaryURL.path,
        flags: SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_EXCLUSIVE
          | SQLITE_OPEN_FULLMUTEX
      )
      guard chmod(temporaryURL.path, 0o600) == 0 else {
        throw HistoryBundleError.indexFailure("could not make the derived index owner-only")
      }
      try configureNewIndex(database)
      try database.execute("BEGIN IMMEDIATE")
      let insertMessage = try database.prepare(
        """
        INSERT INTO messages(
          canonical_id, conversation_id, ordinal, created_at_unix, direction, freshness,
          sender_display_name, conversation_label, payload_kind, payload_summary,
          byte_offset, byte_length, artifact_count, relationship_count
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """)
      let insertSearch = try database.prepare(
        """
        INSERT INTO message_search(
          rowid, payload_summary, sender_display_name, conversation_label
        ) VALUES (?, ?, ?, ?)
        """)
      let insertArtifactReference = try database.prepare(
        "INSERT INTO message_artifacts(message_rowid, artifact_id) VALUES (?, ?)")
      let insertRelationship = try database.prepare(
        "INSERT INTO message_relationships(message_rowid, target_id, resolved) VALUES (?, ?, ?)")
      var messageCount: UInt64 = 0
      let messages = try scanNDJSON(
        bundle.appending(path: messageEvidence.relativePath),
        evidence: messageEvidence,
        phase: .indexingMessages,
        lowerBound: 0.20,
        upperBound: 0.90,
        usingCachedIndex: false,
        workload: workload,
        progress: progress
      ) { data, offset, length in
        let message = try decodeStrict(HistoryMessage.self, from: data, role: "message")
        try validate(
          message: message, conversationIDs: conversationIDs, contactIDs: contactIDs,
          context: manifest.context)
        insertMessage.reset()
        try insertMessage.bind(message.canonicalID, at: 1)
        try insertMessage.bind(message.conversationID, at: 2)
        try insertMessage.bind(message.conversationOrdinal, at: 3)
        try insertMessage.bind(message.createdAtUnix, at: 4)
        try insertMessage.bind(message.direction, at: 5)
        try insertMessage.bind(message.sourceDatabaseFreshness, at: 6)
        try insertMessage.bind(message.senderDisplayName, at: 7)
        try insertMessage.bind(message.conversationLabel, at: 8)
        try insertMessage.bind(message.payloadKind, at: 9)
        try insertMessage.bind(message.payloadSummary, at: 10)
        try insertMessage.bind(offset, at: 11)
        try insertMessage.bind(length, at: 12)
        try insertMessage.bind(message.artifactReferences.count, at: 13)
        try insertMessage.bind(message.relationships.count, at: 14)
        try insertMessage.step()
        let rowID = database.lastInsertedRowID
        insertSearch.reset()
        try insertSearch.bind(rowID, at: 1)
        try insertSearch.bind(message.payloadSummary, at: 2)
        try insertSearch.bind(message.senderDisplayName, at: 3)
        try insertSearch.bind(message.conversationLabel, at: 4)
        try insertSearch.step()
        for reference in message.artifactReferences {
          insertArtifactReference.reset()
          try insertArtifactReference.bind(rowID, at: 1)
          try insertArtifactReference.bind(reference.artifactID, at: 2)
          try insertArtifactReference.step()
        }
        for relationship in message.relationships where relationship.targetCanonicalID != nil {
          insertRelationship.reset()
          try insertRelationship.bind(rowID, at: 1)
          try insertRelationship.bind(relationship.targetCanonicalID, at: 2)
          try insertRelationship.bind(relationship.resolved ? 1 : 0, at: 3)
          try insertRelationship.step()
        }
        messageCount += 1
        if messageCount % 10_000 == 0 {
          try database.execute("COMMIT; BEGIN IMMEDIATE")
        }
      }
      try database.execute("COMMIT; BEGIN IMMEDIATE")

      let insertArtifact = try database.prepare(
        """
        INSERT INTO artifacts(
          artifact_id, kind, role, availability, decode_state, error_code,
          byte_offset, byte_length
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        """)
      let insertArtifactConversation = try database.prepare(
        "INSERT INTO artifact_conversations(artifact_id, conversation_id) VALUES (?, ?)")
      var artifactCount: UInt64 = 0
      var artifactErrorCount: UInt64 = 0
      let artifacts = try scanNDJSON(
        bundle.appending(path: artifactEvidence.relativePath),
        evidence: artifactEvidence,
        phase: .indexingArtifacts,
        lowerBound: 0.90,
        upperBound: 0.98,
        usingCachedIndex: false,
        workload: workload,
        progress: progress
      ) { data, offset, length in
        let artifact = try decodeStrict(HistoryArtifact.self, from: data, role: "artifact")
        try validate(artifact: artifact, conversationIDs: conversationIDs)
        insertArtifact.reset()
        try insertArtifact.bind(artifact.artifactID, at: 1)
        try insertArtifact.bind(artifact.detail?.kind, at: 2)
        try insertArtifact.bind(artifact.detail?.role, at: 3)
        try insertArtifact.bind(artifact.detail?.availability, at: 4)
        try insertArtifact.bind(artifact.detail?.decodeState, at: 5)
        try insertArtifact.bind(artifact.error?.code, at: 6)
        try insertArtifact.bind(offset, at: 7)
        try insertArtifact.bind(length, at: 8)
        try insertArtifact.step()
        for conversationID in artifact.conversationIDs {
          insertArtifactConversation.reset()
          try insertArtifactConversation.bind(artifact.artifactID, at: 1)
          try insertArtifactConversation.bind(conversationID, at: 2)
          try insertArtifactConversation.step()
        }
        artifactCount += 1
        if artifact.error != nil { artifactErrorCount += 1 }
        if artifactCount % 10_000 == 0 {
          try database.execute("COMMIT; BEGIN IMMEDIATE")
        }
      }
      guard artifactErrorCount == manifest.artifactResolutionErrorCount else {
        throw HistoryBundleError.integrityFailure(
          "artifact error count does not match the manifest")
      }
      try database.execute("COMMIT")
      try verifyIndexReferences(database)
      progress(
        loadProgress(
          phase: .finalizingIndex, role: "index", bytes: 0, totalBytes: 1,
          records: 0, totalRecords: 1, lowerBound: 0.98, upperBound: 1,
          workload: workload))
      try Task.checkCancellation()
      try writeMetadata(
        database, manifest: manifest, messageEvidence: messageEvidence,
        artifactEvidence: artifactEvidence)
      let integrity = try database.prepare("PRAGMA integrity_check")
      guard try integrity.step() == SQLITE_ROW, integrity.text(at: 0) == "ok" else {
        throw HistoryBundleError.indexFailure("derived index integrity check failed")
      }
      try Task.checkCancellation()
      progress(
        loadProgress(
          phase: .finalizingIndex, role: "index", bytes: 1, totalBytes: 1,
          records: 1, totalRecords: 1, lowerBound: 0.98, upperBound: 1,
          workload: workload))
      validatedSources = HistoryValidatedSources(messages: messages, artifacts: artifacts)
    }

    let descriptor = open(temporaryURL.path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW)
    guard descriptor >= 0 else {
      throw HistoryBundleError.indexFailure("could not reopen the completed index")
    }
    defer { close(descriptor) }
    guard fsync(descriptor) == 0 else {
      throw HistoryBundleError.indexFailure("could not synchronize the completed index")
    }
    if FileManager.default.fileExists(atPath: indexURL.path) {
      try requirePrivateRegularFile(indexURL)
    }
    guard rename(temporaryURL.path, indexURL.path) == 0 else {
      throw HistoryBundleError.indexFailure("could not atomically publish the derived index")
    }
    try synchronizeDirectory(indexURL.deletingLastPathComponent())
    return validatedSources
  }

  private func configureNewIndex(_ database: SQLiteConnection) throws {
    try database.execute(
      """
      PRAGMA journal_mode = MEMORY;
      PRAGMA synchronous = OFF;
      PRAGMA temp_store = FILE;
      PRAGMA foreign_keys = ON;
      PRAGMA trusted_schema = OFF;
      CREATE TABLE metadata(key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
      CREATE TABLE messages(
        canonical_id TEXT NOT NULL UNIQUE,
        conversation_id TEXT NOT NULL,
        ordinal INTEGER NOT NULL,
        created_at_unix INTEGER,
        direction TEXT,
        freshness TEXT NOT NULL,
        sender_display_name TEXT,
        conversation_label TEXT NOT NULL,
        payload_kind TEXT,
        payload_summary TEXT,
        byte_offset INTEGER NOT NULL,
        byte_length INTEGER NOT NULL,
        artifact_count INTEGER NOT NULL,
        relationship_count INTEGER NOT NULL
      ) STRICT;
      CREATE INDEX messages_conversation_order
        ON messages(conversation_id, ordinal DESC);
      CREATE INDEX messages_created_at ON messages(created_at_unix DESC);
      CREATE VIRTUAL TABLE message_search USING fts5(
        payload_summary, sender_display_name, conversation_label,
        content = 'messages', content_rowid = 'rowid',
        tokenize = 'trigram'
      );
      CREATE TABLE message_artifacts(
        message_rowid INTEGER NOT NULL,
        artifact_id TEXT NOT NULL
      ) STRICT;
      CREATE INDEX message_artifacts_id ON message_artifacts(artifact_id);
      CREATE TABLE message_relationships(
        message_rowid INTEGER NOT NULL,
        target_id TEXT NOT NULL,
        resolved INTEGER NOT NULL CHECK(resolved IN (0, 1))
      ) STRICT;
      CREATE INDEX message_relationships_target ON message_relationships(target_id);
      CREATE TABLE artifacts(
        artifact_id TEXT PRIMARY KEY,
        kind TEXT,
        role TEXT,
        availability TEXT,
        decode_state TEXT,
        error_code TEXT,
        byte_offset INTEGER NOT NULL,
        byte_length INTEGER NOT NULL
      ) STRICT;
      CREATE TABLE artifact_conversations(
        artifact_id TEXT NOT NULL,
        conversation_id TEXT NOT NULL,
        UNIQUE(artifact_id, conversation_id)
      ) STRICT;
      CREATE INDEX artifact_conversations_conversation
        ON artifact_conversations(conversation_id);
      """)
  }

  private func verifyIndexReferences(_ database: SQLiteConnection) throws {
    for (sql, detail) in [
      (
        "SELECT 1 FROM message_artifacts r LEFT JOIN artifacts a ON a.artifact_id = r.artifact_id WHERE a.artifact_id IS NULL LIMIT 1",
        "a message references an absent artifact"
      ),
      (
        "SELECT 1 FROM artifacts a LEFT JOIN message_artifacts r ON r.artifact_id = a.artifact_id WHERE r.artifact_id IS NULL LIMIT 1",
        "an artifact is not referenced by any message"
      ),
      (
        "SELECT 1 FROM message_artifacts r JOIN messages m ON m.rowid = r.message_rowid LEFT JOIN artifact_conversations c ON c.artifact_id = r.artifact_id AND c.conversation_id = m.conversation_id WHERE c.artifact_id IS NULL LIMIT 1",
        "a message references an artifact outside its conversation"
      ),
      (
        "SELECT 1 FROM artifact_conversations c WHERE NOT EXISTS (SELECT 1 FROM message_artifacts r JOIN messages m ON m.rowid = r.message_rowid WHERE r.artifact_id = c.artifact_id AND m.conversation_id = c.conversation_id) LIMIT 1",
        "an artifact claims a conversation without a message reference"
      ),
      (
        "SELECT 1 FROM message_relationships r JOIN messages source ON source.rowid = r.message_rowid LEFT JOIN messages target ON target.canonical_id = r.target_id WHERE r.resolved = 1 AND (target.canonical_id IS NULL OR target.conversation_id <> source.conversation_id) LIMIT 1",
        "a resolved relationship references an absent or different-conversation message"
      ),
    ] {
      let statement = try database.prepare(sql)
      if try statement.step() == SQLITE_ROW {
        throw HistoryBundleError.integrityFailure(detail)
      }
    }
  }

  private func writeMetadata(
    _ database: SQLiteConnection,
    manifest: HistoryBundleManifest,
    messageEvidence: HistoryManifestFile,
    artifactEvidence: HistoryManifestFile
  ) throws {
    let statement = try database.prepare("INSERT INTO metadata(key, value) VALUES (?, ?)")
    for (key, value) in [
      ("indexFormatVersion", Self.indexFormatVersion),
      ("bundleID", manifest.bundleID),
      ("policySHA256", manifest.policySHA256),
      ("checkpointRevision", manifest.context.checkpointRevision),
      ("messagesSHA256", messageEvidence.sha256),
      ("artifactsSHA256", artifactEvidence.sha256),
      ("messageCount", String(messageEvidence.recordCount)),
      ("artifactCount", String(artifactEvidence.recordCount)),
    ] {
      statement.reset()
      try statement.bind(key, at: 1)
      try statement.bind(value, at: 2)
      try statement.step()
    }
  }

  private func reusableIndex(
    at url: URL,
    manifest: HistoryBundleManifest,
    evidence: [String: HistoryManifestFile]
  ) throws -> Bool {
    guard FileManager.default.fileExists(atPath: url.path) else { return false }
    do {
      try requirePrivateRegularFile(url)
      let database = try SQLiteConnection(
        path: url.path, flags: SQLITE_OPEN_READONLY | SQLITE_OPEN_FULLMUTEX)
      let quickCheck = try database.prepare("PRAGMA quick_check")
      guard try quickCheck.step() == SQLITE_ROW, quickCheck.text(at: 0) == "ok" else {
        return false
      }
      let statement = try database.prepare("SELECT key, value FROM metadata")
      var metadata: [String: String] = [:]
      while try statement.step() == SQLITE_ROW {
        if let key = statement.text(at: 0), let value = statement.text(at: 1) {
          metadata[key] = value
        }
      }
      return metadata["indexFormatVersion"] == Self.indexFormatVersion
        && metadata["bundleID"] == manifest.bundleID
        && metadata["policySHA256"] == manifest.policySHA256
        && metadata["checkpointRevision"] == manifest.context.checkpointRevision
        && metadata["messagesSHA256"] == evidence["messages"]?.sha256
        && metadata["artifactsSHA256"] == evidence["artifacts"]?.sha256
        && metadata["messageCount"] == evidence["messages"].map { String($0.recordCount) }
        && metadata["artifactCount"] == evidence["artifacts"].map { String($0.recordCount) }
    } catch {
      return false
    }
  }

  private func loadConversationStatistics(
    from indexURL: URL
  ) throws -> [String: HistoryConversationStatistics] {
    let database = try SQLiteConnection(
      path: indexURL.path, flags: SQLITE_OPEN_READONLY | SQLITE_OPEN_FULLMUTEX)
    let statement = try database.prepare(
      "SELECT conversation_id, COUNT(*), MAX(created_at_unix) FROM messages GROUP BY conversation_id"
    )
    var result: [String: HistoryConversationStatistics] = [:]
    while try statement.step() == SQLITE_ROW {
      guard let conversationID = statement.text(at: 0) else { continue }
      result[conversationID] = HistoryConversationStatistics(
        messageCount: UInt64(statement.int64(at: 1)),
        latestMessageUnix: statement.text(at: 2).flatMap(Int64.init)
      )
    }
    return result
  }

  private func scanNDJSON(
    _ url: URL,
    evidence: HistoryManifestFile,
    phase: HistoryLoadPhase,
    lowerBound: Double,
    upperBound: Double,
    usingCachedIndex: Bool,
    workload: HistoryBundleWorkload,
    progress: ProgressHandler,
    visitor: (Data, UInt64, UInt64) throws -> Void
  ) throws -> HistorySourceFile {
    try Task.checkCancellation()
    let descriptor = try openPrivateRegularFile(url)
    defer { close(descriptor) }
    var metadata = stat()
    guard fstat(descriptor, &metadata) == 0, metadata.st_size >= 0,
      UInt64(metadata.st_size) == evidence.byteCount
    else {
      throw HistoryBundleError.integrityFailure("\(evidence.role) byte count is inconsistent")
    }

    let chunkSize = 1_024 * 1_024
    let maximumLineBytes = 16 * 1_024 * 1_024
    var readBuffer = [UInt8](repeating: 0, count: chunkSize)
    var pending = Data()
    var pendingOffset: UInt64 = 0
    var cursor = 0
    var byteCount: UInt64 = 0
    var recordCount: UInt64 = 0
    var lastReportedBytes: UInt64 = 0
    var hasher = SHA256()

    progress(
      loadProgress(
        phase: phase, role: evidence.role, bytes: 0, totalBytes: evidence.byteCount,
        records: 0, totalRecords: evidence.recordCount, lowerBound: lowerBound,
        upperBound: upperBound, usingCachedIndex: usingCachedIndex, workload: workload))

    while true {
      try Task.checkCancellation()
      let count = Darwin.read(descriptor, &readBuffer, readBuffer.count)
      guard count >= 0 else {
        throw HistoryBundleError.integrityFailure("could not read \(evidence.role)")
      }
      if count == 0 { break }
      let chunk = Data(readBuffer[0..<count])
      hasher.update(data: chunk)
      pending.append(chunk)
      byteCount += UInt64(count)

      while cursor < pending.count,
        let newline = pending[cursor...].firstIndex(of: 0x0A)
      {
        var contentEnd = newline
        if contentEnd > cursor, pending[contentEnd - 1] == 0x0D { contentEnd -= 1 }
        guard contentEnd > cursor else {
          throw HistoryBundleError.integrityFailure("\(evidence.role) contains an empty record")
        }
        let length = contentEnd - cursor
        guard length <= maximumLineBytes else {
          throw HistoryBundleError.integrityFailure(
            "\(evidence.role) record exceeds its size limit")
        }
        let data = pending.subdata(in: cursor..<contentEnd)
        try Task.checkCancellation()
        try visitor(data, pendingOffset + UInt64(cursor), UInt64(length))
        recordCount += 1
        cursor = newline + 1
      }

      if cursor >= chunkSize * 4 {
        pending.removeSubrange(0..<cursor)
        pendingOffset += UInt64(cursor)
        cursor = 0
      }
      if pending.count - cursor > maximumLineBytes {
        throw HistoryBundleError.integrityFailure("\(evidence.role) record exceeds its size limit")
      }
      if byteCount - lastReportedBytes >= 8 * 1_024 * 1_024 {
        progress(
          loadProgress(
            phase: phase, role: evidence.role, bytes: byteCount,
            totalBytes: evidence.byteCount, records: recordCount,
            totalRecords: evidence.recordCount, lowerBound: lowerBound,
            upperBound: upperBound, usingCachedIndex: usingCachedIndex, workload: workload))
        lastReportedBytes = byteCount
      }
    }

    if cursor < pending.count {
      throw HistoryBundleError.integrityFailure(
        "\(evidence.role) has an unterminated final record")
    }

    let digest = hasher.finalize().map { String(format: "%02x", $0) }.joined()
    guard byteCount == evidence.byteCount, recordCount == evidence.recordCount,
      digest == evidence.sha256
    else {
      throw HistoryBundleError.integrityFailure(
        "\(evidence.role) digest, byte count, or record count is inconsistent")
    }
    let source = try HistorySourceFile.retainingValidatedDescriptor(
      descriptor, unchangedFrom: metadata)
    progress(
      loadProgress(
        phase: phase, role: evidence.role, bytes: byteCount, totalBytes: evidence.byteCount,
        records: recordCount, totalRecords: evidence.recordCount, lowerBound: lowerBound,
        upperBound: upperBound, usingCachedIndex: usingCachedIndex, workload: workload))
    return source
  }

  private func requiredEvidence(
    _ role: String,
    from evidence: [String: HistoryManifestFile]
  ) throws -> HistoryManifestFile {
    guard let file = evidence[role] else {
      throw HistoryBundleError.invalidBundle("manifest does not name \(role)")
    }
    return file
  }

  private func bundleIdentity(for manifest: HistoryBundleManifest) throws -> String {
    let identity: [String: Any] = [
      "formatVersion": 1,
      "schema": greenBubblesAIContextSchema,
      "accountId": manifest.context.accountID,
      "replicaId": manifest.context.replicaID,
      "sourceFingerprint": manifest.context.sourceFingerprint,
      "checkpointRevision": manifest.context.checkpointRevision,
      "policySHA256": manifest.policySHA256,
      "policySourceFingerprint": manifest.policySourceFingerprint,
      "destination": manifest.destination,
    ]
    let data = try JSONSerialization.data(
      withJSONObject: identity, options: [.sortedKeys, .withoutEscapingSlashes])
    return SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
  }
}

private final class LockedSet<Element: Hashable>: @unchecked Sendable {
  private let lock = NSLock()
  private var storage = Set<Element>()

  var values: Set<Element> {
    lock.withLock { storage }
  }

  func insert(_ value: Element) -> Bool {
    lock.withLock { storage.insert(value).inserted }
  }
}

private protocol StrictHistoryRoot: Decodable {
  static var allowedJSONFields: Set<String> { get }
  static var requiredJSONFields: Set<String> { get }
}

private struct StrictHistoryEnvelope<Value: StrictHistoryRoot>: Decodable {
  let value: Value

  init(from decoder: Decoder) throws {
    let container = try decoder.container(keyedBy: ArbitraryHistoryCodingKey.self)
    let observed = Set(container.allKeys.map(\.stringValue))
    guard observed.isSubset(of: Value.allowedJSONFields),
      Value.requiredJSONFields.isSubset(of: observed)
    else {
      throw DecodingError.dataCorrupted(
        .init(codingPath: decoder.codingPath, debugDescription: "record field set is invalid"))
    }
    value = try Value(from: decoder)
  }
}

private struct ArbitraryHistoryCodingKey: CodingKey {
  let stringValue: String
  let intValue: Int?

  init?(stringValue: String) {
    self.stringValue = stringValue
    intValue = nil
  }

  init?(intValue: Int) {
    stringValue = String(intValue)
    self.intValue = intValue
  }
}

private func decodeStrict<Value: StrictHistoryRoot>(
  _ type: Value.Type,
  from data: Data,
  role: String
) throws -> Value {
  do {
    return try JSONDecoder().decode(StrictHistoryEnvelope<Value>.self, from: data).value
  } catch {
    throw HistoryBundleError.integrityFailure("\(role) schema is invalid")
  }
}

extension HistoryBundleManifest: StrictHistoryRoot {
  fileprivate static let allowedJSONFields: Set<String> = [
    "formatVersion", "schema", "bundleId", "createdAtUnixNanoseconds", "destination",
    "requesterId", "policySHA256", "policySourceFingerprint", "context",
    "enabledConversationCount", "exportedContactCount", "exportedMessageCount",
    "exportedArtifactCount", "artifactResolutionErrorCount", "exportComplete", "files",
  ]
  fileprivate static let requiredJSONFields = allowedJSONFields
}

extension HistoryConversation: StrictHistoryRoot {
  fileprivate static let allowedJSONFields: Set<String> = [
    "formatVersion", "conversationId", "humanLabel", "kind", "participantCount",
    "participants", "ownerParticipantId", "entityDecodeState", "sourceDatabaseFreshness",
    "capabilities", "messageFields", "notBeforeUnix", "notAfterUnix",
  ]
  fileprivate static let requiredJSONFields = allowedJSONFields
}

extension HistoryContact: StrictHistoryRoot {
  fileprivate static let allowedJSONFields: Set<String> = [
    "formatVersion", "participantId", "displayName", "localProfileAvailable",
    "sourceDatabaseFreshness", "enabledConversationIds", "conversationProfiles",
    "resolutionErrorCode",
  ]
  fileprivate static let requiredJSONFields = allowedJSONFields.subtracting(["resolutionErrorCode"])
}

extension HistoryMessage: StrictHistoryRoot {
  fileprivate static let allowedJSONFields: Set<String> = [
    "formatVersion", "conversationLabel", "senderDisplayName", "canonicalId",
    "conversationId", "sourceDatabaseFreshness", "senderId", "createdAtUnix",
    "conversationOrdinal", "direction", "logicalType", "subType", "payloadKind",
    "payloadSummary", "payloadSummaryTruncated", "artifactReferences", "relationships",
  ]
  fileprivate static let requiredJSONFields: Set<String> = [
    "formatVersion", "conversationLabel", "canonicalId", "conversationId",
    "sourceDatabaseFreshness", "conversationOrdinal",
  ]
}

extension HistoryArtifact: StrictHistoryRoot {
  fileprivate static let allowedJSONFields: Set<String> = [
    "formatVersion", "artifactId", "conversationIds", "detail", "error",
  ]
  fileprivate static let requiredJSONFields: Set<String> = [
    "formatVersion", "artifactId", "conversationIds",
  ]
}

private struct HistoryBundleWorkload {
  static let unknown = HistoryBundleWorkload(byteCount: 0, recordCount: 0)

  let byteCount: UInt64
  let recordCount: UInt64
}

private func bundleWorkload(_ files: [HistoryManifestFile]) throws -> HistoryBundleWorkload {
  var byteCount: UInt64 = 0
  var recordCount: UInt64 = 0
  for file in files {
    let nextBytes = byteCount.addingReportingOverflow(file.byteCount)
    let nextRecords = recordCount.addingReportingOverflow(file.recordCount)
    guard !nextBytes.overflow, !nextRecords.overflow else {
      throw HistoryBundleError.integrityFailure("bundle workload exceeds supported limits")
    }
    byteCount = nextBytes.partialValue
    recordCount = nextRecords.partialValue
  }
  return HistoryBundleWorkload(byteCount: byteCount, recordCount: recordCount)
}

private func loadProgress(
  phase: HistoryLoadPhase,
  role: String?,
  bytes: UInt64,
  totalBytes: UInt64,
  records: UInt64,
  totalRecords: UInt64,
  lowerBound: Double,
  upperBound: Double,
  usingCachedIndex: Bool = false,
  workload: HistoryBundleWorkload = .unknown
) -> HistoryLoadProgress {
  let byteFraction = totalBytes == 0 ? 1 : min(1, Double(bytes) / Double(totalBytes))
  let recordFraction = totalRecords == 0 ? 1 : min(1, Double(records) / Double(totalRecords))
  let phaseFraction = min(byteFraction, recordFraction)
  return HistoryLoadProgress(
    phase: phase,
    fileRole: role,
    completedBytes: bytes,
    totalBytes: totalBytes,
    completedRecords: records,
    totalRecords: totalRecords,
    bundleByteCount: workload.byteCount,
    bundleRecordCount: workload.recordCount,
    phaseFraction: phaseFraction,
    overallFraction: lowerBound + (upperBound - lowerBound) * phaseFraction,
    usingCachedIndex: usingCachedIndex
  )
}

private func isSHA256(_ value: String) -> Bool {
  value.utf8.count == 64
    && value.utf8.allSatisfy {
      (48...57).contains($0) || (97...102).contains($0)
    }
}

private func canonicalArtifactErrorMessage(_ code: String) -> String? {
  switch code {
  case "invalidRequest":
    "The request is invalid; inspect the documented operation schema."
  case "unauthorized":
    "The current owner-created policy does not authorize this read."
  case "notFound":
    "The authorized replica has no matching record."
  case "unavailable":
    "The requested read surface is currently unavailable."
  case "conflict":
    "The request conflicts with the current replica or policy checkpoint."
  case "integrityFailure":
    "The read failed an integrity check; inspect local operator diagnostics."
  default:
    nil
  }
}

private func isSafeRelativePath(_ path: String) -> Bool {
  guard !path.isEmpty, !path.hasPrefix("/") else { return false }
  return path.split(separator: "/", omittingEmptySubsequences: false).allSatisfy {
    !$0.isEmpty && $0 != "." && $0 != ".."
  }
}

private func requirePrivateDirectory(_ url: URL, createIfMissing: Bool) throws {
  if createIfMissing, !FileManager.default.fileExists(atPath: url.path) {
    try FileManager.default.createDirectory(
      at: url,
      withIntermediateDirectories: true,
      attributes: [.posixPermissions: 0o700]
    )
  }
  var metadata = stat()
  guard lstat(url.path, &metadata) == 0,
    metadata.st_mode & S_IFMT == S_IFDIR,
    metadata.st_uid == getuid(),
    metadata.st_mode & 0o077 == 0
  else {
    throw HistoryBundleError.unsafePermissions("directory must be current-user-owned mode 0700")
  }
}

private func requirePrivateRegularFile(_ url: URL) throws {
  let descriptor = try openPrivateRegularFile(url)
  close(descriptor)
}

private func openPrivateRegularFile(_ url: URL) throws -> Int32 {
  let descriptor = open(url.path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW)
  guard descriptor >= 0 else {
    throw HistoryBundleError.unsafePermissions("private file could not be opened safely")
  }
  var metadata = stat()
  guard fstat(descriptor, &metadata) == 0,
    metadata.st_mode & S_IFMT == S_IFREG,
    metadata.st_uid == getuid(),
    metadata.st_nlink == 1,
    metadata.st_mode & 0o077 == 0
  else {
    close(descriptor)
    throw HistoryBundleError.unsafePermissions(
      "bundle files must be current-user-owned, single-link, owner-only regular files")
  }
  return descriptor
}

private func readPrivateFile(_ url: URL, maximumBytes: Int) throws -> Data {
  let descriptor = try openPrivateRegularFile(url)
  defer { close(descriptor) }
  var metadata = stat()
  guard fstat(descriptor, &metadata) == 0, metadata.st_size >= 0,
    metadata.st_size <= maximumBytes
  else {
    throw HistoryBundleError.invalidBundle("manifest exceeds its size limit")
  }
  var result = Data()
  result.reserveCapacity(Int(metadata.st_size))
  var buffer = [UInt8](repeating: 0, count: 64 * 1_024)
  while true {
    let count = Darwin.read(descriptor, &buffer, buffer.count)
    guard count >= 0 else {
      throw HistoryBundleError.invalidBundle("manifest could not be read")
    }
    if count == 0 { break }
    result.append(contentsOf: buffer[0..<count])
    guard result.count <= maximumBytes else {
      throw HistoryBundleError.invalidBundle("manifest exceeds its size limit")
    }
  }
  return result
}

private func synchronizeDirectory(_ url: URL) throws {
  let descriptor = open(url.path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW | O_DIRECTORY)
  guard descriptor >= 0 else {
    throw HistoryBundleError.indexFailure("could not open the index directory")
  }
  defer { close(descriptor) }
  guard fsync(descriptor) == 0 else {
    throw HistoryBundleError.indexFailure("could not synchronize the index directory")
  }
}
