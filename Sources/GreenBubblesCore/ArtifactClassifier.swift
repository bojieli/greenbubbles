import Foundation

public struct ArtifactClassifier: Sendable {
  private static let databaseExtensions: Set<String> = ["db", "sqlite", "sqlite3"]
  private static let indexExtensions: Set<String> = ["idx", "index", "fts"]
  private static let serializedExtensions: Set<String> = ["pb", "protobuf", "archive"]
  private static let configurationExtensions: Set<String> = ["plist", "json", "xml"]
  private static let imageExtensions: Set<String> = [
    "apng", "gif", "heic", "jpeg", "jpg", "png", "webp",
  ]
  private static let audioExtensions: Set<String> = [
    "aac", "amr", "caf", "m4a", "mp3", "ogg", "silk", "wav",
  ]
  private static let videoExtensions: Set<String> = ["m4v", "mov", "mp4", "webm"]
  private static let documentExtensions: Set<String> = [
    "doc", "docx", "pdf", "ppt", "pptx", "rtf", "xls", "xlsx",
  ]

  public init() {}

  public func classify(fileName: String) -> ArtifactKind? {
    let lowercased = fileName.lowercased()

    if lowercased.hasSuffix("-wal") { return .writeAheadLog }
    if lowercased.hasSuffix("-shm") { return .sharedMemory }

    let fileExtension = URL(fileURLWithPath: lowercased).pathExtension
    if Self.databaseExtensions.contains(fileExtension) { return .database }
    if Self.indexExtensions.contains(fileExtension) { return .index }
    if Self.serializedExtensions.contains(fileExtension) { return .serializedData }
    if Self.configurationExtensions.contains(fileExtension) { return .configuration }
    if Self.imageExtensions.contains(fileExtension) { return .image }
    if Self.audioExtensions.contains(fileExtension) { return .audio }
    if Self.videoExtensions.contains(fileExtension) { return .video }
    if Self.documentExtensions.contains(fileExtension) { return .document }
    return nil
  }
}
