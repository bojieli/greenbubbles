import Darwin
import Foundation
import Testing

@testable import GreenBubblesHistory

@Suite("HistoryDirectQueryClientTests")
struct HistoryDirectQueryClientTests {
  @Test("sends the snapshot key only through stdin and decodes a bounded page")
  func decodesConversationPage() async throws {
    let fixture = try DirectQueryFixture(response: conversationResponse)
    defer { fixture.remove() }
    let key = String(repeating: "a", count: 64)

    let page = try await HistoryDirectQueryClient().conversations(
      configuration: fixture.configuration(mode: .snapshotEncrypted),
      keyUTF8: Array(key.utf8),
      limit: 2,
      cursor: "cursor-a"
    )

    #expect(page.operation == "conversations.list")
    #expect(page.items.map(\.id) == ["chat-a"])
    #expect(page.items.first?.displayName == "chat-a")
    #expect(page.items.first?.summary == "hello")
    #expect(
      try fixture.arguments()
        == [
          "conversations", "list", fixture.sourceURL.path, "--limit", "2", "--cursor",
          "cursor-a", "--snapshot-key-stdin",
        ])
    #expect(try fixture.standardInput() == Data("\(key)\n".utf8))
    #expect(!fixture.argumentsText().contains(key))
  }

  @Test("sends search text after the live key on stdin and never in arguments")
  func sendsSearchThroughStandardInput() async throws {
    let fixture = try DirectQueryFixture(response: searchResponse)
    defer { fixture.remove() }
    let key = String(repeating: "b", count: 64)
    let query = "private phrase with spaces"

    let page = try await HistoryDirectQueryClient().search(
      configuration: fixture.configuration(mode: .liveEncrypted),
      keyUTF8: Array(key.utf8),
      query: query,
      conversationID: "chat-a",
      limit: 7,
      cursor: "cursor-search"
    )

    #expect(page.items.first?.snippet == "private phrase")
    #expect(page.items.first?.senderLabel == "alice")
    #expect(
      try fixture.arguments()
        == [
          "messages", "search", fixture.sourceURL.path, "--query-stdin", "--limit", "7",
          "--conversation", "chat-a", "--cursor", "cursor-search", "--passphrase-stdin",
        ])
    #expect(try fixture.standardInput() == Data("\(key)\n\(query)".utf8))
    let arguments = fixture.argumentsText()
    #expect(!arguments.contains(key))
    #expect(!arguments.contains(query))
  }

  @Test("decodes optional contact and sender display names while retaining legacy fallbacks")
  func decodesOptionalDisplayNames() async throws {
    let enrichedConversation = conversationResponse.replacingOccurrences(
      of: #""id":"chat-a""#,
      with: #""id":"chat-a","displayName":"Alice Remark""#
    )
    let conversationFixture = try DirectQueryFixture(response: enrichedConversation)
    defer { conversationFixture.remove() }
    let conversations = try await HistoryDirectQueryClient().conversations(
      configuration: conversationFixture.configuration(mode: .snapshotEncrypted),
      keyUTF8: Array(String(repeating: "a", count: 64).utf8),
      limit: 2
    )
    #expect(conversations.items.first?.contactDisplayName == "Alice Remark")
    #expect(conversations.items.first?.displayName == "Alice Remark")

    let enrichedSearch = searchResponse.replacingOccurrences(
      of: #""sender":"alice""#,
      with: #""sender":"alice","senderDisplayName":"Alice Remark""#
    )
    let searchFixture = try DirectQueryFixture(response: enrichedSearch)
    defer { searchFixture.remove() }
    let search = try await HistoryDirectQueryClient().search(
      configuration: searchFixture.configuration(mode: .liveEncrypted),
      keyUTF8: Array(String(repeating: "b", count: 64).utf8),
      query: "private phrase",
      limit: 7
    )
    #expect(search.items.first?.senderDisplayName == "Alice Remark")
    #expect(search.items.first?.senderLabel == "Alice Remark")
  }

  @Test("uses a private recovery-kit file without sending secret material through stdin")
  func usesSnapshotRecoveryKit() async throws {
    let fixture = try DirectQueryFixture(response: conversationResponse)
    defer { fixture.remove() }

    let page = try await HistoryDirectQueryClient().conversations(
      configuration: fixture.configuration(mode: .snapshotRecoveryKit),
      keyUTF8: [],
      limit: 2
    )

    #expect(page.source.mode == "snapshotEncrypted")
    #expect(
      try fixture.arguments()
        == [
          "conversations", "list", fixture.sourceURL.path, "--limit", "2",
          "--snapshot-recovery-kit", fixture.recoveryKitURL.path,
        ])
    #expect(try fixture.standardInput().isEmpty)
  }

  @Test("uses a local unlock credential without loading portable recovery words")
  func usesSnapshotLocalCredential() async throws {
    let fixture = try DirectQueryFixture(response: conversationResponse)
    defer { fixture.remove() }

    let page = try await HistoryDirectQueryClient().conversations(
      configuration: fixture.configuration(mode: .snapshotLocalCredential),
      keyUTF8: [],
      limit: 2
    )

    #expect(page.source.mode == "snapshotEncrypted")
    #expect(
      try fixture.arguments()
        == [
          "conversations", "list", fixture.sourceURL.path, "--limit", "2",
          "--snapshot-local-credential", fixture.localCredentialURL.path,
        ])
    #expect(try fixture.standardInput().isEmpty)
  }

  @Test("uses an ephemeral Keychain materialization as the local unlock credential")
  func usesSnapshotKeychainCredential() async throws {
    let fixture = try DirectQueryFixture(response: conversationResponse)
    defer { fixture.remove() }

    let page = try await HistoryDirectQueryClient().conversations(
      configuration: fixture.configuration(mode: .snapshotKeychain),
      keyUTF8: [],
      limit: 2
    )

    #expect(page.source.mode == "snapshotEncrypted")
    #expect(
      try fixture.arguments()
        == [
          "conversations", "list", fixture.sourceURL.path, "--limit", "2",
          "--snapshot-local-credential", fixture.localCredentialURL.path,
        ])
    #expect(try fixture.standardInput().isEmpty)
  }

  @Test("sends an Argon2id snapshot passphrase before search text only through stdin")
  func searchesSnapshotWithPassphrase() async throws {
    let response = searchResponse.replacingOccurrences(
      of: #""mode":"liveEncrypted"#,
      with: #""mode":"snapshotEncrypted"#
    )
    let fixture = try DirectQueryFixture(response: response)
    defer { fixture.remove() }
    let passphrase = "correct horse battery staple"
    let query = "bounded phrase"

    let page = try await HistoryDirectQueryClient().search(
      configuration: fixture.configuration(mode: .snapshotPassphrase),
      keyUTF8: Array(passphrase.utf8),
      query: query,
      limit: 7
    )

    #expect(page.source.mode == "snapshotEncrypted")
    #expect(try fixture.standardInput() == Data("\(passphrase)\n\(query)".utf8))
    #expect(try fixture.arguments().last == "--snapshot-passphrase-stdin")
    #expect(!fixture.argumentsText().contains(passphrase))
    #expect(!fixture.argumentsText().contains(query))
  }

  @Test("sends only search text through stdin when a recovery kit unlocks the snapshot")
  func searchesSnapshotWithRecoveryKit() async throws {
    let response = searchResponse.replacingOccurrences(
      of: #""mode":"liveEncrypted""#,
      with: #""mode":"snapshotEncrypted""#
    )
    let fixture = try DirectQueryFixture(response: response)
    defer { fixture.remove() }
    let query = "portable recovery search"

    let page = try await HistoryDirectQueryClient().search(
      configuration: fixture.configuration(mode: .snapshotRecoveryKit),
      keyUTF8: [],
      query: query,
      limit: 7
    )

    #expect(page.source.mode == "snapshotEncrypted")
    #expect(try fixture.standardInput() == Data(query.utf8))
    #expect(!fixture.argumentsText().contains(query))
  }

  @Test("rejects a recovery-kit file readable by another user")
  func rejectsUnsafeRecoveryKitPermissions() async throws {
    let fixture = try DirectQueryFixture(response: conversationResponse)
    defer { fixture.remove() }
    guard chmod(fixture.recoveryKitURL.path, 0o644) == 0 else {
      throw CocoaError(.fileWriteUnknown)
    }

    await #expect(throws: HistoryDirectQueryError.self) {
      _ = try await HistoryDirectQueryClient().conversations(
        configuration: fixture.configuration(mode: .snapshotRecoveryKit),
        keyUTF8: []
      )
    }
  }

  @Test("rejects a local credential beneath a nonprivate directory")
  func rejectsUnsafeLocalCredentialDirectory() async throws {
    let fixture = try DirectQueryFixture(response: conversationResponse)
    defer { fixture.remove() }
    guard chmod(fixture.rootURL.path, 0o755) == 0 else {
      throw CocoaError(.fileWriteUnknown)
    }

    await #expect(throws: HistoryDirectQueryError.self) {
      _ = try await HistoryDirectQueryClient().conversations(
        configuration: fixture.configuration(mode: .snapshotLocalCredential),
        keyUTF8: []
      )
    }
  }

  @Test("decrypted status receives no key material")
  func decodesPlaintextStatus() async throws {
    let fixture = try DirectQueryFixture(response: statusResponse)
    defer { fixture.remove() }

    let status = try await HistoryDirectQueryClient().status(
      configuration: fixture.configuration(mode: .decrypted),
      keyUTF8: []
    )

    #expect(status.databaseCount == 2)
    #expect(status.totalSqliteStorageBytes == 1_100)
    #expect(try fixture.standardInput().isEmpty)
    #expect(try fixture.arguments().last == "--decrypted")
  }

  @Test("rejects a success envelope for another operation")
  func rejectsMismatchedOperation() async throws {
    let fixture = try DirectQueryFixture(response: statusResponse)
    defer { fixture.remove() }

    await #expect(throws: HistoryDirectQueryError.invalidResponse) {
      _ = try await HistoryDirectQueryClient().conversations(
        configuration: fixture.configuration(mode: .decrypted),
        keyUTF8: []
      )
    }
  }

  @Test("rejects a success envelope for another access mode")
  func rejectsMismatchedSourceMode() async throws {
    let fixture = try DirectQueryFixture(response: conversationResponse)
    defer { fixture.remove() }

    await #expect(throws: HistoryDirectQueryError.invalidResponse) {
      _ = try await HistoryDirectQueryClient().conversations(
        configuration: fixture.configuration(mode: .decrypted),
        keyUTF8: []
      )
    }
  }

  @Test("maps a bounded CLI error without exposing stderr")
  func mapsCommandError() async throws {
    let response = """
      {"schema":"greenbubbles.query.v1","formatVersion":1,"operation":"conversations.list","ok":false,"error":{"code":"sourceBusy","message":"The source is busy; retry shortly.","retryable":true}}
      """
    let fixture = try DirectQueryFixture(
      response: response,
      exitStatus: 2,
      standardError: "sensitive diagnostic that must not escape"
    )
    defer { fixture.remove() }

    await #expect(
      throws: HistoryDirectQueryError.commandFailed(
        code: "sourceBusy",
        message: "The source is busy; retry shortly.",
        retryable: true
      )
    ) {
      _ = try await HistoryDirectQueryClient().conversations(
        configuration: fixture.configuration(mode: .decrypted),
        keyUTF8: []
      )
    }
  }

  @Test("rejects stdout beyond the fixed response cap")
  func rejectsOversizedResponse() async throws {
    let fixture = try DirectQueryFixture(responseByteCount: 8 * 1_024 * 1_024 + 1)
    defer { fixture.remove() }

    await #expect(throws: HistoryDirectQueryError.responseTooLarge) {
      _ = try await HistoryDirectQueryClient().conversations(
        configuration: fixture.configuration(mode: .decrypted),
        keyUTF8: []
      )
    }
  }

  @Test("cancels and terminates a local query")
  func cancelsQuery() async throws {
    let fixture = try DirectQueryFixture(response: conversationResponse, delaySeconds: 10)
    defer { fixture.remove() }
    let query = Task {
      try await HistoryDirectQueryClient().conversations(
        configuration: fixture.configuration(mode: .decrypted),
        keyUTF8: []
      )
    }
    try await Task.sleep(for: .milliseconds(100))
    query.cancel()

    await #expect(throws: CancellationError.self) {
      _ = try await query.value
    }
  }

  @Test("terminates a query after its deadline")
  func timesOutQuery() async throws {
    let fixture = try DirectQueryFixture(response: conversationResponse, delaySeconds: 10)
    defer { fixture.remove() }

    await #expect(throws: HistoryDirectQueryError.timedOut) {
      _ = try await HistoryDirectQueryClient(timeoutMillisecondsForTesting: 75).conversations(
        configuration: fixture.configuration(mode: .decrypted),
        keyUTF8: []
      )
    }
  }
}

private struct DirectQueryFixture {
  let rootURL: URL
  let sourceURL: URL
  let executableURL: URL
  let recoveryKitURL: URL
  let localCredentialURL: URL
  private let argumentsURL: URL
  private let inputURL: URL

  init(
    response: String,
    exitStatus: Int = 0,
    standardError: String = "",
    delaySeconds: Int = 0
  ) throws {
    rootURL = FileManager.default.temporaryDirectory.appending(
      path: "greenbubbles-direct-query-tests-\(UUID().uuidString)",
      directoryHint: .isDirectory
    )
    sourceURL = rootURL.appending(path: "source", directoryHint: .isDirectory)
    executableURL = rootURL.appending(path: "fake-greenbubbles-restore")
    recoveryKitURL = rootURL.appending(path: "snapshot-recovery-kit.txt")
    localCredentialURL = rootURL.appending(path: ".snapshot-local-unlock")
    argumentsURL = rootURL.appending(path: "arguments")
    inputURL = rootURL.appending(path: "stdin")
    try makeDirectPrivateDirectory(rootURL)
    try makeDirectPrivateDirectory(sourceURL)
    try makeDirectPrivateFile(Data("placeholder recovery words\n".utf8), at: recoveryKitURL)
    try makeDirectPrivateFile(Data("placeholder local credential\n".utf8), at: localCredentialURL)
    let script = """
      #!/bin/sh
      /usr/bin/printf '%s\\n' "$@" > '\(argumentsURL.path)'
      /bin/cat > '\(inputURL.path)'
      /bin/sleep \(delaySeconds)
      /usr/bin/printf '%s' '\(shellSingleQuoted(response))'
      /usr/bin/printf '%s' '\(shellSingleQuoted(standardError))' >&2
      exit \(exitStatus)
      """
    try makeDirectPrivateFile(Data(script.utf8), at: executableURL)
    guard chmod(executableURL.path, 0o700) == 0 else {
      throw CocoaError(.fileWriteUnknown)
    }
  }

  init(responseByteCount: Int) throws {
    rootURL = FileManager.default.temporaryDirectory.appending(
      path: "greenbubbles-direct-query-tests-\(UUID().uuidString)",
      directoryHint: .isDirectory
    )
    sourceURL = rootURL.appending(path: "source", directoryHint: .isDirectory)
    executableURL = rootURL.appending(path: "fake-greenbubbles-restore")
    recoveryKitURL = rootURL.appending(path: "snapshot-recovery-kit.txt")
    localCredentialURL = rootURL.appending(path: ".snapshot-local-unlock")
    argumentsURL = rootURL.appending(path: "arguments")
    inputURL = rootURL.appending(path: "stdin")
    try makeDirectPrivateDirectory(rootURL)
    try makeDirectPrivateDirectory(sourceURL)
    try makeDirectPrivateFile(Data("placeholder recovery words\n".utf8), at: recoveryKitURL)
    try makeDirectPrivateFile(Data("placeholder local credential\n".utf8), at: localCredentialURL)
    let script = """
      #!/bin/sh
      /usr/bin/printf '%s\\n' "$@" > '\(argumentsURL.path)'
      /bin/cat > '\(inputURL.path)'
      /usr/bin/head -c \(responseByteCount) /dev/zero
      """
    try makeDirectPrivateFile(Data(script.utf8), at: executableURL)
    guard chmod(executableURL.path, 0o700) == 0 else {
      throw CocoaError(.fileWriteUnknown)
    }
  }

  func configuration(mode: HistoryDirectAccessMode) -> HistoryDirectConfiguration {
    HistoryDirectConfiguration(
      executableURL: executableURL,
      sourceURL: sourceURL,
      accessMode: mode,
      recoveryKitURL: mode == .snapshotRecoveryKit ? recoveryKitURL : nil,
      localCredentialURL:
        mode == .snapshotLocalCredential || mode == .snapshotKeychain ? localCredentialURL : nil
    )
  }

  func arguments() throws -> [String] {
    argumentsText().split(separator: "\n", omittingEmptySubsequences: false).dropLast().map(
      String.init)
  }

  func argumentsText() -> String {
    (try? String(contentsOf: argumentsURL, encoding: .utf8)) ?? ""
  }

  func standardInput() throws -> Data { try Data(contentsOf: inputURL) }

  func remove() { try? FileManager.default.removeItem(at: rootURL) }
}

private let conversationResponse = """
  {"schema":"greenbubbles.query.v1","formatVersion":1,"operation":"conversations.list","ok":true,"source":{"mode":"snapshotEncrypted","identity":"source-a"},"consistency":{"guarantee":"singleDatabaseReadStatement","databaseCount":1,"crossDatabaseAtomic":true,"coverageComplete":true,"observedAtUnixMilliseconds":123},"page":{"limit":2,"returned":1,"hasMore":false},"warnings":[],"items":[{"id":"chat-a","summary":"hello","summaryDecodeState":"decoded","summaryTruncated":false,"sortTimestamp":1700000000}]}
  """

private let searchResponse = """
  {"schema":"greenbubbles.query.v1","formatVersion":1,"operation":"messages.search","ok":true,"source":{"mode":"liveEncrypted","identity":"source-a"},"consistency":{"guarantee":"boundedNativeFullTextSearch","databaseCount":1,"crossDatabaseAtomic":true,"coverageComplete":true,"observedAtUnixMilliseconds":123},"page":{"limit":7,"returned":1,"hasMore":false},"warnings":[],"items":[{"id":"message-a","conversationId":"chat-a","sender":"alice","createdAtUnix":1700000000,"sortSequence":9,"messageLocalId":8,"messageType":1,"messageTypeLabel":"text","messageSubtype":0,"messageSubtypeLabel":"none","snippet":"private phrase","snippetTruncated":false}]}
  """

private let statusResponse = """
  {"schema":"greenbubbles.query.v1","formatVersion":1,"operation":"source.status","ok":true,"source":{"mode":"decrypted","identity":"source-a"},"observedAtUnixMilliseconds":123,"databaseCount":2,"databaseBytes":1000,"writeAheadLogCount":1,"writeAheadLogBytes":100,"sharedMemoryCount":0,"sharedMemoryBytes":0,"rollbackJournalCount":0,"rollbackJournalBytes":0,"totalSqliteStorageBytes":1100,"entries":[{"relativePath":"db/message.db","databaseBytes":1000,"writeAheadLogBytes":100,"sharedMemoryBytes":0,"rollbackJournalBytes":0}]}
  """

private func shellSingleQuoted(_ value: String) -> String {
  value.replacingOccurrences(of: "'", with: "'\\''")
}

private func makeDirectPrivateDirectory(_ url: URL) throws {
  try FileManager.default.createDirectory(
    at: url,
    withIntermediateDirectories: false,
    attributes: [.posixPermissions: 0o700]
  )
}

private func makeDirectPrivateFile(_ data: Data, at url: URL) throws {
  guard
    FileManager.default.createFile(
      atPath: url.path,
      contents: data,
      attributes: [.posixPermissions: 0o600]
    )
  else { throw CocoaError(.fileWriteUnknown) }
}
