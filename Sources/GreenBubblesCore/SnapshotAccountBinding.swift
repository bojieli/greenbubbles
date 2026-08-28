import CryptoKit
import Darwin
import Foundation

public enum SnapshotAccountBindingEvidence: String, Codable, Equatable, Sendable {
  case selectedAccountDirectory
}

/// Private snapshot evidence binding one acquisition to one WeChat account.
///
/// `selfSourceIdentifierBase64` is intentionally retained only in the private
/// snapshot manifest. Downstream reports expose the account-scoped participant
/// ID derived from it, never the source WeChat identifier itself.
public struct SnapshotAccountBinding: Codable, Equatable, Sendable {
  public let formatVersion: Int
  public let accountID: String
  public let selfSourceIdentifierBase64: String
  public let evidence: SnapshotAccountBindingEvidence

  public init(
    formatVersion: Int = 1,
    accountID: String,
    selfSourceIdentifierBase64: String,
    evidence: SnapshotAccountBindingEvidence = .selectedAccountDirectory
  ) {
    self.formatVersion = formatVersion
    self.accountID = accountID
    self.selfSourceIdentifierBase64 = selfSourceIdentifierBase64
    self.evidence = evidence
  }
}

public enum SnapshotAccountBindingError: Error, Equatable, CustomStringConvertible {
  case databaseOutsideAccountRoot(String)
  case ambiguousAccountRoots
  case invalidAccountDirectory
  case malformedBinding
  case unsafeAccountDirectory

  public var description: String {
    switch self {
    case .databaseOutsideAccountRoot(let sourceID):
      return "A database is not contained by a WeChat account db_storage directory: \(sourceID)"
    case .ambiguousAccountRoots:
      return
        "Multiple WeChat accounts were selected; run `greenbubbles accounts` and pass "
        + "exactly one opaque ID with `--account`"
    case .invalidAccountDirectory:
      return "The selected WeChat account directory has no usable account identifier"
    case .malformedBinding:
      return "The snapshot account binding is incomplete or malformed"
    case .unsafeAccountDirectory:
      return "The selected WeChat account directory is missing, symbolic, or not a directory"
    }
  }
}

public struct SnapshotAccountBinder: Sendable {
  private let privacy: PathPrivacy

  public init(includeSourcePaths: Bool = false) {
    self.privacy = PathPrivacy(includePaths: includeSourcePaths)
  }

  public func bind(sets: [DatabaseFileSet]) throws -> SnapshotAccountBinding {
    guard !sets.isEmpty else { throw SnapshotAcquisitionPlannerError.noDatabaseSets }
    let sourceFiles = sets.flatMap { databaseSet in
      [databaseSet.database, databaseSet.writeAheadLog, databaseSet.sharedMemory].compactMap { $0 }
    }
    let roots = try Set(sourceFiles.map { try canonicalAccountRoot(containing: $0).path })
    guard roots.count == 1, let rootPath = roots.first else {
      throw SnapshotAccountBindingError.ambiguousAccountRoots
    }
    let root = URL(fileURLWithPath: rootPath, isDirectory: true)
    guard
      let values = try? root.resourceValues(forKeys: [.isDirectoryKey, .isSymbolicLinkKey]),
      values.isDirectory == true,
      values.isSymbolicLink != true
    else {
      throw SnapshotAccountBindingError.unsafeAccountDirectory
    }

    let selfIdentifier = canonicalSelfIdentifier(accountRoot: root)
    guard !selfIdentifier.isEmpty else {
      throw SnapshotAccountBindingError.invalidAccountDirectory
    }
    let canonicalRoot = root.resolvingSymlinksInPath().standardizedFileURL
    let accountDigest = SHA256.hash(data: Data(canonicalRoot.path.utf8))
    let binding = SnapshotAccountBinding(
      accountID: accountDigest.map { String(format: "%02x", $0) }.joined(),
      selfSourceIdentifierBase64: Data(selfIdentifier.utf8).base64EncodedString()
    )
    try validate(binding)
    return binding
  }

  func validate(_ binding: SnapshotAccountBinding) throws {
    guard binding.formatVersion == 1,
      binding.accountID.utf8.count == 64,
      binding.accountID.utf8.allSatisfy({ byte in
        (48...57).contains(byte) || (97...102).contains(byte)
      }),
      let identifierData = Data(base64Encoded: binding.selfSourceIdentifierBase64),
      identifierData.base64EncodedString() == binding.selfSourceIdentifierBase64,
      (1...255).contains(identifierData.count),
      let identifier = String(data: identifierData, encoding: .utf8),
      identifier != ".", identifier != "..",
      !identifier.contains("/"), !identifier.contains("\\"),
      !identifier.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains)
    else {
      throw SnapshotAccountBindingError.malformedBinding
    }
  }

  private func accountRoot(containing database: URL) throws -> URL {
    var candidate = database.standardizedFileURL.deletingLastPathComponent()
    while candidate.path != "/" {
      if candidate.lastPathComponent == "db_storage" {
        return candidate.deletingLastPathComponent().standardizedFileURL
      }
      let parent = candidate.deletingLastPathComponent()
      if parent.path == candidate.path { break }
      candidate = parent
    }
    throw SnapshotAccountBindingError.databaseOutsideAccountRoot(
      privacy.reference(for: database).opaqueID)
  }

  private func canonicalAccountRoot(containing database: URL) throws -> URL {
    let root = try accountRoot(containing: database)
    var metadata = stat()
    guard Darwin.lstat(root.path, &metadata) == 0,
      metadata.st_mode & S_IFMT == S_IFDIR
    else {
      throw SnapshotAccountBindingError.unsafeAccountDirectory
    }
    return root.resolvingSymlinksInPath().standardizedFileURL
  }

  private func canonicalSelfIdentifier(accountRoot: URL) -> String {
    let directoryName = accountRoot.lastPathComponent
    guard let suffixSeparator = directoryName.lastIndex(of: "_") else {
      return directoryName
    }
    let suffixStart = directoryName.index(after: suffixSeparator)
    let suffix = directoryName[suffixStart...]
    let candidate = String(directoryName[..<suffixSeparator])
    guard
      suffix.utf8.count == 4,
      suffix.utf8.allSatisfy({ byte in
        (byte >= 48 && byte <= 57) || (byte >= 65 && byte <= 90) || (byte >= 97 && byte <= 122)
      }),
      !candidate.isEmpty
    else {
      return directoryName
    }

    if directoryName.hasPrefix("wxid_") {
      return candidate.hasPrefix("wxid_") && candidate.count > 5 ? candidate : directoryName
    }

    // Legacy non-wxid account directories are shortened only when WeChat's
    // independent all_users/login evidence confirms the candidate.
    let loginCandidate = accountRoot.deletingLastPathComponent()
      .appending(path: "all_users/login", directoryHint: .isDirectory)
      .appending(path: candidate, directoryHint: .isDirectory)
    if let values = try? loginCandidate.resourceValues(
      forKeys: [.isDirectoryKey, .isSymbolicLinkKey]),
      values.isDirectory == true,
      values.isSymbolicLink != true
    {
      return candidate
    }
    return directoryName
  }
}
