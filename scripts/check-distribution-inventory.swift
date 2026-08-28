#!/usr/bin/env swift

import CryptoKit
import Foundation

enum InventoryError: Error, CustomStringConvertible {
  case commandFailed(String, Int32, String)
  case invalidJSON(String)
  case missingField(String)
  case inconsistentGitRoots
  case missingRepositoryLicense(String)
  case baselineMismatch(String)

  var description: String {
    switch self {
    case .commandFailed(let command, let status, let stderr):
      return "\(command) failed with status \(status): \(stderr)"
    case .invalidJSON(let source):
      return "invalid JSON from \(source)"
    case .missingField(let field):
      return "required dependency metadata is missing: \(field)"
    case .inconsistentGitRoots:
      return "pinned git packages did not resolve from one repository checkout"
    case .missingRepositoryLicense(let package):
      return "could not find a repository-level LICENSE for git package \(package)"
    case .baselineMismatch(let actual):
      return
        "dependency inventory differs from the reviewed baseline. Review the change and update docs/distribution-dependencies.json deliberately.\nActual inventory:\n\(actual)"
    }
  }
}

let scriptURL = URL(fileURLWithPath: #filePath).standardizedFileURL
let repositoryRoot = scriptURL.deletingLastPathComponent().deletingLastPathComponent()
let baselineURL = repositoryRoot.appendingPathComponent("docs/distribution-dependencies.json")
let printOnly = CommandLine.arguments.dropFirst() == ["--print"]

func run(_ command: String, _ arguments: [String]) throws -> Data {
  let process = Process()
  let temporaryRoot = FileManager.default.temporaryDirectory.appendingPathComponent(
    "greenbubbles-distribution-\(UUID().uuidString)",
    isDirectory: true
  )
  try FileManager.default.createDirectory(at: temporaryRoot, withIntermediateDirectories: false)
  defer { try? FileManager.default.removeItem(at: temporaryRoot) }
  let stdoutURL = temporaryRoot.appendingPathComponent("stdout")
  let stderrURL = temporaryRoot.appendingPathComponent("stderr")
  FileManager.default.createFile(atPath: stdoutURL.path, contents: nil)
  FileManager.default.createFile(atPath: stderrURL.path, contents: nil)
  let stdout = try FileHandle(forWritingTo: stdoutURL)
  let stderr = try FileHandle(forWritingTo: stderrURL)
  defer {
    try? stdout.close()
    try? stderr.close()
  }
  process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
  process.arguments = [command] + arguments
  process.currentDirectoryURL = repositoryRoot
  process.standardOutput = stdout
  process.standardError = stderr
  try process.run()
  process.waitUntilExit()
  try stdout.synchronize()
  try stderr.synchronize()

  let output = try Data(contentsOf: stdoutURL)
  let errorOutput = try Data(contentsOf: stderrURL)
  guard process.terminationStatus == 0 else {
    throw InventoryError.commandFailed(
      ([command] + arguments).joined(separator: " "),
      process.terminationStatus,
      String(decoding: errorOutput, as: UTF8.self)
    )
  }
  return output
}

func jsonObject(_ data: Data, source: String) throws -> [String: Any] {
  guard
    let object = try JSONSerialization.jsonObject(with: data) as? [String: Any]
  else {
    throw InventoryError.invalidJSON(source)
  }
  return object
}

func string(_ dictionary: [String: Any], _ key: String, context: String) throws -> String {
  guard let value = dictionary[key] as? String else {
    throw InventoryError.missingField("\(context).\(key)")
  }
  return value
}

func optionalString(_ value: Any?) -> Any {
  value as? String ?? NSNull()
}

func sha256(_ data: Data) -> String {
  SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
}

func repositoryLicenseRoot(for manifestPath: String) -> URL? {
  let fileManager = FileManager.default
  var candidate = URL(fileURLWithPath: manifestPath).deletingLastPathComponent()
  for _ in 0..<10 {
    let license = candidate.appendingPathComponent("LICENSE")
    let manifest = candidate.appendingPathComponent("Cargo.toml")
    if fileManager.fileExists(atPath: license.path)
      && fileManager.fileExists(atPath: manifest.path)
    {
      return candidate
    }
    let parent = candidate.deletingLastPathComponent()
    if parent.path == candidate.path { return nil }
    candidate = parent
  }
  return nil
}

func canonicalJSON(_ object: [String: Any]) throws -> Data {
  try JSONSerialization.data(
    withJSONObject: object,
    options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
  )
}

func buildInventory() throws -> [String: Any] {
  let swiftPackage = try jsonObject(
    run("swift", ["package", "dump-package"]),
    source: "swift package dump-package"
  )
  guard let swiftDependencies = swiftPackage["dependencies"] as? [Any] else {
    throw InventoryError.missingField("swift.dependencies")
  }

  let cargo = try jsonObject(
    run(
      "cargo",
      [
        "metadata", "--locked", "--format-version", "1", "--manifest-path",
        "Native/GreenBubblesRestore/Cargo.toml",
      ]
    ),
    source: "cargo metadata"
  )
  guard let packages = cargo["packages"] as? [[String: Any]] else {
    throw InventoryError.missingField("cargo.packages")
  }
  guard
    let rootPackage = packages.first(where: {
      ($0["name"] as? String) == "greenbubbles-restore" && $0["source"] is NSNull
    })
  else {
    throw InventoryError.missingField("cargo root package")
  }

  guard let directDependencies = rootPackage["dependencies"] as? [[String: Any]] else {
    throw InventoryError.missingField("cargo root dependencies")
  }
  guard
    let resolve = cargo["resolve"] as? [String: Any],
    let nodes = resolve["nodes"] as? [[String: Any]],
    let rootID = rootPackage["id"] as? String,
    let rootNode = nodes.first(where: { ($0["id"] as? String) == rootID }),
    let resolvedDependencies = rootNode["deps"] as? [[String: Any]]
  else {
    throw InventoryError.missingField("cargo root resolution")
  }
  let packagesByID = Dictionary(
    uniqueKeysWithValues: try packages.map {
      (try string($0, "id", context: "cargo package"), $0)
    }
  )
  let normalizedDirectDependencies: [[String: Any]] = try directDependencies.map { dependency in
    let kind = dependency["kind"] as? String ?? "normal"
    let dependencyName = try string(dependency, "name", context: "cargo dependency")
    let resolutionName = (dependency["rename"] as? String ?? dependencyName)
      .replacingOccurrences(of: "-", with: "_")
    guard
      let resolvedID = resolvedDependencies.first(where: {
        ($0["name"] as? String) == resolutionName
      })?["pkg"] as? String,
      let resolvedPackage = packagesByID[resolvedID]
    else {
      throw InventoryError.missingField("resolved direct dependency \(dependencyName)")
    }
    return [
      "features": (dependency["features"] as? [String] ?? []).sorted(),
      "kind": kind,
      "licenseMetadata": optionalString(resolvedPackage["license"]),
      "name": dependencyName,
      "optional": dependency["optional"] as? Bool ?? false,
      "requirement": try string(dependency, "req", context: "cargo dependency"),
      "resolvedVersion": try string(
        resolvedPackage,
        "version",
        context: "resolved direct dependency \(dependencyName)"
      ),
      "source": optionalString(dependency["source"]),
      "usesDefaultFeatures": dependency["uses_default_features"] as? Bool ?? true,
    ]
  }.sorted {
    let left = "\($0["kind"]!)|\($0["name"]!)"
    let right = "\($1["kind"]!)|\($1["name"]!)"
    return left < right
  }

  let gitPackages = packages.filter {
    ($0["source"] as? String)?.hasPrefix("git+") == true
  }
  let normalizedGitPackages: [[String: Any]] = try gitPackages.map { package in
    [
      "licenseMetadata": optionalString(package["license"]),
      "name": try string(package, "name", context: "git package"),
      "source": try string(package, "source", context: "git package"),
      "version": try string(package, "version", context: "git package"),
    ]
  }.sorted { "\($0["name"]!)" < "\($1["name"]!)" }

  let licenseRoots = try gitPackages.map { package -> URL in
    let name = try string(package, "name", context: "git package")
    let manifest = try string(package, "manifest_path", context: "git package")
    guard let root = repositoryLicenseRoot(for: manifest) else {
      throw InventoryError.missingRepositoryLicense(name)
    }
    return root.standardizedFileURL
  }
  guard let gitRoot = licenseRoots.first, licenseRoots.allSatisfy({ $0 == gitRoot }) else {
    throw InventoryError.inconsistentGitRoots
  }
  let upstreamLicense = try Data(contentsOf: gitRoot.appendingPathComponent("LICENSE"))
  let upstreamManifest = try String(
    contentsOf: gitRoot.appendingPathComponent("Cargo.toml"),
    encoding: .utf8
  )
  let workspaceLicense = upstreamManifest.range(
    of: #"(?m)^license\s*=\s*"([^"]+)"\s*$"#,
    options: .regularExpression
  ).map { match -> String in
    let line = String(upstreamManifest[match])
    return line.split(separator: "\"")[1].description
  }

  let unknownLicensePackages: [[String: Any]] = try packages.compactMap { package in
    guard !(package["source"] is NSNull), package["license"] is NSNull else { return nil }
    return [
      "name": try string(package, "name", context: "unknown-license package"),
      "source": try string(package, "source", context: "unknown-license package"),
      "version": try string(package, "version", context: "unknown-license package"),
    ]
  }.sorted { "\($0["name"]!)" < "\($1["name"]!)" }

  let nativePackages = try ["libsqlite3-sys", "silk-rs", "zstd-sys"].map {
    expectedName -> [String: Any] in
    guard let package = packages.first(where: { ($0["name"] as? String) == expectedName }) else {
      throw InventoryError.missingField("resolved native package \(expectedName)")
    }
    return [
      "licenseMetadata": optionalString(package["license"]),
      "name": expectedName,
      "version": try string(package, "version", context: expectedName),
    ]
  }

  let publishable: Bool
  if rootPackage["publish"] is NSNull {
    publishable = true
  } else if let publishList = rootPackage["publish"] as? [Any] {
    publishable = !publishList.isEmpty
  } else {
    throw InventoryError.missingField("cargo root package.publish")
  }
  return [
    "formatVersion": 1,
    "rust": [
      "directDependencies": normalizedDirectDependencies,
      "gitPackages": normalizedGitPackages,
      "gitRepositoryLicense": [
        "licenseSHA256": sha256(upstreamLicense),
        "workspaceLicenseMetadata": workspaceLicense.map { $0 as Any } ?? NSNull(),
      ],
      "nativePackages": nativePackages,
      "package": [
        "license": optionalString(rootPackage["license"]),
        "name": try string(rootPackage, "name", context: "cargo root package"),
        "publishable": publishable,
        "version": try string(rootPackage, "version", context: "cargo root package"),
      ],
      "unknownLicensePackages": unknownLicensePackages,
    ],
    "swift": [
      "externalDependencies": swiftDependencies,
      "package": try string(swiftPackage, "name", context: "swift package"),
    ],
  ]
}

do {
  let inventory = try buildInventory()
  let actualData = try canonicalJSON(inventory)
  let actualText = String(decoding: actualData, as: UTF8.self) + "\n"
  if printOnly {
    print(actualText, terminator: "")
    exit(EXIT_SUCCESS)
  }

  let baseline = try jsonObject(Data(contentsOf: baselineURL), source: baselineURL.path)
  let baselineData = try canonicalJSON(baseline)
  guard baselineData == actualData else {
    throw InventoryError.baselineMismatch(actualText)
  }
  print("Distribution dependency inventory matches the reviewed baseline.")
} catch {
  FileHandle.standardError.write(Data("error: \(error)\n".utf8))
  exit(EXIT_FAILURE)
}
