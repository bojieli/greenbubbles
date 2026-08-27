import CryptoKit
import Darwin
import Foundation

public struct WeChatClientBuildFingerprint: Codable, Equatable, Sendable {
  public let formatVersion: Int
  public let bundleIdentifier: String
  public let marketingVersion: String
  public let buildVersion: String
  public let executableSHA256: String
  public let signingIdentifier: String
  public let teamIdentifier: String
  public let codeDirectorySHA256: String
  public let architectures: [String]
  public let hardenedRuntime: Bool
  public let signatureValid: Bool

  public init(
    formatVersion: Int = 1,
    bundleIdentifier: String,
    marketingVersion: String,
    buildVersion: String,
    executableSHA256: String,
    signingIdentifier: String,
    teamIdentifier: String,
    codeDirectorySHA256: String,
    architectures: [String],
    hardenedRuntime: Bool,
    signatureValid: Bool
  ) {
    self.formatVersion = formatVersion
    self.bundleIdentifier = bundleIdentifier
    self.marketingVersion = marketingVersion
    self.buildVersion = buildVersion
    self.executableSHA256 = executableSHA256
    self.signingIdentifier = signingIdentifier
    self.teamIdentifier = teamIdentifier
    self.codeDirectorySHA256 = codeDirectorySHA256
    self.architectures = architectures
    self.hardenedRuntime = hardenedRuntime
    self.signatureValid = signatureValid
  }
}

public enum ClientBuildFingerprintError: Error, Equatable, CustomStringConvertible {
  case invalidBundle
  case invalidExecutable
  case commandFailed(String)
  case malformedSigningEvidence
  case posix(operation: String, code: Int32)

  public var description: String {
    switch self {
    case .invalidBundle:
      return "The WeChat application bundle has incomplete version metadata"
    case .invalidExecutable:
      return "The WeChat application executable is missing or unsafe"
    case .commandFailed(let command):
      return "Client fingerprint command failed: \(command)"
    case .malformedSigningEvidence:
      return "The WeChat code-signing evidence is incomplete"
    case .posix(let operation, let code):
      return "\(operation) failed with POSIX error \(code)"
    }
  }
}

public struct WeChatClientBuildInspector: Sendable {
  private let homeDirectory: URL

  public init(homeDirectory: URL = FileManager.default.homeDirectoryForCurrentUser) {
    self.homeDirectory = homeDirectory.standardizedFileURL
  }

  public func inspectDefaultInstallation() throws -> WeChatClientBuildFingerprint? {
    let fileManager = FileManager.default
    let candidates = [
      URL(fileURLWithPath: "/Applications/WeChat.app"),
      URL(fileURLWithPath: "/Applications/微信.app"),
      homeDirectory.appending(path: "Applications/WeChat.app"),
      homeDirectory.appending(path: "Applications/微信.app"),
    ]
    guard let application = candidates.first(where: { fileManager.fileExists(atPath: $0.path) })
    else { return nil }
    return try inspect(application: application)
  }

  public func inspect(application: URL) throws -> WeChatClientBuildFingerprint {
    let application = application.standardizedFileURL
    let infoURL = application.appending(path: "Contents/Info.plist")
    guard
      let data = try? Data(contentsOf: infoURL),
      let value = try? PropertyListSerialization.propertyList(from: data, format: nil),
      let info = value as? [String: Any],
      let bundleIdentifier = info["CFBundleIdentifier"] as? String,
      let marketingVersion = info["CFBundleShortVersionString"] as? String,
      let buildVersion = info["CFBundleVersion"] as? String,
      let executableName = info["CFBundleExecutable"] as? String,
      !bundleIdentifier.isEmpty,
      !marketingVersion.isEmpty,
      !buildVersion.isEmpty,
      !executableName.isEmpty
    else { throw ClientBuildFingerprintError.invalidBundle }

    let executable = application.appending(path: "Contents/MacOS/\(executableName)")
      .standardizedFileURL
    let executableRoot = application.appending(path: "Contents/MacOS").standardizedFileURL
    guard executable.path.hasPrefix(executableRoot.path + "/") else {
      throw ClientBuildFingerprintError.invalidExecutable
    }
    var metadata = stat()
    guard Darwin.lstat(executable.path, &metadata) == 0 else {
      throw ClientBuildFingerprintError.posix(operation: "inspect WeChat executable", code: errno)
    }
    guard metadata.st_mode & S_IFMT == S_IFREG, metadata.st_nlink == 1 else {
      throw ClientBuildFingerprintError.invalidExecutable
    }

    let before = fileIdentity(metadata)
    let executableSHA256 = try hashReadOnlyFile(executable)
    guard Darwin.lstat(executable.path, &metadata) == 0, fileIdentity(metadata) == before else {
      throw ClientBuildFingerprintError.invalidExecutable
    }
    let signing = try Self.parseSigningEvidence(
      run("/usr/bin/codesign", ["-d", "--verbose=4", application.path], acceptStatus: 0)
    )
    let signatureValid = (try? run(
      "/usr/bin/codesign",
      ["--verify", "--strict", "--deep", application.path],
      acceptStatus: 0
    )) != nil
    let architectures = try run("/usr/bin/lipo", ["-archs", executable.path], acceptStatus: 0)
      .split(whereSeparator: { $0.isWhitespace })
      .map(String.init)
      .sorted()
    guard !architectures.isEmpty else {
      throw ClientBuildFingerprintError.invalidExecutable
    }

    return WeChatClientBuildFingerprint(
      bundleIdentifier: bundleIdentifier,
      marketingVersion: marketingVersion,
      buildVersion: buildVersion,
      executableSHA256: executableSHA256,
      signingIdentifier: signing.identifier,
      teamIdentifier: signing.teamIdentifier,
      codeDirectorySHA256: signing.codeDirectorySHA256,
      architectures: architectures,
      hardenedRuntime: signing.hardenedRuntime,
      signatureValid: signatureValid
    )
  }

  struct SigningEvidence: Equatable {
    let identifier: String
    let teamIdentifier: String
    let codeDirectorySHA256: String
    let hardenedRuntime: Bool
  }

  static func parseSigningEvidence(_ output: String) throws -> SigningEvidence {
    var identifier: String?
    var teamIdentifier: String?
    var codeDirectorySHA256: String?
    var hardenedRuntime = false
    for line in output.split(whereSeparator: \ .isNewline).map(String.init) {
      if line.hasPrefix("Identifier=") {
        identifier = String(line.dropFirst("Identifier=".count))
      } else if line.hasPrefix("TeamIdentifier=") {
        teamIdentifier = String(line.dropFirst("TeamIdentifier=".count))
      } else if line.hasPrefix("CandidateCDHashFull sha256=") {
        codeDirectorySHA256 = String(line.dropFirst("CandidateCDHashFull sha256=".count))
      } else if line.hasPrefix("CodeDirectory ") {
        hardenedRuntime = line.contains("(runtime)")
      }
    }
    guard
      let identifier,
      let teamIdentifier,
      let codeDirectorySHA256,
      !identifier.isEmpty,
      !teamIdentifier.isEmpty,
      codeDirectorySHA256.count == 64,
      codeDirectorySHA256.allSatisfy({ $0.isHexDigit })
    else { throw ClientBuildFingerprintError.malformedSigningEvidence }
    return SigningEvidence(
      identifier: identifier,
      teamIdentifier: teamIdentifier,
      codeDirectorySHA256: codeDirectorySHA256.lowercased(),
      hardenedRuntime: hardenedRuntime
    )
  }

  private func run(_ executable: String, _ arguments: [String], acceptStatus: Int32) throws -> String {
    let process = Process()
    let output = Pipe()
    process.executableURL = URL(fileURLWithPath: executable)
    process.arguments = arguments
    process.standardOutput = output
    process.standardError = output
    try process.run()
    process.waitUntilExit()
    let data = output.fileHandleForReading.readDataToEndOfFile()
    guard process.terminationReason == .exit, process.terminationStatus == acceptStatus else {
      throw ClientBuildFingerprintError.commandFailed(URL(fileURLWithPath: executable).lastPathComponent)
    }
    guard let value = String(data: data, encoding: .utf8) else {
      throw ClientBuildFingerprintError.commandFailed(URL(fileURLWithPath: executable).lastPathComponent)
    }
    return value.trimmingCharacters(in: .whitespacesAndNewlines)
  }

  private func hashReadOnlyFile(_ url: URL) throws -> String {
    let descriptor = Darwin.open(url.path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW)
    guard descriptor >= 0 else {
      throw ClientBuildFingerprintError.posix(operation: "open WeChat executable", code: errno)
    }
    defer { Darwin.close(descriptor) }
    var hasher = SHA256()
    var buffer = [UInt8](repeating: 0, count: 128 * 1024)
    while true {
      let count = Darwin.read(descriptor, &buffer, buffer.count)
      if count == 0 { break }
      guard count > 0 else {
        throw ClientBuildFingerprintError.posix(operation: "read WeChat executable", code: errno)
      }
      hasher.update(data: Data(buffer[0..<count]))
    }
    return hasher.finalize().map { String(format: "%02x", $0) }.joined()
  }

  private func fileIdentity(_ metadata: stat) -> [Int64] {
    [
      Int64(metadata.st_dev),
      Int64(metadata.st_ino),
      metadata.st_size,
      Int64(metadata.st_mtimespec.tv_sec),
      Int64(metadata.st_mtimespec.tv_nsec),
    ]
  }
}
