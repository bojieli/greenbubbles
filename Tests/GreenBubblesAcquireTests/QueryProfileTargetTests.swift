import Darwin
import Foundation
import Testing

@testable import GreenBubblesAcquire

struct QueryProfileTargetTests {
  // Made-up profiles: two live accounts and a snapshot.
  private let profiles = """
    {
      "schema": "greenbubbles.query-profiles.v1",
      "formatVersion": 1,
      "defaultProfile": "main",
      "profiles": {
        "main": {
          "sourceRoot": "/example/main/db_storage",
          "access": { "mode": "liveWeChatKeyFile", "credentialFile": "/example/keys/main-key" }
        },
        "second": {
          "sourceRoot": "/example/second/db_storage",
          "access": { "mode": "liveWeChatKeyFile", "credentialFile": "/example/keys/second-key" }
        },
        "archive": {
          "sourceRoot": "/example/snapshot",
          "access": { "mode": "snapshotLocalCredential", "credentialFile": "/example/keys/snap" }
        },
        "relative": {
          "sourceRoot": "example/db_storage",
          "access": { "mode": "liveWeChatKeyFile", "credentialFile": "/example/keys/rel" }
        }
      }
    }
    """

  @Test func defaultProfileResolvesWhenNoNameIsGiven() throws {
    let file = try writeProfiles(profiles)
    defer { remove(file) }
    let profile = try QueryProfileTarget.resolve(name: nil, file: file, ownerUID: getuid())
    #expect(profile?.name == "main")
    #expect(profile?.sourceRoot.path == "/example/main/db_storage")
    #expect(profile?.credentialFile.path == "/example/keys/main-key")
  }

  @Test func namedProfileResolves() throws {
    let file = try writeProfiles(profiles)
    defer { remove(file) }
    let profile = try QueryProfileTarget.resolve(name: "second", file: file, ownerUID: getuid())
    #expect(profile?.credentialFile.path == "/example/keys/second-key")
  }

  @Test func missingFileKeepsDefaultsUnlessAProfileWasNamed() throws {
    let file = FileManager.default.temporaryDirectory.appending(
      path: "absent-\(UUID().uuidString).json")
    #expect(try QueryProfileTarget.resolve(name: nil, file: file, ownerUID: getuid()) == nil)
    #expect(throws: QueryProfileError.missingFile) {
      try QueryProfileTarget.resolve(name: "main", file: file, ownerUID: getuid())
    }
  }

  @Test func refusesUnknownSnapshotAndRelativeProfiles() throws {
    let file = try writeProfiles(profiles)
    defer { remove(file) }
    #expect(throws: QueryProfileError.unknownProfile("nope")) {
      try QueryProfileTarget.resolve(name: "nope", file: file, ownerUID: getuid())
    }
    #expect(throws: QueryProfileError.notLiveProfile("archive")) {
      try QueryProfileTarget.resolve(name: "archive", file: file, ownerUID: getuid())
    }
    #expect(throws: QueryProfileError.relativePath("relative")) {
      try QueryProfileTarget.resolve(name: "relative", file: file, ownerUID: getuid())
    }
  }

  @Test func snapshotDefaultKeepsCaptureDefaults() throws {
    let file = try writeProfiles(
      profiles.replacingOccurrences(of: "\"main\",", with: "\"archive\","))
    defer { remove(file) }
    #expect(try QueryProfileTarget.resolve(name: nil, file: file, ownerUID: getuid()) == nil)
  }

  @Test func refusesUnsafeFiles() throws {
    let file = try writeProfiles(profiles)
    defer { remove(file) }

    #expect(throws: QueryProfileError.self) {
      try QueryProfileTarget.resolve(name: nil, file: file, ownerUID: getuid() &+ 1)
    }

    let link = file.deletingLastPathComponent().appending(path: "link-\(UUID().uuidString).json")
    try FileManager.default.createSymbolicLink(at: link, withDestinationURL: file)
    defer { remove(link) }
    #expect(throws: QueryProfileError.self) {
      try QueryProfileTarget.resolve(name: nil, file: link, ownerUID: getuid())
    }

    chmod(file.path, 0o644)
    #expect(throws: QueryProfileError.self) {
      try QueryProfileTarget.resolve(name: nil, file: file, ownerUID: getuid())
    }
  }

  @Test func refusesMalformedJSON() throws {
    let file = try writeProfiles("{\"schema\": \"something-else\", \"profiles\": {}}")
    defer { remove(file) }
    #expect(throws: QueryProfileError.malformed) {
      try QueryProfileTarget.resolve(name: nil, file: file, ownerUID: getuid())
    }
  }

  private func writeProfiles(_ text: String) throws -> URL {
    let file = FileManager.default.temporaryDirectory
      .appending(path: "query-profiles-\(UUID().uuidString).json")
    try Data(text.utf8).write(to: file)
    chmod(file.path, 0o600)
    return file
  }

  private func remove(_ url: URL) {
    try? FileManager.default.removeItem(at: url)
  }
}
