// Derived from wcdb-key-tool (https://github.com/TANGandXUE/wcdb-key-tool), MIT license.

import Darwin
import Foundation

public enum SecretOutputError: Error, Equatable, CustomStringConvertible {
  case outputExists
  case posix(operation: String, code: Int32)

  public var description: String {
    switch self {
    case .outputExists:
      return "The passphrase output file already exists; pass --overwrite to replace it"
    case .posix(let operation, let code):
      return "\(operation) failed with POSIX error \(code)"
    }
  }
}

/// The only egress point for a captured passphrase: writes exactly 64
/// lowercase hexadecimal characters plus a trailing newline to an
/// owner-specified file. The parent directory is created or enforced to mode
/// 0700, the file is mode 0600, and creation is exclusive (`O_CREAT | O_EXCL`,
/// failing rather than overwriting) unless `overwrite` is set.
public struct SecretOutputWriter: Sendable {
  public init() {}

  public func write(
    _ secret: PassphraseSecret,
    to url: URL,
    overwrite: Bool = false
  ) throws {
    let parent = url.standardizedFileURL.deletingLastPathComponent()
    try FileManager.default.createDirectory(at: parent, withIntermediateDirectories: true)
    try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: parent.path)

    var flags = O_WRONLY | O_CREAT | O_CLOEXEC | O_NOFOLLOW
    if overwrite {
      flags |= O_TRUNC
    } else {
      flags |= O_EXCL
    }
    let descriptor = Darwin.open(url.path, flags, 0o600)
    guard descriptor >= 0 else {
      if errno == EEXIST, !overwrite { throw SecretOutputError.outputExists }
      throw SecretOutputError.posix(operation: "open passphrase output", code: errno)
    }
    defer { Darwin.close(descriptor) }
    guard Darwin.fchmod(descriptor, 0o600) == 0 else {
      throw SecretOutputError.posix(operation: "restrict passphrase output", code: errno)
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
