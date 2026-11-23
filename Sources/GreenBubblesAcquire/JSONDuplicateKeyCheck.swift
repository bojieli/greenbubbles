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
    switch problem(in: data) {
    case .duplicate(let key)?: return key
    case .malformed?: return "(unreadable)"
    case nil: return nil
    }
  }

  /// Every rule serde_json applies that JSONSerialization may not: no
  /// duplicate fields, no unpaired UTF-16 surrogate escapes, no raw control
  /// characters in strings, and nesting no deeper than serde_json's 128.
  static func problem(in data: Data) -> Failure? {
    var scanner = Scanner(bytes: Array(data))
    do {
      try scanner.value(depth: 0)
      // serde_json allows only whitespace after the top-level value.
      try scanner.end()
      return nil
    } catch let failure as Failure {
      return failure
    } catch {
      return .malformed
    }
  }

  enum Failure: Error, Equatable {
    case duplicate(String)
    case malformed
  }

  static let maximumDepth = 128

  private struct Scanner {
    let bytes: [UInt8]
    var index = 0

    init(bytes: [UInt8]) {
      // A byte-order mark is rejected before this runs, as in Rust.
      self.bytes = bytes
    }

    mutating func value(depth: Int) throws {
      skipWhitespace()
      guard index < bytes.count else { throw Failure.malformed }
      switch bytes[index] {
      case UInt8(ascii: "{"):
        guard depth < JSONDuplicateKeyCheck.maximumDepth else { throw Failure.malformed }
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
          try value(depth: depth + 1)
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
        guard depth < JSONDuplicateKeyCheck.maximumDepth else { throw Failure.malformed }
        index += 1
        skipWhitespace()
        if peek(UInt8(ascii: "]")) {
          index += 1
          return
        }
        while true {
          try value(depth: depth + 1)
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

    mutating func end() throws {
      skipWhitespace()
      guard index == bytes.count else { throw Failure.malformed }
    }

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
          let text = String(decoding: units, as: UTF16.self)
          // An unpaired surrogate escape decodes to U+FFFD; serde_json rejects it.
          guard Array(text.utf16) == units else { throw Failure.malformed }
          return text
        }
        guard byte >= 0x20 else { throw Failure.malformed }
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
