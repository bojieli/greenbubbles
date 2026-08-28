import CryptoKit
import Darwin
import Foundation
import Testing

@testable import GreenBubblesHistory

@Suite("HistoryLiveMediaResolverTests")
struct HistoryLiveMediaResolverTests {
  @Test("uses stdin-only authorization and creates a digest-verified private preview")
  func resolvesVerifiedPreview() async throws {
    let fixture = try LiveMediaFixture()
    defer { fixture.remove() }
    let progress = MediaProgressRecorder()

    let media = try await HistoryLiveMediaResolver().resolve(
      conversationID: "conversation-a",
      artifactID: "artifact-a",
      configuration: fixture.configuration,
      replicaKeyUTF8: Array(String(repeating: "a", count: 64).utf8),
      progress: { progress.append($0) }
    )

    #expect(media.artifactID == "artifact-a")
    #expect(media.kind == "image")
    #expect(media.format == "png")
    #expect(media.previewURL != fixture.sourceURL)
    #expect(try Data(contentsOf: media.previewURL) == fixture.sourceData)
    #expect(progress.values.first?.phase == .requestingAuthorization)
    #expect(progress.values.last?.phase == .ready)
    let permissions =
      try FileManager.default.attributesOfItem(atPath: media.previewURL.path)[.posixPermissions]
      as? NSNumber
    #expect(permissions?.intValue == 0o600)
  }

  @Test("rejects malformed keys before invoking the CLI")
  func rejectsMalformedKey() async throws {
    let fixture = try LiveMediaFixture()
    defer { fixture.remove() }
    await #expect(throws: HistoryLiveMediaError.invalidReplicaKey) {
      _ = try await HistoryLiveMediaResolver().resolve(
        conversationID: "conversation-a",
        artifactID: "artifact-a",
        configuration: fixture.configuration,
        replicaKeyUTF8: Array("not-a-key".utf8)
      )
    }
  }

  @Test("rejects a response that is not bound to its request")
  func rejectsMismatchedResponse() async throws {
    let fixture = try LiveMediaFixture(bindRequestID: false)
    defer { fixture.remove() }
    await #expect(throws: HistoryLiveMediaError.invalidResponse) {
      _ = try await HistoryLiveMediaResolver().resolve(
        conversationID: "conversation-a",
        artifactID: "artifact-a",
        configuration: fixture.configuration,
        replicaKeyUTF8: Array(String(repeating: "a", count: 64).utf8)
      )
    }
  }

  @Test("cancels the local media process")
  func cancelsMediaProcess() async throws {
    let fixture = try LiveMediaFixture(responseDelaySeconds: 10)
    defer { fixture.remove() }
    let resolution = Task {
      try await HistoryLiveMediaResolver().resolve(
        conversationID: "conversation-a",
        artifactID: "artifact-a",
        configuration: fixture.configuration,
        replicaKeyUTF8: Array(String(repeating: "a", count: 64).utf8)
      )
    }
    try await Task.sleep(for: .milliseconds(100))
    resolution.cancel()
    await #expect(throws: CancellationError.self) {
      _ = try await resolution.value
    }
  }
}

private final class MediaProgressRecorder: @unchecked Sendable {
  private let lock = NSLock()
  private var storage: [HistoryMediaResolutionProgress] = []
  var values: [HistoryMediaResolutionProgress] { lock.withLock { storage } }
  func append(_ value: HistoryMediaResolutionProgress) {
    lock.withLock { storage.append(value) }
  }
}

private struct LiveMediaFixture {
  let rootURL: URL
  let sourceURL: URL
  let sourceData: Data
  let configuration: HistoryLiveMediaConfiguration

  init(bindRequestID: Bool = true, responseDelaySeconds: Int = 0) throws {
    rootURL = FileManager.default.temporaryDirectory.appending(
      path: "greenbubbles-live-media-tests-\(UUID().uuidString)")
    let scratch = rootURL.appending(path: "scratch")
    let previews = rootURL.appending(path: "previews")
    try createPrivateDirectory(rootURL)
    try createPrivateDirectory(scratch)
    try createPrivateDirectory(previews)
    sourceURL = rootURL.appending(path: "source.png")
    sourceData = Data(repeating: 0x5A, count: 2 * 1_024 * 1_024 + 17)
    try createPrivateFile(sourceData, at: sourceURL)
    let digest = SHA256.hash(data: sourceData).map { String(format: "%02x", $0) }.joined()
    let response: [String: Any] = [
      "formatVersion": 1,
      "schema": "greenbubbles.ai-query.v1",
      "apiVersion": "greenbubbles.connector.v1",
      "requestId": "REQUEST_ID",
      "ok": true,
      "context": [
        "accountId": "account-1",
        "replicaId": "replica-1",
        "sourceFingerprint": "source-1",
        "checkpointRevision": "checkpoint-1",
      ],
      "result": [
        "kind": "artifact",
        "value": [
          "artifactId": "artifact-a",
          "kind": "image",
          "source": [
            "absolutePath": sourceURL.path,
            "byteCount": sourceData.count,
            "sha256": digest,
            "format": "png",
          ],
        ],
      ],
    ]
    let responseData = try JSONSerialization.data(
      withJSONObject: response, options: [.sortedKeys, .withoutEscapingSlashes])
    let responseText = String(decoding: responseData, as: UTF8.self)
    let executable = rootURL.appending(path: "fake-greenbubbles-restore")
    let responseCommand =
      bindRequestID
      ? """
      request_id=$(/usr/bin/sed -n 's/.*\"requestId\":\"\\([^\"]*\\)\".*/\\1/p' "$5")
      printf '%s' '\(responseText)' | /usr/bin/sed "s/REQUEST_ID/$request_id/"
      """
      : "printf '%s' '\(responseText)'"
    let script = """
      #!/bin/sh
      IFS= read -r replica_key
      if [ "${#replica_key}" -ne 64 ]; then
        exit 9
      fi
      /bin/sleep \(responseDelaySeconds)
      \(responseCommand)
      """
    try createPrivateFile(Data(script.utf8), at: executable)
    guard chmod(executable.path, 0o700) == 0 else {
      throw CocoaError(.fileWriteUnknown)
    }
    configuration = HistoryLiveMediaConfiguration(
      executableURL: executable,
      replicaURL: rootURL.appending(path: "replica.db"),
      policyURL: rootURL.appending(path: "policy.json"),
      auditURL: rootURL.appending(path: "audit.ndjson"),
      sessionDirectory: rootURL,
      scratchDirectory: scratch,
      previewDirectory: previews,
      expectedAccountID: "account-1",
      expectedReplicaID: "replica-1",
      expectedSourceFingerprint: "source-1"
    )
  }

  func remove() {
    try? FileManager.default.removeItem(at: rootURL)
  }
}

private func createPrivateDirectory(_ url: URL) throws {
  try FileManager.default.createDirectory(
    at: url,
    withIntermediateDirectories: false,
    attributes: [.posixPermissions: 0o700]
  )
}

private func createPrivateFile(_ data: Data, at url: URL) throws {
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
