import CryptoKit
import Darwin
import Foundation

public enum SnapshotAcquisitionMode: String, Codable, Equatable, Sendable {
  case bootstrap
  case incremental
  case integrityScan
}

public struct SnapshotSourceFileInventory: Codable, Equatable, Sendable {
  public let role: SnapshotFileRole
  public let fingerprint: SourceFileFingerprint
  public let contentSHA256: String?

  public init(
    role: SnapshotFileRole,
    fingerprint: SourceFileFingerprint,
    contentSHA256: String? = nil
  ) {
    self.role = role
    self.fingerprint = fingerprint
    self.contentSHA256 = contentSHA256
  }
}

public struct SnapshotSourceSetInventory: Codable, Equatable, Sendable {
  public let sourceSetID: String
  public let logicalPath: String
  public let files: [SnapshotSourceFileInventory]

  public init(
    sourceSetID: String,
    logicalPath: String,
    files: [SnapshotSourceFileInventory]
  ) {
    self.sourceSetID = sourceSetID
    self.logicalPath = logicalPath
    self.files = files.sorted { $0.role.rawValue < $1.role.rawValue }
  }
}

public struct SnapshotAcquisitionEvidence: Codable, Equatable, Sendable {
  public let formatVersion: Int
  public let mode: SnapshotAcquisitionMode
  public let previousSourceFingerprint: String?
  public let reconciliationWindowSeconds: Int
  public let changedSourceSetIDs: [String]
  public let reconciliationSourceSetIDs: [String]
  public let deletedSourceSetIDs: [String]
  public let sourceSets: [SnapshotSourceSetInventory]
  public let lastIntegrityScanAt: Date?

  public init(
    formatVersion: Int = 2,
    mode: SnapshotAcquisitionMode,
    previousSourceFingerprint: String?,
    reconciliationWindowSeconds: Int,
    changedSourceSetIDs: [String],
    reconciliationSourceSetIDs: [String],
    deletedSourceSetIDs: [String],
    sourceSets: [SnapshotSourceSetInventory],
    lastIntegrityScanAt: Date? = nil
  ) {
    self.formatVersion = formatVersion
    self.mode = mode
    self.previousSourceFingerprint = previousSourceFingerprint
    self.reconciliationWindowSeconds = reconciliationWindowSeconds
    self.changedSourceSetIDs = changedSourceSetIDs.sorted()
    self.reconciliationSourceSetIDs = reconciliationSourceSetIDs.sorted()
    self.deletedSourceSetIDs = deletedSourceSetIDs.sorted()
    self.sourceSets = sourceSets.sorted { $0.sourceSetID < $1.sourceSetID }
    self.lastIntegrityScanAt = lastIntegrityScanAt
  }

  public var selectedSourceSetIDs: [String] {
    Array(Set(changedSourceSetIDs).union(reconciliationSourceSetIDs)).sorted()
  }
}

public struct SnapshotAcquisitionPlan: Sendable {
  public let evidence: SnapshotAcquisitionEvidence
  public let selectedSets: [DatabaseFileSet]

  let allSets: [DatabaseFileSet]

  public var isNoOp: Bool {
    evidence.mode == .incremental
      && selectedSets.isEmpty
      && evidence.deletedSourceSetIDs.isEmpty
  }
}

public enum SnapshotAcquisitionPlannerError: Error, Equatable, CustomStringConvertible {
  case noDatabaseSets
  case invalidReconciliationWindow
  case invalidIntegrityScanInterval
  case unsafeSource(String)
  case sourceChanged(String)

  public var description: String {
    switch self {
    case .noDatabaseSets:
      return "No database sets were supplied"
    case .invalidReconciliationWindow:
      return "The reconciliation window must not be negative"
    case .invalidIntegrityScanInterval:
      return "The integrity-scan interval must be positive"
    case .unsafeSource(let sourceID):
      return "A snapshot source is missing, symbolic, or not a regular file: \(sourceID)"
    case .sourceChanged(let sourceID):
      return "Source inventory changed while taking the snapshot: \(sourceID)"
    }
  }
}

public struct SnapshotAcquisitionPlanner: Sendable {
  private let privacy: PathPrivacy

  public init(includeSourcePaths: Bool = false) {
    self.privacy = PathPrivacy(includePaths: includeSourcePaths)
  }

  public func plan(
    sets: [DatabaseFileSet],
    previousManifest: SnapshotManifest? = nil,
    forceIntegrityScan: Bool = false,
    integrityScanInterval: TimeInterval? = nil,
    reconciliationWindow: TimeInterval = 15 * 60,
    now: Date = Date()
  ) throws -> SnapshotAcquisitionPlan {
    guard !sets.isEmpty else { throw SnapshotAcquisitionPlannerError.noDatabaseSets }
    guard reconciliationWindow >= 0, reconciliationWindow <= Double(Int.max) else {
      throw SnapshotAcquisitionPlannerError.invalidReconciliationWindow
    }
    if let integrityScanInterval,
      integrityScanInterval <= 0 || integrityScanInterval > Double(Int.max)
    {
      throw SnapshotAcquisitionPlannerError.invalidIntegrityScanInterval
    }
    let orderedSets = sets.sorted { $0.database.path < $1.database.path }
    let previousInventory = previousManifest.map(inventoryBySet(from:)) ?? [:]
    let currentInventory = try orderedSets.map { set in
      try inventory(for: set, carrying: previousInventory)
    }
    let currentByID = Dictionary(uniqueKeysWithValues: currentInventory.map { ($0.sourceSetID, $0) })
    let currentIDs = Set(currentByID.keys)
    let previousIDs = Set(previousInventory.keys)
    let previousIntegrityScanAt = previousManifest.flatMap { manifest in
      manifest.acquisition?.lastIntegrityScanAt
        ?? ((manifest.acquisition?.mode == .bootstrap || manifest.acquisition?.mode == .integrityScan)
          ? manifest.createdAt : nil)
    }
    let integrityScanDue = integrityScanInterval.map { interval in
      previousIntegrityScanAt.map { now.timeIntervalSince($0) >= interval } ?? true
    } ?? false
    let mode: SnapshotAcquisitionMode = if previousManifest == nil {
      .bootstrap
    } else if forceIntegrityScan || integrityScanDue {
      .integrityScan
    } else {
      .incremental
    }

    var changed = Set<String>()
    var reconciliation = Set<String>()
    switch mode {
    case .bootstrap, .integrityScan:
      changed = currentIDs
    case .incremental:
      for sourceSetID in currentIDs {
        guard let current = currentByID[sourceSetID] else { continue }
        guard let previous = previousInventory[sourceSetID],
          metadataEquivalent(previous, current)
        else {
          changed.insert(sourceSetID)
          continue
        }
        let newestModification = current.files.map(\.fingerprint.modifiedDate).max() ?? .distantPast
        if now.timeIntervalSince(newestModification) <= reconciliationWindow {
          reconciliation.insert(sourceSetID)
        }
      }
    }

    let selectedIDs = changed.union(reconciliation)
    let selectedSets = orderedSets.filter {
      selectedIDs.contains(privacy.reference(for: $0.database).opaqueID)
    }
    let evidence = SnapshotAcquisitionEvidence(
      mode: mode,
      previousSourceFingerprint: previousManifest?.sourceFingerprint,
      reconciliationWindowSeconds: Int(reconciliationWindow),
      changedSourceSetIDs: Array(changed),
      reconciliationSourceSetIDs: Array(reconciliation),
      deletedSourceSetIDs: Array(previousIDs.subtracting(currentIDs)),
      sourceSets: currentInventory,
      lastIntegrityScanAt: mode == .incremental ? previousIntegrityScanAt : now
    )
    return SnapshotAcquisitionPlan(
      evidence: evidence,
      selectedSets: selectedSets,
      allSets: orderedSets
    )
  }

  func verify(_ plan: SnapshotAcquisitionPlan) throws {
    let expected = Dictionary(
      uniqueKeysWithValues: plan.evidence.sourceSets.map { ($0.sourceSetID, $0) })
    let current = try plan.allSets.map { try inventory(for: $0, carrying: expected) }
    guard current.count == expected.count else {
      throw SnapshotAcquisitionPlannerError.sourceChanged("source-set-count")
    }
    for item in current {
      guard let prior = expected[item.sourceSetID], metadataEquivalent(prior, item) else {
        throw SnapshotAcquisitionPlannerError.sourceChanged(item.sourceSetID)
      }
    }
  }

  func finalizedEvidence(
    for plan: SnapshotAcquisitionPlan,
    entries: [SnapshotEntry]
  ) throws -> SnapshotAcquisitionEvidence {
    let digests = Dictionary(
      uniqueKeysWithValues: entries.map { ("\($0.sourceSetID):\($0.role.rawValue)", $0.sha256) })
    let finalized = plan.evidence.sourceSets.map { sourceSet in
      SnapshotSourceSetInventory(
        sourceSetID: sourceSet.sourceSetID,
        logicalPath: sourceSet.logicalPath,
        files: sourceSet.files.map { file in
          SnapshotSourceFileInventory(
            role: file.role,
            fingerprint: file.fingerprint,
            contentSHA256: digests["\(sourceSet.sourceSetID):\(file.role.rawValue)"]
              ?? file.contentSHA256
          )
        }
      )
    }
    if let missing = finalized.lazy.flatMap(\.files).first(where: { $0.contentSHA256 == nil }) {
      throw SnapshotAcquisitionPlannerError.sourceChanged(missing.role.rawValue)
    }
    return SnapshotAcquisitionEvidence(
      mode: plan.evidence.mode,
      previousSourceFingerprint: plan.evidence.previousSourceFingerprint,
      reconciliationWindowSeconds: plan.evidence.reconciliationWindowSeconds,
      changedSourceSetIDs: plan.evidence.changedSourceSetIDs,
      reconciliationSourceSetIDs: plan.evidence.reconciliationSourceSetIDs,
      deletedSourceSetIDs: plan.evidence.deletedSourceSetIDs,
      sourceSets: finalized,
      lastIntegrityScanAt: plan.evidence.lastIntegrityScanAt
    )
  }

  func sourceFingerprint(for evidence: SnapshotAcquisitionEvidence) -> String {
    var hasher = SHA256()
    for sourceSet in evidence.sourceSets {
      hasher.update(data: Data(sourceSet.sourceSetID.utf8))
      hasher.update(data: Data([0]))
      hasher.update(data: Data(sourceSet.logicalPath.utf8))
      for file in sourceSet.files {
        let fields = [
          file.role.rawValue,
          String(file.fingerprint.deviceID),
          String(file.fingerprint.fileID),
          String(file.fingerprint.byteCount),
          String(file.fingerprint.modifiedSeconds),
          String(file.fingerprint.modifiedNanoseconds),
          file.contentSHA256 ?? "missing",
        ]
        for field in fields {
          hasher.update(data: Data([0x1f]))
          hasher.update(data: Data(field.utf8))
        }
      }
      hasher.update(data: Data([0x1e]))
    }
    return hasher.finalize().map { String(format: "%02x", $0) }.joined()
  }

  private func inventory(
    for set: DatabaseFileSet,
    carrying previous: [String: SnapshotSourceSetInventory]
  ) throws -> SnapshotSourceSetInventory {
    let sourceSetID = privacy.reference(for: set.database).opaqueID
    let previousFiles = Dictionary(
      uniqueKeysWithValues: (previous[sourceSetID]?.files ?? []).map { ($0.role, $0) })
    let discovered = try currentFiles(for: set)
    let files = try discovered.map { role, url in
      let fingerprint = try fingerprint(at: url)
      let carriedDigest = previousFiles[role].flatMap { prior in
        prior.fingerprint == fingerprint ? prior.contentSHA256 : nil
      }
      return SnapshotSourceFileInventory(
        role: role,
        fingerprint: fingerprint,
        contentSHA256: carriedDigest
      )
    }
    return SnapshotSourceSetInventory(
      sourceSetID: sourceSetID,
      logicalPath: set.logicalPath,
      files: files
    )
  }

  private func currentFiles(for set: DatabaseFileSet) throws -> [(SnapshotFileRole, URL)] {
    var result: [(SnapshotFileRole, URL)] = [(.database, set.database)]
    for (role, suffix) in [
      (SnapshotFileRole.writeAheadLog, "-wal"),
      (.sharedMemory, "-shm"),
    ] {
      let url = URL(fileURLWithPath: set.database.path + suffix)
      var metadata = stat()
      if Darwin.lstat(url.path, &metadata) == 0 {
        guard metadata.st_mode & S_IFMT == S_IFREG else {
          throw SnapshotAcquisitionPlannerError.unsafeSource(privacy.reference(for: url).opaqueID)
        }
        result.append((role, url))
      } else if errno != ENOENT {
        throw SnapshotAcquisitionPlannerError.unsafeSource(privacy.reference(for: url).opaqueID)
      }
    }
    return result
  }

  private func fingerprint(at url: URL) throws -> SourceFileFingerprint {
    var metadata = stat()
    guard Darwin.lstat(url.path, &metadata) == 0,
      metadata.st_mode & S_IFMT == S_IFREG,
      metadata.st_nlink == 1
    else {
      throw SnapshotAcquisitionPlannerError.unsafeSource(privacy.reference(for: url).opaqueID)
    }
    return SourceFileFingerprint(
      deviceID: UInt64(metadata.st_dev),
      fileID: UInt64(metadata.st_ino),
      byteCount: Int64(metadata.st_size),
      modifiedSeconds: Int64(metadata.st_mtimespec.tv_sec),
      modifiedNanoseconds: Int64(metadata.st_mtimespec.tv_nsec)
    )
  }

  private func metadataEquivalent(
    _ previous: SnapshotSourceSetInventory,
    _ current: SnapshotSourceSetInventory
  ) -> Bool {
    guard previous.sourceSetID == current.sourceSetID,
      previous.logicalPath == current.logicalPath,
      previous.files.count == current.files.count
    else { return false }
    return zip(previous.files, current.files).allSatisfy { previousFile, currentFile in
      previousFile.role == currentFile.role
        && previousFile.fingerprint == currentFile.fingerprint
    }
  }

  private func inventoryBySet(from manifest: SnapshotManifest) -> [String: SnapshotSourceSetInventory] {
    if let acquisition = manifest.acquisition {
      return Dictionary(uniqueKeysWithValues: acquisition.sourceSets.map { ($0.sourceSetID, $0) })
    }
    let grouped = Dictionary(grouping: manifest.entries, by: \.sourceSetID)
    return grouped.reduce(into: [:]) { result, item in
      let database = item.value.first(where: { $0.role == .database })
      result[item.key] = SnapshotSourceSetInventory(
        sourceSetID: item.key,
        logicalPath: database?.logicalPath ?? item.value[0].logicalPath,
        files: item.value.map {
          SnapshotSourceFileInventory(
            role: $0.role,
            fingerprint: $0.fingerprint,
            contentSHA256: $0.sha256
          )
        }
      )
    }
  }
}

private extension SourceFileFingerprint {
  var modifiedDate: Date {
    Date(
      timeIntervalSince1970: TimeInterval(modifiedSeconds)
        + TimeInterval(modifiedNanoseconds) / 1_000_000_000
    )
  }
}
