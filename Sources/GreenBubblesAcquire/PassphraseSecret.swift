// Derived from wcdb-key-tool (https://github.com/TANGandXUE/wcdb-key-tool), MIT license.

import Darwin
import Foundation

public enum PassphraseSecretError: Error, Equatable, CustomStringConvertible {
  case invalidPassphrase

  public var description: String {
    switch self {
    case .invalidPassphrase:
      return "The passphrase must be exactly 32 bytes"
    }
  }
}

/// A 32-byte SQLCipher passphrase held in memory for the shortest possible time.
///
/// The value is never `Codable`, never interpolated into strings or errors, and
/// the only permitted egress is `SecretOutputWriter`. The backing buffer is
/// zeroed with `memset_s` on deinit.
public final class PassphraseSecret {
  public static let byteCount = 32

  private var bytes: [UInt8]

  public init(bytes: [UInt8]) throws {
    guard bytes.count == Self.byteCount else {
      throw PassphraseSecretError.invalidPassphrase
    }
    self.bytes = bytes
  }

  /// Reads one line from standard input, mirroring the `greenbubbles`
  /// `--passphrase-stdin` contract: a 64-character hexadecimal line is decoded,
  /// anything else is taken as raw bytes; the result must be exactly 32 bytes.
  public static func readFromStandardInput() throws -> PassphraseSecret {
    guard let line = readLine(strippingNewline: true) else {
      throw PassphraseSecretError.invalidPassphrase
    }
    let trimmed = line.trimmingCharacters(in: .whitespaces)
    var decoded: [UInt8]
    if trimmed.count == 64, trimmed.allSatisfy({ $0.isHexDigit }) {
      var value: [UInt8] = []
      value.reserveCapacity(Self.byteCount)
      var index = trimmed.startIndex
      while index < trimmed.endIndex {
        let next = trimmed.index(index, offsetBy: 2)
        guard let byte = UInt8(trimmed[index..<next], radix: 16) else {
          throw PassphraseSecretError.invalidPassphrase
        }
        value.append(byte)
        index = next
      }
      decoded = value
    } else {
      decoded = Array(trimmed.utf8)
    }
    defer { Self.zeroize(&decoded) }
    return try PassphraseSecret(bytes: decoded)
  }

  public func withUnsafeBytes<R>(_ body: (UnsafeRawBufferPointer) throws -> R) rethrows -> R {
    try bytes.withUnsafeBytes(body)
  }

  public static func zeroize(_ buffer: inout [UInt8]) {
    buffer.withUnsafeMutableBytes { raw in
      guard let base = raw.baseAddress, !raw.isEmpty else { return }
      memset_s(base, raw.count, 0, raw.count)
    }
  }

  deinit {
    Self.zeroize(&bytes)
  }
}
