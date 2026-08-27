import Darwin
import Foundation
import Testing

@testable import GreenBubblesAcquire

struct SecretOutputWriterTests {
  // Synthetic fixture pattern, not a real secret.
  private let passphraseHex =
    "00112233445566778899aabbccddeeff102132435465768798a9bacbdcedfe0f"

  @Test func writesHexWithOwnerOnlyPermissions() throws {
    let base = try makeTemporaryDirectory()
    defer { try? FileManager.default.removeItem(at: base) }
    let output = base.appending(path: "nested/passphrase.txt")

    let secret = try PassphraseSecret(bytes: bytesFromHex(passphraseHex))
    try SecretOutputWriter().write(secret, to: output)

    let data = try Data(contentsOf: output)
    #expect(String(decoding: data, as: UTF8.self) == passphraseHex + "\n")
    #expect(try posixMode(of: output) == 0o600)
    #expect(try posixMode(of: output.deletingLastPathComponent()) == 0o700)
  }

  @Test func refusesToOverwriteUnlessRequested() throws {
    let base = try makeTemporaryDirectory()
    defer { try? FileManager.default.removeItem(at: base) }
    let output = base.appending(path: "passphrase.txt")

    let secret = try PassphraseSecret(bytes: bytesFromHex(passphraseHex))
    let writer = SecretOutputWriter()
    try writer.write(secret, to: output)

    #expect(throws: SecretOutputError.outputExists) {
      try writer.write(secret, to: output)
    }

    var replacementBytes = [UInt8](repeating: 0x5A, count: 32)
    let replacement = try PassphraseSecret(bytes: replacementBytes)
    try writer.write(replacement, to: output, overwrite: true)
    let data = try Data(contentsOf: output)
    #expect(
      String(decoding: data, as: UTF8.self)
        == String(repeating: "5a", count: 32) + "\n"
    )
    #expect(try posixMode(of: output) == 0o600)
    PassphraseSecret.zeroize(&replacementBytes)
  }

  @Test func enforcesPermissionsOnAPreExistingParentDirectory() throws {
    let base = try makeTemporaryDirectory()
    defer { try? FileManager.default.removeItem(at: base) }
    let parent = base.appending(path: "loose", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: parent, withIntermediateDirectories: false)
    try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: parent.path)

    let secret = try PassphraseSecret(bytes: bytesFromHex(passphraseHex))
    let output = parent.appending(path: "passphrase.txt")
    try SecretOutputWriter().write(secret, to: output)

    #expect(try posixMode(of: parent) == 0o700)
    #expect(try posixMode(of: output) == 0o600)
  }

  private func makeTemporaryDirectory() throws -> URL {
    let url = FileManager.default.temporaryDirectory.appending(
      path: "greenbubbles-acquire-writer-tests-\(UUID().uuidString)",
      directoryHint: .isDirectory
    )
    try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
    return url
  }

  private func posixMode(of url: URL) throws -> Int32 {
    var metadata = stat()
    guard Darwin.lstat(url.path, &metadata) == 0 else {
      throw NSError(domain: NSPOSIXErrorDomain, code: Int(errno))
    }
    return Int32(metadata.st_mode & 0o777)
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
