// Derived from wcdb-key-tool (https://github.com/TANGandXUE/wcdb-key-tool), MIT license.

import Foundation

/// Parses lldb `memory read --size 1 --count 32 --format x` hexdump output into
/// captured 32-byte passphrases.
///
/// A byte line looks like `0x16fdfddd8: 0x00 0x11 0x22 0x33 0x44 0x55 0x66 0x77`
/// (lowercase hexadecimal, one address prefix, byte groups only). Any other
/// lldb output is ignored. Each `memory read --count 32` contributes exactly
/// 32 bytes; successive reads become successive candidates. A read that ends
/// before 32 bytes is discarded and never mixed into another read. See
/// `LLDBMemoryReadStream` for how read boundaries are found.
public enum LLDBOutputParser {
  public static func parsePassphrase(from output: String) -> [UInt8]? {
    parsePassphrases(from: output).first
  }

  public static func parsePassphrases(from output: String) -> [[UInt8]] {
    var stream = LLDBMemoryReadStream()
    var bytes = Array(output.utf8)
    return stream.consume(&bytes).candidates + stream.finish().candidates
  }

  /// Detects a failed `process attach` so the capture can fail fast instead of
  /// stalling until the timeout. Returns lldb's error detail when found.
  public static func detectAttachFailure(in output: String) -> String? {
    for rawLine in output.split(whereSeparator: \.isNewline) {
      let line = rawLine.trimmingCharacters(in: .whitespaces)
      guard line.hasPrefix("error:") else { continue }
      let detail = String(line.dropFirst("error:".count))
        .trimmingCharacters(in: .whitespaces)
      if detail.contains("attach") || detail.contains("Not allowed") {
        return detail
      }
    }
    return nil
  }

  /// Detects the target process exiting (or being detached) before the
  /// breakpoint hit, e.g. when WeChat's logout flow restarts its main process.
  public static func detectTargetExit(in output: String) -> Bool {
    for rawLine in output.split(whereSeparator: \.isNewline) {
      let line = rawLine.trimmingCharacters(in: .whitespaces)
      guard line.hasPrefix("Process ") else { continue }
      if line.contains(" exited") || line.contains(" detached") {
        return true
      }
    }
    return false
  }

  /// The PID in a `Process <pid> stopped` status line, which lldb prints once
  /// an attach has stopped the process (and again from `process status`).
  /// Any other line returns nil.
  public static func detectProcessStop(in line: String) -> pid_t? {
    let words = line.trimmingCharacters(in: .whitespaces).split(separator: " ")
    guard words.count == 3, words[0] == "Process", words[2] == "stopped",
      words[1].allSatisfy(\.isASCII), words[1].allSatisfy(\.isNumber),
      let processID = pid_t(words[1]), processID > 0
    else { return nil }
    return processID
  }

  /// Whether a line is a `memory read --format x` byte-dump line (the shape
  /// that carries the captured bytes; used to redact verbose output).
  public static func isByteLine(_ line: String) -> Bool {
    var utf8 = Array(line.utf8)
    defer { PassphraseSecret.zeroize(&utf8) }
    guard var parsed = parseByteLine(utf8[...]) else { return false }
    PassphraseSecret.zeroize(&parsed.bytes)
    return true
  }

  /// Parses one byte-dump line, already free of its line terminator. Returns
  /// the line's address and bytes, or nil when the line has any other shape.
  /// The caller zeroizes the returned bytes.
  static func parseByteLine(_ line: ArraySlice<UInt8>) -> (address: UInt64, bytes: [UInt8])? {
    let line = trimmed(line)
    guard line.count > 2, line.first == zero, line[line.startIndex + 1] == lowercaseX,
      let colon = line.firstIndex(of: colon)
    else { return nil }
    let addressDigits = line[(line.startIndex + 2)..<colon]
    guard !addressDigits.isEmpty, addressDigits.count <= 16 else { return nil }
    var address: UInt64 = 0
    for digit in addressDigits {
      guard let value = lowercaseHexValue(digit) else { return nil }
      address = address << 4 | UInt64(value)
    }
    var bytes: [UInt8] = []
    var index = colon + 1
    while index < line.endIndex {
      if isSpace(line[index]) {
        index += 1
        continue
      }
      // Each token is exactly `0x` and two lowercase hex digits.
      guard line.endIndex - index >= 4, line[index] == zero, line[index + 1] == lowercaseX,
        let high = lowercaseHexValue(line[index + 2]),
        let low = lowercaseHexValue(line[index + 3]),
        index + 4 == line.endIndex || isSpace(line[index + 4])
      else {
        PassphraseSecret.zeroize(&bytes)
        return nil
      }
      bytes.append(high << 4 | low)
      index += 4
    }
    guard !bytes.isEmpty else { return nil }
    return (address, bytes)
  }

  static func trimmed(_ line: ArraySlice<UInt8>) -> ArraySlice<UInt8> {
    var start = line.startIndex
    var end = line.endIndex
    while start < end, isSpace(line[start]) { start += 1 }
    while end > start, isSpace(line[end - 1]) { end -= 1 }
    return line[start..<end]
  }

  private static let zero = UInt8(ascii: "0")
  private static let lowercaseX = UInt8(ascii: "x")
  private static let colon = UInt8(ascii: ":")

  private static func isSpace(_ byte: UInt8) -> Bool {
    byte == UInt8(ascii: " ") || byte == UInt8(ascii: "\t") || byte == UInt8(ascii: "\r")
  }

  private static func lowercaseHexValue(_ byte: UInt8) -> UInt8? {
    switch byte {
    case UInt8(ascii: "0")...UInt8(ascii: "9"): return byte - UInt8(ascii: "0")
    case UInt8(ascii: "a")...UInt8(ascii: "f"): return byte - UInt8(ascii: "a") + 10
    default: return nil
    }
  }
}

/// Incremental parser for lldb's output during a capture.
///
/// Output is consumed as it arrives. Complete lines are parsed and dropped,
/// and only an unfinished last line is kept between chunks. Every consumed
/// buffer that may hold hexdump bytes is zeroized before it is released, so
/// a rejected candidate's plaintext is not retained or parsed again.
///
/// A read ends after 32 bytes, at any other non-blank line (the next lldb
/// command, its error, or process status), or at a byte line whose address
/// does not continue the previous line. A read that ends short is discarded,
/// so an interrupted read cannot shift the bytes of the reads after it.
struct LLDBMemoryReadStream {
  struct Drained {
    /// Complete 32-byte reads, in order. The caller zeroizes them.
    var candidates: [[UInt8]] = []
    var attachFailure: String?
    var targetExited = false
    /// PIDs from `Process <pid> stopped` lines, in order.
    var stoppedProcessIDs: [pid_t] = []
  }

  private var pendingLine: [UInt8] = []
  private var currentRead: [UInt8] = []
  private var nextAddress: UInt64?

  init() {
    pendingLine.reserveCapacity(4096)
    currentRead.reserveCapacity(PassphraseSecret.byteCount)
  }

  /// Consumes `chunk`, zeroizing and emptying it. Returns what the newly
  /// completed lines contained.
  mutating func consume(_ chunk: inout [UInt8]) -> Drained {
    var drained = Drained()
    for byte in chunk {
      if byte == UInt8(ascii: "\n") {
        processLine(&drained)
      } else {
        pendingLine.append(byte)
      }
    }
    PassphraseSecret.zeroize(&chunk)
    chunk.removeAll()
    return drained
  }

  /// Ends the stream: the unfinished last line is processed as a complete
  /// line, and a short read in progress is discarded.
  mutating func finish() -> Drained {
    var drained = Drained()
    processLine(&drained)
    discardCurrentRead()
    return drained
  }

  /// Wipes any unfinished line and short read without producing a candidate.
  mutating func discard() {
    PassphraseSecret.zeroize(&pendingLine)
    pendingLine.removeAll(keepingCapacity: true)
    discardCurrentRead()
  }

  private mutating func processLine(_ drained: inout Drained) {
    defer {
      PassphraseSecret.zeroize(&pendingLine)
      pendingLine.removeAll(keepingCapacity: true)
    }
    let line = LLDBOutputParser.trimmed(pendingLine[...])
    guard !line.isEmpty else { return }
    if var parsed = LLDBOutputParser.parseByteLine(line) {
      defer { PassphraseSecret.zeroize(&parsed.bytes) }
      if let expected = nextAddress, expected != parsed.address {
        discardCurrentRead()
      }
      for byte in parsed.bytes {
        currentRead.append(byte)
        if currentRead.count == PassphraseSecret.byteCount {
          drained.candidates.append(currentRead)
          discardCurrentRead()
        }
      }
      nextAddress = currentRead.isEmpty ? nil : parsed.address &+ UInt64(parsed.bytes.count)
      return
    }
    // Any other line ends the read in progress.
    discardCurrentRead()
    let text = String(decoding: line, as: UTF8.self)
    if drained.attachFailure == nil, let detail = LLDBOutputParser.detectAttachFailure(in: text) {
      drained.attachFailure = detail
    }
    if LLDBOutputParser.detectTargetExit(in: text) {
      drained.targetExited = true
    }
    if let processID = LLDBOutputParser.detectProcessStop(in: text) {
      drained.stoppedProcessIDs.append(processID)
    }
  }

  private mutating func discardCurrentRead() {
    PassphraseSecret.zeroize(&currentRead)
    currentRead.removeAll(keepingCapacity: true)
    nextAddress = nil
  }
}
