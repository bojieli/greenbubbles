import Darwin
import Foundation

public enum QueryProfileError: Error, Equatable, CustomStringConvertible {
  case missingFile
  case unsafeFile(String)
  case malformed(String)
  case invalidSettings(String)
  case unknownProfile(String)
  case noDefaultProfile
  case notLiveProfile(name: String, mode: String)
  case invalidEnvironment(String)
  case unsafeSourceRoot(String)

  public var description: String {
    switch self {
    case .missingFile:
      return "--profile was given, but no query profile file exists"
    case .unsafeFile(let reason):
      return "A GreenBubbles configuration file is not safe to read: \(reason)"
    case .malformed(let reason):
      return "The query profile file is not valid greenbubbles.query-profiles.v1: \(reason)"
    case .invalidSettings(let reason):
      return "~/.greenbubbles/config.toml is not valid: \(reason)"
    case .unknownProfile(let name):
      return "No query profile is named \(name)"
    case .noDefaultProfile:
      return "The query profile file has no defaultProfile; select one with --profile"
    case .notLiveProfile(let name, let mode):
      return
        "Query profile \(name) uses \(mode) access, not liveWeChatKeyFile; pass --profile "
        + "with a live profile, or give both --db-root and --output"
    case .unsafeSourceRoot(let reason):
      return "The database root is not usable: \(reason)"
    case .invalidEnvironment(let variable):
      return "\(variable) must be an absolute, non-root path"
    }
  }
}

/// Where `greenbubbles-acquire` reads the key's data folder and writes the key,
/// resolved the way the Rust CLI resolves an ordinary query, so that the key
/// lands in the file queries will read.
public struct AcquireTarget: Equatable, Sendable {
  /// The query profile in use, if any.
  public let profileName: String?
  /// The `db_storage` root; nil means auto-discovery.
  public let sourceRoot: URL?
  /// Where queries read the key; nil means `~/.greenbubbles-acquire/passphrase.txt`.
  public let credentialFile: URL?
  /// How `sourceRoot` was chosen, for the preflight line.
  public let sourceRootOrigin: String?

  public init(
    profileName: String? = nil,
    sourceRoot: URL? = nil,
    credentialFile: URL? = nil,
    sourceRootOrigin: String? = nil
  ) {
    self.profileName = profileName
    self.sourceRoot = sourceRoot
    self.credentialFile = credentialFile
    self.sourceRootOrigin = sourceRootOrigin
  }

  public static let none = AcquireTarget()

  /// When capture would write to the key file queries read for this target
  /// (its configured file, or the default file when only a root is
  /// configured), an explicit database root for a different account would
  /// put the wrong key there. Returns how this target's root is chosen when
  /// `databaseRoot` differs from it — or may differ, when a key file is
  /// configured but its root is auto-discovered — else nil. The caller then
  /// requires an explicit `--output` (fail closed).
  ///
  /// Roots are compared after `canonicalize` (realpath, run as the invoking
  /// user), so symbolic links can't make two spellings name different
  /// accounts; a root that can't be resolved counts as different.
  public func originConflicting(
    withDatabaseRoot databaseRoot: URL?,
    canonicalize: (URL) -> URL? = QueryProfileTarget.canonicalPath
  ) -> String? {
    guard let databaseRoot else { return nil }
    guard let sourceRoot else {
      return credentialFile == nil ? nil : "auto-discovered for the configured key file"
    }
    if let resolvedOverride = canonicalize(databaseRoot),
      let resolvedSource = canonicalize(sourceRoot),
      resolvedOverride.path == resolvedSource.path
    {
      return nil
    }
    return sourceRootOrigin ?? "configured"
  }
}

/// Mirrors the Rust CLI (`Native/GreenBubbles/src/query_profile.rs` and
/// `load_configured_query_invocation` in `main.rs`):
///
/// 1. When `query-profiles.json` exists it is the only source: `--profile` or
///    its `defaultProfile` (an error when absent). A file whose single profile
///    still has the `/ABSOLUTE/...` placeholder counts as no file unless
///    `--profile` names it.
/// 2. Otherwise `config.toml` applies: `profile.default` (which needs a
///    profile file), or `source.root` and `source.passphrase_file`.
/// 3. Otherwise auto-discovery and the default key path.
///
/// The format is owned by the Rust CLI; this reader is equally strict
/// (schema, `formatVersion` 1, no unknown fields, profile-name rules, file
/// safety) and refuses rather than guesses.
public enum QueryProfileTarget {
  public static let schema = "greenbubbles.query-profiles.v1"
  public static let formatVersion = 1
  public static let liveMode = "liveWeChatKeyFile"
  public static let profilesEnvironmentVariable = "GREENBUBBLES_QUERY_PROFILES_FILE"
  public static let settingsEnvironmentVariable = "GREENBUBBLES_CONFIG_FILE"
  static let maximumProfileCount = 64
  static let maximumProfileNameBytes = 64
  static let fileModes: Set<String> = [
    "liveWeChatKeyFile", "snapshotLocalCredential", "snapshotRecoveryKit",
    "snapshotPassphraseFile", "snapshotRawKeyFile",
  ]

  public struct Locations: Equatable, Sendable {
    public let profilesFile: URL
    let settingsOverride: String?
    let home: URL

    /// `config.toml`. Like the Rust CLI, `GREENBUBBLES_CONFIG_FILE` is
    /// checked only when the settings file is actually consulted, so a bad
    /// value never breaks a selected profile.
    public func settingsFile() throws -> URL {
      try QueryProfileTarget.location(
        override: settingsOverride, variable: settingsEnvironmentVariable,
        home: home, fileName: "config.toml")
    }
  }

  /// Applies the Rust location rules: the environment variables win and must
  /// be absolute non-root paths; otherwise `~/.greenbubbles/...`.
  public static func locations(environment: [String: String], home: URL) throws -> Locations {
    Locations(
      profilesFile: try location(
        override: environment[profilesEnvironmentVariable], variable: profilesEnvironmentVariable,
        home: home, fileName: "query-profiles.json"),
      settingsOverride: environment[settingsEnvironmentVariable],
      home: home
    )
  }

  static func location(override: String?, variable: String, home: URL, fileName: String) throws
    -> URL
  {
    if let value = override {
      guard isAbsoluteNonRoot(value) else {
        throw QueryProfileError.invalidEnvironment(variable)
      }
      return URL(fileURLWithPath: value)
    }
    return home.appending(path: ".greenbubbles").appending(path: fileName)
  }

  /// Resolves the target. Run inside `InvokingUser.perform(_:)` with
  /// `ownerUID` set to that user, so both files are opened with the user's
  /// rights and must belong to the user.
  public static func resolve(
    requestedProfile: String?,
    locations: Locations,
    ownerUID: uid_t
  ) throws -> AcquireTarget {
    if let requestedProfile { try validateProfileName(requestedProfile) }
    switch try PrivateConfigurationFile.read(
      locations.profilesFile, ownerUID: ownerUID, description: "query profile file")
    {
    case .contents(let data):
      let store = try ProfileStore.parse(data)
      let (name, profile) = try store.select(requestedProfile)
      // An untouched template still has the printed placeholder path.
      if requestedProfile == nil, store.profiles.count == 1, profile.usesPlaceholderSource {
        return try implicitTarget(locations: locations, ownerUID: ownerUID)
      }
      return try target(for: profile, named: name)
    case .absent:
      if requestedProfile != nil { throw QueryProfileError.missingFile }
      return try implicitTarget(locations: locations, ownerUID: ownerUID)
    }
  }

  private static func implicitTarget(locations: Locations, ownerUID: uid_t) throws -> AcquireTarget
  {
    // Rust's load_query_settings: only a missing file means "no settings";
    // any other failure to inspect it is an error, checked before the
    // folder rules so a missing file in any folder is simply absent.
    let settingsFile = try locations.settingsFile()
    var info = stat()
    if lstat(settingsFile.path, &info) != 0 {
      if errno == ENOENT { return .none }
      throw QueryProfileError.unsafeFile("the settings file (config.toml) is unavailable")
    }
    guard
      case .contents(let data) = try PrivateConfigurationFile.read(
        settingsFile, ownerUID: ownerUID, description: "settings file (config.toml)")
    else {
      throw QueryProfileError.unsafeFile("the settings file (config.toml) is unavailable")
    }
    let settings = try Settings.parse(data)
    if let profileName = settings.defaultProfile {
      guard settings.sourceRoot == nil, settings.passphraseFile == nil else {
        throw QueryProfileError.invalidSettings(
          "profile.default cannot be combined with source.root or source.passphrase_file")
      }
      guard
        case .contents(let data) = try PrivateConfigurationFile.read(
          locations.profilesFile, ownerUID: ownerUID, description: "query profile file")
      else {
        throw QueryProfileError.invalidSettings(
          "profile.default names \(profileName), but no query profile file exists")
      }
      let store = try ProfileStore.parse(data)
      let (name, profile) = try store.select(profileName)
      return try target(for: profile, named: name)
    }
    return AcquireTarget(
      sourceRoot: settings.sourceRoot.map { URL(fileURLWithPath: $0) },
      credentialFile: settings.passphraseFile.map { URL(fileURLWithPath: $0) },
      sourceRootOrigin: settings.sourceRoot == nil ? nil : "from config.toml source.root"
    )
  }

  private static func target(for profile: ProfileStore.Profile, named name: String) throws
    -> AcquireTarget
  {
    guard profile.mode == liveMode, let credential = profile.credentialFile else {
      throw QueryProfileError.notLiveProfile(name: name, mode: profile.mode)
    }
    return AcquireTarget(
      profileName: name,
      sourceRoot: URL(fileURLWithPath: profile.sourceRoot),
      credentialFile: URL(fileURLWithPath: credential),
      sourceRootOrigin: "from query profile \(name)"
    )
  }

  /// Rust's `validate_absolute_non_root_path` requires an absolute path with
  /// a parent. Rust drops `.` and repeated `/` when finding the parent, so
  /// "/", "//", and "/." have none; this also rejects any path that resolves
  /// lexically to the root, such as "/.." or "/a/..", which would make a
  /// database root walk the whole filesystem.
  static func isAbsoluteNonRoot(_ path: String) -> Bool {
    guard path.hasPrefix("/"), !path.contains("\0") else { return false }
    var depth = 0
    for component in path.split(separator: "/", omittingEmptySubsequences: true) {
      switch component {
      case ".": continue
      case "..": depth = max(depth - 1, 0)
      default: depth += 1
      }
    }
    return depth > 0
  }

  /// The path with every symbolic link, `.`, and `..` resolved by the
  /// filesystem (realpath), or nil when it doesn't exist or can't be read.
  /// Call inside `InvokingUser.perform(_:)` so it resolves with the user's
  /// rights.
  public static func canonicalPath(_ url: URL) -> URL? {
    guard let resolved = realpath(url.path, nil) else { return nil }
    defer { free(resolved) }
    return URL(fileURLWithPath: String(cString: resolved), isDirectory: true)
  }

  /// The checks Rust's `LiveQuerySource::open` applies to the root exactly as
  /// given, before canonicalising it: `symlink_metadata` must succeed, the
  /// path must be a real directory (not a symbolic link), and it must be
  /// owned by the user running the query. Call inside
  /// `InvokingUser.perform(_:)` with that user's uid.
  public static func checkLiveSourceRoot(_ url: URL, ownerUID: uid_t) throws {
    var info = stat()
    guard lstat(url.path, &info) == 0 else {
      throw QueryProfileError.unsafeSourceRoot("database root is unavailable")
    }
    guard info.st_mode & S_IFMT == S_IFDIR else {
      throw QueryProfileError.unsafeSourceRoot(
        "database root must be a real directory, not a symbolic link")
    }
    guard info.st_uid == ownerUID else {
      throw QueryProfileError.unsafeSourceRoot("database root must be owned by the current user")
    }
  }

  /// A database root may not contain `..`: Rust resolves it through the
  /// filesystem while URL standardisation would drop it lexically, and the
  /// two can name different accounts behind a symbolic link.
  public static func hasParentComponent(_ path: String) -> Bool {
    path.split(separator: "/").contains("..")
  }

  static func validateProfileName(_ name: String) throws {
    let bytes = Array(name.utf8)
    let allowed = bytes.allSatisfy { byte in
      (byte >= 0x30 && byte <= 0x39) || (byte >= 0x41 && byte <= 0x5A)
        || (byte >= 0x61 && byte <= 0x7A) || byte == 0x2E || byte == 0x5F || byte == 0x2D
    }
    guard !bytes.isEmpty, bytes.count <= maximumProfileNameBytes, name != ".", name != "..",
      allowed
    else {
      throw QueryProfileError.malformed(
        "profile names use 1..64 ASCII letters, digits, '.', '_', or '-'")
    }
  }

  // MARK: - query-profiles.json

  struct ProfileStore: Equatable {
    struct Profile: Equatable {
      let sourceRoot: String
      let mode: String
      let credentialFile: String?

      var usesPlaceholderSource: Bool {
        sourceRoot.split(separator: "/").contains("ABSOLUTE")
      }
    }

    let defaultProfile: String?
    let profiles: [String: Profile]

    static func parse(_ data: Data) throws -> ProfileStore {
      // serde_json::from_slice reads UTF-8 only and rejects a byte-order
      // mark; JSONSerialization would accept both, so check first.
      // A NUL byte is never valid JSON text; rejecting it also stops
      // Foundation from detecting BOM-less UTF-16 or UTF-32.
      let bytes = [UInt8](data)
      guard !bytes.starts(with: utf8ByteOrderMark), !bytes.contains(0), strictUTF8(bytes) != nil
      else {
        throw QueryProfileError.malformed("the file must be UTF-8 JSON without a byte-order mark")
      }
      let object: Any
      do {
        object = try JSONSerialization.jsonObject(with: data, options: [])
      } catch {
        throw QueryProfileError.malformed("the file is not JSON")
      }
      switch JSONDuplicateKeyCheck.problem(in: data) {
      case .duplicate(let key)?:
        throw QueryProfileError.malformed("the field \(key) appears twice in one object")
      case .malformed?:
        throw QueryProfileError.malformed(
          "the file has a string or nesting that the greenbubbles CLI rejects")
      case nil: break
      }
      let top = try table(object, "the file")
      try onlyKeys(top, ["schema", "formatVersion", "defaultProfile", "profiles"], "the file")
      guard top["schema"] as? String == QueryProfileTarget.schema else {
        throw QueryProfileError.malformed("unsupported schema")
      }
      guard let version = top["formatVersion"] as? NSNumber, isInteger(version),
        version.intValue == QueryProfileTarget.formatVersion
      else {
        throw QueryProfileError.malformed("unsupported format version")
      }
      var defaultProfile: String?
      switch top["defaultProfile"] {
      case nil, is NSNull: defaultProfile = nil
      case let name as String: defaultProfile = name
      default: throw QueryProfileError.malformed("defaultProfile must be a string")
      }
      guard top["profiles"] != nil else { throw QueryProfileError.malformed("profiles is missing") }
      let entries = try table(top["profiles"]!, "profiles")
      guard !entries.isEmpty, entries.count <= QueryProfileTarget.maximumProfileCount else {
        throw QueryProfileError.malformed("profile count must be between 1 and 64")
      }
      var profiles: [String: Profile] = [:]
      for (name, value) in entries {
        try QueryProfileTarget.validateProfileName(name)
        profiles[name] = try profile(value)
      }
      if let defaultProfile {
        try QueryProfileTarget.validateProfileName(defaultProfile)
        guard profiles[defaultProfile] != nil else {
          throw QueryProfileError.malformed("defaultProfile does not name a configured profile")
        }
      }
      return ProfileStore(defaultProfile: defaultProfile, profiles: profiles)
    }

    func select(_ requested: String?) throws -> (String, Profile) {
      guard let name = requested ?? defaultProfile else {
        throw QueryProfileError.noDefaultProfile
      }
      try QueryProfileTarget.validateProfileName(name)
      guard let profile = profiles[name] else { throw QueryProfileError.unknownProfile(name) }
      return (name, profile)
    }

    private static func profile(_ value: Any) throws -> Profile {
      let fields = try table(value, "a profile")
      try onlyKeys(fields, ["sourceRoot", "access"], "a profile")
      guard let sourceRoot = fields["sourceRoot"] as? String,
        QueryProfileTarget.isAbsoluteNonRoot(sourceRoot),
        !QueryProfileTarget.hasParentComponent(sourceRoot)
      else {
        throw QueryProfileError.malformed(
          "sourceRoot must be an absolute non-root path without '..' components")
      }
      guard fields["access"] != nil else { throw QueryProfileError.malformed("access is missing") }
      let access = try table(fields["access"]!, "access")
      guard let mode = access["mode"] as? String else {
        throw QueryProfileError.malformed("access.mode is missing")
      }
      if mode == "decrypted" {
        try onlyKeys(access, ["mode"], "access")
        return Profile(sourceRoot: sourceRoot, mode: mode, credentialFile: nil)
      }
      guard QueryProfileTarget.fileModes.contains(mode) else {
        throw QueryProfileError.malformed("unknown access mode")
      }
      try onlyKeys(access, ["mode", "credentialFile"], "access")
      guard let credential = access["credentialFile"] as? String,
        QueryProfileTarget.isAbsoluteNonRoot(credential)
      else {
        throw QueryProfileError.malformed("credentialFile must be an absolute non-root path")
      }
      return Profile(sourceRoot: sourceRoot, mode: mode, credentialFile: credential)
    }

    private static func table(_ value: Any, _ what: String) throws -> [String: Any] {
      guard let table = value as? [String: Any] else {
        throw QueryProfileError.malformed("\(what) must be a JSON object")
      }
      return table
    }

    private static func onlyKeys(_ table: [String: Any], _ allowed: Set<String>, _ what: String)
      throws
    {
      if let unknown = table.keys.first(where: { !allowed.contains($0) }) {
        throw QueryProfileError.malformed("\(what) has an unknown field \(unknown)")
      }
    }

    private static func isInteger(_ number: NSNumber) -> Bool {
      CFGetTypeID(number) != CFBooleanGetTypeID() && !CFNumberIsFloatType(number)
    }
  }

  // MARK: - config.toml

  /// The `config.toml` keys the Rust CLI accepts, read with a deliberately
  /// small TOML subset: `[source]`, `[profile]`, and `[output]` tables with
  /// `key = "string"` lines and comments. Valid TOML outside that subset
  /// (dotted keys, inline tables, multi-line strings) is refused rather than
  /// guessed; write the file in the plain form shown in QUERY_PROFILES.md.
  struct Settings: Equatable {
    var sourceRoot: String?
    var passphraseFile: String?
    var defaultProfile: String?

    static func parse(_ data: Data) throws -> Settings {
      // Rust's toml parser skips one leading UTF-8 byte-order mark.
      var bytes = [UInt8](data)
      if bytes.starts(with: utf8ByteOrderMark) { bytes.removeFirst(utf8ByteOrderMark.count) }
      guard let decoded = strictUTF8(bytes) else {
        throw QueryProfileError.invalidSettings("the file is not UTF-8")
      }
      // Everything below works on Unicode scalars, as the TOML grammar does;
      // Swift Characters would merge "\r\n" or a quote with a following
      // combining mark into one unit.
      let lines = try splitLines(Array(decoded.unicodeScalars))
      let allowed: [String: Set<String>] = [
        "source": ["root", "passphrase_file"],
        "profile": ["default"],
        "output": ["format"],
      ]
      var values: [String: [String: String]] = [:]
      var current: String?
      for line in lines {
        let trimmed = trimBlanks(line[...])
        guard let first = trimmed.first else { continue }
        if first == "#" {
          try validateComment(trimmed)
          continue
        }
        if first == "[" {
          guard trimmed.dropFirst().first != "[", let close = trimmed.firstIndex(of: "]") else {
            throw unsupported()
          }
          let name = trimBlanks(trimmed[(trimmed.startIndex + 1)..<close])
          guard isBareKey(name) else { throw unsupported() }
          try requireEndOfLine(trimmed[(close + 1)...])
          let table = String(String.UnicodeScalarView(name))
          guard allowed[table] != nil else {
            throw QueryProfileError.invalidSettings(
              "settings file only accepts [source], [profile], and [output]")
          }
          guard values[table] == nil else {
            throw QueryProfileError.invalidSettings("duplicate table")
          }
          values[table] = [:]
          current = table
          continue
        }
        let keyEnd = trimmed.firstIndex(where: { !isBareKeyScalar($0) }) ?? trimmed.endIndex
        let keyScalars = trimmed[trimmed.startIndex..<keyEnd]
        guard !keyScalars.isEmpty else { throw unsupported() }
        let afterKey = trimBlanks(trimmed[keyEnd...])
        guard afterKey.first == "=" else { throw unsupported() }
        let key = String(String.UnicodeScalarView(keyScalars))
        guard let table = current else {
          throw QueryProfileError.invalidSettings(
            "settings file only accepts [source], [profile], and [output]")
        }
        guard allowed[table]!.contains(key) else {
          throw QueryProfileError.invalidSettings("[\(table)] does not accept \(key)")
        }
        let (value, remainder) = try parseString(
          trimBlanks(afterKey.dropFirst()), field: "\(table).\(key)")
        try requireEndOfLine(remainder)
        guard values[table]![key] == nil else {
          throw QueryProfileError.invalidSettings("duplicate key")
        }
        guard !value.isEmpty else {
          throw QueryProfileError.invalidSettings("\(table).\(key) must be a non-empty string")
        }
        values[table]![key] = value
      }

      var settings = Settings()
      if let root = values["source"]?["root"] {
        guard QueryProfileTarget.isAbsoluteNonRoot(root),
          !QueryProfileTarget.hasParentComponent(root)
        else {
          throw QueryProfileError.invalidSettings(
            "source.root must be an absolute non-root path without '..' components")
        }
        settings.sourceRoot = root
      }
      if let file = values["source"]?["passphrase_file"] {
        guard QueryProfileTarget.isAbsoluteNonRoot(file) else {
          throw QueryProfileError.invalidSettings(
            "source.passphrase_file must be an absolute non-root path")
        }
        settings.passphraseFile = file
      }
      if let name = values["profile"]?["default"] {
        do { try QueryProfileTarget.validateProfileName(name) } catch {
          throw QueryProfileError.invalidSettings("profile.default is not a valid profile name")
        }
        settings.defaultProfile = name
      }
      if let format = values["output"]?["format"], format != "brief", format != "json" {
        throw QueryProfileError.invalidSettings("output.format must be brief or json")
      }
      return settings
    }

    private typealias Scalars = ArraySlice<Unicode.Scalar>

    // TOML newlines are LF and CRLF only; a lone CR is an error.
    private static func splitLines(_ scalars: [Unicode.Scalar]) throws -> [[Unicode.Scalar]] {
      var lines: [[Unicode.Scalar]] = []
      var line: [Unicode.Scalar] = []
      var index = 0
      while index < scalars.count {
        let scalar = scalars[index]
        if scalar == "\r" {
          guard index + 1 < scalars.count, scalars[index + 1] == "\n" else {
            throw QueryProfileError.invalidSettings("a carriage return is not a TOML line ending")
          }
          index += 1
          continue
        }
        if scalar == "\n" {
          lines.append(line)
          line = []
        } else {
          line.append(scalar)
        }
        index += 1
      }
      lines.append(line)
      return lines
    }

    // TOML whitespace is only space and tab.
    private static func trimBlanks(_ text: Scalars) -> Scalars {
      let isBlank: (Unicode.Scalar) -> Bool = { $0 == " " || $0 == "\t" }
      guard let start = text.firstIndex(where: { !isBlank($0) }),
        let end = text.lastIndex(where: { !isBlank($0) })
      else { return text[text.endIndex...] }
      return text[start...end]
    }

    // unquoted-key = 1*( ALPHA / DIGIT / "-" / "_" )
    private static func isBareKeyScalar(_ scalar: Unicode.Scalar) -> Bool {
      switch scalar.value {
      case 0x41...0x5A, 0x61...0x7A, 0x30...0x39, 0x2D, 0x5F: return true
      default: return false
      }
    }

    private static func isBareKey(_ text: Scalars) -> Bool {
      !text.isEmpty && text.allSatisfy(isBareKeyScalar)
    }

    // non-ascii = %x80-D7FF / %xE000-10FFFF (Swift scalars exclude surrogates)
    private static func isNonASCII(_ value: UInt32) -> Bool { value >= 0x80 }

    // comment = "#" *non-eol; non-eol = %x09 / %x20-7E / non-ascii
    private static func validateComment(_ text: Scalars) throws {
      for scalar in text.dropFirst() {
        let value = scalar.value
        guard value == 0x09 || (0x20...0x7E).contains(value) || isNonASCII(value) else {
          throw QueryProfileError.invalidSettings("a comment contains a control character")
        }
      }
    }

    private static func unsupported() -> QueryProfileError {
      QueryProfileError.invalidSettings(
        "greenbubbles-acquire reads only [table] headers and key = \"string\" lines")
    }

    private static func requireEndOfLine(_ rest: Scalars) throws {
      let trimmed = trimBlanks(rest)
      guard let first = trimmed.first else { return }
      guard first == "#" else { throw unsupported() }
      try validateComment(trimmed)
    }

    // A TOML 1.1 basic ("...") or literal ('...') single-line string, with
    // the toml crate's character rules:
    //   basic-unescaped = wschar / %x21 / %x23-5B / %x5D-7E / non-ascii
    //   literal-char    = %x09 / %x20-26 / %x28-7E / non-ascii
    //   escapes         = \" \\ \b \e \f \n \r \t \xHH \uHHHH \UHHHHHHHH
    private static func parseString(_ text: Scalars, field: String) throws -> (String, Scalars) {
      guard let quote = text.first, quote == "\"" || quote == "'" else {
        throw QueryProfileError.invalidSettings("\(field) must be a non-empty string")
      }
      if text.starts(with: [quote, quote, quote]) { throw unsupported() }
      var result = String.UnicodeScalarView()
      var index = text.startIndex + 1
      while index < text.endIndex {
        let scalar = text[index]
        let value = scalar.value
        if scalar == quote {
          return (String(result), text[(index + 1)...])
        }
        if quote == "'" {
          guard value == 0x09 || (0x20...0x7E).contains(value) || isNonASCII(value) else {
            throw QueryProfileError.invalidSettings("\(field) contains a control character")
          }
          result.append(scalar)
          index += 1
          continue
        }
        if scalar == "\\" {
          index += 1
          guard index < text.endIndex else { break }
          let escape = text[index]
          index += 1
          switch escape {
          case "\"": result.append("\"")
          case "\\": result.append("\\")
          case "b": result.append("\u{08}")
          case "e": result.append("\u{1B}")
          case "f": result.append("\u{0C}")
          case "n": result.append("\n")
          case "r": result.append("\r")
          case "t": result.append("\t")
          case "x", "u", "U":
            let length = escape == "x" ? 2 : escape == "u" ? 4 : 8
            guard index + length <= text.endIndex else { throw unsupported() }
            var code: UInt32 = 0
            for digit in text[index..<(index + length)] {
              guard let nibble = hexValue(digit) else { throw unsupported() }
              code = code << 4 | nibble
            }
            guard let decoded = Unicode.Scalar(code) else { throw unsupported() }
            result.append(decoded)
            index += length
          default: throw unsupported()
          }
          continue
        }
        guard
          value == 0x09 || value == 0x20 || value == 0x21 || (0x23...0x5B).contains(value)
            || (0x5D...0x7E).contains(value) || isNonASCII(value)
        else {
          throw QueryProfileError.invalidSettings("\(field) contains a control character")
        }
        result.append(scalar)
        index += 1
      }
      throw unsupported()
    }

    private static func hexValue(_ scalar: Unicode.Scalar) -> UInt32? {
      switch scalar.value {
      case 0x30...0x39: return scalar.value - 0x30
      case 0x41...0x46: return scalar.value - 0x41 + 10
      case 0x61...0x66: return scalar.value - 0x61 + 10
      default: return nil
      }
    }
  }
}

let utf8ByteOrderMark: [UInt8] = [0xEF, 0xBB, 0xBF]

/// Decodes UTF-8 without repairs or byte-order-mark handling: nil unless the
/// bytes are valid UTF-8, so the text matches exactly what Rust would read.
func strictUTF8(_ bytes: [UInt8]) -> String? {
  let text = String(decoding: bytes, as: UTF8.self)
  return Array(text.utf8) == bytes ? text : nil
}
