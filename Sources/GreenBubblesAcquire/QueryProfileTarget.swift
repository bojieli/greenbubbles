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
  case unsafeCredentialPath(String)
  case invalidEnvironment(String)

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
    case .unsafeCredentialPath(let field):
      return
        "\(field) must be an absolute path without '.' or '..' components; "
        + "greenbubbles-acquire will not write a key through it"
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
    public let settingsFile: URL
  }

  /// Applies the Rust location rules: the environment variables win and must
  /// be absolute non-root paths; otherwise `~/.greenbubbles/...`.
  public static func locations(environment: [String: String], home: URL) throws -> Locations {
    func location(_ variable: String, _ fileName: String) throws -> URL {
      if let value = environment[variable] {
        guard isAbsoluteNonRoot(value) else {
          throw QueryProfileError.invalidEnvironment(variable)
        }
        return URL(fileURLWithPath: value)
      }
      return home.appending(path: ".greenbubbles").appending(path: fileName)
    }
    return Locations(
      profilesFile: try location(profilesEnvironmentVariable, "query-profiles.json"),
      settingsFile: try location(settingsEnvironmentVariable, "config.toml")
    )
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
    let settings: Settings
    switch try PrivateConfigurationFile.read(
      locations.settingsFile, ownerUID: ownerUID, description: "settings file (config.toml)")
    {
    case .absent: return .none
    case .contents(let data): settings = try Settings.parse(data)
    }
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
    if let passphraseFile = settings.passphraseFile {
      try requireSafeCredentialPath(passphraseFile, field: "source.passphrase_file")
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
    try requireSafeCredentialPath(credential, field: "credentialFile of query profile \(name)")
    return AcquireTarget(
      profileName: name,
      sourceRoot: URL(fileURLWithPath: profile.sourceRoot),
      credentialFile: URL(fileURLWithPath: credential),
      sourceRootOrigin: "from query profile \(name)"
    )
  }

  // Stricter than the Rust reader: the key is written here, so the path must
  // mean the same thing to the kernel and to a reader of the file.
  static func requireSafeCredentialPath(_ path: String, field: String) throws {
    let components = path.split(separator: "/", omittingEmptySubsequences: true)
    guard isAbsoluteNonRoot(path), !components.contains(where: { $0 == "." || $0 == ".." })
    else {
      throw QueryProfileError.unsafeCredentialPath(field)
    }
  }

  static func isAbsoluteNonRoot(_ path: String) -> Bool {
    path.hasPrefix("/") && !path.contains("\0")
      && path.contains(where: { $0 != "/" })
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
      let object: Any
      do {
        object = try JSONSerialization.jsonObject(with: data, options: [])
      } catch {
        throw QueryProfileError.malformed("the file is not JSON")
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
        QueryProfileTarget.isAbsoluteNonRoot(sourceRoot)
      else {
        throw QueryProfileError.malformed("sourceRoot must be an absolute non-root path")
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
      guard let text = String(data: data, encoding: .utf8) else {
        throw QueryProfileError.invalidSettings("the file is not UTF-8")
      }
      let allowed: [String: Set<String>] = [
        "source": ["root", "passphrase_file"],
        "profile": ["default"],
        "output": ["format"],
      ]
      var values: [String: [String: String]] = [:]
      var current: String?
      for rawLine in text.split(separator: "\n", omittingEmptySubsequences: false) {
        var line = Substring(rawLine)
        if line.hasSuffix("\r") { line = line.dropLast() }
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        if trimmed.isEmpty || trimmed.hasPrefix("#") { continue }
        if trimmed.hasPrefix("[") {
          guard !trimmed.hasPrefix("[["), let close = trimmed.firstIndex(of: "]") else {
            throw unsupported()
          }
          let name = trimmed[trimmed.index(after: trimmed.startIndex)..<close]
            .trimmingCharacters(in: .whitespaces)
          try requireComment(trimmed[trimmed.index(after: close)...])
          guard allowed[name] != nil else {
            throw QueryProfileError.invalidSettings(
              "settings file only accepts [source], [profile], and [output]")
          }
          guard values[name] == nil else {
            throw QueryProfileError.invalidSettings("duplicate table")
          }
          values[name] = [:]
          current = name
          continue
        }
        guard let equals = trimmed.firstIndex(of: "=") else { throw unsupported() }
        let key = trimmed[..<equals].trimmingCharacters(in: .whitespaces)
        guard !key.isEmpty,
          key.allSatisfy({ $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "_" || $0 == "-") })
        else { throw unsupported() }
        guard let table = current else {
          throw QueryProfileError.invalidSettings(
            "settings file only accepts [source], [profile], and [output]")
        }
        guard allowed[table]!.contains(key) else {
          throw QueryProfileError.invalidSettings("[\(table)] does not accept \(key)")
        }
        let rest = trimmed[trimmed.index(after: equals)...].drop(while: { $0 == " " || $0 == "\t" })
        let (value, remainder) = try parseString(rest, field: "\(table).\(key)")
        try requireComment(remainder)
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
        guard QueryProfileTarget.isAbsoluteNonRoot(root) else {
          throw QueryProfileError.invalidSettings("source.root must be an absolute non-root path")
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

    private static func unsupported() -> QueryProfileError {
      QueryProfileError.invalidSettings(
        "greenbubbles-acquire reads only [table] headers and key = \"string\" lines")
    }

    private static func requireComment(_ rest: Substring) throws {
      let trimmed = rest.trimmingCharacters(in: .whitespaces)
      guard trimmed.isEmpty || trimmed.hasPrefix("#") else { throw unsupported() }
    }

    // A TOML basic ("...") or literal ('...') single-line string.
    private static func parseString(_ text: Substring, field: String) throws -> (String, Substring)
    {
      guard let quote = text.first, quote == "\"" || quote == "'" else {
        throw QueryProfileError.invalidSettings("\(field) must be a non-empty string")
      }
      if text.hasPrefix("\"\"\"") || text.hasPrefix("'''") { throw unsupported() }
      var result = ""
      var index = text.index(after: text.startIndex)
      while index < text.endIndex {
        let character = text[index]
        if character == quote {
          return (result, text[text.index(after: index)...])
        }
        if quote == "\"", character == "\\" {
          index = text.index(after: index)
          guard index < text.endIndex else { break }
          switch text[index] {
          case "\"": result.append("\"")
          case "\\": result.append("\\")
          case "b": result.append("\u{08}")
          case "t": result.append("\t")
          case "n": result.append("\n")
          case "f": result.append("\u{0C}")
          case "r": result.append("\r")
          case "u", "U":
            let length = text[index] == "u" ? 4 : 8
            let start = text.index(after: index)
            guard let end = text.index(start, offsetBy: length, limitedBy: text.endIndex),
              let scalarValue = UInt32(text[start..<end], radix: 16),
              let scalar = Unicode.Scalar(scalarValue)
            else { throw unsupported() }
            result.unicodeScalars.append(scalar)
            index = text.index(before: end)
          default: throw unsupported()
          }
        } else {
          guard character != "\n" else { break }
          result.append(character)
        }
        index = text.index(after: index)
      }
      throw unsupported()
    }
  }
}
