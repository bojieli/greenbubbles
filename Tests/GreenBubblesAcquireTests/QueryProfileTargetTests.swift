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

  @Test func dottedCredentialFileIsReadButNotWritten() throws {
    // The reader accepts what Rust accepts (so preflight and verify work);
    // the write-path policy applies to the selected output instead.
    let home = try TemporaryHome()
    try home.writeProfiles(profiles)
    let target = try home.resolve("dotted")
    #expect(target.credentialFile?.path == "/Users/x/../../etc/key")
    #expect(throws: SecretOutputError.dotComponents) {
      try SecretOutputWriter.checkDestination(target.credentialFile!)
    }
    for path in ["/a/./key", "/a/b/../key", "/..", "/"] {
      #expect(throws: SecretOutputError.self) {
        try SecretOutputWriter.checkDestination(URL(fileURLWithPath: path))
      }
    }
    try SecretOutputWriter.checkDestination(URL(fileURLWithPath: "/Users/x/.greenbubbles/key"))
  }

  @Test func explicitRootMustMatchTheConfiguredRootWhoseKeyCaptureWrites() {
    let root = URL(fileURLWithPath: "/example/settings/db_storage")
    let other = URL(fileURLWithPath: "/example/other/db_storage")
    // config.toml with only source.root: queries read the default key file.
    let settingsRootOnly = AcquireTarget(
      sourceRoot: root, sourceRootOrigin: "from config.toml source.root")
    #expect(
      settingsRootOnly.originConflicting(withDatabaseRoot: other)
        == "from config.toml source.root")
    // These made-up paths don't exist, so they get made-up identities here;
    // without that, a root whose identity can't be read always counts as
    // different.
    let madeUp: (URL) -> QueryProfileTarget.FileIdentity? = {
      $0.standardizedFileURL.path == root.path
        ? QueryProfileTarget.FileIdentity(device: 1, inode: 2) : nil
    }
    #expect(
      settingsRootOnly.originConflicting(withDatabaseRoot: root, identify: madeUp) == nil)
    #expect(
      settingsRootOnly.originConflicting(
        withDatabaseRoot: URL(fileURLWithPath: "/example/settings/./db_storage"),
        identify: madeUp) == nil)
    #expect(settingsRootOnly.originConflicting(withDatabaseRoot: root) != nil)
    #expect(settingsRootOnly.originConflicting(withDatabaseRoot: nil) == nil)
    let profile = AcquireTarget(
      profileName: "main", sourceRoot: root,
      credentialFile: URL(fileURLWithPath: "/example/keys/main"),
      sourceRootOrigin: "from query profile main")
    #expect(profile.originConflicting(withDatabaseRoot: other) == "from query profile main")
    // Nothing configured: --db-root is how a root is chosen at all.
    #expect(AcquireTarget.none.originConflicting(withDatabaseRoot: other) == nil)
  }

  @Test func settingsRootOnlyIsAConfiguredRoot() throws {
    let home = try TemporaryHome()
    try home.writeSettings("[source]\nroot = \"/example/settings/db_storage\"\n")
    let target = try home.resolve(nil)
    #expect(target.credentialFile == nil)
    #expect(
      target.originConflicting(withDatabaseRoot: URL(fileURLWithPath: "/example/other")) != nil)
  }

  @Test func configuredKeyFileWithoutADiscoverableRootNeedsExplicitOutputForAnyDbRoot() throws {
    let home = try TemporaryHome()
    try home.writeSettings("[source]\npassphrase_file = \"/example/keys/settings-key\"\n")
    let target = try home.resolve(nil)
    #expect(target.sourceRoot == nil)
    #expect(
      target.sourceRootDiscoveryFailure
        == .liveSourceUnavailable("no live WeChat database was found"))
    #expect(target.credentialFile?.path == "/example/keys/settings-key")
    #expect(
      target.originConflicting(withDatabaseRoot: URL(fileURLWithPath: "/example/any")) != nil)
    #expect(target.originConflicting(withDatabaseRoot: nil) == nil)
  }

  // MARK: Discovery for a configured key file (Rust's discover_default_live_source_root)

  @Test func configuredKeyFileUsesTheRootQueriesDiscover() throws {
    let home = try TemporaryHome()
    try home.writeSettings("[source]\npassphrase_file = \"/example/keys/settings-key\"\n")
    let live = try home.makeAccount("live", ageDays: 0)
    _ = try home.makeAccount("stale", ageDays: 15)
    let target = try home.resolve(nil)
    #expect(target.sourceRoot?.path == live.path)
    #expect(target.sourceRootDiscovered)
    #expect(target.sourceRootDiscoveryFailure == nil)
    #expect(target.credentialFile?.path == "/example/keys/settings-key")
    // A query may discover under a different $HOME, so any --db-root still
    // needs an explicit --output.
    #expect(target.originConflicting(withDatabaseRoot: live) != nil)
    #expect(target.originConflicting(withDatabaseRoot: nil) == nil)
  }

  @Test func configuredKeyFileFromAnOverriddenSettingsFileIsNotGuessed() throws {
    let home = try TemporaryHome()
    _ = try home.makeAccount("live", ageDays: 0)
    let settings = home.directory.appending(path: ".greenbubbles/elsewhere.toml")
    try Data("[source]\npassphrase_file = \"/example/keys/k\"\n".utf8).write(to: settings)
    chmod(settings.path, 0o600)
    let locations = try QueryProfileTarget.locations(
      environment: [QueryProfileTarget.settingsEnvironmentVariable: settings.path],
      home: home.directory)
    let target = try QueryProfileTarget.resolve(
      requestedProfile: nil, locations: locations, ownerUID: getuid())
    #expect(target.sourceRoot == nil)
    #expect(target.sourceRootDiscoveryFailure != nil)
    #expect(target.credentialFile?.path == "/example/keys/k")
  }

  @Test func discoveryRefusesTwoAccountsWrittenWithinFourteenDays() throws {
    let home = try TemporaryHome()
    _ = try home.makeAccount("a", ageDays: 0)
    _ = try home.makeAccount("b", ageDays: 13)
    #expect(throws: QueryProfileError.self) { try home.discover() }
    // A group container whose name contains "wechat" is a candidate too.
    let home2 = try TemporaryHome()
    let group = try home2.makeAccount(
      "acct", ageDays: 0,
      filesRoot: home2.directory.appending(
        path: "Library/Group Containers/5A4RE8SF68.com.tencent.XinWeChat/xwechat_files"))
    #expect(try home2.discover().path == group.path)
    _ = try home2.makeAccount("other", ageDays: 1)
    #expect(throws: QueryProfileError.self) { try home2.discover() }
  }

  @Test func discoverySkipsRootsRustSkips() throws {
    let home = try TemporaryHome()
    let live = try home.makeAccount("live", ageDays: 20)
    // Newer, but each unusable: incomplete, no regular files, a symbolic
    // link, owned by someone else, or in a group container without "wechat".
    let incomplete = home.filesRoot.appending(path: "incomplete/db_storage/contact")
    try FileManager.default.createDirectory(at: incomplete, withIntermediateDirectories: true)
    try Data().write(to: incomplete.appending(path: "c.db"))
    let empty = home.filesRoot.appending(path: "empty/db_storage")
    for component in ["contact", "session", "message"] {
      try FileManager.default.createDirectory(
        at: empty.appending(path: component), withIntermediateDirectories: true)
    }
    try FileManager.default.createDirectory(
      at: home.filesRoot.appending(path: "linked"), withIntermediateDirectories: true)
    let elsewhere = try home.makeAccount(
      "real", ageDays: 0, filesRoot: home.directory.appending(path: "elsewhere"))
    #expect(
      symlink(elsewhere.path, home.filesRoot.appending(path: "linked/db_storage").path) == 0)
    _ = try home.makeAccount(
      "acct", ageDays: 0,
      filesRoot: home.directory.appending(
        path: "Library/Group Containers/group.example.other/xwechat_files"))
    #expect(try home.discover().path == live.path)
    #expect(throws: QueryProfileError.self) { try home.discover(ownerUID: getuid() &+ 1) }
  }

  @Test func rootsAreComparedByIdentityNotSpelling() throws {
    let home = try TemporaryHome()
    // "é" composed (NFC) and decomposed (NFD): different strings to Swift's
    // `==` only after normalization, and different paths to the kernel on a
    // normalization-sensitive volume.
    let composed = home.directory.appending(path: "caf\u{E9}/db_storage")
    try FileManager.default.createDirectory(at: composed, withIntermediateDirectories: true)
    let decomposed = URL(fileURLWithPath: home.directory.path + "/cafe\u{301}/db_storage")
    let target = AcquireTarget(
      profileName: "a", sourceRoot: composed,
      credentialFile: URL(fileURLWithPath: "/example/keys/a"),
      sourceRootOrigin: "from query profile a")
    let identity = QueryProfileTarget.fileIdentity(composed)
    #expect(identity != nil)
    // The decomposed spelling conflicts unless it is the same directory.
    let sameDirectory = QueryProfileTarget.fileIdentity(decomposed) == identity
    #expect((target.originConflicting(withDatabaseRoot: decomposed) == nil) == sameDirectory)
    // A failed lookup on either side is a conflict.
    let missing = home.directory.appending(path: "missing")
    #expect(target.originConflicting(withDatabaseRoot: missing) != nil)
    let missingSource = AcquireTarget(
      profileName: "b", sourceRoot: missing, credentialFile: URL(fileURLWithPath: "/k"),
      sourceRootOrigin: "from query profile b")
    #expect(missingSource.originConflicting(withDatabaseRoot: missing) != nil)
  }

  @Test func tomlWhitespaceIsOnlySpaceAndTab() throws {
    let home = try TemporaryHome()
    try home.writeSettings("\t[source]  \n \troot\t=  \"/example/tabs\"\t# ok\n")
    #expect(try home.resolve(nil).sourceRoot?.path == "/example/tabs")
    for text in [
      "\u{00A0}[source]\nroot = \"/example/x\"\n",
      "[source]\n\u{00A0}root = \"/example/x\"\n",
      "[source]\nroot\u{00A0}= \"/example/x\"\n",
      "[\u{2003}source]\nroot = \"/example/x\"\n",
      "[source]\nroot = \"/example/x\"\u{00A0}# c\n",
      "\u{3000}# comment\n",
    ] {
      try home.writeSettings(text)
      #expect(throws: QueryProfileError.self) { try home.resolve(nil) }
    }
  }

  @Test func byteOrderMarkMatchesRust() throws {
    let bom = Data([0xEF, 0xBB, 0xBF])
    // serde_json::from_slice rejects a BOM and non-UTF-8 encodings.
    let json = Data(
      #"{"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1, "profiles": {"a": {"sourceRoot": "/s", "access": {"mode": "decrypted"}}}}"#
        .utf8)
    _ = try QueryProfileTarget.ProfileStore.parse(json)
    #expect(throws: QueryProfileError.self) {
      try QueryProfileTarget.ProfileStore.parse(bom + json)
    }
    let utf16 = String(decoding: json, as: UTF8.self).data(using: .utf16)!
    #expect(throws: QueryProfileError.self) { try QueryProfileTarget.ProfileStore.parse(utf16) }

    // The toml crate's lexer skips exactly one leading BOM.
    let toml = Data("[source]\nroot = \"/example/bom\"\n".utf8)
    #expect(try QueryProfileTarget.Settings.parse(bom + toml).sourceRoot == "/example/bom")
    #expect(throws: QueryProfileError.self) {
      try QueryProfileTarget.Settings.parse(bom + bom + toml)
    }
    #expect(throws: QueryProfileError.self) {
      try QueryProfileTarget.Settings.parse(Data([0x23, 0xFF, 0x0A]))
    }
  }

  @Test func rootEquivalentPathsAreRefused() throws {
    for path in ["/", "/.", "//", "/./.", "/..", "/a/..", "/./../", "", "relative", "/a\0b"] {
      #expect(!QueryProfileTarget.isAbsoluteNonRoot(path), "\(path)")
    }
    for path in ["/a", "/a/../b", "//a", "/./a/."] {
      #expect(QueryProfileTarget.isAbsoluteNonRoot(path), "\(path)")
    }
    for root in ["/", "/.", "//", "/./.", "/.."] {
      let json = """
        {"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1, "defaultProfile": "a",
         "profiles": {"a": {"sourceRoot": "\(root)",
           "access": {"mode": "liveWeChatKeyFile", "credentialFile": "/k/key"}}}}
        """
      #expect(throws: QueryProfileError.self) {
        try QueryProfileTarget.ProfileStore.parse(Data(json.utf8))
      }
      #expect(throws: QueryProfileError.self) {
        try QueryProfileTarget.Settings.parse(Data("[source]\nroot = \"\(root)\"\n".utf8))
      }
      #expect(throws: QueryProfileError.self) {
        try QueryProfileTarget.locations(
          environment: ["GREENBUBBLES_QUERY_PROFILES_FILE": root],
          home: URL(fileURLWithPath: "/example"))
      }
    }
  }

  @Test(arguments: [
    "[source]\nroot = \"/a\u{1B}b\"\n",  // raw ESC in a basic string
    "[source]\nroot = \"/a\u{7F}b\"\n",  // raw DEL
    "[source]\nroot = \"/a\u{01}b\"\n",
    "[source]\nroot = \"/a\u{00}b\"\n",
    "[source]\nroot = '/a\u{1B}b'\n",  // raw ESC in a literal string
    "[source]\nroot = '/a\u{7F}b'\n",
    "[source]\nroot = '/a\u{0C}b'\n",
    "# comment with \u{1B} escape\n",
    "[source] # \u{7F}\n",
    "[source]\nroot = \"/a\" # \u{01}\n",
    "[source]\nroot = \"/a\\x4\"\n",  // short \x escape
    "[source]\nroot = \"/a\\u+041\"\n",  // sign is not a hex digit
    "[source]\nroot = \"/a\\uD800\"\n",  // surrogate is not a scalar
    "[source]\nroot = \"/a\\q\"\n",
    "[source]\nroot = \"/a\"\u{301}\n",  // combining mark after the closing quote
  ])
  func tomlControlCharactersAreRefusedBeforeSelection(text: String) throws {
    #expect(throws: QueryProfileError.self) {
      try QueryProfileTarget.Settings.parse(Data(text.utf8))
    }
    let home = try TemporaryHome()
    try home.writeSettings(text)
    #expect(throws: QueryProfileError.self) { try home.resolve(nil) }
  }

  @Test func tomlStringsAcceptWhatTheTomlCrateAccepts() throws {
    let parsed = try QueryProfileTarget.Settings.parse(
      Data(
        "[source]\nroot = \"/a\tb\\x41\\u0042\\U00000043\\e\\\"\"\npassphrase_file = '/k\tey\\n' # é\n"
          .utf8))
    #expect(parsed.sourceRoot == "/a\tbABC\u{1B}\"")
    #expect(parsed.passphraseFile == "/k\tey\\n")
  }

  @Test func jsonStringRulesMatchSerde() {
    let base =
      #"{"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1, "profiles": {"a": {"sourceRoot": "SOURCE", "access": {"mode": "decrypted"}}}}"#
    let loneSurrogate = base.replacingOccurrences(of: "SOURCE", with: #"/a\uD800"#)
    #expect(throws: QueryProfileError.self) {
      try QueryProfileTarget.ProfileStore.parse(Data(loneSurrogate.utf8))
    }
    let pair = base.replacingOccurrences(of: "SOURCE", with: #"/a😀"#)
    #expect(throws: Never.self) { try QueryProfileTarget.ProfileStore.parse(Data(pair.utf8)) }
    let deep = String(repeating: "[", count: 200) + String(repeating: "]", count: 200)
    #expect(JSONDuplicateKeyCheck.problem(in: Data(deep.utf8)) == .malformed)
    #expect(JSONDuplicateKeyCheck.problem(in: Data("{\"a\u{01}\": 1}".utf8)) == .malformed)
  }

  @Test func parentComponentsAreRefusedInDatabaseRoots() throws {
    #expect(QueryProfileTarget.hasParentComponent("/a/../b"))
    #expect(QueryProfileTarget.hasParentComponent("/a/b/.."))
    #expect(!QueryProfileTarget.hasParentComponent("/a/..b/c."))
    let json =
      #"{"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1, "profiles": {"a": {"sourceRoot": "/x/link/../db_storage", "access": {"mode": "liveWeChatKeyFile", "credentialFile": "/k/key"}}}}"#
    #expect(throws: QueryProfileError.self) {
      try QueryProfileTarget.ProfileStore.parse(Data(json.utf8))
    }
    #expect(throws: QueryProfileError.self) {
      try QueryProfileTarget.Settings.parse(Data("[source]\nroot = \"/x/link/../db\"\n".utf8))
    }
  }

  @Test func conflictCheckComparesRootsThroughTheFilesystem() throws {
    // accounts/A/db_storage is the configured account. "link" points into
    // A, so "link/../db_storage" is A's root to the kernel, while a lexical
    // comparison would see the decoy "db_storage" next to the link.
    let home = try TemporaryHome()
    let base = home.directory
    let accountA = base.appending(path: "accounts/A/db_storage")
    let inner = base.appending(path: "accounts/A/inner")
    let decoy = base.appending(path: "db_storage")
    for folder in [accountA, inner, decoy] {
      try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    }
    let link = base.appending(path: "link")
    #expect(symlink(inner.path, link.path) == 0)
    let alias = base.appending(path: "alias")
    #expect(symlink(accountA.path, alias.path) == 0)

    let target = AcquireTarget(
      profileName: "a", sourceRoot: accountA,
      credentialFile: URL(fileURLWithPath: "/example/keys/a"),
      sourceRootOrigin: "from query profile a")
    let throughLink = URL(fileURLWithPath: link.path + "/../db_storage")
    #expect(target.originConflicting(withDatabaseRoot: throughLink) == nil)
    #expect(target.originConflicting(withDatabaseRoot: alias) == nil)
    #expect(target.originConflicting(withDatabaseRoot: decoy) == "from query profile a")
    // A root that can't be resolved is treated as a different account.
    #expect(
      target.originConflicting(withDatabaseRoot: base.appending(path: "missing")) != nil)
    #expect(
      QueryProfileTarget.canonicalPath(throughLink)?.path
        == QueryProfileTarget.canonicalPath(accountA)?.path)
    #expect(
      QueryProfileTarget.fileIdentity(throughLink) == QueryProfileTarget.fileIdentity(accountA))
  }

  @Test func jsonMustBeASingleUTF8Value() throws {
    let json =
      #"{"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1, "profiles": {"a": {"sourceRoot": "/s", "access": {"mode": "decrypted"}}}}"#
    let utf16BigEndian = json.data(using: .utf16BigEndian)!
    #expect(!utf16BigEndian.starts(with: [0xFE, 0xFF]))  // BOM-less fixture
    #expect(throws: QueryProfileError.self) {
      try QueryProfileTarget.ProfileStore.parse(utf16BigEndian)
    }
    let utf16LittleEndian = json.data(using: .utf16LittleEndian)!
    #expect(throws: QueryProfileError.self) {
      try QueryProfileTarget.ProfileStore.parse(utf16LittleEndian)
    }
    #expect(JSONDuplicateKeyCheck.problem(in: Data((json + " \n").utf8)) == nil)
    #expect(JSONDuplicateKeyCheck.problem(in: Data((json + " {}").utf8)) == .malformed)
    #expect(JSONDuplicateKeyCheck.problem(in: Data((json + "x").utf8)) == .malformed)
  }

  @Test func liveSourceRootChecksMatchRustOnThePathAsGiven() throws {
    let home = try TemporaryHome()
    let root = home.directory.appending(path: "account/db_storage")
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    try QueryProfileTarget.checkLiveSourceRoot(root, ownerUID: getuid())

    // A symbolic link to a valid root is refused even though it resolves.
    let link = home.directory.appending(path: "linked-db_storage")
    #expect(symlink(root.path, link.path) == 0)
    #expect(throws: QueryProfileError.self) {
      try QueryProfileTarget.checkLiveSourceRoot(link, ownerUID: getuid())
    }
    // Owned by someone else, missing, or not a directory.
    #expect(throws: QueryProfileError.self) {
      try QueryProfileTarget.checkLiveSourceRoot(root, ownerUID: getuid() &+ 1)
    }
    #expect(throws: QueryProfileError.self) {
      try QueryProfileTarget.checkLiveSourceRoot(
        home.directory.appending(path: "missing"), ownerUID: getuid())
    }
    let file = home.directory.appending(path: "file")
    try Data("x".utf8).write(to: file)
    #expect(throws: QueryProfileError.self) {
      try QueryProfileTarget.checkLiveSourceRoot(file, ownerUID: getuid())
    }
  }

  @Test func duplicateFieldsAreRefusedLikeRust() throws {
    let duplicates = [
      #"{"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1, "profiles": {"a": {"sourceRoot": "/s", "access": {"mode": "liveWeChatKeyFile", "credentialFile": "/home/me/key", "credentialFile": "/etc/key"}}}}"#,
      #"{"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1, "profiles": {"a": {"sourceRoot": "/s", "access": {"mode": "liveWeChatKeyFile", "credentialFile": "/home/me/key", "\u0063redentialFile": "/etc/key"}}}}"#,
      #"{"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1, "formatVersion": 1, "profiles": {"a": {"sourceRoot": "/s", "access": {"mode": "decrypted"}}}}"#,
      #"{"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1, "profiles": {"a": {"sourceRoot": "/s", "sourceRoot": "/t", "access": {"mode": "decrypted"}}}}"#,
      #"{"schema": "greenbubbles.query-profiles.v1", "formatVersion": 1, "profiles": {"a": {"sourceRoot": "/s", "access": {"mode": "decrypted"}}, "a": {"sourceRoot": "/t", "access": {"mode": "decrypted"}}}}"#,
    ]
    for json in duplicates {
      #expect(throws: QueryProfileError.self) {
        try QueryProfileTarget.ProfileStore.parse(Data(json.utf8))
      }
    }
    let distinct =
      #"{"a": {"k": 1}, "b": {"k": [1, "x\"y", {"k": null}]}, "\uD83D\uDE00": true, "😀x": 2}"#
    #expect(JSONDuplicateKeyCheck.firstDuplicateKey(in: Data(distinct.utf8)) == nil)
    let surrogateDuplicate = #"{"\uD83D\uDE00": 1, "😀": 2}"#
    #expect(JSONDuplicateKeyCheck.firstDuplicateKey(in: Data(surrogateDuplicate.utf8)) != nil)
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
    #expect(try defaults.settingsFile().path == "/example/home/.greenbubbles/config.toml")
    let custom = try QueryProfileTarget.locations(
      environment: ["GREENBUBBLES_QUERY_PROFILES_FILE": "/elsewhere/p.json"], home: home)
    #expect(custom.profilesFile.path == "/elsewhere/p.json")
    for value in ["relative.json", "/", ""] {
      #expect(throws: QueryProfileError.self) {
        try QueryProfileTarget.locations(
          environment: ["GREENBUBBLES_QUERY_PROFILES_FILE": value], home: home)
      }
      // Checked only when config.toml is consulted, as in Rust.
      let settingsOverride = try QueryProfileTarget.locations(
        environment: ["GREENBUBBLES_CONFIG_FILE": value], home: home)
      #expect(throws: QueryProfileError.self) { try settingsOverride.settingsFile() }
    }
  }

  @Test func badSettingsOverrideDoesNotBreakASelectedProfile() throws {
    let home = try TemporaryHome()
    try home.writeProfiles(profiles)
    for value in ["", "relative.toml", "/"] {
      let locations = try QueryProfileTarget.locations(
        environment: ["GREENBUBBLES_CONFIG_FILE": value], home: home.directory)
      let target = try QueryProfileTarget.resolve(
        requestedProfile: nil, locations: locations, ownerUID: getuid())
      #expect(target.profileName == "main")
      // Without a profile file, the override is consulted and must be valid.
      let noProfiles = try QueryProfileTarget.locations(
        environment: [
          "GREENBUBBLES_CONFIG_FILE": value,
          "GREENBUBBLES_QUERY_PROFILES_FILE": home.directory.appending(path: "none.json").path,
        ], home: home.directory)
      #expect(throws: QueryProfileError.self) {
        try QueryProfileTarget.resolve(
          requestedProfile: nil, locations: noProfiles, ownerUID: getuid())
      }
    }
  }

  @Test func missingSettingsFileIsAbsentEvenInAnOpenFolder() throws {
    let home = try TemporaryHome()
    let open = home.directory.appending(path: "open", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: open, withIntermediateDirectories: false)
    chmod(open.path, 0o755)
    let locations = try QueryProfileTarget.locations(
      environment: ["GREENBUBBLES_CONFIG_FILE": open.appending(path: "config.toml").path],
      home: home.directory)
    #expect(
      try QueryProfileTarget.resolve(
        requestedProfile: nil, locations: locations, ownerUID: getuid())
        == .none)
  }

  @Test func unreadableSettingsFileIsAnErrorNotAbsent() throws {
    guard geteuid() != 0 else { return }
    let home = try TemporaryHome()
    let locked = home.directory.appending(path: "locked", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: locked, withIntermediateDirectories: false)
    let settings = locked.appending(path: "config.toml")
    try Data("[source]\nroot = \"/r\"\n".utf8).write(to: settings)
    chmod(settings.path, 0o600)
    chmod(locked.path, 0o000)
    defer { chmod(locked.path, 0o700) }
    let locations = try QueryProfileTarget.locations(
      environment: ["GREENBUBBLES_CONFIG_FILE": settings.path], home: home.directory)
    #expect(throws: QueryProfileError.self) {
      try QueryProfileTarget.resolve(
        requestedProfile: nil, locations: locations, ownerUID: getuid())
    }
  }

  @Test func crlfSettingsAreReadLikeRust() throws {
    let home = try TemporaryHome()
    try home.writeSettings(
      "# leading comment\r\n[source]\r\nroot = \"/example/crlf/db_storage\"\r\n"
        + "passphrase_file = \"/example/keys/crlf-key\" # note\r\n")
    let target = try home.resolve(nil)
    #expect(target.sourceRoot?.path == "/example/crlf/db_storage")
    #expect(target.credentialFile?.path == "/example/keys/crlf-key")

    try home.writeSettings("[source]\r\npassphrase_file = \"/example/keys/k\"\r\n")
    #expect(try home.resolve(nil).credentialFile?.path == "/example/keys/k")

    // A lone carriage return is not a TOML line ending.
    try home.writeSettings("# comment\r[source]\nroot = \"/example/x\"\n")
    #expect(throws: QueryProfileError.self) { try home.resolve(nil) }
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
    chmod(try home.locations.settingsFile().path, 0o644)
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
  func writeSettings(_ text: String) throws { try write(text, to: locations.settingsFile()) }

  func resolve(_ name: String?, ownerUID: uid_t = getuid()) throws -> AcquireTarget {
    try QueryProfileTarget.resolve(requestedProfile: name, locations: locations, ownerUID: ownerUID)
  }

  func discover(ownerUID: uid_t = getuid()) throws -> URL {
    try QueryProfileTarget.discoverDefaultLiveSourceRoot(home: directory, ownerUID: ownerUID)
  }

  /// The app container's xwechat_files, where Rust looks first.
  var filesRoot: URL {
    directory.appending(
      path: "Library/Containers/com.tencent.xinWeChat/Data/Documents/xwechat_files")
  }

  /// A made-up account whose database files were last written `ageDays` ago.
  func makeAccount(_ name: String, ageDays: Int, filesRoot: URL? = nil) throws -> URL {
    let root = (filesRoot ?? self.filesRoot).appending(path: name).appending(path: "db_storage")
    let modified = Date().addingTimeInterval(-Double(ageDays) * 24 * 60 * 60)
    for component in ["contact", "session", "message"] {
      let folder = root.appending(path: component)
      try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
      let file = folder.appending(path: component + ".db")
      try Data("x".utf8).write(to: file)
      try FileManager.default.setAttributes([.modificationDate: modified], ofItemAtPath: file.path)
    }
    // Folder timestamps don't count; make them look fresh.
    try FileManager.default.setAttributes([.modificationDate: Date()], ofItemAtPath: root.path)
    return root
  }

  private func write(_ text: String, to url: URL) throws {
    unlink(url.path)
    try Data(text.utf8).write(to: url)
    chmod(url.path, 0o600)
  }
}
