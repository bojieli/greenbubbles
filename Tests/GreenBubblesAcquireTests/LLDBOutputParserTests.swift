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
    #expect(LLDBOutputParser.parsePassphrases(from: output).count == 1)
  }

  @Test func parsesSuccessiveMemoryReadsAsSeparateCandidates() {
    let second = """
      0x16fdfde00: 0x01 0x01 0x01 0x01 0x01 0x01 0x01 0x01
      0x16fdfde08: 0x02 0x02 0x02 0x02 0x02 0x02 0x02 0x02
      0x16fdfde10: 0x03 0x03 0x03 0x03 0x03 0x03 0x03 0x03
      0x16fdfde18: 0x04 0x04 0x04 0x04 0x04 0x04 0x04 0x04
      """
    let output = """
      \(hexdumpLines)
      (lldb) continue
      Process 69454 resuming
      \(second)
      """
    let candidates = LLDBOutputParser.parsePassphrases(from: output)
    #expect(candidates.count == 2)
    #expect(candidates[0] == expectedBytes)
    #expect(
      candidates[1] == [UInt8](repeating: 0x01, count: 8)
        + [UInt8](repeating: 0x02, count: 8)
        + [UInt8](repeating: 0x03, count: 8)
        + [UInt8](repeating: 0x04, count: 8))
    #expect(LLDBOutputParser.parsePassphrase(from: output) == expectedBytes)
  }

  @Test func partialSecondReadDoesNotMixWithTheFirstCandidate() {
    let output =
      hexdumpLines + "\n" + """
        0x16fdfde00: 0x01 0x01 0x01 0x01 0x01 0x01 0x01 0x01
        0x16fdfde08: 0x02 0x02 0x02 0x02 0x02 0x02 0x02 0x02
        """
    let candidates = LLDBOutputParser.parsePassphrases(from: output)
    #expect(candidates.count == 1)
    #expect(candidates[0] == expectedBytes)
  }

  @Test func shortReadEndedByAnErrorDoesNotShiftTheNextRead() {
    let output = """
      (lldb)  memory read --size 1 --count 32 --format x $x1
      0x16fdfddd8: 0xaa 0xaa 0xaa 0xaa 0xaa 0xaa 0xaa 0xaa
      0x16fdfdde0: 0xbb 0xbb 0xbb 0xbb 0xbb 0xbb 0xbb 0xbb
      error: memory read failed for 0x16fdfdde8
      (lldb)  continue
      Process 69454 resuming
      (lldb)  memory read --size 1 --count 32 --format x $x1
      \(hexdumpLines)
      """
    #expect(LLDBOutputParser.parsePassphrases(from: output) == [expectedBytes])
  }

  @Test func shortReadFollowedDirectlyByANewAddressIsDiscarded() {
    // No separator line between the reads: the address jump ends the first.
    let output = """
      0x200000000: 0xaa 0xaa 0xaa 0xaa 0xaa 0xaa 0xaa 0xaa
      0x200000008: 0xbb 0xbb 0xbb 0xbb 0xbb 0xbb 0xbb 0xbb
      \(hexdumpLines)
      """
    #expect(LLDBOutputParser.parsePassphrases(from: output) == [expectedBytes])
  }

  @Test func streamKeepsPartialLinesAcrossChunksButNotShortReads() {
    var stream = LLDBMemoryReadStream()
    let full = Array((hexdumpLines + "\n").utf8)
    var candidates: [[UInt8]] = []
    // Split the output mid-line, mid-token.
    for range in [0..<20, 20..<75, 75..<full.count] {
      var chunk = Array(full[range])
      candidates += stream.consume(&chunk).candidates
      #expect(chunk.isEmpty)
    }
    #expect(candidates == [expectedBytes])

    var short = Array("0x300000000: 0x01 0x02 0x03 0x04 0x05 0x06 0x07 0x08\n".utf8)
    #expect(stream.consume(&short).candidates.isEmpty)
    var error = Array("error: memory read failed\n".utf8)
    #expect(stream.consume(&error).candidates.isEmpty)
    var next = Array((hexdumpLines + "\n").utf8)
    #expect(stream.consume(&next).candidates == [expectedBytes])
  }

  @Test func discardDropsAnUnterminatedCompleteReadWithoutACandidate() {
    var stream = LLDBMemoryReadStream()
    // The chunk ends after the last hex digit, before the newline.
    var chunk = Array(hexdumpLines.utf8)
    #expect(stream.consume(&chunk).candidates.isEmpty)
    stream.discard()
    #expect(stream.finish().candidates.isEmpty)
  }

  @Test func streamReportsAttachFailureAndExitPerLine() {
    var stream = LLDBMemoryReadStream()
    var chunk = Array(
      "error: attach failed: attach failed (Not allowed to attach to process.)\n".utf8)
    let drained = stream.consume(&chunk)
    #expect(drained.attachFailure?.contains("attach failed") == true)
    var exit = Array("Process 69454 exited with status = 0 (0x00000000)".utf8)
    #expect(!stream.consume(&exit).targetExited)
    #expect(stream.finish().targetExited)
  }

  @Test func detectsAttachFailure() {
    let output = """
      (lldb) process attach -p 66097
      error: attach failed: attach failed (Not allowed to attach to process.)
      (lldb)
      """
    let detail = LLDBOutputParser.detectAttachFailure(in: output)
    #expect(detail != nil)
    #expect(detail?.contains("attach failed") == true)
  }

  @Test func ignoresUnrelatedErrorsForAttachFailure() {
    #expect(LLDBOutputParser.detectAttachFailure(in: "error: Process must be launched.") == nil)
    #expect(
      LLDBOutputParser.detectAttachFailure(in: "Breakpoint 1: no locations (pending).") == nil)
    #expect(LLDBOutputParser.detectAttachFailure(in: "") == nil)
  }

  @Test func detectsTargetExitAndDetach() {
    #expect(
      LLDBOutputParser.detectTargetExit(in: "Process 69454 exited with status = 0 (0x00000000)"))
    #expect(LLDBOutputParser.detectTargetExit(in: "Process 69454 detached"))
    #expect(!LLDBOutputParser.detectTargetExit(in: "Process 69454 resuming"))
    #expect(!LLDBOutputParser.detectTargetExit(in: "Process 69454 launched: '/tmp/x' (arm64)"))
    #expect(!LLDBOutputParser.detectTargetExit(in: ""))
  }
}
