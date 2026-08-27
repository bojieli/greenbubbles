import CryptoKit
import Darwin
import Foundation

public enum SnapshotFileRole: String, Codable, Sendable {
  case database
  case writeAheadLog
  case sharedMemory
}

public struct SourceFileFingerprint: Codable, Equatable, Sendable {
  public let deviceID: UInt64
  public let fileID: UInt64
  public let byteCount: Int64
  public let modifiedSeconds: Int64
  public let modifiedNanoseconds: Int64

  public init(
    deviceID: UInt64,
    fileID: UInt64,
    byteCount: Int64,
    modifiedSeconds: Int64,
    modifiedNanoseconds: Int64
  ) {
    self.deviceID = deviceID
    self.fileID = fileID
    self.byteCount = byteCount
    self.modifiedSeconds = modifiedSeconds
    self.modifiedNanoseconds = modifiedNanoseconds
  }
}

public struct SnapshotEntry: Codable, Equatable, Sendable {
  public let source: PathReference
  public let sourceSetID: String
  public let logicalPath: String
  public let relativePath: String
  public let role: SnapshotFileRole
  public let fingerprint: SourceFileFingerprint
  public let sha256: String

  public init(
    source: PathReference,
    sourceSetID: String,
    logicalPath: String,
    relativePath: String,
    role: SnapshotFileRole,
    fingerprint: SourceFileFingerprint,
    sha256: String
  ) {
    self.source = source
    self.sourceSetID = sourceSetID
    self.logicalPath = logicalPath
    self.relativePath = relativePath
    self.role = role
    self.fingerprint = fingerprint
    self.sha256 = sha256
  }
}

public struct SnapshotManifest: Codable, Equatable, Sendable {
  public let manifestFormatVersion: Int
  public let snapshotID: UUID
  public let createdAt: Date
  public let sourceFingerprint: String
  public let clientBuild: WeChatClientBuildFingerprint?
  public let entries: [SnapshotEntry]

  public init(
    manifestFormatVersion: Int = 2,
    snapshotID: UUID,
    createdAt: Date,
    sourceFingerprint: String,
    clientBuild: WeChatClientBuildFingerprint? = nil,
    entries: [SnapshotEntry]
  ) {
    self.manifestFormatVersion = manifestFormatVersion
    self.snapshotID = snapshotID
    self.createdAt = createdAt
    self.sourceFingerprint = sourceFingerprint
    self.clientBuild = clientBuild
    self.entries = entries
  }
}

public struct DatabaseFileSet: Equatable, Sendable {
  public let database: URL
  public let writeAheadLog: URL?
  public let sharedMemory: URL?
  public let logicalPath: String

  public init(
    database: URL,
    writeAheadLog: URL? = nil,
    sharedMemory: URL? = nil,
    logicalPath: String? = nil
  ) {
    self.database = database.standardizedFileURL
    self.writeAheadLog = writeAheadLog?.standardizedFileURL
    self.sharedMemory = sharedMemory?.standardizedFileURL
    self.logicalPath = logicalPath ?? database.lastPathComponent
  }
}

public enum SnapshotError: Error, Equatable, CustomStringConvertible {
  case noDatabaseSets
  case sourceChanged(String)
  case sourceIsNotRegularFile(String)
  case unsafeSnapshotLocation
  case posix(operation: String, code: Int32)

  public var description: String {
    switch self {
    case .noDatabaseSets:
      return "No database sets were supplied"
    case .sourceChanged(let sourceID):
      return "Source changed while taking the snapshot: \(sourceID)"
    case .sourceIsNotRegularFile(let sourceID):
      return "Snapshot source is not a regular file: \(sourceID)"
    case .unsafeSnapshotLocation:
      return "Refusing to operate on an unsafe snapshot location"
    case .posix(let operation, let code):
      return "\(operation) failed with POSIX error \(code)"
    }
  }
}

public struct DatabaseSetPlanner: Sendable {
  private let classifier = ArtifactClassifier()

  public init() {}

  public func findDatabaseSets(in roots: [URL], maxDepth: Int = 20) -> [DatabaseFileSet] {
    let fileManager = FileManager.default
    var databases: [URL] = []

    for root in roots {
      guard
        let enumerator = fileManager.enumerator(
          at: root.standardizedFileURL,
          includingPropertiesForKeys: [.isRegularFileKey, .isSymbolicLinkKey],
          options: [.skipsPackageDescendants, .skipsHiddenFiles]
        )
      else { continue }

      for case let url as URL in enumerator {
        if enumerator.level > maxDepth {
          enumerator.skipDescendants()
          continue
        }
        guard
          let values = try? url.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey]),
          values.isRegularFile == true,
          values.isSymbolicLink != true,
          classifier.classify(fileName: url.lastPathComponent) == .database
        else { continue }
        databases.append(url.standardizedFileURL)
      }
    }

    return databases.sorted { $0.path < $1.path }.map { database in
      let wal = URL(fileURLWithPath: database.path + "-wal")
      let shm = URL(fileURLWithPath: database.path + "-shm")
      return DatabaseFileSet(
        database: database,
        writeAheadLog: fileManager.fileExists(atPath: wal.path) ? wal : nil,
        sharedMemory: fileManager.fileExists(atPath: shm.path) ? shm : nil,
        logicalPath: logicalPath(for: database)
      )
    }
  }

  private func logicalPath(for database: URL) -> String {
    let components = database.standardizedFileURL.pathComponents
    if let storageIndex = components.lastIndex(of: "db_storage"),
      storageIndex + 1 < components.count
    {
      return components[(storageIndex + 1)...].joined(separator: "/")
    }
    return database.deletingLastPathComponent().lastPathComponent + "/" + database.lastPathComponent
  }
}

public final class SnapshotLease: @unchecked Sendable {
  public let directory: URL
  public let manifest: SnapshotManifest

  private let lock = NSLock()
  private var automaticCleanupEnabled: Bool
  private var hasCleanedUp = false

  fileprivate init(directory: URL, manifest: SnapshotManifest, cleanUpOnDeinit: Bool) {
    self.directory = directory
    self.manifest = manifest
    self.automaticCleanupEnabled = cleanUpOnDeinit
  }

  public func preserveAfterExit() {
    lock.withLock {
      automaticCleanupEnabled = false
    }
  }

  public func cleanUp() throws {
    let shouldRemove = lock.withLock { () -> Bool in
      guard !hasCleanedUp else { return false }
      hasCleanedUp = true
      return true
    }
    if shouldRemove {
      try FileManager.default.removeItem(at: directory)
    }
  }

  deinit {
    let shouldRemove = lock.withLock { () -> Bool in
      guard automaticCleanupEnabled, !hasCleanedUp else { return false }
      hasCleanedUp = true
      return true
    }
    if shouldRemove {
      try? FileManager.default.removeItem(at: directory)
    }
  }
}

struct SnapshotHooks: Sendable {
  var afterCopy: @Sendable (URL) -> Void = { _ in }
}

public struct ReadOnlySnapshotter: Sendable {
  public static let directoryPrefix = "snapshot-"

  private let baseDirectory: URL
  private let maxRetries: Int
  private let privacy: PathPrivacy
  private let hooks: SnapshotHooks
  private let clientBuild: WeChatClientBuildFingerprint?

  public init(
    baseDirectory: URL = FileManager.default.temporaryDirectory
      .appending(path: "greenbubbles-snapshots", directoryHint: .isDirectory),
    maxRetries: Int = 2,
    includeSourcePaths: Bool = false,
    clientBuild: WeChatClientBuildFingerprint? = nil
  ) {
    self.init(
      baseDirectory: baseDirectory,
      maxRetries: maxRetries,
      includeSourcePaths: includeSourcePaths,
      clientBuild: clientBuild,
      hooks: SnapshotHooks()
    )
  }

  init(
    baseDirectory: URL,
    maxRetries: Int,
    includeSourcePaths: Bool,
    clientBuild: WeChatClientBuildFingerprint? = nil,
    hooks: SnapshotHooks
  ) {
    self.baseDirectory = baseDirectory.standardizedFileURL
    self.maxRetries = max(0, maxRetries)
    self.privacy = PathPrivacy(includePaths: includeSourcePaths)
    self.clientBuild = clientBuild
    self.hooks = hooks
  }

  public func createSnapshot(
    of sets: [DatabaseFileSet],
    cleanUpOnDeinit: Bool = true
  ) throws -> SnapshotLease {
    guard !sets.isEmpty else { throw SnapshotError.noDatabaseSets }
    try ensureOwnerOnlyDirectory(baseDirectory)

    var lastError: Error?
    for _ in 0...maxRetries {
      let snapshotID = UUID()
      let directory = baseDirectory.appending(
        path: Self.directoryPrefix + snapshotID.uuidString,
        directoryHint: .isDirectory
      )

      do {
        try ensureSafeChild(directory, of: baseDirectory)
        try ensureOwnerOnlyDirectory(directory)
        let manifest = try snapshotAttempt(sets: sets, snapshotID: snapshotID, into: directory)
        return SnapshotLease(
          directory: directory,
          manifest: manifest,
          cleanUpOnDeinit: cleanUpOnDeinit
        )
      } catch {
        lastError = error
        try? FileManager.default.removeItem(at: directory)
      }
    }

    throw lastError ?? SnapshotError.noDatabaseSets
  }

  private func snapshotAttempt(
    sets: [DatabaseFileSet],
    snapshotID: UUID,
    into directory: URL
  ) throws -> SnapshotManifest {
    let sources = try sets.enumerated().flatMap { index, set in
      try plannedFiles(for: set, setIndex: index)
    }
    let baseline = try Dictionary(
      uniqueKeysWithValues: sources.map { source in
        (source.source.standardizedFileURL.path, try fingerprint(at: source.source))
      })

    var entries: [SnapshotEntry] = []
    for source in sources {
      let destination = directory.appending(path: source.relativePath)
      try ensureOwnerOnlyDirectory(destination.deletingLastPathComponent())
      let result = try copyFromReadOnlyDescriptor(source.source, to: destination)
      guard result.fingerprint == baseline[source.source.path] else {
        throw SnapshotError.sourceChanged(privacy.reference(for: source.source).opaqueID)
      }
      hooks.afterCopy(source.source)
      entries.append(
        SnapshotEntry(
          source: privacy.reference(for: source.source),
          sourceSetID: source.setID,
          logicalPath: source.logicalPath,
          relativePath: source.relativePath,
          role: source.role,
          fingerprint: result.fingerprint,
          sha256: result.sha256
        ))
    }

    for source in sources {
      let after = try fingerprint(at: source.source)
      guard after == baseline[source.source.path] else {
        throw SnapshotError.sourceChanged(privacy.reference(for: source.source).opaqueID)
      }
    }

    let sortedEntries = entries.sorted { $0.relativePath < $1.relativePath }
    let manifest = SnapshotManifest(
      snapshotID: snapshotID,
      createdAt: Date(),
      sourceFingerprint: aggregateFingerprint(sortedEntries),
      clientBuild: clientBuild,
      entries: sortedEntries
    )
    try writeManifest(manifest, to: directory.appending(path: "manifest.json"))
    return manifest
  }

  private struct PlannedFile {
    let source: URL
    let setID: String
    let logicalPath: String
    let relativePath: String
    let role: SnapshotFileRole
  }

  private func plannedFiles(for set: DatabaseFileSet, setIndex: Int) throws -> [PlannedFile] {
    let setID = privacy.reference(for: set.database).opaqueID
    let prefix = String(format: "sets/%04d", setIndex)
    var files = [
      PlannedFile(
        source: set.database,
        setID: setID,
        logicalPath: set.logicalPath,
        relativePath: "\(prefix)/database.db",
        role: .database
      )
    ]
    if let wal = set.writeAheadLog {
      files.append(
        PlannedFile(
          source: wal,
          setID: setID,
          logicalPath: set.logicalPath + "-wal",
          relativePath: "\(prefix)/database.db-wal",
          role: .writeAheadLog
        ))
    }
    if let shm = set.sharedMemory {
      files.append(
        PlannedFile(
          source: shm,
          setID: setID,
          logicalPath: set.logicalPath + "-shm",
          relativePath: "\(prefix)/database.db-shm",
          role: .sharedMemory
        ))
    }
    return files
  }

  private func copyFromReadOnlyDescriptor(
    _ source: URL,
    to destination: URL
  ) throws -> (fingerprint: SourceFileFingerprint, sha256: String) {
    let sourceDescriptor = Darwin.open(source.path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW)
    guard sourceDescriptor >= 0 else {
      throw SnapshotError.posix(operation: "open source read-only", code: errno)
    }
    defer { Darwin.close(sourceDescriptor) }

    let before = try fingerprint(descriptor: sourceDescriptor, source: source)
    let destinationDescriptor = Darwin.open(
      destination.path,
      O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW,
      S_IRUSR | S_IWUSR
    )
    guard destinationDescriptor >= 0 else {
      throw SnapshotError.posix(operation: "create snapshot file", code: errno)
    }
    defer { Darwin.close(destinationDescriptor) }

    var hasher = SHA256()
    var buffer = [UInt8](repeating: 0, count: 128 * 1024)
    while true {
      let readCount = Darwin.read(sourceDescriptor, &buffer, buffer.count)
      if readCount == 0 { break }
      if readCount < 0 {
        if errno == EINTR { continue }
        throw SnapshotError.posix(operation: "read source", code: errno)
      }

      hasher.update(data: Data(buffer[0..<readCount]))
      var written = 0
      while written < readCount {
        let writeCount = buffer.withUnsafeBytes { bytes in
          Darwin.write(
            destinationDescriptor,
            bytes.baseAddress!.advanced(by: written),
            readCount - written
          )
        }
        if writeCount < 0 {
          if errno == EINTR { continue }
          throw SnapshotError.posix(operation: "write snapshot", code: errno)
        }
        written += writeCount
      }
    }

    guard Darwin.fsync(destinationDescriptor) == 0 else {
      throw SnapshotError.posix(operation: "sync snapshot", code: errno)
    }
    let after = try fingerprint(descriptor: sourceDescriptor, source: source)
    guard before == after else {
      throw SnapshotError.sourceChanged(privacy.reference(for: source).opaqueID)
    }
    return (before, hasher.finalize().map { String(format: "%02x", $0) }.joined())
  }

  private func fingerprint(at url: URL) throws -> SourceFileFingerprint {
    var metadata = stat()
    guard Darwin.lstat(url.path, &metadata) == 0 else {
      throw SnapshotError.posix(operation: "stat source", code: errno)
    }
    return try fingerprint(metadata: metadata, source: url)
  }

  private func fingerprint(descriptor: Int32, source: URL) throws -> SourceFileFingerprint {
    var metadata = stat()
    guard Darwin.fstat(descriptor, &metadata) == 0 else {
      throw SnapshotError.posix(operation: "stat source descriptor", code: errno)
    }
    return try fingerprint(metadata: metadata, source: source)
  }

  private func fingerprint(metadata: stat, source: URL) throws -> SourceFileFingerprint {
    guard metadata.st_mode & S_IFMT == S_IFREG else {
      throw SnapshotError.sourceIsNotRegularFile(privacy.reference(for: source).opaqueID)
    }
    return SourceFileFingerprint(
      deviceID: UInt64(metadata.st_dev),
      fileID: UInt64(metadata.st_ino),
      byteCount: Int64(metadata.st_size),
      modifiedSeconds: Int64(metadata.st_mtimespec.tv_sec),
      modifiedNanoseconds: Int64(metadata.st_mtimespec.tv_nsec)
    )
  }

  private func aggregateFingerprint(_ entries: [SnapshotEntry]) -> String {
    var hasher = SHA256()
    for entry in entries {
      let value = [
        entry.source.opaqueID,
        String(entry.fingerprint.deviceID),
        String(entry.fingerprint.fileID),
        String(entry.fingerprint.byteCount),
        String(entry.fingerprint.modifiedSeconds),
        String(entry.fingerprint.modifiedNanoseconds),
        entry.sha256,
      ].joined(separator: ":")
      hasher.update(data: Data(value.utf8))
    }
    return hasher.finalize().map { String(format: "%02x", $0) }.joined()
  }

  private func writeManifest(_ manifest: SnapshotManifest, to url: URL) throws {
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
    encoder.dateEncodingStrategy = .iso8601
    let data = try encoder.encode(manifest)
    let descriptor = Darwin.open(
      url.path,
      O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW,
      S_IRUSR | S_IWUSR
    )
    guard descriptor >= 0 else {
      throw SnapshotError.posix(operation: "create manifest", code: errno)
    }
    defer { Darwin.close(descriptor) }

    try data.withUnsafeBytes { bytes in
      var written = 0
      while written < bytes.count {
        let count = Darwin.write(
          descriptor,
          bytes.baseAddress!.advanced(by: written),
          bytes.count - written
        )
        if count < 0 {
          if errno == EINTR { continue }
          throw SnapshotError.posix(operation: "write manifest", code: errno)
        }
        written += count
      }
    }
    guard Darwin.fsync(descriptor) == 0 else {
      throw SnapshotError.posix(operation: "sync manifest", code: errno)
    }
  }

  private func ensureOwnerOnlyDirectory(_ directory: URL) throws {
    try FileManager.default.createDirectory(
      at: directory,
      withIntermediateDirectories: true,
      attributes: [.posixPermissions: 0o700]
    )
    guard Darwin.chmod(directory.path, S_IRWXU) == 0 else {
      throw SnapshotError.posix(operation: "secure snapshot directory", code: errno)
    }
  }

  private func ensureSafeChild(_ child: URL, of parent: URL) throws {
    let parentPath = parent.standardizedFileURL.path
    let childPath = child.standardizedFileURL.path
    guard childPath.hasPrefix(parentPath + "/"),
      child.lastPathComponent.hasPrefix(Self.directoryPrefix)
    else {
      throw SnapshotError.unsafeSnapshotLocation
    }
  }
}

public struct SnapshotJanitor: Sendable {
  public init() {}

  @discardableResult
  public func removeExpiredSnapshots(
    in baseDirectory: URL,
    olderThan age: TimeInterval,
    now: Date = Date()
  ) throws -> Int {
    guard age >= 0 else { throw SnapshotError.unsafeSnapshotLocation }
    let base = baseDirectory.standardizedFileURL
    let fileManager = FileManager.default
    let children = try fileManager.contentsOfDirectory(
      at: base,
      includingPropertiesForKeys: [.isDirectoryKey, .creationDateKey],
      options: [.skipsHiddenFiles]
    )
    var removed = 0
    for child in children {
      let standardized = child.standardizedFileURL
      guard
        standardized.deletingLastPathComponent() == base,
        standardized.lastPathComponent.hasPrefix(ReadOnlySnapshotter.directoryPrefix),
        let values = try? standardized.resourceValues(forKeys: [.isDirectoryKey, .creationDateKey]),
        values.isDirectory == true,
        let createdAt = values.creationDate,
        now.timeIntervalSince(createdAt) >= age
      else { continue }
      try fileManager.removeItem(at: standardized)
      removed += 1
    }
    return removed
  }
}
