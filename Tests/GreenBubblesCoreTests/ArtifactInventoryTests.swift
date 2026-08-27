import Foundation
import Testing

@testable import GreenBubblesCore

struct ArtifactInventoryTests {
  @Test func inventoriesMetadataWithoutExposingPathsByDefault() throws {
    let fixture = try TemporaryFixture()
    defer { fixture.remove() }

    try fixture.createFile("account/messages.db", bytes: 4)
    try fixture.createFile("account/messages.db-wal", bytes: 8)
    try fixture.createFile("account/avatar.png", bytes: 2)
    try fixture.createFile("account/ignored.txt", bytes: 1)

    let report = ArtifactInventory(
      options: InventoryOptions(
        maxDepth: 4,
        maxArtifacts: 100,
        includePaths: false
      )
    ).inventory(roots: [(fixture.root, .supplied)])

    #expect(report.artifacts.count == 3)
    #expect(Set(report.artifacts.map(\.kind)) == [.database, .writeAheadLog, .image])
    #expect(report.artifacts.allSatisfy { $0.location.path == nil })
    #expect(report.artifacts.allSatisfy { $0.byteCount != nil })
    #expect(!report.reachedArtifactLimit)
  }

  @Test func pathDisclosureRequiresExplicitOption() throws {
    let fixture = try TemporaryFixture()
    defer { fixture.remove() }
    try fixture.createFile("messages.sqlite", bytes: 1)

    let report = ArtifactInventory(
      options: InventoryOptions(
        includePaths: true
      )
    ).inventory(roots: [(fixture.root, .supplied)])

    #expect(report.roots.first?.location.path == fixture.root.standardizedFileURL.path)
    #expect(report.artifacts.first?.location.path?.hasSuffix("messages.sqlite") == true)
  }

  @Test func stopsAtArtifactLimit() throws {
    let fixture = try TemporaryFixture()
    defer { fixture.remove() }
    try fixture.createFile("one.db", bytes: 1)
    try fixture.createFile("two.db", bytes: 1)

    let report = ArtifactInventory(
      options: InventoryOptions(
        maxArtifacts: 1
      )
    ).inventory(roots: [(fixture.root, .supplied)])

    #expect(report.artifacts.count == 1)
    #expect(report.reachedArtifactLimit)
  }
}

private struct TemporaryFixture {
  let root: URL

  init() throws {
    root = FileManager.default.temporaryDirectory
      .appending(path: "greenbubbles-tests-\(UUID().uuidString)", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
  }

  func createFile(_ relativePath: String, bytes: Int) throws {
    let url = root.appending(path: relativePath)
    try FileManager.default.createDirectory(
      at: url.deletingLastPathComponent(),
      withIntermediateDirectories: true
    )
    let created = FileManager.default.createFile(
      atPath: url.path,
      contents: Data(repeating: 0x47, count: bytes)
    )
    if !created {
      throw CocoaError(.fileWriteUnknown)
    }
  }

  func remove() {
    try? FileManager.default.removeItem(at: root)
  }
}
