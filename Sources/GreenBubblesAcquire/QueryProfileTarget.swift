import Darwin
import Foundation

/// The two paths `greenbubbles-acquire` needs from a live query profile: the
/// account's `db_storage` root and the key file that profile reads.
public struct LiveQueryProfile: Equatable, Sendable {
  public let name: String
  public let sourceRoot: URL
  public let credentialFile: URL
}

public enum QueryProfileError: Error, Equatable, CustomStringConvertible {
  case missingFile
  case unsafeFile(String)
  case malformed
  case unknownProfile(String)
  case notLiveProfile(String)
  case relativePath(String)

  public var description: String {
    switch self {
    case .missingFile:
      return "--profile was given, but no query profile file exists"
    case .unsafeFile(let reason):
      return "The query profile file is not safe to read: \(reason)"
    case .malformed:
      return "The query profile file is not valid greenbubbles.query-profiles.v1 JSON"
    case .unknownProfile(let name):
      return "No query profile is named \(name)"
    case .notLiveProfile(let name):
      return
        "Query profile \(name) does not use liveWeChatKeyFile access, so capture cannot write its key"
    case .relativePath(let name):
      return "Query profile \(name) has a path that is not absolute"
    }
  }
}

/// Reads the user's `query-profiles.json` so key capture writes the key file
/// that queries will read. The Rust CLI owns the format; this reader takes only
/// the fields capture needs and applies the same file-safety rules (regular
/// file, no symlink, owned by the user, no group or other access).
public enum QueryProfileTarget {
  public static let liveMode = "liveWeChatKeyFile"

  /// Returns the profile named `name`, or the file's `defaultProfile` when
  /// `name` is nil. Returns nil when no name was asked for and the file is
  /// absent, has no default, or its default is not a live profile: capture
  /// then keeps its built-in defaults.
  public static func resolve(name: String?, file: URL, ownerUID: uid_t) throws -> LiveQueryProfile?
  {
    var info = stat()
    guard lstat(file.path, &info) == 0 else {
      if errno == ENOENT {
        if name != nil { throw QueryProfileError.missingFile }
        return nil
      }
      throw QueryProfileError.unsafeFile("stat failed with POSIX error \(errno)")
    }
    guard info.st_mode & S_IFMT == S_IFREG else {
      throw QueryProfileError.unsafeFile("not a regular file (symlinks are refused)")
    }
    guard info.st_uid == ownerUID else {
      throw QueryProfileError.unsafeFile("not owned by the invoking user")
    }
    guard info.st_mode & 0o077 == 0 else {
      throw QueryProfileError.unsafeFile("group or other users can access it; run chmod 600")
    }

    let document: Document
    do {
      document = try JSONDecoder().decode(Document.self, from: Data(contentsOf: file))
    } catch {
      throw QueryProfileError.malformed
    }
    guard document.schema == "greenbubbles.query-profiles.v1" else {
      throw QueryProfileError.malformed
    }

    guard let selected = name ?? document.defaultProfile else { return nil }
    guard let profile = document.profiles[selected] else {
      throw QueryProfileError.unknownProfile(selected)
    }
    guard profile.access.mode == liveMode, let credential = profile.access.credentialFile else {
      if name != nil { throw QueryProfileError.notLiveProfile(selected) }
      return nil
    }
    guard profile.sourceRoot.hasPrefix("/"), credential.hasPrefix("/") else {
      throw QueryProfileError.relativePath(selected)
    }
    return LiveQueryProfile(
      name: selected,
      sourceRoot: URL(fileURLWithPath: profile.sourceRoot).standardizedFileURL,
      credentialFile: URL(fileURLWithPath: credential).standardizedFileURL
    )
  }

  private struct Document: Decodable {
    let schema: String
    let defaultProfile: String?
    let profiles: [String: Profile]
  }

  private struct Profile: Decodable {
    let sourceRoot: String
    let access: Access
  }

  private struct Access: Decodable {
    let mode: String
    let credentialFile: String?
  }
}
