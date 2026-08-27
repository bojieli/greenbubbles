import CommonCrypto
import CryptoKit
import Foundation
import Testing

@testable import GreenBubblesAcquire

struct SQLCipherKeyVerifierTests {
  // Known-answer vector computed with Python hashlib.pbkdf2_hmac during the
  // 2026-08-27 live validation. The passphrase is the synthetic lldb fixture
  // pattern, not a real secret.
  private let passphraseHex =
    "00112233445566778899aabbccddeeff102132435465768798a9bacbdcedfe0f"
  private let saltHex = "000102030405060708090a0b0c0d0e0f"
  private let expectedKeyHex =
    "558f516983adfb41ebc008ec57bbcca6b87301326710cce41760982d67bd984a"

  @Test func pbkdf2MatchesKnownAnswerVector() throws {
    let passphrase = try PassphraseSecret(bytes: bytesFromHex(passphraseHex))
    let derived = try SQLCipherKeyVerifier.deriveEncryptionKey(
      passphrase: passphrase,
      salt: bytesFromHex(saltHex)
    )
    #expect(derived.map { String(format: "%02x", $0) }.joined() == expectedKeyHex)
  }

  @Test func page1HMACVerificationAcceptsAndRejects() throws {
    let passphrase = bytesFromHex(passphraseHex)
    let salt = bytesFromHex(saltHex)
    var page = syntheticPage(salt: salt)
    let encryptionKey = pbkdf2(password: passphrase, salt: salt, rounds: 256_000)
    embedHMAC(into: &page, encryptionKey: encryptionKey)

    #expect(try SQLCipherKeyVerifier.verify(encryptionKey: encryptionKey, page1: page))

    var corruptedPage = page
    corruptedPage[4095] ^= 0xFF
    #expect(try !SQLCipherKeyVerifier.verify(encryptionKey: encryptionKey, page1: corruptedPage))

    var corruptedBody = page
    corruptedBody[100] ^= 0xFF
    #expect(try !SQLCipherKeyVerifier.verify(encryptionKey: encryptionKey, page1: corruptedBody))

    var wrongKey = encryptionKey
    wrongKey[0] ^= 0xFF
    #expect(try !SQLCipherKeyVerifier.verify(encryptionKey: wrongKey, page1: page))

    let otherSaltPage = syntheticPage(salt: [UInt8](repeating: 0xAB, count: 16))
    #expect(try !SQLCipherKeyVerifier.verify(encryptionKey: encryptionKey, page1: otherSaltPage))
  }

  @Test func verifierRejectsMalformedInputs() throws {
    let key = [UInt8](repeating: 0, count: 32)
    let page = [UInt8](repeating: 0, count: 4096)
    #expect(throws: KeyVerifierError.invalidKeyLength) {
      try SQLCipherKeyVerifier.verify(encryptionKey: [UInt8](repeating: 0, count: 31), page1: page)
    }
    #expect(throws: KeyVerifierError.invalidPageSize) {
      try SQLCipherKeyVerifier.verify(encryptionKey: key, page1: [UInt8](repeating: 0, count: 2048))
    }
    let passphrase = try PassphraseSecret(bytes: key)
    #expect(throws: KeyVerifierError.invalidSaltLength) {
      try SQLCipherKeyVerifier.deriveEncryptionKey(
        passphrase: passphrase,
        salt: [UInt8](repeating: 0, count: 8)
      )
    }
  }

  private func syntheticPage(salt: [UInt8]) -> [UInt8] {
    var page = [UInt8](repeating: 0, count: 4096)
    page.replaceSubrange(0..<16, with: salt)
    for index in 16..<4032 {
      page[index] = UInt8(truncatingIfNeeded: index &* 31 &+ 7)
    }
    return page
  }

  /// Independently computes and stores the SQLCipher4 page-1 HMAC using
  /// CommonCrypto PBKDF2 for the MAC key and CryptoKit for the HMAC.
  private func embedHMAC(into page: inout [UInt8], encryptionKey: [UInt8]) {
    let macSalt = page[0..<16].map { $0 ^ 0x3A }
    let macKey = pbkdf2(password: encryptionKey, salt: macSalt, rounds: 2)
    var hmac = HMAC<SHA512>(key: SymmetricKey(data: macKey))
    hmac.update(data: Data(page[16..<4032]))
    var pageNumber = UInt32(1).littleEndian
    withUnsafeBytes(of: &pageNumber) { hmac.update(data: Data($0)) }
    page.replaceSubrange(4032..<4096, with: Array(hmac.finalize()))
  }

  private func pbkdf2(password: [UInt8], salt: [UInt8], rounds: UInt32) -> [UInt8] {
    var derived = [UInt8](repeating: 0, count: 32)
    let status = derived.withUnsafeMutableBytes { derivedBuffer in
      password.withUnsafeBufferPointer { passwordBuffer in
        salt.withUnsafeBufferPointer { saltBuffer in
          CCKeyDerivationPBKDF(
            CCPBKDFAlgorithm(kCCPBKDF2),
            passwordBuffer.baseAddress?.withMemoryRebound(to: Int8.self, capacity: 32) { $0 },
            password.count,
            saltBuffer.baseAddress,
            salt.count,
            CCPseudoRandomAlgorithm(kCCPRFHmacAlgSHA512),
            rounds,
            derivedBuffer.baseAddress?.assumingMemoryBound(to: UInt8.self),
            derivedBuffer.count
          )
        }
      }
    }
    precondition(status == kCCSuccess)
    return derived
  }

  private func bytesFromHex(_ hex: String) -> [UInt8] {
    var bytes: [UInt8] = []
    var index = hex.startIndex
    while index < hex.endIndex {
      let next = hex.index(index, offsetBy: 2)
      bytes.append(UInt8(hex[index..<next], radix: 16)!)
      index = next
    }
    return bytes
  }
}
