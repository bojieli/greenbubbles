import CryptoKit
import Darwin
import Foundation

public enum SnapshotFileRole: String, Codable, Sendable {
  case database
  case writeAheadLog
  case sharedMemory
}

public enum SnapshotCaptureMethod: String, Codable, Sendable {
  case atomicCopyOnWriteClone
  case verifiedByteCopy
}

public enum SnapshotTiming {
  public static func milliseconds(_ duration: Duration) -> UInt64 {
    let components = duration.components
    guard components.seconds >= 0, components.attoseconds >= 0 else { return 0 }
    let seconds = UInt64(components.seconds)
    let (wholeMilliseconds, secondsOverflow) = seconds.multipliedReportingOverflow(by: 1_000)
    guard !secondsOverflow else { return UInt64.max }
    let fractionalMilliseconds = UInt64(components.attoseconds / 1_000_000_000_000_000)
    let (result, additionOverflow) = wholeMilliseconds.addingReportingOverflow(
      fractionalMilliseconds)
    return additionOverflow ? UInt64.max : result
  }
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
  public let captureMethod: SnapshotCaptureMethod?

  public init(
    source: PathReference,
    sourceSetID: String,
    logicalPath: String,
    relativePath: String,
    role: SnapshotFileRole,
    fingerprint: SourceFileFingerprint,
    sha256: String,
    captureMethod: SnapshotCaptureMethod? = nil
  ) {
    self.source = source
    self.sourceSetID = sourceSetID
    self.logicalPath = logicalPath
    self.relativePath = relativePath
    self.role = role
    self.fingerprint = fingerprint
    self.sha256 = sha256
    self.captureMethod = captureMethod
  }
}

public struct SnapshotManifest: Codable, Equatable, Sendable {
  public let manifestFormatVersion: Int
  public let snapshotID: UUID
  public let createdAt: Date
  public let sourceFingerprint: String
  public let clientBuild: WeChatClientBuildFingerprint?
  public let acquisition: SnapshotAcquisitionEvidence?
  public let entries: [SnapshotEntry]

  public init(
    manifestFormatVersion: Int = 3,
    snapshotID: UUID,
    createdAt: Date,
    sourceFingerprint: String,
    clientBuild: WeChatClientBuildFingerprint? = nil,
    acquisition: SnapshotAcquisitionEvidence? = nil,
    entries: [SnapshotEntry]
  ) {
    self.manifestFormatVersion = manifestFormatVersion
    self.snapshotID = snapshotID
    self.createdAt = createdAt
    self.sourceFingerprint = sourceFingerprint
    self.clientBuild = clientBuild
    self.acquisition = acquisition
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
  var allowAtomicClone = true
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
    let plan = try SnapshotAcquisitionPlanner(includeSourcePaths: privacy.includePaths).plan(
      sets: sets
    )
    return try createSnapshot(of: plan, cleanUpOnDeinit: cleanUpOnDeinit)
  }

  public func createSnapshot(
    of plan: SnapshotAcquisitionPlan,
    cleanUpOnDeinit: Bool = true
  ) throws -> SnapshotLease {
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
        let manifest = try snapshotAttempt(plan: plan, snapshotID: snapshotID, into: directory)
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
    plan: SnapshotAcquisitionPlan,
    snapshotID: UUID,
    into directory: URL
  ) throws -> SnapshotManifest {
    let acquisitionPlanner = SnapshotAcquisitionPlanner(includeSourcePaths: privacy.includePaths)
    try acquisitionPlanner.verifyForSnapshot(plan)
    var captured:
      [(
        source: PlannedFile,
        fingerprint: SourceFileFingerprint,
        captureMethod: SnapshotCaptureMethod
      )] = []

    for (index, set) in plan.selectedSets.enumerated() {
      let sources = try plannedFiles(for: set, setIndex: index)
      let baseline = try Dictionary(
        uniqueKeysWithValues: sources.map { source in
          (source.source.standardizedFileURL.path, try fingerprint(at: source.source))
        })
      var copied: [(source: PlannedFile, fingerprint: SourceFileFingerprint, cloned: Bool)] = []
      for source in sources {
        let destination = directory.appending(path: source.relativePath)
        try ensureOwnerOnlyDirectory(destination.deletingLastPathComponent())
        let result = try cloneOrCopyFromReadOnlyDescriptor(source.source, to: destination)
        guard result.fingerprint == baseline[source.source.path] else {
          throw SnapshotError.sourceChanged(privacy.reference(for: source.source).opaqueID)
        }
        hooks.afterCopy(source.source)
        copied.append((source, result.fingerprint, result.usedAtomicClone))
      }

      let sourcesRequiringGroupStability =
        copied.allSatisfy(\.cloned)
        ? copied.filter { $0.source.role == .database }
        : copied
      for item in sourcesRequiringGroupStability {
        let after = try fingerprint(at: item.source.source)
        guard after == baseline[item.source.source.path] else {
          throw SnapshotError.sourceChanged(privacy.reference(for: item.source.source).opaqueID)
        }
      }
      captured.append(
        contentsOf: copied.map {
          (
            $0.source,
            $0.fingerprint,
            $0.cloned ? .atomicCopyOnWriteClone : .verifiedByteCopy
          )
        })
    }

    try acquisitionPlanner.verifyForSnapshot(plan)

    let sortedEntries = try captured.map { item in
      let destination = directory.appending(path: item.source.relativePath)
      return SnapshotEntry(
        source: privacy.reference(for: item.source.source),
        sourceSetID: item.source.setID,
        logicalPath: item.source.logicalPath,
        relativePath: item.source.relativePath,
        role: item.source.role,
        fingerprint: item.fingerprint,
        sha256: try sha256(ofSnapshotFile: destination, expected: item.fingerprint),
        captureMethod: item.captureMethod
      )
    }.sorted { $0.relativePath < $1.relativePath }
    let acquisition = try acquisitionPlanner.finalizedEvidence(for: plan, entries: sortedEntries)
    let manifest = SnapshotManifest(
      snapshotID: snapshotID,
      createdAt: Date(),
      sourceFingerprint: acquisitionPlanner.sourceFingerprint(for: acquisition),
      clientBuild: clientBuild,
      acquisition: acquisition,
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

  private func cloneOrCopyFromReadOnlyDescriptor(
    _ source: URL,
    to destination: URL
  ) throws -> (fingerprint: SourceFileFingerprint, usedAtomicClone: Bool) {
    let sourceDescriptor = Darwin.open(source.path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW)
    guard sourceDescriptor >= 0 else {
      throw SnapshotError.posix(operation: "open source read-only", code: errno)
    }
    defer { Darwin.close(sourceDescriptor) }

    let before = try fingerprint(descriptor: sourceDescriptor, source: source)
    let destinationDirectory = destination.deletingLastPathComponent()
    let destinationDirectoryDescriptor = Darwin.open(
      destinationDirectory.path,
      O_RDONLY | O_CLOEXEC | O_DIRECTORY | O_NOFOLLOW
    )
    guard destinationDirectoryDescriptor >= 0 else {
      throw SnapshotError.posix(operation: "open snapshot directory", code: errno)
    }
    defer { Darwin.close(destinationDirectoryDescriptor) }

    var cloneError = ENOTSUP
    if hooks.allowAtomicClone {
      let cloneResult = destination.lastPathComponent.withCString { name in
        fclonefileat(sourceDescriptor, destinationDirectoryDescriptor, name, 0)
      }
      if cloneResult == 0 {
        guard Darwin.chmod(destination.path, S_IRUSR | S_IWUSR) == 0 else {
          throw SnapshotError.posix(operation: "secure cloned snapshot", code: errno)
        }
        let after = try fingerprint(descriptor: sourceDescriptor, source: source)
        guard before == after else {
          throw SnapshotError.sourceChanged(privacy.reference(for: source).opaqueID)
        }
        return (before, true)
      }
      cloneError = errno
    }
    guard
      cloneError == ENOTSUP || cloneError == EXDEV || cloneError == EINVAL || cloneError == ENOSYS
    else {
      throw SnapshotError.posix(operation: "clone snapshot", code: cloneError)
    }

    let destinationDescriptor = Darwin.open(
      destination.path,
      O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW,
      S_IRUSR | S_IWUSR
    )
    guard destinationDescriptor >= 0 else {
      throw SnapshotError.posix(operation: "create snapshot file", code: errno)
    }
    defer { Darwin.close(destinationDescriptor) }

    var buffer = [UInt8](repeating: 0, count: 128 * 1024)
    while true {
      let readCount = Darwin.read(sourceDescriptor, &buffer, buffer.count)
      if readCount == 0 { break }
      if readCount < 0 {
        if errno == EINTR { continue }
        throw SnapshotError.posix(operation: "read source", code: errno)
      }

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
    return (before, false)
  }

  private func sha256(
    ofSnapshotFile url: URL,
    expected: SourceFileFingerprint
  ) throws -> String {
    let descriptor = Darwin.open(url.path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW)
    guard descriptor >= 0 else {
      throw SnapshotError.posix(operation: "open snapshot for verification", code: errno)
    }
    defer { Darwin.close(descriptor) }
    let observed = try fingerprint(descriptor: descriptor, source: url)
    guard observed.byteCount == expected.byteCount else {
      throw SnapshotError.sourceChanged(privacy.reference(for: url).opaqueID)
    }
    var hasher = SHA256()
    var buffer = [UInt8](repeating: 0, count: 128 * 1024)
    while true {
      let count = Darwin.read(descriptor, &buffer, buffer.count)
      if count == 0 { break }
      if count < 0 {
        if errno == EINTR { continue }
        throw SnapshotError.posix(operation: "hash snapshot", code: errno)
      }
      hasher.update(data: Data(buffer[0..<count]))
    }
    return hasher.finalize().map { String(format: "%02x", $0) }.joined()
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
