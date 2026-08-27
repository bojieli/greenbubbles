import Foundation
import Testing

@testable import GreenBubblesAcquire

struct LLDBOutputParserTests {
  // Hexdump lines copied from the real lldb output captured during the
  // 2026-08-27 mechanism test (.tmp/lldb-mech-test/lldb_out.txt). The bytes
  // are the synthetic pattern 00 11 22 ... fe 0f, not a real secret.
  private let hexdumpLines = """
    0x16fdfddd8: 0x00 0x11 0x22 0x33 0x44 0x55 0x66 0x77
    0x16fdfdde0: 0x88 0x99 0xaa 0xbb 0xcc 0xdd 0xee 0xff
    0x16fdfdde8: 0x10 0x21 0x32 0x43 0x54 0x65 0x76 0x87
    0x16fdfddf0: 0x98 0xa9 0xba 0xcb 0xdc 0xed 0xfe 0x0f
    """

  private var expectedBytes: [UInt8] {
    [
      0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77,
      0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF,
      0x10, 0x21, 0x32, 0x43, 0x54, 0x65, 0x76, 0x87,
      0x98, 0xA9, 0xBA, 0xCB, 0xDC, 0xED, 0xFE, 0x0F,
    ]
  }

  @Test func parsesPassphraseFromRealisticLLDBOutput() {
    let output = """
      (lldb) breakpoint set -n CCKeyDerivationPBKDF -c '$x2 == 32'
      Breakpoint 1: no locations (pending).
      (lldb) breakpoint command add 1
      (lldb) process continue
      Process 69454 resuming
      1 location added to breakpoint 1
      (lldb)  memory read --size 1 --count 32 --format x $x1
      \(hexdumpLines)
      (lldb)  process continue
      Process 69454 resuming
      """
    #expect(LLDBOutputParser.parsePassphrase(from: output) == expectedBytes)
  }

  @Test func ignoresDisassemblyNoiseThatLooksHexadecimal() {
    let output = """
      dyld`_dyld_start:
      ->  0x1000109c0 <+0>:  mov    x0, sp
          0x1000109c4 <+4>:  and    sp, x0, #0xfffffffffffffff0
      \(hexdumpLines)
      """
    #expect(LLDBOutputParser.parsePassphrase(from: output) == expectedBytes)
  }

  @Test func returnsNilForIncompleteCapture() {
    let threeLines = """
      0x16fdfddd8: 0x00 0x11 0x22 0x33 0x44 0x55 0x66 0x77
      0x16fdfdde0: 0x88 0x99 0xaa 0xbb 0xcc 0xdd 0xee 0xff
      0x16fdfdde8: 0x10 0x21 0x32 0x43 0x54 0x65 0x76 0x87
      """
    #expect(LLDBOutputParser.parsePassphrase(from: threeLines) == nil)
  }

  @Test func returnsNilForMalformedInput() {
    #expect(LLDBOutputParser.parsePassphrase(from: "") == nil)
    #expect(LLDBOutputParser.parsePassphrase(from: "no hexdump here\nat all") == nil)
    #expect(LLDBOutputParser.parsePassphrase(from: "0x1234: 0x00 0x11 0xzz 0x33") == nil)
    #expect(LLDBOutputParser.parsePassphrase(from: "0x1234: 0x00 0x11 0x2 0x33") == nil)
  }

  @Test func ignoresUppercaseHexLines() {
    let uppercase = """
      0x16FDFDDD8: 0x00 0x11 0x22 0x33 0x44 0x55 0x66 0x77
      0x16FDFDDE0: 0x88 0x99 0xAA 0xBB 0xCC 0xDD 0xEE 0xFF
      0x16FDFDDE8: 0x10 0x21 0x32 0x43 0x54 0x65 0x76 0x87
      0x16FDFDDF0: 0x98 0xA9 0xBA 0xCB 0xDC 0xED 0xFE 0x0F
      """
    #expect(LLDBOutputParser.parsePassphrase(from: uppercase) == nil)
  }

  @Test func keepsOnlyTheFirstThirtyTwoBytes() {
    let output =
      hexdumpLines + "\n" + """
        0x16fdfddf8: 0xde 0xad 0xbe 0xef 0xde 0xad 0xbe 0xef
        """
    #expect(LLDBOutputParser.parsePassphrase(from: output) == expectedBytes)
  }
}
