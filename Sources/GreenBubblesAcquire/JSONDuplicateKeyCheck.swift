import Foundation

/// Finds a key that appears twice in one JSON object.
///
/// `JSONSerialization` keeps only one of the duplicates, while the Rust CLI's
/// derived deserializer rejects duplicate fields. Without this check the two
/// readers could pick different values, for example different key files.
/// Run it only on text `JSONSerialization` has already accepted; it assumes
/// valid JSON and reports anything unexpected as a duplicate (fail closed).
enum JSONDuplicateKeyCheck {
  /// Returns the first duplicated key, or nil when every object's keys are
  /// distinct after decoding escapes.
  static func firstDuplicateKey(in data: Data) -> String? {
    var scanner = Scanner(bytes: Array(data))
    do {
      try scanner.value()
      return nil
    } catch Failure.duplicate(let key) {
      return key
    } catch {
      return "(unreadable)"
    }
  }

  private enum Failure: Error {
    case duplicate(String)
    case malformed
  }

  private struct Scanner {
    let bytes: [UInt8]
    var index = 0

    init(bytes: [UInt8]) {
      // A byte-order mark is rejected before this runs, as in Rust.
      self.bytes = bytes
    }

    mutating func value() throws {
      skipWhitespace()
      guard index < bytes.count else { throw Failure.malformed }
      switch bytes[index] {
      case UInt8(ascii: "{"):
        index += 1
        var keys = Set<String>()
        skipWhitespace()
        if peek(UInt8(ascii: "}")) {
          index += 1
          return
        }
        while true {
          skipWhitespace()
          let key = try string()
          guard keys.insert(key).inserted else { throw Failure.duplicate(key) }
          skipWhitespace()
          guard peek(UInt8(ascii: ":")) else { throw Failure.malformed }
          index += 1
          try value()
          skipWhitespace()
          if peek(UInt8(ascii: ",")) {
            index += 1
            continue
          }
          guard peek(UInt8(ascii: "}")) else { throw Failure.malformed }
          index += 1
          return
        }
      case UInt8(ascii: "["):
        index += 1
        skipWhitespace()
        if peek(UInt8(ascii: "]")) {
          index += 1
          return
        }
        while true {
          try value()
          skipWhitespace()
          if peek(UInt8(ascii: ",")) {
            index += 1
            continue
          }
          guard peek(UInt8(ascii: "]")) else { throw Failure.malformed }
          index += 1
          return
        }
      case UInt8(ascii: "\""):
        _ = try string()
      default:
        // Numbers, true, false, null: skip to the next structural byte.
        while index < bytes.count, !Self.structural.contains(bytes[index]) { index += 1 }
      }
    }

    private static let structural: Set<UInt8> = [
      UInt8(ascii: ","), UInt8(ascii: "}"), UInt8(ascii: "]"),
      UInt8(ascii: " "), UInt8(ascii: "\t"), UInt8(ascii: "\n"), UInt8(ascii: "\r"),
    ]

    private func peek(_ byte: UInt8) -> Bool { index < bytes.count && bytes[index] == byte }

    private mutating func skipWhitespace() {
      while index < bytes.count,
        [UInt8(ascii: " "), UInt8(ascii: "\t"), UInt8(ascii: "\n"), UInt8(ascii: "\r")]
          .contains(bytes[index])
      {
        index += 1
      }
    }

    // Decodes a JSON string, including escapes and surrogate pairs.
    private mutating func string() throws -> String {
      guard peek(UInt8(ascii: "\"")) else { throw Failure.malformed }
      index += 1
      var units: [UInt16] = []
      var raw: [UInt8] = []
      func flushRaw() {
        units.append(contentsOf: String(decoding: raw, as: UTF8.self).utf16)
        raw.removeAll()
      }
      while index < bytes.count {
        let byte = bytes[index]
        index += 1
        if byte == UInt8(ascii: "\"") {
          flushRaw()
          return String(decoding: units, as: UTF16.self)
        }
        guard byte == UInt8(ascii: "\\") else {
          raw.append(byte)
          continue
        }
        flushRaw()
        guard index < bytes.count else { throw Failure.malformed }
        let escape = bytes[index]
        index += 1
        switch escape {
        case UInt8(ascii: "\""): units.append(0x22)
        case UInt8(ascii: "\\"): units.append(0x5C)
        case UInt8(ascii: "/"): units.append(0x2F)
        case UInt8(ascii: "b"): units.append(0x08)
        case UInt8(ascii: "f"): units.append(0x0C)
        case UInt8(ascii: "n"): units.append(0x0A)
        case UInt8(ascii: "r"): units.append(0x0D)
        case UInt8(ascii: "t"): units.append(0x09)
        case UInt8(ascii: "u"):
          guard index + 4 <= bytes.count,
            let unit = UInt16(String(decoding: bytes[index..<index + 4], as: UTF8.self), radix: 16)
          else { throw Failure.malformed }
          units.append(unit)
          index += 4
        default: throw Failure.malformed
        }
      }
      throw Failure.malformed
    }
  }
}
