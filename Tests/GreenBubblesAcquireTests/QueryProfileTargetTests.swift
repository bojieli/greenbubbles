import Darwin
import Foundation
import Testing

@testable import GreenBubblesAcquire

/// Made-up profiles in temporary folders. The accepted and rejected shapes
/// follow the Rust CLI's tests in `Native/GreenBubbles/src/query_profile.rs`.
struct QueryProfileTargetTests {
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
        "plain": {
          "sourceRoot": "/example/plain",
          "access": { "mode": "decrypted" }
        },
        "dotted": {
          "sourceRoot": "/example/dotted/db_storage",
          "access": { "mode": "liveWeChatKeyFile", "credentialFile": "/Users/x/../../etc/key" }
        }
      }
    }
    """

  // MARK: Selection

  @Test func defaultProfileResolvesWhenNoNameIsGiven() throws {
    let home = try TemporaryHome()
    try home.writeProfiles(profiles)
    let target = try home.resolve(nil)
    #expect(target.profileName == "main")
    #expect(target.sourceRoot?.path == "/example/main/db_storage")
    #expect(target.credentialFile?.path == "/example/keys/main-key")
    #expect(target.sourceRootOrigin == "from query profile main")
  }

  @Test func namedProfileResolves() throws {
    let home = try TemporaryHome()
    try home.writeProfiles(profiles)
    #expect(try home.resolve("second").credentialFile?.path == "/example/keys/second-key")
  }

  @Test func missingFileKeepsDefaultsUnlessAProfileWasNamed() throws {
    let home = try TemporaryHome()
    #expect(try home.resolve(nil) == .none)
    #expect(throws: QueryProfileError.missingFile) { try home.resolve("main") }
  }

  @Test func missingDefaultProfileIsAnErrorLikeRust() throws {
    let home = try TemporaryHome()
    try home.writeProfiles(
      profiles.replacingOccurrences(of: "\"defaultProfile\": \"main\",", with: ""))
    #expect(throws: QueryProfileError.noDefaultProfile) { try home.resolve(nil) }
    #expect(try home.resolve("second").profileName == "second")
  }

  @Test func refusesUnknownAndNonLiveProfiles() throws {
    let home = try TemporaryHome()
    try home.writeProfiles(profiles)
    #expect(throws: QueryProfileError.unknownProfile("nope")) { try home.resolve("nope") }
    #expect(
      throws: QueryProfileError.notLiveProfile(name: "archive", mode: "snapshotLocalCredential")
    ) {
      try home.resolve("archive")
    }
    #expect(throws: QueryProfileError.notLiveProfile(name: "plain", mode: "decrypted")) {
      try home.resolve("plain")
    }
  }

  @Test func nonLiveDefaultIsAnErrorNotASilentFallback() throws {
    let home = try TemporaryHome()
    try home.writeProfiles(
      profiles.replacingOccurrences(
        of: "\"defaultProfile\": \"main\"", with: "\"defaultProfile\": \"archive\""))
    #expect(throws: QueryProfileError.self) { try home.resolve(nil) }
  }

  @Test func credentialFileWithDotDotIsRefused() throws {
    let home = try TemporaryHome()
    try home.writeProfiles(profiles)
    #expect(throws: QueryProfileError.self) { try home.resolve("dotted") }
    for path in ["/a/./key", "/a/b/../key", "/..", "/"] {
      #expect(throws: QueryProfileError.self) {
        try QueryProfileTarget.requireSafeCredentialPath(path, field: "test")
      }
    }
    try QueryProfileTarget.requireSafeCredentialPath("/Users/x/.greenbubbles/key", field: "test")
  }

  @Test func untouchedPlaceholderTemplateCountsAsNoProfile() throws {
    let home = try TemporaryHome()
    let template = """
      {"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1, "defaultProfile": "live",
       "profiles": {"live": {"sourceRoot": "/ABSOLUTE/PATH/TO/db_storage",
         "access": {"mode": "liveWeChatKeyFile", "credentialFile": "/ABSOLUTE/PATH/TO/key"}}}}
      """
    try home.writeProfiles(template)
    #expect(try home.resolve(nil) == .none)
    // Named explicitly, it is used as written.
    #expect(try home.resolve("live").profileName == "live")
  }

  // MARK: Strict format, as in Rust

  @Test(arguments: [
    // wrong schema
    #"{"schema": "other", "formatVersion": 1, "profiles": {"a": {"sourceRoot": "/s", "access": {"mode": "decrypted"}}}}"#,
    // formatVersion missing, wrong, fractional, or boolean
    #"{"schema": "greenbubbles.query-profiles.v1", "profiles": {"a": {"sourceRoot": "/s", "access": {"mode": "decrypted"}}}}"#,
    #"{"schema": "greenbubbles.query-profiles.v1", "formatVersion": 2, "profiles": {"a": {"sourceRoot": "/s", "access": {"mode": "decrypted"}}}}"#,
    #"{"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1.5, "profiles": {"a": {"sourceRoot": "/s", "access": {"mode": "decrypted"}}}}"#,
    #"{"schema": "greenbubbles.query-profiles.v1", "formatVersion": true, "profiles": {"a": {"sourceRoot": "/s", "access": {"mode": "decrypted"}}}}"#,
    // unknown fields at each level
    #"{"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1, "extra": 1, "profiles": {"a": {"sourceRoot": "/s", "access": {"mode": "decrypted"}}}}"#,
    #"{"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1, "profiles": {"a": {"sourceRoot": "/s", "access": {"mode": "decrypted"}, "secret": "x"}}}"#,
    #"{"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1, "profiles": {"a": {"sourceRoot": "/s", "access": {"mode": "decrypted", "passphrase": "x"}}}}"#,
    #"{"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1, "profiles": {"a": {"sourceRoot": "/s", "access": {"mode": "liveWeChatKeyFile", "credentialFile": "/k", "key": "x"}}}}"#,
    // bad profile names
    #"{"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1, "profiles": {"../live": {"sourceRoot": "/s", "access": {"mode": "decrypted"}}}}"#,
    #"{"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1, "profiles": {"a b": {"sourceRoot": "/s", "access": {"mode": "decrypted"}}}}"#,
    #"{"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1, "profiles": {"..": {"sourceRoot": "/s", "access": {"mode": "decrypted"}}}}"#,
    #"{"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1, "profiles": {"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa": {"sourceRoot": "/s", "access": {"mode": "decrypted"}}}}"#,
    // defaultProfile naming nothing
    #"{"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1, "defaultProfile": "b", "profiles": {"a": {"sourceRoot": "/s", "access": {"mode": "decrypted"}}}}"#,
    // empty profiles, relative or root paths, unknown mode
    #"{"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1, "profiles": {}}"#,
    #"{"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1, "profiles": {"a": {"sourceRoot": "s", "access": {"mode": "decrypted"}}}}"#,
    #"{"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1, "profiles": {"a": {"sourceRoot": "/", "access": {"mode": "decrypted"}}}}"#,
    #"{"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1, "profiles": {"a": {"sourceRoot": "/s", "access": {"mode": "liveWeChatKeyFile", "credentialFile": "k"}}}}"#,
    #"{"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1, "profiles": {"a": {"sourceRoot": "/s", "access": {"mode": "somethingElse"}}}}"#,
    // not JSON
    "schema = 1",
  ])
  func refusesWhatRustRefuses(json: String) throws {
    #expect(throws: QueryProfileError.self) {
      try QueryProfileTarget.ProfileStore.parse(Data(json.utf8))
    }
  }

  @Test func acceptsAMinimalStore() throws {
    let store = try QueryProfileTarget.ProfileStore.parse(
      Data(
        #"{"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1, "defaultProfile": null, "profiles": {"Main_1.x-y": {"sourceRoot": "/s", "access": {"mode": "decrypted"}}}}"#
          .utf8))
    #expect(store.defaultProfile == nil)
    #expect(store.profiles.keys.sorted() == ["Main_1.x-y"])
  }

  @Test func refusesBadRequestedNames() throws {
    let home = try TemporaryHome()
    try home.writeProfiles(profiles)
    #expect(throws: QueryProfileError.self) { try home.resolve("../main") }
    #expect(throws: QueryProfileError.self) { try home.resolve("") }
  }

  // MARK: File safety

  @Test func refusesUnsafeProfileFiles() throws {
    let home = try TemporaryHome()
    try home.writeProfiles(profiles)
    let file = home.locations.profilesFile

    #expect(throws: QueryProfileError.self) { try home.resolve(nil, ownerUID: getuid() &+ 1) }

    chmod(file.path, 0o644)
    #expect(throws: QueryProfileError.self) { try home.resolve(nil) }
    chmod(file.path, 0o600)

    let extraLink = home.directory.appending(path: "hard-link.json")
    #expect(link(file.path, extraLink.path) == 0)
    #expect(throws: QueryProfileError.self) { try home.resolve(nil) }
    unlink(extraLink.path)

    let real = home.directory.appending(path: "real.json")
    #expect(rename(file.path, real.path) == 0)
    #expect(symlink(real.path, file.path) == 0)
    #expect(throws: QueryProfileError.self) { try home.resolve(nil) }
    unlink(file.path)

    try Data(count: 64 * 1024 + 1).write(to: file)
    chmod(file.path, 0o600)
    #expect(throws: QueryProfileError.self) { try home.resolve(nil) }

    try Data().write(to: file)
    chmod(file.path, 0o600)
    #expect(throws: QueryProfileError.self) { try home.resolve(nil) }
  }

  @Test func refusesAnOpenOrSymlinkedConfigurationFolder() throws {
    let home = try TemporaryHome()
    try home.writeProfiles(profiles)
    let folder = home.locations.profilesFile.deletingLastPathComponent()
    chmod(folder.path, 0o755)
    #expect(throws: QueryProfileError.self) { try home.resolve(nil) }
    chmod(folder.path, 0o700)

    let moved = home.directory.appending(path: "elsewhere")
    #expect(rename(folder.path, moved.path) == 0)
    #expect(symlink(moved.path, folder.path) == 0)
    #expect(throws: QueryProfileError.self) { try home.resolve(nil) }
  }

  @Test func environmentPathsMustBeAbsolute() throws {
    let home = URL(fileURLWithPath: "/example/home")
    let defaults = try QueryProfileTarget.locations(environment: [:], home: home)
    #expect(defaults.profilesFile.path == "/example/home/.greenbubbles/query-profiles.json")
    #expect(defaults.settingsFile.path == "/example/home/.greenbubbles/config.toml")
    let custom = try QueryProfileTarget.locations(
      environment: ["GREENBUBBLES_QUERY_PROFILES_FILE": "/elsewhere/p.json"], home: home)
    #expect(custom.profilesFile.path == "/elsewhere/p.json")
    for value in ["relative.json", "/", ""] {
      #expect(throws: QueryProfileError.self) {
        try QueryProfileTarget.locations(
          environment: ["GREENBUBBLES_QUERY_PROFILES_FILE": value], home: home)
      }
      #expect(throws: QueryProfileError.self) {
        try QueryProfileTarget.locations(
          environment: ["GREENBUBBLES_CONFIG_FILE": value], home: home)
      }
    }
  }

  // MARK: config.toml

  @Test func settingsSupplyRootAndKeyWhenNoProfileFileExists() throws {
    let home = try TemporaryHome()
    try home.writeSettings(
      """
      # comment
      [source]
      root = "/example/settings/db_storage"   # trailing comment
      passphrase_file = '/example/keys/settings-key'

      [output]
      format = "json"
      """)
    let target = try home.resolve(nil)
    #expect(target.profileName == nil)
    #expect(target.sourceRoot?.path == "/example/settings/db_storage")
    #expect(target.credentialFile?.path == "/example/keys/settings-key")
    #expect(target.sourceRootOrigin == "from config.toml source.root")
  }

  @Test func profileFileWinsOverSettings() throws {
    let home = try TemporaryHome()
    try home.writeProfiles(profiles)
    try home.writeSettings("[source]\nroot = \"/example/settings/db_storage\"\n")
    #expect(try home.resolve(nil).profileName == "main")
  }

  @Test func settingsDefaultProfileNeedsAProfileFile() throws {
    let home = try TemporaryHome()
    try home.writeSettings("[profile]\ndefault = \"second\"\n")
    #expect(throws: QueryProfileError.self) { try home.resolve(nil) }

    // With the placeholder template present, profile.default is honoured.
    let template = """
      {"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1, "defaultProfile": "live",
       "profiles": {"live": {"sourceRoot": "/ABSOLUTE/PATH/TO/db_storage",
         "access": {"mode": "liveWeChatKeyFile", "credentialFile": "/ABSOLUTE/key"}}}}
      """
    try home.writeProfiles(template)
    #expect(throws: QueryProfileError.unknownProfile("second")) { try home.resolve(nil) }
    try home.writeSettings("[profile]\ndefault = \"live\"\n")
    #expect(try home.resolve(nil).profileName == "live")
  }

  @Test(arguments: [
    "passphrase = \"secret\"\n",
    "[source]\nroot = \"relative/db_storage\"\n",
    "[source]\nroot = \"\"\n",
    "[source]\nkey = \"/x\"\n",
    "[output]\nformat = \"pretty\"\n",
    "[other]\n",
    "[profile]\ndefault = \"../x\"\n",
    "[profile]\ndefault = \"a\"\n[source]\nroot = \"/r\"\n",
    "[source]\nroot = \"/r\"\nroot = \"/s\"\n",
    "source.root = \"/r\"\n",
    "[source]\nroot = \"\"\"/r\"\"\"\n",
    "[source]\npassphrase_file = \"/a/../key\"\n",
    "[source]\nroot = 5\n",
  ])
  func refusesSettingsRustRefusesOrThisReaderCannotRead(text: String) throws {
    let home = try TemporaryHome()
    try home.writeSettings(text)
    #expect(throws: QueryProfileError.self) { try home.resolve(nil) }
  }

  @Test func unsafeSettingsFileIsRefused() throws {
    let home = try TemporaryHome()
    try home.writeSettings("[source]\nroot = \"/r\"\n")
    chmod(home.locations.settingsFile.path, 0o644)
    #expect(throws: QueryProfileError.self) { try home.resolve(nil) }
  }
}

/// A private temporary folder standing in for the user's home.
private final class TemporaryHome {
  let directory: URL
  let locations: QueryProfileTarget.Locations

  init() throws {
    directory = FileManager.default.temporaryDirectory.appending(
      path: "greenbubbles-acquire-profile-tests-\(UUID().uuidString)", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(
      at: directory.appending(path: ".greenbubbles"), withIntermediateDirectories: true,
      attributes: [.posixPermissions: 0o700])
    chmod(directory.path, 0o700)
    locations = try QueryProfileTarget.locations(environment: [:], home: directory)
  }

  deinit {
    let folder = locations.profilesFile.deletingLastPathComponent().path
    chmod(folder, 0o700)
    try? FileManager.default.removeItem(at: directory)
  }

  func writeProfiles(_ text: String) throws { try write(text, to: locations.profilesFile) }
  func writeSettings(_ text: String) throws { try write(text, to: locations.settingsFile) }

  func resolve(_ name: String?, ownerUID: uid_t = getuid()) throws -> AcquireTarget {
    try QueryProfileTarget.resolve(requestedProfile: name, locations: locations, ownerUID: ownerUID)
  }

  private func write(_ text: String, to url: URL) throws {
    unlink(url.path)
    try Data(text.utf8).write(to: url)
    chmod(url.path, 0o600)
  }
}
