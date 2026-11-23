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

  @Test func createsEveryMissingLevelOwnerOnlyAndOwnedByTheUser() throws {
    let base = try makeTemporaryDirectory()
    defer { try? FileManager.default.removeItem(at: base) }
    let output = base.appending(path: "a/b/c/passphrase.txt")

    let secret = try PassphraseSecret(bytes: bytesFromHex(passphraseHex))
    let outcome = try SecretOutputWriter().write(secret, to: output, as: currentUser())
    #expect(outcome.parentIsPrivate)

    for level in ["a", "a/b", "a/b/c", "a/b/c/passphrase.txt"] {
      var info = stat()
      #expect(lstat(base.appending(path: level).path, &info) == 0)
      #expect(info.st_uid == getuid())
    }
    for level in ["a", "a/b", "a/b/c"] {
      #expect(try posixMode(of: base.appending(path: level)) == 0o700)
    }
  }

  @Test func refusesToOverwriteAFileOwnedBySomeoneElse() throws {
    let base = try makeTemporaryDirectory()
    defer { try? FileManager.default.removeItem(at: base) }
    let output = base.appending(path: "passphrase.txt")
    let original = Data("someone else's file".utf8)
    try original.write(to: output)
    chmod(output.path, 0o644)

    // The test process owns the file; acting for another uid, the writer must
    // treat it as somebody else's and leave it untouched.
    let other = InvokingUser(
      name: "other", uid: getuid() &+ 1, gid: getgid(),
      homeDirectory: FileManager.default.homeDirectoryForCurrentUser)
    let secret = try PassphraseSecret(bytes: bytesFromHex(passphraseHex))
    #expect(throws: SecretOutputError.notOwnedByUser) {
      try SecretOutputWriter().write(secret, to: output, overwrite: true, as: other)
    }
    #expect(try Data(contentsOf: output) == original)
    #expect(try posixMode(of: output) == 0o644)
  }

  @Test func refusesASymlinkedParentFolder() throws {
    let base = try makeTemporaryDirectory()
    defer { try? FileManager.default.removeItem(at: base) }
    let real = base.appending(path: "real", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: real, withIntermediateDirectories: false)
    let linked = base.appending(path: "linked")
    try FileManager.default.createSymbolicLink(at: linked, withDestinationURL: real)

    let secret = try PassphraseSecret(bytes: bytesFromHex(passphraseHex))
    #expect(throws: SecretOutputError.symbolicLinkParent) {
      try SecretOutputWriter().write(secret, to: linked.appending(path: "passphrase.txt"))
    }
    #expect(try FileManager.default.contentsOfDirectory(atPath: real.path).isEmpty)
  }

  @Test func leavesFoldersTheUserCannotWriteAlone() throws {
    guard geteuid() != 0 else { return }
    let base = try makeTemporaryDirectory()
    defer {
      chmod(base.appending(path: "locked").path, 0o700)
      try? FileManager.default.removeItem(at: base)
    }
    let locked = base.appending(path: "locked", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: locked, withIntermediateDirectories: false)
    chmod(locked.path, 0o500)

    let secret = try PassphraseSecret(bytes: bytesFromHex(passphraseHex))
    // Missing levels below a folder the user cannot write are not created.
    #expect(throws: SecretOutputError.self) {
      try SecretOutputWriter().write(secret, to: locked.appending(path: "new/passphrase.txt"))
    }
    #expect(throws: SecretOutputError.self) {
      try SecretOutputWriter().write(secret, to: locked.appending(path: "passphrase.txt"))
    }
    // A system folder outside the user's rights stays untouched.
    #expect(throws: SecretOutputError.self) {
      try SecretOutputWriter().write(
        secret, to: URL(fileURLWithPath: "/private/etc/greenbubbles-test-\(UUID().uuidString)"))
    }
  }

  @Test func reportsAFolderTheRustReaderWouldRefuse() throws {
    let base = try makeTemporaryDirectory()
    defer { try? FileManager.default.removeItem(at: base) }
    chmod(base.path, 0o755)
    let secret = try PassphraseSecret(bytes: bytesFromHex(passphraseHex))
    let outcome = try SecretOutputWriter().write(secret, to: base.appending(path: "passphrase.txt"))
    #expect(!outcome.parentIsPrivate)
  }

  @Test func refusesDotComponentsBeforeTouchingTheDisk() throws {
    let base = try makeTemporaryDirectory()
    defer { try? FileManager.default.removeItem(at: base) }
    let secret = try PassphraseSecret(bytes: bytesFromHex(passphraseHex))
    let dotted = URL(fileURLWithPath: base.path + "/new/../passphrase.txt")
    #expect(throws: SecretOutputError.dotComponents) {
      try SecretOutputWriter().write(secret, to: dotted)
    }
    #expect(try FileManager.default.contentsOfDirectory(atPath: base.path).isEmpty)
  }

  @Test func refusesARelativePath() throws {
    let secret = try PassphraseSecret(bytes: bytesFromHex(passphraseHex))
    #expect(throws: SecretOutputError.relativePath) {
      try SecretOutputWriter().write(secret, to: URL(string: "relative/passphrase.txt")!)
    }
  }

  private func currentUser() -> InvokingUser {
    InvokingUser(
      name: NSUserName(), uid: geteuid(), gid: getegid(),
      homeDirectory: FileManager.default.homeDirectoryForCurrentUser)
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

    try Data(repeating: 0, count: 128).write(to: output)
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

  @Test(arguments: [false, true])
  func refusesLinkedOutputsWithoutChangingTheTarget(symbolic: Bool) throws {
    let base = try makeTemporaryDirectory()
    defer { try? FileManager.default.removeItem(at: base) }
    let target = base.appending(path: "fixture.txt")
    let output = base.appending(path: "linked-output.txt")
    let original = Data("unchanged test content".utf8)
    try original.write(to: target)
    try FileManager.default.setAttributes([.posixPermissions: 0o640], ofItemAtPath: target.path)
    if symbolic {
      try FileManager.default.createSymbolicLink(at: output, withDestinationURL: target)
    } else {
      try FileManager.default.linkItem(at: target, to: output)
    }

    let secret = try PassphraseSecret(bytes: bytesFromHex(passphraseHex))
    #expect(throws: SecretOutputError.self) {
      try SecretOutputWriter().write(secret, to: output, overwrite: true)
    }
    #expect(try Data(contentsOf: target) == original)
    #expect(try posixMode(of: target) == 0o640)
  }

  @Test func refusesAnOpenFIFO() throws {
    let base = try makeTemporaryDirectory()
    defer { try? FileManager.default.removeItem(at: base) }
    let output = base.appending(path: "fixture.fifo")
    guard Darwin.mkfifo(output.path, 0o600) == 0 else {
      throw NSError(domain: NSPOSIXErrorDomain, code: Int(errno))
    }
    let reader = Darwin.open(output.path, O_RDONLY | O_NONBLOCK)
    guard reader >= 0 else { throw NSError(domain: NSPOSIXErrorDomain, code: Int(errno)) }
    defer { Darwin.close(reader) }

    let secret = try PassphraseSecret(bytes: bytesFromHex(passphraseHex))
    #expect(throws: SecretOutputError.unsafeOutputFile) {
      try SecretOutputWriter().write(secret, to: output, overwrite: true)
    }
  }

  @Test(arguments: [0o755, 0o1777])
  func preservesPermissionsOnExistingParent(mode: Int) throws {
    let base = try makeTemporaryDirectory()
    defer { try? FileManager.default.removeItem(at: base) }
    let parent = base.appending(path: "loose", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: parent, withIntermediateDirectories: false)
    try FileManager.default.setAttributes([.posixPermissions: mode], ofItemAtPath: parent.path)

    let secret = try PassphraseSecret(bytes: bytesFromHex(passphraseHex))
    let output = parent.appending(path: "passphrase.txt")
    try SecretOutputWriter().write(secret, to: output)

    #expect(try posixMode(of: parent) == Int32(mode))
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
    return Int32(metadata.st_mode & 0o7777)
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
