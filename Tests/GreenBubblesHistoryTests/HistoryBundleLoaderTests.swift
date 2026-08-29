import CryptoKit
import Foundation
import Testing

@testable import GreenBubblesHistory

@Suite("HistoryBundleLoaderTests")
struct HistoryBundleLoaderTests {
  @Test("validates, indexes, searches, pages, and reuses a private bundle")
  func loadsAndQueriesBundle() async throws {
    let fixture = try HistoryBundleFixture()
    defer { fixture.remove() }
    let progress = ProgressRecorder()
    let loader = HistoryBundleLoader()

    let session = try await loader.load(
      bundleURL: fixture.bundleURL,
      indexDirectory: fixture.indexURL,
      progress: { progress.append($0) }
    )

    #expect(session.manifest.exportedMessageCount == 3)
    #expect(session.conversations.count == 2)
    #expect(session.contacts.count == 3)
    #expect(session.conversationStatistics["conversation-a"]?.messageCount == 2)
    #expect(session.conversationStatistics["conversation-b"]?.messageCount == 1)
    #expect(session.reusedIndex == false)
    #expect(progress.values.last?.phase == .ready)
    #expect(progress.values.last?.overallFraction == 1)
    #expect(
      zip(progress.values, progress.values.dropFirst()).allSatisfy {
        $0.overallFraction <= $1.overallFraction
      })
    #expect(progress.values.contains { $0.phase == .indexingMessages && $0.totalRecords == 3 })

    let store = try HistoryStore(session: session)
    let firstPage = try await store.messages(conversationID: "conversation-a", limit: 1)
    #expect(firstPage.messages.map(\.canonicalID) == ["message-2"])
    #expect(firstPage.nextCursor != nil)
    let secondPage = try await store.messages(
      conversationID: "conversation-a", before: firstPage.nextCursor, limit: 1)
    #expect(secondPage.messages.map(\.canonicalID) == ["message-1"])
    #expect(secondPage.nextCursor == nil)

    let search = try await store.searchMessages(query: "budget meeting")
    #expect(search.map(\.canonicalID) == ["message-1"])
    let shortSearch = try await store.searchMessages(query: "预算")
    #expect(shortSearch.map(\.canonicalID) == ["message-3"])
    let artifact = try await store.artifact(artifactID: "artifact-1")
    #expect(artifact?.detail?.kind == "image")
    #expect(artifact?.detail?.availability == "downloaded")

    let cachedProgress = ProgressRecorder()
    let cached = try await loader.load(
      bundleURL: fixture.bundleURL,
      indexDirectory: fixture.indexURL,
      progress: { cachedProgress.append($0) }
    )
    #expect(cached.reusedIndex)
    #expect(cachedProgress.values.contains { $0.usingCachedIndex })
  }

  @Test("rejects digest tampering and unsafe permissions")
  func rejectsTamperingAndDisclosure() async throws {
    let tampered = try HistoryBundleFixture()
    defer { tampered.remove() }
    let messageURL = tampered.bundleURL.appending(path: "messages.jsonl")
    let handle = try FileHandle(forWritingTo: messageURL)
    try handle.seekToEnd()
    try handle.write(contentsOf: Data(" ".utf8))
    try handle.close()

    await #expect(throws: HistoryBundleError.self) {
      try await HistoryBundleLoader().load(
        bundleURL: tampered.bundleURL,
        indexDirectory: tampered.indexURL
      )
    }

    let disclosed = try HistoryBundleFixture()
    defer { disclosed.remove() }
    let contactsURL = disclosed.bundleURL.appending(path: "contacts.jsonl")
    try FileManager.default.setAttributes(
      [.posixPermissions: 0o644], ofItemAtPath: contactsURL.path)
    await #expect(throws: HistoryBundleError.self) {
      try await HistoryBundleLoader().load(
        bundleURL: disclosed.bundleURL,
        indexDirectory: disclosed.indexURL
      )
    }
  }

  @Test("rejects unknown record fields even when hashes and counts match")
  func rejectsSchemaExpansion() async throws {
    let fixture = try HistoryBundleFixture(extraMessageField: true)
    defer { fixture.remove() }
    await #expect(throws: HistoryBundleError.self) {
      try await HistoryBundleLoader().load(
        bundleURL: fixture.bundleURL,
        indexDirectory: fixture.indexURL
      )
    }
  }

  @Test("rejects references that cross their conversation boundary")
  func rejectsCrossConversationReferences() async throws {
    let relationship = try HistoryBundleFixture(crossConversationRelationship: true)
    defer { relationship.remove() }
    await #expect(throws: HistoryBundleError.self) {
      try await HistoryBundleLoader().load(
        bundleURL: relationship.bundleURL,
        indexDirectory: relationship.indexURL
      )
    }

    let artifact = try HistoryBundleFixture(mismatchedArtifactConversation: true)
    defer { artifact.remove() }
    await #expect(throws: HistoryBundleError.self) {
      try await HistoryBundleLoader().load(
        bundleURL: artifact.bundleURL,
        indexDirectory: artifact.indexURL
      )
    }
  }

  @Test("accepts legacy v1 bundles and rejects v2 sender-direction disagreement")
  func versionsAndAccountDirection() async throws {
    let legacy = try HistoryBundleFixture(legacyFormat: true)
    defer { legacy.remove() }
    let legacySession = try await HistoryBundleLoader().load(
      bundleURL: legacy.bundleURL,
      indexDirectory: legacy.indexURL
    )
    #expect(legacySession.manifest.formatVersion == 1)
    #expect(legacySession.manifest.context.selfParticipantID == nil)

    let conflicted = try HistoryBundleFixture(directionConflict: true)
    defer { conflicted.remove() }
    await #expect(throws: HistoryBundleError.self) {
      try await HistoryBundleLoader().load(
        bundleURL: conflicted.bundleURL,
        indexDirectory: conflicted.indexURL
      )
    }

    let mislabeledProfile = try HistoryBundleFixture(selfProfileDisplayName: "Me")
    defer { mislabeledProfile.remove() }
    await #expect(throws: HistoryBundleError.self) {
      try await HistoryBundleLoader().load(
        bundleURL: mislabeledProfile.bundleURL,
        indexDirectory: mislabeledProfile.indexURL
      )
    }
  }

  @Test("streams and pages a multi-transaction synthetic history")
  func indexesLargeHistory() async throws {
    let extraMessageCount = 20_500
    let fixture = try HistoryBundleFixture(extraMessageCount: extraMessageCount)
    defer { fixture.remove() }
    let session = try await HistoryBundleLoader().load(
      bundleURL: fixture.bundleURL,
      indexDirectory: fixture.indexURL
    )
    #expect(session.manifest.exportedMessageCount == UInt64(extraMessageCount + 3))
    #expect(
      session.conversationStatistics["conversation-a"]?.messageCount
        == UInt64(extraMessageCount + 2))
    let store = try HistoryStore(session: session)
    let page = try await store.messages(conversationID: "conversation-a", limit: 100)
    #expect(page.messages.count == 100)
    #expect(page.nextCursor != nil)
    let search = try await store.searchMessages(query: "scale marker 20499")
    #expect(search.count == 1)
    #expect(search.first?.canonicalID == "scale-message-20499")
  }

  @Test("fails closed when an indexed source changes after opening")
  func detectsPostOpenMutation() async throws {
    let fixture = try HistoryBundleFixture()
    defer { fixture.remove() }
    let session = try await HistoryBundleLoader().load(
      bundleURL: fixture.bundleURL,
      indexDirectory: fixture.indexURL
    )
    let store = try HistoryStore(session: session)
    let messageURL = fixture.bundleURL.appending(path: "messages.jsonl")
    let handle = try FileHandle(forWritingTo: messageURL)
    try handle.seekToEnd()
    try handle.write(contentsOf: Data(" ".utf8))
    try handle.close()

    await #expect(throws: HistoryBundleError.self) {
      _ = try await store.messages(conversationID: "conversation-a")
    }
  }

  @Test("rejects path replacement between validation and store creation")
  func rejectsPostValidationPathReplacement() async throws {
    let fixture = try HistoryBundleFixture()
    defer { fixture.remove() }
    let session = try await HistoryBundleLoader().load(
      bundleURL: fixture.bundleURL,
      indexDirectory: fixture.indexURL
    )
    let messageURL = fixture.bundleURL.appending(path: "messages.jsonl")
    let displacedURL = fixture.rootURL.appending(path: "validated-messages.jsonl")
    try FileManager.default.moveItem(at: messageURL, to: displacedURL)
    try privateFile(try Data(contentsOf: displacedURL), at: messageURL)

    let store = try HistoryStore(session: session)
    await #expect(throws: HistoryBundleError.self) {
      _ = try await store.messages(conversationID: "conversation-a")
    }
  }

  @Test("cancels an in-flight index without publishing it")
  func cancelsIndexing() async throws {
    let fixture = try HistoryBundleFixture(extraMessageCount: 20_500)
    defer { fixture.remove() }
    let gate = ProgressGate()
    let load = Task {
      try await HistoryBundleLoader().load(
        bundleURL: fixture.bundleURL,
        indexDirectory: fixture.indexURL,
        progress: { update in
          if update.phase == .indexingMessages { gate.signal() }
        }
      )
    }
    #expect(await gate.wait() == .success)
    load.cancel()
    await #expect(throws: CancellationError.self) {
      _ = try await load.value
    }
    #expect(try FileManager.default.contentsOfDirectory(atPath: fixture.indexURL.path).isEmpty)
  }
}

private final class ProgressRecorder: @unchecked Sendable {
  private let lock = NSLock()
  private var storage: [HistoryLoadProgress] = []

  var values: [HistoryLoadProgress] { lock.withLock { storage } }

  func append(_ value: HistoryLoadProgress) {
    lock.withLock { storage.append(value) }
  }
}

private final class ProgressGate: @unchecked Sendable {
  private let semaphore = DispatchSemaphore(value: 0)

  func signal() {
    semaphore.signal()
  }

  /// Waits for the first progress signal.
  ///
  /// The blocking wait runs on a global queue rather than on the caller's
  /// thread. The task being waited on runs on Swift's cooperative pool, which
  /// has only as many threads as the machine has cores, so blocking a
  /// cooperative thread here — while the rest of the suite runs in parallel —
  /// can starve the very task that is meant to signal us, and the deadline
  /// then expires with nothing actually wrong. The budget is generous for the
  /// same reason: this waits for a scheduling event, not for a performance
  /// bound the test means to assert.
  func wait(timeout: TimeInterval = 120) async -> DispatchTimeoutResult {
    await withCheckedContinuation { continuation in
      DispatchQueue.global().async { [semaphore] in
        continuation.resume(returning: semaphore.wait(timeout: .now() + timeout))
      }
    }
  }
}

private struct HistoryBundleFixture {
  let rootURL: URL
  let bundleURL: URL
  let indexURL: URL

  init(
    extraMessageField: Bool = false,
    extraMessageCount: Int = 0,
    crossConversationRelationship: Bool = false,
    mismatchedArtifactConversation: Bool = false,
    legacyFormat: Bool = false,
    directionConflict: Bool = false,
    selfProfileDisplayName: String? = nil
  ) throws {
    rootURL = FileManager.default.temporaryDirectory.appending(
      path: "greenbubbles-history-tests-\(UUID().uuidString)")
    bundleURL = rootURL.appending(path: "bundle")
    indexURL = rootURL.appending(path: "index")
    try privateDirectory(rootURL)
    try privateDirectory(bundleURL)
    try privateDirectory(indexURL)

    let formatVersion = legacyFormat ? 1 : 2
    let schema = legacyFormat ? greenBubblesLegacyAIContextSchema : greenBubblesAIContextSchema
    let selfParticipantID = String(repeating: "e", count: 64)
    let selfDisplayName = legacyFormat ? "Me" : "You"
    let groupOwnerField = legacyFormat ? "ownerParticipantId" : "groupOwnerParticipantId"

    let conversations: [[String: Any]] = [
      [
        "formatVersion": formatVersion,
        "conversationId": "conversation-a",
        "humanLabel": "Project Group",
        "kind": "group",
        "participantCount": 2,
        "participants": [
          [
            "participantId": selfParticipantID, "displayName": selfDisplayName,
            "role": "member",
          ],
          ["participantId": "alice", "displayName": "Alice", "role": "member"],
        ],
        groupOwnerField: legacyFormat ? selfParticipantID : "alice",
        "entityDecodeState": "complete",
        "sourceDatabaseFreshness": "fresh",
        "capabilities": ["listConversations", "readRecentMessages", "searchMessages"],
        "messageFields": ["sender", "createdAt", "direction", "type", "content", "attachments"],
        "notBeforeUnix": NSNull(),
        "notAfterUnix": NSNull(),
      ],
      [
        "formatVersion": formatVersion,
        "conversationId": "conversation-b",
        "humanLabel": "预算讨论",
        "kind": "direct",
        "participantCount": 1,
        "participants": [
          ["participantId": "bob", "displayName": "小博", "role": "recipient"]
        ],
        groupOwnerField: NSNull(),
        "entityDecodeState": "complete",
        "sourceDatabaseFreshness": "preservedStale",
        "capabilities": ["readRecentMessages", "searchMessages"],
        "messageFields": ["sender", "createdAt", "content"],
        "notBeforeUnix": NSNull(),
        "notAfterUnix": NSNull(),
      ],
    ]
    let contacts: [[String: Any]] = [
      [
        "formatVersion": formatVersion,
        "participantId": selfParticipantID,
        "displayName": selfDisplayName,
        "localProfileAvailable": true,
        "sourceDatabaseFreshness": "fresh",
        "enabledConversationIds": ["conversation-a"],
        "conversationProfiles": [
          [
            "conversationId": "conversation-a", "conversationLabel": "Project Group",
            "displayName": selfProfileDisplayName ?? selfDisplayName, "role": "member",
          ]
        ],
      ],
      [
        "formatVersion": formatVersion,
        "participantId": "alice",
        "displayName": "Alice",
        "localProfileAvailable": true,
        "sourceDatabaseFreshness": "fresh",
        "enabledConversationIds": ["conversation-a"],
        "conversationProfiles": [
          [
            "conversationId": "conversation-a", "conversationLabel": "Project Group",
            "displayName": "Alice", "role": "member",
          ]
        ],
      ],
      [
        "formatVersion": formatVersion,
        "participantId": "bob",
        "displayName": "小博",
        "localProfileAvailable": false,
        "sourceDatabaseFreshness": "preservedStale",
        "enabledConversationIds": ["conversation-b"],
        "conversationProfiles": [
          [
            "conversationId": "conversation-b", "conversationLabel": "预算讨论",
            "displayName": "小博", "role": "recipient",
          ]
        ],
      ],
    ]
    var messages: [[String: Any]] = [
      [
        "formatVersion": formatVersion,
        "conversationLabel": "Project Group",
        "senderDisplayName": "Alice",
        "canonicalId": "message-1",
        "conversationId": "conversation-a",
        "sourceDatabaseFreshness": "fresh",
        "senderId": "alice",
        "createdAtUnix": 1_700_000_001,
        "conversationOrdinal": 1,
        "direction": "incoming",
        "logicalType": 1,
        "payloadKind": "text",
        "payloadSummary": "Budget meeting notes",
      ],
      [
        "formatVersion": formatVersion,
        "conversationLabel": "Project Group",
        "senderDisplayName": selfDisplayName,
        "canonicalId": "message-2",
        "conversationId": "conversation-a",
        "sourceDatabaseFreshness": "fresh",
        "senderId": selfParticipantID,
        "createdAtUnix": 1_700_000_002,
        "conversationOrdinal": 2,
        "direction": directionConflict ? "incoming" : "outgoing",
        "logicalType": 3,
        "payloadKind": "image",
        "payloadSummary": "Design mockup",
        "artifactReferences": [
          ["artifactId": "artifact-1", "role": "original", "preferred": true]
        ],
        "relationships": [
          ["kind": "reply", "targetCanonicalId": "message-1", "resolved": true]
        ],
      ],
      [
        "formatVersion": formatVersion,
        "conversationLabel": "预算讨论",
        "senderDisplayName": "小博",
        "canonicalId": "message-3",
        "conversationId": "conversation-b",
        "sourceDatabaseFreshness": "preservedStale",
        "senderId": "bob",
        "createdAtUnix": 1_700_000_003,
        "conversationOrdinal": 1,
        "direction": "incoming",
        "logicalType": 1,
        "payloadKind": "text",
        "payloadSummary": "预算已经确认",
      ],
    ]
    if extraMessageField {
      messages[0]["unexpectedField"] = "must fail closed"
    }
    if crossConversationRelationship {
      messages[1]["relationships"] = [
        ["kind": "reply", "targetCanonicalId": "message-3", "resolved": true]
      ]
    }
    if extraMessageCount > 0 {
      for index in 0..<extraMessageCount {
        messages.append([
          "formatVersion": formatVersion,
          "conversationLabel": "Project Group",
          "senderDisplayName": "Alice",
          "canonicalId": "scale-message-\(index)",
          "conversationId": "conversation-a",
          "sourceDatabaseFreshness": "fresh",
          "senderId": "alice",
          "createdAtUnix": 1_700_100_000 + index,
          "conversationOrdinal": 3 + index,
          "direction": "incoming",
          "logicalType": 1,
          "payloadKind": "text",
          "payloadSummary": "Synthetic scale marker \(index)",
        ])
      }
    }
    let artifacts: [[String: Any]] = [
      [
        "formatVersion": formatVersion,
        "artifactId": "artifact-1",
        "conversationIds": [
          mismatchedArtifactConversation ? "conversation-b" : "conversation-a"
        ],
        "detail": [
          "kind": "image",
          "role": "original",
          "availability": "downloaded",
          "decodeState": "notRequired",
          "source": [
            "origin": "downloadedSource",
            "accountRelativePath": "attachments/mockup.png",
            "byteCount": 1_024,
            "sha256": String(repeating: "c", count: 64),
            "format": "png",
          ],
          "verificationState": "connectorDigestVerified",
        ],
      ]
    ]

    let fileEvidence = try [
      writeJSONL(conversations, role: "conversations", name: "conversations.jsonl"),
      writeJSONL(contacts, role: "contacts", name: "contacts.jsonl"),
      writeJSONL(messages, role: "messages", name: "messages.jsonl"),
      writeJSONL(artifacts, role: "artifacts", name: "artifacts.jsonl"),
    ]
    let policySHA = String(repeating: "a", count: 64)
    var context: [String: Any] = [
      "accountId": "account-1",
      "replicaId": "replica-1",
      "sourceFingerprint": "source-fingerprint-1",
      "checkpointRevision": "checkpoint-1",
      "health": "degraded",
      "clientBuildCompatibility": "supportedCompatible",
      "archiveScope": "partialDatabaseCoverage",
      "authoritativeDatabaseCoverage": false,
      "totalDatabaseCount": 2,
      "freshDatabaseCount": 1,
      "unavailableDatabaseCount": 1,
      "preservedStaleDatabaseCount": 1,
      "conversationCount": 2,
      "participantCount": 2,
      "messageCount": messages.count,
      "artifactCount": 1,
      "semanticGapCount": 0,
      "messageCandidateGapCount": 0,
      "unavailableArtifactCount": 0,
      "artifactDecodeGapCount": 0,
      "entityDecodeGapCount": 0,
      "checkpointAgeSeconds": 20,
      "sourceCoverageComplete": false,
      "limitationCodes": ["unavailableDatabases", "preservedStaleDatabases"],
      "coverageNote": "One database is unavailable; preserved records may be stale.",
    ]
    if !legacyFormat {
      context["selfParticipantId"] = selfParticipantID
    }
    var identity: [String: Any] = [
      "formatVersion": formatVersion,
      "schema": schema,
      "accountId": "account-1",
      "replicaId": "replica-1",
      "sourceFingerprint": "source-fingerprint-1",
      "checkpointRevision": "checkpoint-1",
      "policySHA256": policySHA,
      "policySourceFingerprint": "source-fingerprint-1",
      "destination": "local",
    ]
    if !legacyFormat {
      identity["selfParticipantId"] = selfParticipantID
    }
    let identityData = try JSONSerialization.data(
      withJSONObject: identity, options: [.sortedKeys, .withoutEscapingSlashes])
    let bundleID = hexDigest(identityData)
    let manifest: [String: Any] = [
      "formatVersion": formatVersion,
      "schema": schema,
      "bundleId": bundleID,
      "createdAtUnixNanoseconds": 1_700_000_000_000_000_000 as UInt64,
      "destination": "local",
      "requesterId": "history-tests",
      "policySHA256": policySHA,
      "policySourceFingerprint": "source-fingerprint-1",
      "context": context,
      "enabledConversationCount": 2,
      "exportedContactCount": 3,
      "exportedMessageCount": messages.count,
      "exportedArtifactCount": 1,
      "artifactResolutionErrorCount": 0,
      "exportComplete": true,
      "files": fileEvidence,
    ]
    let manifestData = try JSONSerialization.data(
      withJSONObject: manifest, options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes])
    try privateFile(manifestData, at: bundleURL.appending(path: "manifest.json"))
  }

  func remove() {
    try? FileManager.default.removeItem(at: rootURL)
  }

  private func writeJSONL(
    _ records: [[String: Any]], role: String, name: String
  ) throws -> [String: Any] {
    var data = Data()
    for record in records {
      data.append(
        try JSONSerialization.data(
          withJSONObject: record, options: [.sortedKeys, .withoutEscapingSlashes]))
      data.append(Data("\n".utf8))
    }
    try privateFile(data, at: bundleURL.appending(path: name))
    return [
      "role": role,
      "relativePath": name,
      "recordCount": records.count,
      "byteCount": data.count,
      "sha256": hexDigest(data),
    ]
  }
}

private func privateDirectory(_ url: URL) throws {
  try FileManager.default.createDirectory(
    at: url,
    withIntermediateDirectories: false,
    attributes: [.posixPermissions: 0o700]
  )
}

private func privateFile(_ data: Data, at url: URL) throws {
  guard
    FileManager.default.createFile(
      atPath: url.path,
      contents: data,
      attributes: [.posixPermissions: 0o600]
    )
  else {
    throw CocoaError(.fileWriteUnknown)
  }
}

private func hexDigest(_ data: Data) -> String {
  SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
}
