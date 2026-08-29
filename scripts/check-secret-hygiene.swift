#!/usr/bin/env swift

import Foundation

enum HygieneError: Error, CustomStringConvertible {
  case gitFailed(String)
  case violations(String)

  var description: String {
    switch self {
    case .gitFailed(let detail):
      "git command failed: \(detail)"
    case .violations(let detail):
      "secret-hygiene violations found:\n\(detail)"
    }
  }
}

let scriptURL = URL(fileURLWithPath: #filePath).standardizedFileURL
let repositoryRoot = scriptURL.deletingLastPathComponent().deletingLastPathComponent()
let scriptRelativePath = "scripts/check-secret-hygiene.swift"
let stagedMode = CommandLine.arguments.dropFirst().contains("--staged")

// Rules target secret-shaped material only. Plain 64-hex strings are allowed:
// pinned build hashes legitimately appear in sources and docs.
let bannedFileNames: [String] = [
  "all_keys.json", "wechat-passphrase.json", "signing-key.json", "signing-seed.json",
]
let bannedPathComponents: [String] = ["decrypted"]
let bannedContentRules: [(label: String, pattern: String)] = [
  ("WCDB raw-key literal", #"x'[0-9a-fA-F]{96}'"#),
  ("JSON passphrase field", #""passphrase"\s*:\s*"[0-9a-fA-F]{64}""#),
  ("JSON enc_key field", #""enc_key"\s*:\s*"[0-9a-fA-F]{64}""#),
  ("SQLCipher PRAGMA key", #"(?i)PRAGMA\s+key\s*=\s*"?x?'?[0-9a-fA-F]{64,}"?'?"#),
  // The send adapter's release signing seed. Only the verifying (public) key
  // may ever be committed, and it is pinned at build time, not stored here.
  ("send signing-key seed", #""signingKeySeedHex"\s*:\s*"[0-9a-fA-F]{64}""#),
]

func runGit(_ arguments: [String]) throws -> String {
  let process = Process()
  let output = Pipe()
  process.executableURL = URL(fileURLWithPath: "/usr/bin/git")
  process.arguments = ["-C", repositoryRoot.path] + arguments
  process.standardOutput = output
  process.standardError = output
  try process.run()
  let data = output.fileHandleForReading.readDataToEndOfFile()
  process.waitUntilExit()
  guard process.terminationReason == .exit, process.terminationStatus == 0 else {
    throw HygieneError.gitFailed(String(decoding: data, as: UTF8.self))
  }
  return String(decoding: data, as: UTF8.self)
}

func trackedPaths() throws -> [String] {
  if stagedMode {
    return try runGit(["diff", "--cached", "--name-only", "--diff-filter=ACMR"])
      .split(whereSeparator: \.isNewline).map(String.init)
  }
  return try runGit(["ls-files"])
    .split(whereSeparator: \.isNewline).map(String.init)
}

func stagedContent(_ path: String) throws -> String? {
  let process = Process()
  let output = Pipe()
  process.executableURL = URL(fileURLWithPath: "/usr/bin/git")
  process.arguments = ["-C", repositoryRoot.path, "show", ":\(path)"]
  process.standardOutput = output
  try process.run()
  let data = output.fileHandleForReading.readDataToEndOfFile()
  process.waitUntilExit()
  guard process.terminationReason == .exit, process.terminationStatus == 0 else {
    return nil
  }
  return String(data: data, encoding: .utf8)
}

func workingTreeContent(_ path: String) -> String? {
  let url = repositoryRoot.appending(path: path)
  guard let data = try? Data(contentsOf: url), data.count <= 16 * 1024 * 1024 else {
    return nil
  }
  return String(data: data, encoding: .utf8)
}

func run() throws {
  var violations: [String] = []
  let compiledRules = try bannedContentRules.map {
    (label: $0.label, expression: try NSRegularExpression(pattern: $0.pattern))
  }

  for path in try trackedPaths() {
    guard path != scriptRelativePath else { continue }

    let baseName = URL(fileURLWithPath: path).lastPathComponent
    if bannedFileNames.contains(baseName) || baseName.hasSuffix("-passphrase.json") {
      violations.append("\(path): banned file name")
    }
    let components = path.split(separator: "/").map(String.init)
    if components.contains(where: { bannedPathComponents.contains($0) }) {
      violations.append("\(path): banned path component")
    }

    guard let content = stagedMode ? try stagedContent(path) : workingTreeContent(path)
    else {
      continue
    }
    let range = NSRange(content.startIndex..<content.endIndex, in: content)
    for rule in compiledRules {
      if rule.expression.firstMatch(in: content, range: range) != nil {
        violations.append("\(path): contains \(rule.label)")
      }
    }
  }

  guard violations.isEmpty else {
    throw HygieneError.violations(violations.map { "  \($0)" }.joined(separator: "\n"))
  }

  let scope = stagedMode ? "staged" : "tracked"
  print("Secret hygiene check passed (\(scope) files clean).")
}

do {
  try run()
} catch {
  FileHandle.standardError.write(Data("error: \(error)\n".utf8))
  exit(1)
}
