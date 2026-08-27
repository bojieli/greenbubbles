import Foundation
import Testing

@testable import GreenBubblesAcquire

struct LLDBCaptureScriptTests {
  private let provenOrdering = [
    "settings set target.preload-symbols false",
    "process attach -p 4242",
    "breakpoint set -n CCKeyDerivationPBKDF",
    "breakpoint command add 1",
    "memory read --size 1 --count 32 --format x",
    "detach",
    "quit",
    "DONE",
    "process continue",
  ]

  @Test func arm64ScriptUsesArgumentRegistersInProvenOrder() {
    let script = LLDBCaptureScript.script(processID: 4242, architecture: .arm64)
    #expect(script.contains("breakpoint set -n CCKeyDerivationPBKDF -c '$x2 == 32'"))
    #expect(script.contains("memory read --size 1 --count 32 --format x $x1"))
    #expect(script.hasSuffix("process continue\n"))
    assertProvenOrdering(in: script)
  }

  @Test func x86_64ScriptUsesArgumentRegistersInProvenOrder() {
    let script = LLDBCaptureScript.script(processID: 4242, architecture: .x86_64)
    #expect(script.contains("breakpoint set -n CCKeyDerivationPBKDF -c '$rdx == 32'"))
    #expect(script.contains("memory read --size 1 --count 32 --format x $rsi"))
    assertProvenOrdering(in: script)
  }

  @Test func registersMatchArchitecture() {
    #expect(LLDBTargetArchitecture.arm64.passwordRegister == "x1")
    #expect(LLDBTargetArchitecture.arm64.lengthRegister == "x2")
    #expect(LLDBTargetArchitecture.x86_64.passwordRegister == "rsi")
    #expect(LLDBTargetArchitecture.x86_64.lengthRegister == "rdx")
  }

  private func assertProvenOrdering(in script: String) {
    var lastIndex = script.startIndex
    for command in provenOrdering {
      guard let range = script.range(of: command) else {
        Issue.record("generated script is missing: \(command)")
        return
      }
      #expect(range.lowerBound >= lastIndex)
      lastIndex = range.lowerBound
    }
  }
}
