import CommonCrypto
import Foundation
import Testing

@testable import GreenBubblesAcquire

struct LLDBCaptureScriptTests {
  private let commandOrder = [
    "settings set target.preload-symbols false",
    "settings set target.skip-prologue false",
    "process attach -p 4242",
    "breakpoint set -n CCKeyDerivationPBKDF",
    "breakpoint command add 1",
    "memory read --size 1 --count 32 --format x",
    "continue\n",
    "DONE\n",
    "process continue\n",
  ]

  @Test func arm64ScriptStopsOnlyOnTheSQLCipherDerivation() {
    let script = LLDBCaptureScript.script(processID: 4242, architecture: .arm64)
    let condition = "$x0 == 2 && $x2 == 32 && $x4 == 16 && $x5 == 5 && $x6 == 256000"
    #expect(
      script.contains("breakpoint set -n CCKeyDerivationPBKDF -c '\(condition)'")
    )
    #expect(LLDBTargetArchitecture.arm64.derivationCondition == condition)
    #expect(script.contains("memory read --size 1 --count 32 --format x $x1"))
    #expect(script.hasSuffix("process continue\n"))
    #expect(!script.contains("\ndetach\n"))
    #expect(!script.contains("\nquit\n"))
    assertCommandOrder(in: script)
  }

  @Test func x86_64ScriptReadsRoundsFromTheEntryStackSlot() {
    let script = LLDBCaptureScript.script(processID: 4242, architecture: .x86_64)
    let condition =
      "$rdi == 2 && $rdx == 32 && $r8 == 16 && $r9 == 5 && *(unsigned int *)($rsp + 8) == 256000"
    #expect(
      script.contains("breakpoint set -n CCKeyDerivationPBKDF -c '\(condition)'")
    )
    #expect(LLDBTargetArchitecture.x86_64.derivationCondition == condition)
    #expect(script.contains("memory read --size 1 --count 32 --format x $rsi"))
    assertCommandOrder(in: script)
  }

  @Test func registersMatchTheCCKeyDerivationPBKDFABI() {
    #expect(LLDBTargetArchitecture.arm64.passwordRegister == "x1")
    #expect(LLDBTargetArchitecture.arm64.lengthRegister == "x2")
    #expect(LLDBTargetArchitecture.arm64.algorithmRegister == "x0")
    #expect(LLDBTargetArchitecture.arm64.saltLengthRegister == "x4")
    #expect(LLDBTargetArchitecture.arm64.prfRegister == "x5")
    #expect(LLDBTargetArchitecture.arm64.roundsExpression == "$x6")

    #expect(LLDBTargetArchitecture.x86_64.passwordRegister == "rsi")
    #expect(LLDBTargetArchitecture.x86_64.lengthRegister == "rdx")
    #expect(LLDBTargetArchitecture.x86_64.algorithmRegister == "rdi")
    #expect(LLDBTargetArchitecture.x86_64.saltLengthRegister == "r8")
    #expect(LLDBTargetArchitecture.x86_64.prfRegister == "r9")
    #expect(LLDBTargetArchitecture.x86_64.roundsExpression == "*(unsigned int *)($rsp + 8)")
  }

  @Test func derivationShapeUsesTheSQLCipherConstants() {
    let call = SQLCipherAccountKeyDerivation.expected
    #expect(call.algorithm == 2)
    #expect(call.algorithm == Int(kCCPBKDF2))
    #expect(call.passwordLength == 32)
    #expect(call.passwordLength == PassphraseSecret.byteCount)
    #expect(call.saltLength == 16)
    #expect(call.saltLength == SQLCipherKeyVerifier.saltSize)
    #expect(call.prf == 5)
    #expect(call.prf == Int(kCCPRFHmacAlgSHA512))
    #expect(Int(kCCPRFHmacAlgSHA256) == 3)
    #expect(call.rounds == 256_000)
    #expect(call.rounds == Int(SQLCipherKeyVerifier.passphraseRounds))
  }

  @Test func unrelatedPBKDFCallDoesNotMatchTheAccountKeyShape() {
    let account = SQLCipherAccountKeyDerivation.expected
    let unrelated = SQLCipherAccountKeyDerivation(
      algorithm: Int(kCCPBKDF2),
      passwordLength: 32,
      saltLength: 16,
      prf: Int(kCCPRFHmacAlgSHA256),
      rounds: 1_000
    )
    #expect(account != unrelated)
    #expect(
      account
        != SQLCipherAccountKeyDerivation(
          algorithm: account.algorithm,
          passwordLength: 31,
          saltLength: account.saltLength,
          prf: account.prf,
          rounds: account.rounds
        ))
    #expect(
      account
        != SQLCipherAccountKeyDerivation(
          algorithm: account.algorithm,
          passwordLength: account.passwordLength,
          saltLength: 8,
          prf: account.prf,
          rounds: account.rounds
        ))
    #expect(
      account
        != SQLCipherAccountKeyDerivation(
          algorithm: 1,
          passwordLength: account.passwordLength,
          saltLength: account.saltLength,
          prf: account.prf,
          rounds: account.rounds
        ))
    #expect(
      account
        != SQLCipherAccountKeyDerivation(
          algorithm: account.algorithm,
          passwordLength: account.passwordLength,
          saltLength: account.saltLength,
          prf: account.prf,
          rounds: 2
        ))
  }

  private func assertCommandOrder(in script: String) {
    var lastIndex = script.startIndex
    for command in commandOrder {
      guard let range = script.range(of: command, range: lastIndex..<script.endIndex) else {
        Issue.record("generated script is missing: \(command)")
        return
      }
      #expect(range.lowerBound >= lastIndex)
      lastIndex = range.upperBound
    }
  }
}
