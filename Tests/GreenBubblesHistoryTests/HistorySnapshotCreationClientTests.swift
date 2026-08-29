import Darwin
import Foundation
import Testing

@testable import GreenBubblesHistory

@Suite("HistorySnapshotCreationClientTests")
struct HistorySnapshotCreationClientTests {
  @Test("creates and parses a private 24-word recovery kit without printing words")
  func createsRecoveryKit() async throws {
    let fixture = try SnapshotCreationFixture()
    defer { fixture.remove() }
    let runner = SnapshotRecordingRunner { invocation in
      #expect(
        invocation.arguments
          == ["snapshot", "recovery-kit", "create", fixture.recoveryKitURL.path]
      )
      #expect(invocation.standardInput.isEmpty)
      try fixture.writeRecoveryKit(words: recoveryWords)
      return Data(recoveryKitReport.utf8)
    }
    let client = HistorySnapshotCreationClient(runner: runner)

    let kit = try await client.createRecoveryKit(
      executableURL: fixture.executableURL,
      outputURL: fixture.recoveryKitURL
    )

    #expect(kit.words == recoveryWords)
    #expect(kit.sha256.count == 64)
    #expect(runner.invocations.count == 1)
    #expect(!String(decoding: runner.invocations[0].standardInput, as: UTF8.self).contains("abandon"))
    var metadata = stat()
    #expect(lstat(fixture.recoveryKitURL.path, &metadata) == 0)
    #expect(metadata.st_mode & 0o077 == 0)
  }

  @Test("requires exact selected-word confirmation")
  func confirmsSelectedWords() throws {
    let challenge = try HistorySnapshotWordChallenge(
      zeroBasedPositions: [23, 0, 8, 4]
    )
    #expect(challenge.zeroBasedPositions == [0, 4, 8, 23])
    #expect(
      challenge.accepts(
        responses: [
          0: " ABANDON ",
          4: recoveryWords[4],
          8: recoveryWords[8],
          23: recoveryWords[23],
        ],
        words: recoveryWords
      )
    )
    #expect(
      !challenge.accepts(
        responses: [
          0: recoveryWords[0],
          4: recoveryWords[4],
          8: "wrong",
          23: recoveryWords[23],
        ],
        words: recoveryWords
      )
    )
  }

  @Test("sends source key then passphrase only through stdin and binds every protector")
  func createsSnapshotWithOrderedSecrets() async throws {
    let fixture = try SnapshotCreationFixture()
    defer { fixture.remove() }
    try fixture.writeRecoveryKit(words: recoveryWords)
    try fixture.writeLocalCredential()
    let runner = SnapshotRecordingRunner { invocation in
      if invocation.arguments.prefix(3) == ["snapshot", "recovery-kit", "validate"] {
        return Data(recoveryKitReport.utf8)
      }
      return Data(snapshotManifest().utf8)
    }
    let client = HistorySnapshotCreationClient(runner: runner)
    let kit = try client.readRecoveryKit(fixture.recoveryKitURL)
    let sourceKey = String(repeating: "a", count: 64)
    let passphrase = "correct horse battery staple"

    let result = try await client.createSnapshot(
      request: HistorySnapshotCreationRequest(
        executableURL: fixture.executableURL,
        sourceURL: fixture.sourceURL,
        outputURL: fixture.outputURL,
        recoveryKit: kit,
        localCredentialURL: fixture.localCredentialURL,
        sourceAccess: .encryptedWeChat,
        stableCapture: true
      ),
      sourceKeyUTF8: Array(sourceKey.utf8),
      snapshotPassphraseUTF8: Array(passphrase.utf8)
    )

    #expect(result.snapshotID == String(repeating: "c", count: 64))
    #expect(result.databaseCount == 2)
    #expect(result.hasRecoveryWords)
    #expect(result.hasLocalCredential)
    #expect(result.hasPassphrase)
    #expect(result.recoveryVerified)
    #expect(runner.invocations.count == 2)
    #expect(
      runner.invocations[0].arguments
        == [
          "snapshot", "recovery-kit", "validate", fixture.recoveryKitURL.path,
        ]
    )
    #expect(runner.invocations[0].standardInput.isEmpty)
    #expect(
      runner.invocations[1].arguments
        == [
          "snapshot", "create-capture", fixture.sourceURL.path, fixture.outputURL.path,
          "--source-passphrase-stdin", "--snapshot-recovery-kit",
          fixture.recoveryKitURL.path, "--snapshot-local-credential",
          fixture.localCredentialURL.path, "--snapshot-passphrase-stdin",
        ]
    )
    #expect(
      runner.invocations[1].standardInput
        == Array("\(sourceKey)\n\(passphrase)\n".utf8)
    )
    let arguments = runner.invocations[1].arguments.joined(separator: " ")
    #expect(!arguments.contains(sourceKey))
    #expect(!arguments.contains(passphrase))
    #expect(!arguments.contains(recoveryWords.joined(separator: " ")))
  }

  @Test("detects recovery-file changes before invoking snapshot conversion")
  func rejectsTamperedRecoveryKit() async throws {
    let fixture = try SnapshotCreationFixture()
    defer { fixture.remove() }
    try fixture.writeRecoveryKit(words: recoveryWords)
    let runner = SnapshotRecordingRunner { _ in Data(snapshotManifest().utf8) }
    let client = HistorySnapshotCreationClient(runner: runner)
    let kit = try client.readRecoveryKit(fixture.recoveryKitURL)
    var changedWords = recoveryWords
    changedWords[7] = "zoo"
    try fixture.replaceRecoveryKit(words: changedWords)

    await #expect(throws: HistorySnapshotCreationError.invalidRecoveryKit) {
      _ = try await client.createSnapshot(
        request: HistorySnapshotCreationRequest(
          executableURL: fixture.executableURL,
          sourceURL: fixture.sourceURL,
          outputURL: fixture.outputURL,
          recoveryKit: kit,
          sourceAccess: .decryptedSQLite
        ),
        sourceKeyUTF8: []
      )
    }
    #expect(runner.invocations.isEmpty)
  }

  @Test("rejects a manifest where secondary protectors replace 24-word recovery")
  func requiresPortableBIP39Protector() async throws {
    let fixture = try SnapshotCreationFixture()
    defer { fixture.remove() }
    try fixture.writeRecoveryKit(words: recoveryWords)
    try fixture.writeLocalCredential()
    let manifest = snapshotManifest(
      protectorKinds: ["localCredentialV1", "argon2idPassphraseV1"]
    )
    let runner = SnapshotRecordingRunner { invocation in
      invocation.arguments.prefix(3) == ["snapshot", "recovery-kit", "validate"]
        ? Data(recoveryKitReport.utf8) : Data(manifest.utf8)
    }
    let client = HistorySnapshotCreationClient(runner: runner)
    let kit = try client.readRecoveryKit(fixture.recoveryKitURL)

    await #expect(throws: HistorySnapshotCreationError.invalidResponse) {
      _ = try await client.createSnapshot(
        request: HistorySnapshotCreationRequest(
          executableURL: fixture.executableURL,
          sourceURL: fixture.sourceURL,
          outputURL: fixture.outputURL,
          recoveryKit: kit,
          localCredentialURL: fixture.localCredentialURL,
          sourceAccess: .decryptedSQLite
        ),
        sourceKeyUTF8: [],
        snapshotPassphraseUTF8: Array("long independent passphrase".utf8)
      )
    }
  }

  @Test("creates the local convenience credential through an explicit private file")
  func createsLocalCredential() async throws {
    let fixture = try SnapshotCreationFixture()
    defer { fixture.remove() }
    let runner = SnapshotRecordingRunner { invocation in
      #expect(
        invocation.arguments
          == [
            "snapshot", "local-credential", "create", fixture.localCredentialURL.path,
          ]
      )
      try fixture.writeLocalCredential()
      return Data(localCredentialReport.utf8)
    }

    let data = try await HistorySnapshotCreationClient(runner: runner).createLocalCredential(
      executableURL: fixture.executableURL,
      outputURL: fixture.localCredentialURL
    )

    #expect(String(decoding: data, as: UTF8.self).hasPrefix("GREENBUBBLES LOCAL UNLOCK"))
    #expect(runner.invocations.count == 1)
    #expect(runner.invocations[0].standardInput.isEmpty)
  }
}

private struct SnapshotRunnerInvocation: Sendable {
  let arguments: [String]
  let standardInput: [UInt8]
}

private final class SnapshotRecordingRunner: @unchecked Sendable,
  HistorySnapshotCommandRunning
{
  private let lock = NSLock()
  private var recorded: [SnapshotRunnerInvocation] = []
  private let handler: @Sendable (SnapshotRunnerInvocation) throws -> Data

  init(handler: @escaping @Sendable (SnapshotRunnerInvocation) throws -> Data) {
    self.handler = handler
  }

  var invocations: [SnapshotRunnerInvocation] {
    lock.withLock { recorded }
  }

  func run(
    executableURL: URL,
    arguments: [String],
    standardInput: [UInt8],
    maximumOutputBytes: Int,
    maximumErrorBytes: Int,
    timeoutMilliseconds: Int
  ) async throws -> Data {
    _ = executableURL
    _ = maximumOutputBytes
    _ = maximumErrorBytes
    _ = timeoutMilliseconds
    let invocation = SnapshotRunnerInvocation(
      arguments: arguments,
      standardInput: standardInput
    )
    lock.withLock { recorded.append(invocation) }
    return try handler(invocation)
  }
}

private final class SnapshotCreationFixture: @unchecked Sendable {
  let rootURL: URL
  let sourceURL: URL
  let outputURL: URL
  let executableURL: URL
  let recoveryKitURL: URL
  let localCredentialURL: URL

  init() throws {
    rootURL = FileManager.default.temporaryDirectory.appending(
      path: "greenbubbles-snapshot-client-tests-\(UUID().uuidString)",
      directoryHint: .isDirectory
    )
    sourceURL = rootURL.appending(path: "source", directoryHint: .isDirectory)
    outputURL = rootURL.appending(path: "new-snapshot", directoryHint: .isDirectory)
    executableURL = rootURL.appending(path: "greenbubbles-restore")
    recoveryKitURL = rootURL.appending(path: "recovery-kit.txt")
    localCredentialURL = rootURL.appending(path: ".snapshot-local-unlock")
    try makeSnapshotPrivateDirectory(rootURL)
    try makeSnapshotPrivateDirectory(sourceURL)
    try makeSnapshotPrivateFile(Data("#!/bin/sh\n".utf8), at: executableURL)
    guard chmod(executableURL.path, 0o700) == 0 else {
      throw CocoaError(.fileWriteUnknown)
    }
  }

  func writeRecoveryKit(words: [String]) throws {
    try makeSnapshotPrivateFile(recoveryKitData(words: words), at: recoveryKitURL)
  }

  func replaceRecoveryKit(words: [String]) throws {
    try FileManager.default.removeItem(at: recoveryKitURL)
    try writeRecoveryKit(words: words)
  }

  func writeLocalCredential() throws {
    let credential = """
      GREENBUBBLES LOCAL UNLOCK CREDENTIAL
      format: 1
      credential-id: \(String(repeating: "d", count: 64))
      secret: \(String(repeating: "e", count: 64))

      """
    try makeSnapshotPrivateFile(Data(credential.utf8), at: localCredentialURL)
  }

  func remove() { try? FileManager.default.removeItem(at: rootURL) }
}

private let recoveryWords = [
  "abandon", "ability", "able", "about", "above", "absent", "absorb", "abstract",
  "absurd", "abuse", "access", "accident", "account", "accuse", "achieve", "acid",
  "acoustic", "acquire", "across", "act", "action", "actor", "actress", "actual",
]

private let recoveryKitReport = """
  {"schema":"greenbubbles.recovery-kit.v1","formatVersion":1,"wordCount":24,"checksumValidated":true,"portable":true,"fileCreated":true}
  """

private let localCredentialReport = """
  {"schema":"greenbubbles.local-unlock-credential.v1","formatVersion":1,"localConvenience":true,"portable":false,"fileCreated":true}
  """

private func snapshotManifest(
  protectorKinds: [String] = [
    "bip39English24", "localCredentialV1", "argon2idPassphraseV1",
  ]
) -> String {
  let protectors = protectorKinds.map { #"{"kind":"\#($0)"}"# }.joined(separator: ",")
  return """
    {"schema":"greenbubbles.recoverable-snapshot.v2","formatVersion":2,"snapshotId":"\(String(repeating: "c", count: 64))","protection":{"independentOfWechatKey":true,"plaintextDatabaseFiles":false,"protectors":[\(protectors)]},"recoveryVerified":true,"databases":[{"relativePath":"message/message_0.db"},{"relativePath":"contact/contact.db"}]}
    """
}

private func recoveryKitData(words: [String]) -> Data {
  Data(
    """
    GREENBUBBLES RECOVERY KIT
    format: 1
    language: english
    words: \(words.joined(separator: " "))

    """.utf8
  )
}

private func makeSnapshotPrivateDirectory(_ url: URL) throws {
  try FileManager.default.createDirectory(
    at: url,
    withIntermediateDirectories: false,
    attributes: [.posixPermissions: 0o700]
  )
}

private func makeSnapshotPrivateFile(_ data: Data, at url: URL) throws {
  guard
    FileManager.default.createFile(
      atPath: url.path,
      contents: data,
      attributes: [.posixPermissions: 0o600]
    )
  else { throw CocoaError(.fileWriteUnknown) }
}
