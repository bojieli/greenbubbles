// Derived from wcdb-key-tool (https://github.com/TANGandXUE/wcdb-key-tool), MIT license.

import Darwin
import Foundation

public enum SecretOutputError: Error, Equatable, CustomStringConvertible {
  case outputExists
  case unsafeOutputFile
  case notOwnedByUser
  case relativePath
  case dotComponents
  case symbolicLinkParent
  case posix(operation: String, code: Int32)

  public var description: String {
    switch self {
    case .outputExists:
      return "The passphrase output file already exists; pass --overwrite to replace it"
    case .unsafeOutputFile:
      return "Passphrase output must be a regular file with a single hard link"
    case .notOwnedByUser:
      return "The existing passphrase output file belongs to another user; it was not changed"
    case .relativePath:
      return "The passphrase output path must be absolute"
    case .dotComponents:
      return
        "The passphrase output path must not contain '.' or '..' components; "
        + "give the plain path (a profile's credentialFile or --output)"
    case .symbolicLinkParent:
      return
        "The folder that would hold the passphrase output is a symbolic link; "
        + "give its real path instead (for example /private/tmp rather than /tmp)"
    case .posix(let operation, let code):
      return "\(operation) failed with POSIX error \(code)"
    }
  }
}

/// The only egress point for a captured passphrase: writes exactly 64
/// lowercase hexadecimal characters plus a trailing newline to an
/// owner-specified file. Missing parent folders are created with mode 0700
/// and existing ones keep their permissions; the file is mode 0600, and
/// creation is exclusive (`O_CREAT | O_EXCL`, failing rather than
/// overwriting) unless `overwrite` is set.
///
/// Every filesystem step — each `mkdir`, the `open`, `fchmod`, `ftruncate`,
/// and `write` — runs inside `user.perform(_:)`. Under `sudo` that is the
/// invoking user's identity, so the kernel refuses anything that user could
/// not do, and the file is that user's without any `chown`.
public struct SecretOutputWriter: Sendable {
  public struct Outcome: Equatable, Sendable {
    /// Whether the parent folder is a real folder owned by the user with no
    /// group or other access, as the Rust CLI requires before it reads a key.
    public let parentIsPrivate: Bool
  }

  public init() {}

  /// The write-path policy, checked by `write` and, before any capture
  /// starts, by the CLI: an absolute path with no `.` or `..` components, so
  /// the path means the same to the kernel and to anyone reading it.
  public static func checkDestination(_ url: URL) throws {
    let path = url.path
    guard path.hasPrefix("/") else { throw SecretOutputError.relativePath }
    let components = path.split(separator: "/", omittingEmptySubsequences: true)
    guard !components.isEmpty, !components.contains(where: { $0 == "." || $0 == ".." }) else {
      throw SecretOutputError.dotComponents
    }
  }

  @discardableResult
  public func write(
    _ secret: PassphraseSecret,
    to url: URL,
    overwrite: Bool = false,
    as user: InvokingUser? = nil
  ) throws -> Outcome {
    try Self.checkDestination(url)
    let user = try user ?? InvokingUser.current()
    return try user.perform {
      try writeAsUser(secret, to: url.path, overwrite: overwrite, ownerUID: user.uid)
    }
  }

  private func writeAsUser(
    _ secret: PassphraseSecret,
    to path: String,
    overwrite: Bool,
    ownerUID: uid_t
  ) throws -> Outcome {
    guard path.hasPrefix("/") else { throw SecretOutputError.relativePath }
    let parentPath = (path as NSString).deletingLastPathComponent
    try Self.createMissingFolders(parentPath)
    // Open the folder itself without following a link, check it through the
    // descriptor, and create the file relative to it, so the folder checked
    // is the folder written to.
    let folder = Darwin.open(parentPath, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
    guard folder >= 0 else {
      let code = errno
      var link = stat()
      if code == ELOOP || (Darwin.lstat(parentPath, &link) == 0 && link.st_mode & S_IFMT == S_IFLNK)
      {
        throw SecretOutputError.symbolicLinkParent
      }
      throw SecretOutputError.posix(operation: "open passphrase folder", code: code)
    }
    defer { Darwin.close(folder) }
    var parent = stat()
    guard Darwin.fstat(folder, &parent) == 0 else {
      throw SecretOutputError.posix(operation: "inspect passphrase folder", code: errno)
    }
    let name = (path as NSString).lastPathComponent
    guard !name.isEmpty, name != "/", !name.contains("/") else {
      throw SecretOutputError.dotComponents
    }

    var flags = O_WRONLY | O_CREAT | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK
    if !overwrite {
      flags |= O_EXCL
    }
    let descriptor = Darwin.openat(folder, name, flags, 0o600)
    guard descriptor >= 0 else {
      if errno == EEXIST, !overwrite { throw SecretOutputError.outputExists }
      throw SecretOutputError.posix(operation: "open passphrase output", code: errno)
    }
    defer { Darwin.close(descriptor) }
    // Check the open descriptor before changing permissions or contents.
    var metadata = stat()
    guard Darwin.fstat(descriptor, &metadata) == 0 else {
      throw SecretOutputError.posix(operation: "inspect passphrase output", code: errno)
    }
    guard (metadata.st_mode & S_IFMT) == S_IFREG, metadata.st_nlink == 1 else {
      throw SecretOutputError.unsafeOutputFile
    }
    guard metadata.st_uid == ownerUID else { throw SecretOutputError.notOwnedByUser }
    guard Darwin.fchmod(descriptor, 0o600) == 0 else {
      throw SecretOutputError.posix(operation: "restrict passphrase output", code: errno)
    }

    if overwrite && Darwin.ftruncate(descriptor, 0) != 0 {
      throw SecretOutputError.posix(operation: "truncate passphrase output", code: errno)
    }

    var payload = Self.hexPayload(of: secret)
    defer { PassphraseSecret.zeroize(&payload) }
    try payload.withUnsafeBytes { buffer in
      guard let base = buffer.baseAddress else { return }
      var written = 0
      while written < buffer.count {
        let count = Darwin.write(descriptor, base.advanced(by: written), buffer.count - written)
        guard count > 0 else {
          throw SecretOutputError.posix(operation: "write passphrase output", code: errno)
        }
        written += count
      }
    }
    Darwin.fsync(descriptor)
    return Outcome(
      parentIsPrivate: parent.st_mode & S_IFMT == S_IFDIR && parent.st_uid == ownerUID
        && parent.st_mode & 0o077 == 0)
  }

  // Creates each missing level with mode 0700, from the top down. Runs as
  // the user, so it can only create what the user could create.
  private static func createMissingFolders(_ path: String) throws {
    var prefix = ""
    for component in path.split(separator: "/", omittingEmptySubsequences: true) {
      prefix += "/" + component
      var info = stat()
      if Darwin.lstat(prefix, &info) == 0 { continue }
      guard errno == ENOENT else {
        throw SecretOutputError.posix(operation: "inspect passphrase folder", code: errno)
      }
      if Darwin.mkdir(prefix, 0o700) != 0, errno != EEXIST {
        throw SecretOutputError.posix(operation: "create passphrase folder", code: errno)
      }
    }
  }

  private static func hexPayload(of secret: PassphraseSecret) -> [UInt8] {
    let digits = Array("0123456789abcdef".utf8)
    var payload: [UInt8] = []
    payload.reserveCapacity(65)
    secret.withUnsafeBytes { buffer in
      for byte in buffer {
        payload.append(digits[Int(byte >> 4)])
        payload.append(digits[Int(byte & 0x0F)])
      }
    }
    payload.append(0x0A)
    return payload
  }
}
