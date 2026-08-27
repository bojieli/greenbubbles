import Foundation
import Testing

@testable import GreenBubblesCore

@Suite("SnapshotAcquisitionPlannerTests")
struct SnapshotAcquisitionPlannerTests {
  @Test
  func selectsChangedSetsAndCarriesVerifiedHashesForUnchangedSets() throws {
    let fixture = try AcquisitionFixture()
    defer { fixture.remove() }
    let first = try fixture.createFile("source/first.db", bytes: [1, 2, 3])
    let second = try fixture.createFile("source/second.db", bytes: [4, 5, 6])
    let sets = [
      DatabaseFileSet(database: first, logicalPath: "message/first.db"),
      DatabaseFileSet(database: second, logicalPath: "message/second.db"),
    ]
    let snapshotter = ReadOnlySnapshotter(baseDirectory: fixture.snapshots, maxRetries: 0)
    let bootstrap = try snapshotter.createSnapshot(of: sets, cleanUpOnDeinit: false)
    defer { try? bootstrap.cleanUp() }

    #expect(bootstrap.manifest.manifestFormatVersion == 3)
    #expect(bootstrap.manifest.acquisition?.mode == .bootstrap)
    #expect(bootstrap.manifest.entries.count == 2)
    #expect(
      bootstrap.manifest.acquisition?.sourceSets
        .flatMap(\.files)
        .allSatisfy { $0.contentSHA256?.count == 64 } == true)

    try Data([9, 8, 7, 6]).write(to: second)
    let plan = try SnapshotAcquisitionPlanner().plan(
      sets: sets,
      previousManifest: bootstrap.manifest,
      reconciliationWindow: 1,
      now: Date().addingTimeInterval(3_600)
    )
    #expect(plan.evidence.mode == .incremental)
    #expect(plan.evidence.changedSourceSetIDs.count == 1)
    #expect(plan.evidence.reconciliationSourceSetIDs.isEmpty)
    #expect(plan.selectedSets.map(\.logicalPath) == ["message/second.db"])

    let incremental = try snapshotter.createSnapshot(of: plan, cleanUpOnDeinit: false)
    defer { try? incremental.cleanUp() }
    #expect(incremental.manifest.entries.count == 1)
    #expect(incremental.manifest.acquisition?.sourceSets.count == 2)
    #expect(incremental.manifest.sourceFingerprint != bootstrap.manifest.sourceFingerprint)

    let noOp = try SnapshotAcquisitionPlanner().plan(
      sets: sets,
      previousManifest: incremental.manifest,
      reconciliationWindow: 1,
      now: Date().addingTimeInterval(7_200)
    )
    #expect(noOp.isNoOp)
    let unchanged = try snapshotter.createSnapshot(of: noOp, cleanUpOnDeinit: false)
    defer { try? unchanged.cleanUp() }
    #expect(unchanged.manifest.entries.isEmpty)
    #expect(unchanged.manifest.sourceFingerprint == incremental.manifest.sourceFingerprint)
  }

  @Test
  func reconciliationWindowAndIntegrityScanSelectExpectedSets() throws {
    let fixture = try AcquisitionFixture()
    defer { fixture.remove() }
    let first = try fixture.createFile("source/first.db", bytes: [1])
    let second = try fixture.createFile("source/second.db", bytes: [2])
    let sets = [DatabaseFileSet(database: first), DatabaseFileSet(database: second)]
    let snapshotter = ReadOnlySnapshotter(baseDirectory: fixture.snapshots, maxRetries: 0)
    let bootstrap = try snapshotter.createSnapshot(of: sets, cleanUpOnDeinit: false)
    defer { try? bootstrap.cleanUp() }

    let withinWindow = try SnapshotAcquisitionPlanner().plan(
      sets: sets,
      previousManifest: bootstrap.manifest,
      reconciliationWindow: 3_600,
      now: Date()
    )
    #expect(withinWindow.evidence.changedSourceSetIDs.isEmpty)
    #expect(withinWindow.evidence.reconciliationSourceSetIDs.count == 2)
    #expect(withinWindow.selectedSets.count == 2)

    let integrity = try SnapshotAcquisitionPlanner().plan(
      sets: sets,
      previousManifest: bootstrap.manifest,
      forceIntegrityScan: true,
      now: Date().addingTimeInterval(7_200)
    )
    #expect(integrity.evidence.mode == .integrityScan)
    #expect(integrity.evidence.changedSourceSetIDs.count == 2)
    #expect(integrity.selectedSets.count == 2)
  }

  @Test
  func detectsDeletedSourceSetsWithoutSelectingUnchangedData() throws {
    let fixture = try AcquisitionFixture()
    defer { fixture.remove() }
    let first = try fixture.createFile("source/first.db", bytes: [1])
    let second = try fixture.createFile("source/second.db", bytes: [2])
    let allSets = [DatabaseFileSet(database: first), DatabaseFileSet(database: second)]
    let snapshotter = ReadOnlySnapshotter(baseDirectory: fixture.snapshots, maxRetries: 0)
    let bootstrap = try snapshotter.createSnapshot(of: allSets, cleanUpOnDeinit: false)
    defer { try? bootstrap.cleanUp() }

    let plan = try SnapshotAcquisitionPlanner().plan(
      sets: [DatabaseFileSet(database: first)],
      previousManifest: bootstrap.manifest,
      reconciliationWindow: 1,
      now: Date().addingTimeInterval(3_600)
    )
    #expect(plan.selectedSets.isEmpty)
    #expect(plan.evidence.deletedSourceSetIDs.count == 1)
    #expect(!plan.isNoOp)
  }
}

private struct AcquisitionFixture {
  let root: URL
  let snapshots: URL

  init() throws {
    root = FileManager.default.temporaryDirectory.appending(
      path: "greenbubbles-acquisition-tests-\(UUID().uuidString)",
      directoryHint: .isDirectory
    )
    snapshots = root.appending(path: "snapshots", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: snapshots, withIntermediateDirectories: true)
  }

  func createFile(_ relativePath: String, bytes: [UInt8]) throws -> URL {
    let url = root.appending(path: relativePath)
    try FileManager.default.createDirectory(
      at: url.deletingLastPathComponent(),
      withIntermediateDirectories: true
    )
    try Data(bytes).write(to: url)
    return url
  }

  func remove() {
    try? FileManager.default.removeItem(at: root)
  }
}
