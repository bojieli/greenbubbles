// Derived from wcdb-key-tool (https://github.com/TANGandXUE/wcdb-key-tool), MIT license.

import Darwin
import Foundation

public struct DatabaseSaltEntry: Equatable, Sendable {
  public let relativePath: String
  public let salt: [UInt8]
  public let page1: [UInt8]

  public init(relativePath: String, salt: [UInt8], page1: [UInt8]) {
    self.relativePath = relativePath
    self.salt = salt
    self.page1 = page1
  }
}

public enum SaltInventoryError: Error, Equatable, CustomStringConvertible {
  case unreadableRoot
  case posix(operation: String, code: Int32)

  public var description: String {
    switch self {
    case .unreadableRoot:
      return "The database root is missing or is not a directory"
    case .posix(let operation, let code):
      return "\(operation) failed with POSIX error \(code)"
    }
  }
}

/// Read-only collection of database page-1 salts under a supplied db root.
///
/// Files are opened with `O_RDONLY | O_CLOEXEC | O_NOFOLLOW`; only regular
/// `.db` files of at least one page are collected. `-wal`/`-shm` journals,
/// symlinks, and undersized files are skipped. The salt is the first 16 bytes
/// of page 1; the full page is retained for HMAC verification.
public struct DatabaseSaltInventory: Sendable {
  public let entries: [DatabaseSaltEntry]
  public let skippedFileCount: Int

  public init(root: URL) throws {
    let root = root.standardizedFileURL
    var metadata = stat()
    guard Darwin.lstat(root.path, &metadata) == 0, metadata.st_mode & S_IFMT == S_IFDIR else {
      throw SaltInventoryError.unreadableRoot
    }
    var collector = Collector(root: root)
    collector.collect(relativeDirectory: "")
    self.entries = collector.entries.sorted { $0.relativePath < $1.relativePath }
    self.skippedFileCount = collector.skippedFileCount
  }

  public var distinctSalts: [[UInt8]] {
    var seen = Set<Data>()
    var salts: [[UInt8]] = []
    for entry in entries where seen.insert(Data(entry.salt)).inserted {
      salts.append(entry.salt)
    }
    return salts
  }

  /// A representative page 1 for each distinct salt, for verification.
  public var saltVerificationSamples: [(salt: [UInt8], page1: [UInt8])] {
    var seen = Set<Data>()
    var samples: [(salt: [UInt8], page1: [UInt8])] = []
    for entry in entries where seen.insert(Data(entry.salt)).inserted {
      samples.append((salt: entry.salt, page1: entry.page1))
    }
    return samples
  }

  private struct Collector {
    let root: URL
    var entries: [DatabaseSaltEntry] = []
    var skippedFileCount = 0

    mutating func collect(relativeDirectory: String) {
      let directory =
        relativeDirectory.isEmpty
        ? root
        : root.appending(path: relativeDirectory, directoryHint: .isDirectory)
      let children =
        (try? FileManager.default.contentsOfDirectory(
          at: directory,
          includingPropertiesForKeys: [.isDirectoryKey, .isRegularFileKey, .isSymbolicLinkKey],
          options: [.skipsHiddenFiles]
        )) ?? []
      for child in children {
        let name = child.lastPathComponent
        let relativePath = relativeDirectory.isEmpty ? name : relativeDirectory + "/" + name
        guard
          let values = try? child.resourceValues(
            forKeys: [.isDirectoryKey, .isRegularFileKey, .isSymbolicLinkKey])
        else {
          skippedFileCount += 1
          continue
        }
        if values.isDirectory == true, values.isSymbolicLink != true {
          collect(relativeDirectory: relativePath)
          continue
        }
        guard values.isRegularFile == true, values.isSymbolicLink != true else {
          continue
        }
        guard name.hasSuffix(".db"), !name.hasSuffix("-wal"), !name.hasSuffix("-shm") else {
          continue
        }
        guard let page1 = Self.readPage1(child.standardizedFileURL) else {
          skippedFileCount += 1
          continue
        }
        entries.append(
          DatabaseSaltEntry(
            relativePath: relativePath,
            salt: Array(page1[0..<SQLCipherKeyVerifier.saltSize]),
            page1: page1
          ))
      }
    }

    static func readPage1(_ url: URL) -> [UInt8]? {
      let descriptor = Darwin.open(url.path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW)
      guard descriptor >= 0 else { return nil }
      defer { Darwin.close(descriptor) }
      var metadata = stat()
      guard Darwin.fstat(descriptor, &metadata) == 0 else { return nil }
      guard metadata.st_mode & S_IFMT == S_IFREG,
        metadata.st_size >= Int64(SQLCipherKeyVerifier.pageSize)
      else { return nil }
      var page = [UInt8](repeating: 0, count: SQLCipherKeyVerifier.pageSize)
      var filled = 0
      while filled < page.count {
        let count = page.withUnsafeMutableBytes { buffer in
          Darwin.read(descriptor, buffer.baseAddress!.advanced(by: filled), buffer.count - filled)
        }
        guard count > 0 else { return nil }
        filled += count
      }
      return page
    }
  }
}
