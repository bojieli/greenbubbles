import Darwin
import Dispatch
import Foundation

public enum FileSystemWakeupReason: String, Codable, CaseIterable, Hashable, Sendable {
  case write
  case delete
  case rename
  case attribute
  case extend
  case link
  case revoke
  case unknown
}

public struct FileSystemWakeup: Codable, Equatable, Sendable {
  public let root: PathReference
  public let reasons: Set<FileSystemWakeupReason>
  public let observedAt: Date

  public init(
    root: PathReference,
    reasons: Set<FileSystemWakeupReason>,
    observedAt: Date
  ) {
    self.root = root
    self.reasons = reasons
    self.observedAt = observedAt
  }
}

public enum FileSystemWakeupMonitorError: Error, Equatable, CustomStringConvertible {
  case noRoots
  case sourceIsNotDirectory(String)
  case posix(operation: String, code: Int32)

  public var description: String {
    switch self {
    case .noRoots:
      return "At least one filesystem wake-up root is required"
    case .sourceIsNotDirectory(let sourceID):
      return "Filesystem wake-up source is not a directory: \(sourceID)"
    case .posix(let operation, let code):
      return "\(operation) failed with POSIX error \(code)"
    }
  }
}

/// A low-latency hint source. Events never stand in for database reconciliation.
public final class FileSystemWakeupMonitor: @unchecked Sendable {
  private let queue: DispatchQueue
  private let privacy: PathPrivacy
  private let lock = NSLock()
  private var sources: [DispatchSourceFileSystemObject] = []

  public init(
    includePaths: Bool = false,
    queue: DispatchQueue = DispatchQueue(label: "greenbubbles.filesystem-wakeup")
  ) {
    self.queue = queue
    self.privacy = PathPrivacy(includePaths: includePaths)
  }

  public func start(
    roots: [URL],
    handler: @escaping @Sendable (FileSystemWakeup) -> Void
  ) throws {
    guard !roots.isEmpty else { throw FileSystemWakeupMonitorError.noRoots }
    stop()

    var created: [DispatchSourceFileSystemObject] = []
    do {
      for root in roots {
        let standardized = root.standardizedFileURL
        let reference = privacy.reference(for: standardized)
        var metadata = stat()
        guard Darwin.lstat(standardized.path, &metadata) == 0 else {
          throw FileSystemWakeupMonitorError.posix(operation: "inspect wake-up root", code: errno)
        }
        guard metadata.st_mode & S_IFMT == S_IFDIR else {
          throw FileSystemWakeupMonitorError.sourceIsNotDirectory(reference.opaqueID)
        }
        let descriptor = Darwin.open(
          standardized.path,
          O_EVTONLY | O_CLOEXEC | O_NOFOLLOW
        )
        guard descriptor >= 0 else {
          throw FileSystemWakeupMonitorError.posix(operation: "open wake-up root", code: errno)
        }
        let source = DispatchSource.makeFileSystemObjectSource(
          fileDescriptor: descriptor,
          eventMask: [.write, .delete, .rename, .attrib, .extend, .link, .revoke],
          queue: queue
        )
        source.setEventHandler {
          handler(
            FileSystemWakeup(
              root: reference,
              reasons: Self.reasons(for: source.data),
              observedAt: Date()
            ))
        }
        source.setCancelHandler {
          Darwin.close(descriptor)
        }
        source.resume()
        created.append(source)
      }
    } catch {
      for source in created { source.cancel() }
      throw error
    }

    lock.withLock {
      sources = created
    }
  }

  public func stop() {
    let active = lock.withLock { () -> [DispatchSourceFileSystemObject] in
      defer { sources.removeAll() }
      return sources
    }
    for source in active { source.cancel() }
  }

  deinit {
    stop()
  }

  static func reasons(
    for event: DispatchSource.FileSystemEvent
  ) -> Set<FileSystemWakeupReason> {
    var result = Set<FileSystemWakeupReason>()
    if event.contains(.write) { result.insert(.write) }
    if event.contains(.delete) { result.insert(.delete) }
    if event.contains(.rename) { result.insert(.rename) }
    if event.contains(.attrib) { result.insert(.attribute) }
    if event.contains(.extend) { result.insert(.extend) }
    if event.contains(.link) { result.insert(.link) }
    if event.contains(.revoke) { result.insert(.revoke) }
    if result.isEmpty { result.insert(.unknown) }
    return result
  }
}
