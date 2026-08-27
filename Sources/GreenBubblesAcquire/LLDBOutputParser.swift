// Derived from wcdb-key-tool (https://github.com/TANGandXUE/wcdb-key-tool), MIT license.

import Foundation

/// Parses lldb `memory read --size 1 --count 32 --format x` hexdump output into
/// the captured 32-byte passphrase.
///
/// A byte line looks like `0x16fdfddd8: 0x00 0x11 0x22 0x33 0x44 0x55 0x66 0x77`
/// (lowercase hexadecimal, one address prefix, byte groups only). Any other
/// lldb output is ignored. Returns the first 32 bytes once at least 32 have
/// been collected, or `nil` while the output is still incomplete or malformed.
public enum LLDBOutputParser {
  public static func parsePassphrase(from output: String) -> [UInt8]? {
    var bytes: [UInt8] = []
    for rawLine in output.split(whereSeparator: \.isNewline) {
      let line = rawLine.trimmingCharacters(in: .whitespaces)
      guard let bytesInLine = parseByteLine(line) else { continue }
      bytes.append(contentsOf: bytesInLine)
    }
    guard bytes.count >= 32 else { return nil }
    return Array(bytes.prefix(32))
  }

  private static func parseByteLine(_ line: String) -> [UInt8]? {
    guard line.hasPrefix("0x"), let colon = line.firstIndex(of: ":") else { return nil }
    let address = line[line.index(line.startIndex, offsetBy: 2)..<colon]
    guard !address.isEmpty, address.allSatisfy({ $0.isHexDigit && !$0.isUppercase }) else {
      return nil
    }
    let tokens = line[line.index(after: colon)...]
      .split(whereSeparator: \.isWhitespace)
    guard !tokens.isEmpty else { return nil }
    var bytes: [UInt8] = []
    for token in tokens {
      guard token.hasPrefix("0x"), token.count == 4 else { return nil }
      let value = token.dropFirst(2)
      guard value.allSatisfy({ $0.isHexDigit && !$0.isUppercase }),
        let byte = UInt8(value, radix: 16)
      else { return nil }
      bytes.append(byte)
    }
    return bytes
  }
}
