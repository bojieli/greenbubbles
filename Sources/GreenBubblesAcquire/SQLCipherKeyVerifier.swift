// Derived from wcdb-key-tool (https://github.com/TANGandXUE/wcdb-key-tool), MIT license.

import CommonCrypto
import CryptoKit
import Foundation

public enum KeyVerifierError: Error, Equatable, CustomStringConvertible {
  case invalidSaltLength
  case invalidKeyLength
  case invalidPageSize
  case derivationFailed(code: Int32)

  public var description: String {
    switch self {
    case .invalidSaltLength:
      return "A database salt must be exactly 16 bytes"
    case .invalidKeyLength:
      return "An encryption key must be exactly 32 bytes"
    case .invalidPageSize:
      return "A database page must be exactly 4096 bytes"
    case .derivationFailed(let code):
      return "Key derivation failed with CommonCrypto status \(code)"
    }
  }
}

/// Pure SQLCipher 4 key derivation and page-1 HMAC verification. No I/O.
///
/// - Page size 4096, reserve 80 bytes (16 IV + 64 HMAC).
/// - `enc_key = PBKDF2-HMAC-SHA512(passphrase, salt, 256000 rounds, 32 bytes)`.
/// - `mac_salt = salt XOR 0x3A`;
///   `mac_key = PBKDF2-HMAC-SHA512(enc_key, mac_salt, 2 rounds, 32 bytes)`;
///   `HMAC-SHA512(mac_key, page1[16..<4032] || little-endian UInt32 page number 1)`
///   compared in constant time against `page1[4032..<4096]`.
public enum SQLCipherKeyVerifier {
  public static let pageSize = 4096
  public static let saltSize = 16
  public static let keySize = 32
  public static let hmacSize = 64
  public static let passphraseRounds: UInt32 = 256_000
  public static let macKeyRounds: UInt32 = 2

  public static func deriveEncryptionKey(
    passphrase: PassphraseSecret,
    salt: [UInt8]
  ) throws -> [UInt8] {
    guard salt.count == saltSize else { throw KeyVerifierError.invalidSaltLength }
    return try passphrase.withUnsafeBytes { password in
      try pbkdf2(password: password, salt: salt, rounds: passphraseRounds)
    }
  }

  public static func verify(encryptionKey: [UInt8], page1: [UInt8]) throws -> Bool {
    guard encryptionKey.count == keySize else { throw KeyVerifierError.invalidKeyLength }
    guard page1.count == pageSize else { throw KeyVerifierError.invalidPageSize }

    let salt = page1[0..<saltSize]
    let macSalt = salt.map { $0 ^ 0x3A }
    let macKey = try encryptionKey.withUnsafeBytes { password in
      try pbkdf2(password: password, salt: macSalt, rounds: macKeyRounds)
    }

    var hmac = HMAC<SHA512>(key: SymmetricKey(data: macKey))
    hmac.update(data: Data(page1[saltSize..<(pageSize - hmacSize)]))
    var pageNumber = UInt32(1).littleEndian
    withUnsafeBytes(of: &pageNumber) { hmac.update(data: Data($0)) }
    let digest = hmac.finalize()

    return constantTimeEqual(Array(digest), Array(page1[(pageSize - hmacSize)..<pageSize]))
  }

  static func pbkdf2(
    password: UnsafeRawBufferPointer,
    salt: [UInt8],
    rounds: UInt32
  ) throws -> [UInt8] {
    var derived = [UInt8](repeating: 0, count: keySize)
    let status = derived.withUnsafeMutableBytes { derivedBuffer in
      salt.withUnsafeBufferPointer { saltBuffer in
        CCKeyDerivationPBKDF(
          CCPBKDFAlgorithm(kCCPBKDF2),
          password.baseAddress?.assumingMemoryBound(to: Int8.self),
          password.count,
          saltBuffer.baseAddress,
          saltBuffer.count,
          CCPseudoRandomAlgorithm(kCCPRFHmacAlgSHA512),
          rounds,
          derivedBuffer.baseAddress?.assumingMemoryBound(to: UInt8.self),
          derivedBuffer.count
        )
      }
    }
    guard status == kCCSuccess else {
      PassphraseSecret.zeroize(&derived)
      throw KeyVerifierError.derivationFailed(code: status)
    }
    return derived
  }

  static func constantTimeEqual(_ lhs: [UInt8], _ rhs: [UInt8]) -> Bool {
    guard lhs.count == rhs.count else { return false }
    var difference: UInt8 = 0
    for index in lhs.indices {
      difference |= lhs[index] ^ rhs[index]
    }
    return difference == 0
  }
}
