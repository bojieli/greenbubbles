import CryptoKit
import Darwin
import Foundation
import Testing

@testable import GreenBubblesAcquire

/// The capture command's verify-then-write step, driven by a stand-in lldb
/// (a shell script that prints `memory read` output) against synthetic
/// SQLCipher pages and a real temporary output file.
struct VerifiedPassphraseCaptureTests {
  // Synthetic fixture patterns, not real secrets. The test database opens
  // with `account`; `unrelated` opens nothing.
  private let account = [UInt8](repeating: 0x22, count: 32)
  private let unrelated = [UInt8](repeating: 0x11, count: 32)

  // MARK: No-write guarantee on the filesystem

  enum LLDBEnding: CaseIterable, Sendable {
    /// lldb stays attached until the capture deadline passes.
    case timesOut
    /// lldb exits on its own before the deadline.
    case exits
  }

  @Test(arguments: LLDBEnding.allCases, [false, true])
  func rejectedCandidateLeavesTheOutputUntouched(ending: LLDBEnding, outputExists: Bool) throws {
    let base = try makeTemporaryDirectory()
    defer { try? FileManager.default.removeItem(at: base) }
    let inventory = try makeInventory(in: base, openingWith: account)
    let output = base.appending(path: "keys/passphrase.txt")
    let original = Data("existing key file\n".utf8)
    if outputExists {
      try FileManager.default.createDirectory(
        at: output.deletingLastPathComponent(), withIntermediateDirectories: true)
      try original.write(to: output)
    }
    let lldb = try makeFakeLLDB(in: base, printing: [unrelated], thenExit: ending == .exits)

    #expect(throws: (any Error).self) {
      try VerifiedPassphraseCapture.captureAndWrite(
        inventory: inventory,
        deadline: .distantFuture,
        to: output,
        overwrite: true,
        as: currentUser()
      ) { accept in
        try runCapture(lldb, deadline: Date().addingTimeInterval(1.5), accept: accept)
      }
    }

    if outputExists {
      #expect(try Data(contentsOf: output) == original)
    } else {
      #expect(!FileManager.default.fileExists(atPath: output.path))
      #expect(!FileManager.default.fileExists(atPath: output.deletingLastPathComponent().path))
    }
  }

  /// Control for the test above: the same harness does write an accepted key.
  @Test func acceptedCandidateIsWritten() throws {
    let base = try makeTemporaryDirectory()
    defer { try? FileManager.default.removeItem(at: base) }
    let inventory = try makeInventory(in: base, openingWith: account)
    let output = base.appending(path: "keys/passphrase.txt")
    try FileManager.default.createDirectory(
      at: output.deletingLastPathComponent(), withIntermediateDirectories: true)
    try Data("existing key file\n".utf8).write(to: output)
    let lldb = try makeFakeLLDB(in: base, printing: [unrelated, account], thenExit: false)

    let written = try VerifiedPassphraseCapture.captureAndWrite(
      inventory: inventory,
      deadline: .distantFuture,
      to: output,
      overwrite: true,
      as: currentUser()
    ) { accept in
      try runCapture(lldb, deadline: Date().addingTimeInterval(30), accept: accept)
    }

    #expect(written.verifiedSaltCount == 1)
    let expected = account.map { String(format: "%02x", $0) }.joined() + "\n"
    #expect(String(decoding: try Data(contentsOf: output), as: UTF8.self) == expected)
  }

  // MARK: lldb exit after a rejection

  @Test func lldbExitAfterARejectionIsReportedAsTheExit() throws {
    let base = try makeTemporaryDirectory()
    defer { try? FileManager.default.removeItem(at: base) }
    let lldb = try makeFakeLLDB(in: base, printing: [unrelated], thenExit: true)
    var offered = 0
    #expect(throws: CaptureError.exitedWithoutCapture(rejectedCandidateCount: 1)) {
      try runCapture(lldb, deadline: Date().addingTimeInterval(30)) { _ in
        offered += 1
        return false
      }
    }
    #expect(offered == 1)
  }

  @Test func exitDescriptionKeepsTheRejectionAsContext() {
    #expect(
      CaptureError.exitedWithoutCapture().description
        == "lldb exited before a passphrase was captured")
    #expect(
      CaptureError.exitedWithoutCapture(rejectedCandidateCount: 2).description
        == "lldb exited before a passphrase was captured (earlier, 2 captured candidates "
        + "did not open your databases). No output was created or replaced.")
  }

  // MARK: Incomplete verification

  @Test func verificationCutShortByTheDeadlineIsReportedAsIncomplete() throws {
    let base = try makeTemporaryDirectory()
    defer { try? FileManager.default.removeItem(at: base) }
    let inventory = try makeInventory(in: base, openingWith: account, extraSalts: 2)
    // The deadline passes after the first salt is checked.
    var clock = 0.0
    #expect(
      throws: CaptureVerificationError.incomplete(
        checkedSaltCount: 1, distinctSaltCount: 3, fullyRejectedCount: 0)
    ) {
      try VerifiedPassphraseCapture.captureAndWrite(
        inventory: inventory,
        deadline: Date(timeIntervalSinceReferenceDate: 5),
        to: base.appending(path: "passphrase.txt"),
        overwrite: false,
        as: currentUser(),
        now: {
          defer { clock += 10 }
          return Date(timeIntervalSinceReferenceDate: clock)
        },
        capture: { accept in try offer([unrelated], to: accept) }
      )
    }
  }

  @Test func incompleteCheckAfterAFullRejectionCountsTheEarlierOne() throws {
    let base = try makeTemporaryDirectory()
    defer { try? FileManager.default.removeItem(at: base) }
    let inventory = try makeInventory(in: base, openingWith: account, extraSalts: 1)
    // Two salts per full check; the deadline passes during the second candidate.
    var calls = 0
    #expect(
      throws: CaptureVerificationError.incomplete(
        checkedSaltCount: 0, distinctSaltCount: 2, fullyRejectedCount: 1)
    ) {
      try VerifiedPassphraseCapture.captureAndWrite(
        inventory: inventory,
        deadline: Date(timeIntervalSinceReferenceDate: 100),
        to: base.appending(path: "passphrase.txt"),
        overwrite: false,
        as: currentUser(),
        now: {
          calls += 1
          return Date(timeIntervalSinceReferenceDate: calls <= 2 ? 0 : 200)
        },
        capture: { accept in try offer([unrelated, unrelated], to: accept) }
      )
    }
  }

  @Test func completeChecksWithNoMatchAreReportedAsNoMatch() throws {
    let base = try makeTemporaryDirectory()
    defer { try? FileManager.default.removeItem(at: base) }
    let inventory = try makeInventory(in: base, openingWith: account, extraSalts: 1)
    #expect(throws: CaptureVerificationError.noMatch(distinctSaltCount: 2, databaseCount: 2)) {
      try VerifiedPassphraseCapture.captureAndWrite(
        inventory: inventory,
        deadline: .distantFuture,
        to: base.appending(path: "passphrase.txt"),
        overwrite: false,
        as: currentUser()
      ) { accept in
        try offer([unrelated], to: accept)
      }
    }
    #expect(!FileManager.default.fileExists(atPath: base.appending(path: "passphrase.txt").path))
  }

  @Test func incompleteDescriptionDoesNotClaimAMismatch() {
    let text = CaptureVerificationError.incomplete(
      checkedSaltCount: 1, distinctSaltCount: 3, fullyRejectedCount: 1
    ).description
    #expect(!text.contains("no match"))
    #expect(text.contains("checking 1 of 3 distinct database salts"))
    #expect(text.contains("1 earlier candidate matched none of the salts"))
    #expect(text.contains("No output was created or replaced."))
  }

  @Test func saltVerificationReportsHowManySaltsWereChecked() {
    var clock = 0.0
    let tally = DatabaseSaltInventory.tallyVerified(
      [1, 2, 3],
      shouldStop: { clock >= 15 },
      verify: { salt in
        clock += 10
        return salt == 1
      }
    )
    #expect(tally == SaltVerification(verifiedCount: 1, checkedCount: 2, distinctSaltCount: 3))
    #expect(!tally.isComplete)
  }

  // MARK: Helpers

  /// Stands in for `LLDBPassphraseCapture.capture`: offers each candidate,
  /// then reports what the real loop reports once all were rejected.
  private func offer(
    _ candidates: [[UInt8]],
    to accept: (PassphraseSecret) throws -> Bool
  ) throws -> PassphraseSecret {
    for bytes in candidates {
      let secret = try PassphraseSecret(bytes: bytes)
      if try accept(secret) { return secret }
    }
    throw CaptureError.noAcceptableCandidate
  }

  /// One attach of the real capture loop, against the stand-in lldb.
  private func runCapture(
    _ capture: LLDBPassphraseCapture,
    deadline: Date,
    accept: (PassphraseSecret) throws -> Bool
  ) throws -> PassphraseSecret {
    var rejectedCandidateCount = 0
    var exitObservedAt: UInt64?
    return try capture.runSingleCapture(
      processID: 1,
      architecture: .current,
      deadline: deadline,
      accept: accept,
      rejectedCandidateCount: &rejectedCandidateCount,
      exitObservedAt: &exitObservedAt
    )
  }

  /// A script that waits for the capture script on stdin (so the write never
  /// meets a closed pipe), prints a `memory read` for each candidate, and then
  /// exits or stays attached.
  private func makeFakeLLDB(
    in base: URL,
    printing candidates: [[UInt8]],
    thenExit: Bool
  ) throws -> LLDBPassphraseCapture {
    let output = candidates.enumerated().map { index, bytes in
      CaptureCandidateCursorTests.hexdump(of: bytes, at: UInt64(0x1000 * (index + 1)))
        + "(lldb) continue\n"
    }.joined()
    let script =
      "#!/bin/sh\nhead -c 1 >/dev/null\nprintf '%s' '\(output)'\n"
      + (thenExit ? "exit 0\n" : "exec sleep 60\n")
    let url = base.appending(path: "fake-lldb-\(UUID().uuidString)")
    try Data(script.utf8).write(to: url)
    #expect(chmod(url.path, 0o700) == 0)
    return LLDBPassphraseCapture(lldbPath: url.path)
  }

  /// A database root with one database that `passphrase` opens and
  /// `extraSalts` more (distinct salts) that nothing opens.
  private func makeInventory(
    in base: URL,
    openingWith passphrase: [UInt8],
    extraSalts: Int = 0
  ) throws -> DatabaseSaltInventory {
    let root = base.appending(path: "db_storage", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    let salt = [UInt8](repeating: 0x5A, count: 16)
    try Data(try validPage(salt: salt, passphrase: passphrase))
      .write(to: root.appending(path: "a_account.db"))
    for extra in 0..<extraSalts {
      try Data(syntheticPage(salt: [UInt8](repeating: UInt8(0x60 + extra), count: 16)))
        .write(to: root.appending(path: "b_other_\(extra).db"))
    }
    return try DatabaseSaltInventory(root: root)
  }

  private func syntheticPage(salt: [UInt8]) -> [UInt8] {
    var page = [UInt8](repeating: 0, count: 4096)
    page.replaceSubrange(0..<16, with: salt)
    for index in 16..<4032 {
      page[index] = UInt8(truncatingIfNeeded: index &* 31 &+ 7)
    }
    return page
  }

  /// A page 1 carrying the SQLCipher 4 HMAC for `passphrase`.
  private func validPage(salt: [UInt8], passphrase: [UInt8]) throws -> [UInt8] {
    var page = syntheticPage(salt: salt)
    let encryptionKey = try SQLCipherKeyVerifier.deriveEncryptionKey(
      passphrase: try PassphraseSecret(bytes: passphrase),
      salt: salt
    )
    let macKey = try encryptionKey.withUnsafeBytes { password in
      try SQLCipherKeyVerifier.pbkdf2(
        password: password, salt: salt.map { $0 ^ 0x3A }, rounds: 2)
    }
    var hmac = HMAC<SHA512>(key: SymmetricKey(data: macKey))
    hmac.update(data: Data(page[16..<4032]))
    var pageNumber = UInt32(1).littleEndian
    withUnsafeBytes(of: &pageNumber) { hmac.update(data: Data($0)) }
    page.replaceSubrange(4032..<4096, with: Array(hmac.finalize()))
    return page
  }

  private func currentUser() -> InvokingUser {
    InvokingUser(
      name: NSUserName(), uid: geteuid(), gid: getegid(),
      homeDirectory: FileManager.default.homeDirectoryForCurrentUser)
  }

  private func makeTemporaryDirectory() throws -> URL {
    let url = FileManager.default.temporaryDirectory.appending(
      path: "greenbubbles-acquire-verified-capture-tests-\(UUID().uuidString)",
      directoryHint: .isDirectory
    )
    try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
    return url
  }
}
