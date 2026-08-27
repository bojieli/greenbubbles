#!/usr/bin/env swift

import Foundation

enum ProfileCheckError: Error, CustomStringConvertible {
  case missing(String)
  case ambiguous(String, Int)
  case mismatch(String)

  var description: String {
    switch self {
    case .missing(let field):
      "could not extract pinned-build field: \(field)"
    case .ambiguous(let field, let count):
      "pinned-build field is ambiguous: \(field) matched \(count) times"
    case .mismatch(let detail):
      "Swift and Rust pinned-build profiles differ:\n\(detail)"
    }
  }
}

struct PinnedBuildProfile: Codable, Equatable {
  let profileID: String
  let bundleIdentifier: String
  let marketingVersion: String
  let buildVersion: String
  let executableSHA256: String
  let signingIdentifier: String
  let teamIdentifier: String
  let codeDirectorySHA256: String
  let architectures: [String]
  let hardenedRuntime: Bool
  let signatureValid: Bool
}

let scriptURL = URL(fileURLWithPath: #filePath).standardizedFileURL
let repositoryRoot = scriptURL.deletingLastPathComponent().deletingLastPathComponent()

func read(_ relativePath: String) throws -> String {
  try String(
    contentsOf: repositoryRoot.appending(path: relativePath),
    encoding: .utf8
  )
}

func captures(_ pattern: String, in source: String) throws -> [String] {
  let expression = try NSRegularExpression(pattern: pattern)
  let sourceRange = NSRange(source.startIndex..<source.endIndex, in: source)
  return expression.matches(in: source, range: sourceRange).compactMap { match in
    guard match.numberOfRanges == 2,
      let range = Range(match.range(at: 1), in: source)
    else { return nil }
    return String(source[range])
  }
}

func oneCapture(_ pattern: String, in source: String, label: String) throws -> String {
  let values = try captures(pattern, in: source)
  guard !values.isEmpty else { throw ProfileCheckError.missing(label) }
  guard values.count == 1 else {
    throw ProfileCheckError.ambiguous(label, values.count)
  }
  return values[0]
}

func stringField(_ name: String, in block: String, swift: Bool) throws -> String {
  let suffix = swift ? "" : #"\.to_string\(\)"#
  return try oneCapture(
    NSRegularExpression.escapedPattern(for: name) + #"\s*:\s*"([^\"]+)""# + suffix,
    in: block,
    label: name
  )
}

func booleanField(_ name: String, in block: String) throws -> Bool {
  let value = try oneCapture(
    NSRegularExpression.escapedPattern(for: name) + #"\s*:\s*(true|false)"#,
    in: block,
    label: name
  )
  return value == "true"
}

func quotedValues(_ source: String, label: String) throws -> [String] {
  let values = try captures(#""([^\"]+)""#, in: source).sorted()
  guard !values.isEmpty else { throw ProfileCheckError.missing(label) }
  return values
}

func canonicalJSON(_ profile: PinnedBuildProfile) throws -> String {
  let encoder = JSONEncoder()
  encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
  return String(decoding: try encoder.encode(profile), as: UTF8.self)
}

let swiftSource = try read(
  "Sources/GreenBubblesCore/WeChatIntegrationSurfaceInspector.swift"
)
let rustSource = try read("Native/GreenBubblesRestore/src/manifest.rs")

let swiftBlock = try oneCapture(
  #"(?s)static let pinnedWeChat4112 = WeChatClientBuildFingerprint\((.*?)\n  \)"#,
  in: swiftSource,
  label: "Swift pinnedWeChat4112 block"
)
let rustBlock = try oneCapture(
  #"(?s)pub\(crate\) fn supported_client_build\(\) -> ClientBuildFingerprint \{\s*ClientBuildFingerprint \{(.*?)\n    \}\n\}"#,
  in: rustSource,
  label: "Rust supported_client_build block"
)

let swiftMarketingVersion = try stringField(
  "marketingVersion",
  in: swiftBlock,
  swift: true
)
let swiftBuildVersion = try stringField("buildVersion", in: swiftBlock, swift: true)
let swiftArchitectures = try quotedValues(
  oneCapture(
    #"architectures\s*:\s*\[([^\]]+)\]"#,
    in: swiftBlock,
    label: "Swift architectures"
  ),
  label: "Swift architecture value"
)
let swiftProfile = PinnedBuildProfile(
  profileID: "wechat-macos-\(swiftMarketingVersion)-\(swiftBuildVersion)",
  bundleIdentifier: try stringField("bundleIdentifier", in: swiftBlock, swift: true),
  marketingVersion: swiftMarketingVersion,
  buildVersion: swiftBuildVersion,
  executableSHA256: try stringField("executableSHA256", in: swiftBlock, swift: true),
  signingIdentifier: try stringField("signingIdentifier", in: swiftBlock, swift: true),
  teamIdentifier: try stringField("teamIdentifier", in: swiftBlock, swift: true),
  codeDirectorySHA256: try stringField(
    "codeDirectorySHA256",
    in: swiftBlock,
    swift: true
  ),
  architectures: swiftArchitectures,
  hardenedRuntime: try booleanField("hardenedRuntime", in: swiftBlock),
  signatureValid: try booleanField("signatureValid", in: swiftBlock)
)

let rustArchitectures = try quotedValues(
  oneCapture(
    #"architectures\s*:\s*vec!\[([^\]]+)\]"#,
    in: rustBlock,
    label: "Rust architectures"
  ),
  label: "Rust architecture value"
)
let rustProfile = PinnedBuildProfile(
  profileID: try oneCapture(
    #"const SUPPORTED_PROFILE_ID: &str = "([^\"]+)";"#,
    in: rustSource,
    label: "SUPPORTED_PROFILE_ID"
  ),
  bundleIdentifier: try stringField("bundle_identifier", in: rustBlock, swift: false),
  marketingVersion: try stringField("marketing_version", in: rustBlock, swift: false),
  buildVersion: try stringField("build_version", in: rustBlock, swift: false),
  executableSHA256: try oneCapture(
    #"(?s)const SUPPORTED_EXECUTABLE_SHA256: &str =\s*"([^\"]+)";"#,
    in: rustSource,
    label: "SUPPORTED_EXECUTABLE_SHA256"
  ),
  signingIdentifier: try stringField("signing_identifier", in: rustBlock, swift: false),
  teamIdentifier: try stringField("team_identifier", in: rustBlock, swift: false),
  codeDirectorySHA256: try oneCapture(
    #"(?s)const SUPPORTED_CODE_DIRECTORY_SHA256: &str =\s*"([^\"]+)";"#,
    in: rustSource,
    label: "SUPPORTED_CODE_DIRECTORY_SHA256"
  ),
  architectures: rustArchitectures,
  hardenedRuntime: try booleanField("hardened_runtime", in: rustBlock),
  signatureValid: try booleanField("signature_valid", in: rustBlock)
)

guard swiftProfile == rustProfile else {
  throw ProfileCheckError.mismatch(
    "Swift:\n\(try canonicalJSON(swiftProfile))\nRust:\n\(try canonicalJSON(rustProfile))"
  )
}

if CommandLine.arguments.dropFirst() == ["--print"] {
  print(try canonicalJSON(swiftProfile))
} else {
  print("Swift and Rust pinned-build profiles match exactly.")
}
