import Testing

@testable import GreenBubblesCore

struct ArtifactClassifierTests {
  private let classifier = ArtifactClassifier()

  @Test(
    "Classifies database families and SQLite sidecars",
    arguments: [
      ("message.db", ArtifactKind.database),
      ("contacts.SQLITE", ArtifactKind.database),
      ("message.db-wal", ArtifactKind.writeAheadLog),
      ("message.db-shm", ArtifactKind.sharedMemory),
    ])
  func databaseKinds(fileName: String, expected: ArtifactKind) {
    #expect(classifier.classify(fileName: fileName) == expected)
  }

  @Test(
    "Classifies representative content artifacts",
    arguments: [
      ("photo.webp", ArtifactKind.image),
      ("voice.silk", ArtifactKind.audio),
      ("clip.mp4", ArtifactKind.video),
      ("payload.pb", ArtifactKind.serializedData),
      ("settings.plist", ArtifactKind.configuration),
    ])
  func contentKinds(fileName: String, expected: ArtifactKind) {
    #expect(classifier.classify(fileName: fileName) == expected)
  }

  @Test func ignoresUnrelatedFiles() {
    #expect(classifier.classify(fileName: "README") == nil)
    #expect(classifier.classify(fileName: "program.dylib") == nil)
  }
}
