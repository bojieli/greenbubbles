import CryptoKit
import Foundation

/// The canonical byte encoder shared with the Rust control plane. Strings are
/// written as their UTF-8 bytes followed by one NUL; unsigned integers are
/// written as decimal ASCII followed by one NUL. A NUL inside a field
/// invalidates the encoding rather than producing ambiguous bytes.
///
/// This mirrors `CanonicalWriter` in `send_contract.rs` exactly. Floating point
/// is deliberately absent: window-relative geometry is carried as integer
/// parts-per-million so the signed bytes are reproducible in both languages.
public struct CanonicalWriter {
  private var bytes: Data
  private var valid = true

  public init(domain: String) {
    bytes = Data()
    text(domain)
  }

  public mutating func text(_ value: String) {
    let encoded = Data(value.utf8)
    if encoded.contains(0) {
      valid = false
      return
    }
    bytes.append(encoded)
    bytes.append(0)
  }

  public mutating func number(_ value: UInt64) {
    bytes.append(Data(String(value).utf8))
    bytes.append(0)
  }

  public mutating func flag(_ value: Bool) {
    text(value ? "true" : "false")
  }

  public func finish() -> Data? {
    valid ? bytes : nil
  }
}

/// Digest helpers used by every signed or bound send artifact.
public enum SendDigest {
  /// Lowercase hexadecimal SHA-256 of the given bytes.
  public static func sha256Hex(_ data: Data) -> String {
    SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
  }

  /// Whether a string is exactly 64 hexadecimal characters.
  public static func isSHA256Hex(_ value: String) -> Bool {
    value.count == 64 && value.allSatisfy(\.isHexDigit)
  }
}

/// The single text normalization both the on-screen gates and replica
/// reconciliation use: trim the ends and fold every run of Unicode whitespace
/// into one space. Vision's line output and WeChat's re-decoded content both
/// vary only in whitespace for a plain text send, so comparing normalized forms
/// is exact where comparing raw bytes would be brittle.
public enum SendText {
  /// Folds whitespace exactly as `normalized_send_text` does in Rust.
  public static func normalized(_ value: String) -> String {
    value.split(whereSeparator: \.isWhitespace).joined(separator: " ")
  }

  /// Digest of the normalized form.
  public static func normalizedSHA256(_ value: String) -> String {
    SendDigest.sha256Hex(Data(normalized(value).utf8))
  }

  /// Whether two strings are equal after normalization. Used by both on-screen
  /// gates so a rendering that inserts a line break is not a mismatch.
  public static func matches(_ left: String, _ right: String) -> Bool {
    normalized(left) == normalized(right)
  }
}
