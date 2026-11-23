import Darwin
import Foundation

/// Finds app sandbox containers that hold WeChat 4.x account data.
///
/// The standard client keeps its data in
/// `~/Library/Containers/com.tencent.xinWeChat/Data/Documents/xwechat_files`.
/// A duplicated app copy with a different bundle identifier gets its own
/// container, so every `~/Library/Containers/<bundle id>` is checked for that
/// same relative path.
///
/// The scan lists `~/Library/Containers` once and then only `lstat`s the fixed
/// path below each entry. It never lists or reads inside another app's
/// container, so it does not walk unrelated app data (which can raise macOS
/// privacy prompts). Any error is treated as "not found", and every path
/// component below `Containers` must be a real directory, not a symbolic link.
struct WeChatAppContainerScanner: Sendable {
  static let standardBundleIdentifier = "com.tencent.xinWeChat"
  static let dataSubpath = ["Data", "Documents", "xwechat_files"]

  struct Container: Equatable, Sendable {
    /// The container directory name, which macOS sets to the app's bundle ID.
    let bundleIdentifier: String
    let containerRoot: URL
    let xwechatFilesRoot: URL
  }

  let homeDirectory: URL

  init(homeDirectory: URL) {
    self.homeDirectory = homeDirectory.standardizedFileURL
  }

  var containersRoot: URL {
    homeDirectory.appending(path: "Library/Containers", directoryHint: .isDirectory)
  }

  /// The standard container first, then other app copies sorted by bundle ID.
  ///
  /// Names come from the directory listing, so `bundleIdentifier` is the
  /// on-disk spelling; the standard name is probed directly only when the
  /// listing has no entry that matches it case-insensitively (for example
  /// when listing fails). Every candidate gets the same no-symlink check, and
  /// candidates that resolve to the same `xwechat_files` directory (device and
  /// inode) are reported once, which also covers case-insensitive volumes.
  ///
  /// When a differently cased standard entry is the same directory as the
  /// hard-coded standard path, the hard-coded path is kept for the URLs, as
  /// earlier versions used, so path-derived account IDs do not change; only
  /// `bundleIdentifier` takes the on-disk spelling.
  func containers(fileManager: FileManager = .default) -> [Container] {
    let names = ((try? fileManager.contentsOfDirectory(atPath: containersRoot.path)) ?? [])
      .filter { !$0.hasPrefix(".") }
    let isStandard = { (name: String) in
      name.caseInsensitiveCompare(Self.standardBundleIdentifier) == .orderedSame
    }
    let standardNames = names.filter(isStandard).sorted()
    let ordered =
      (standardNames.isEmpty ? [Self.standardBundleIdentifier] : standardNames)
      + names.filter { !isStandard($0) }.sorted()
    let canonicalStandard = safeContainer(named: Self.standardBundleIdentifier)
    let canonicalIdentity = canonicalStandard.flatMap { Self.fileIdentity($0.xwechatFilesRoot) }

    var seen = Set<FileIdentity>()
    return ordered.compactMap { name in
      guard let container = safeContainer(named: name),
        let identity = Self.fileIdentity(container.xwechatFilesRoot),
        seen.insert(identity).inserted
      else { return nil }
      if let canonicalStandard, identity == canonicalIdentity {
        return Container(
          bundleIdentifier: container.bundleIdentifier,
          containerRoot: canonicalStandard.containerRoot,
          xwechatFilesRoot: canonicalStandard.xwechatFilesRoot
        )
      }
      return container
    }
  }

  struct FileIdentity: Hashable {
    let device: dev_t
    let inode: ino_t
  }

  /// Identity of the entry itself (`lstat`); a symbolic link is its own entry.
  static func fileIdentity(_ url: URL) -> FileIdentity? {
    var metadata = stat()
    guard Darwin.lstat(url.path, &metadata) == 0 else { return nil }
    return FileIdentity(device: metadata.st_dev, inode: metadata.st_ino)
  }

  /// Identity of the directory a path resolves to (`stat`, following links).
  static func targetIdentity(_ url: URL) -> FileIdentity? {
    var metadata = stat()
    guard fstatat(AT_FDCWD, url.path, &metadata, 0) == 0 else { return nil }
    return FileIdentity(device: metadata.st_dev, inode: metadata.st_ino)
  }

  private func safeContainer(named name: String) -> Container? {
    guard !name.isEmpty, !name.contains("/") else { return nil }
    let container = makeContainer(named: name)
    var url = container.containerRoot
    guard Self.isRealDirectory(url) else { return nil }
    for component in Self.dataSubpath {
      url = url.appending(path: component, directoryHint: .isDirectory)
      guard Self.isRealDirectory(url) else { return nil }
    }
    return container
  }

  private func makeContainer(named name: String) -> Container {
    let root = containersRoot.appending(path: name, directoryHint: .isDirectory)
    var files = root
    for component in Self.dataSubpath {
      files = files.appending(path: component, directoryHint: .isDirectory)
    }
    return Container(
      bundleIdentifier: name,
      containerRoot: root.standardizedFileURL,
      xwechatFilesRoot: files.standardizedFileURL
    )
  }

  /// `lstat` only: a symbolic link, a missing entry, or any error is rejected.
  static func isRealDirectory(_ url: URL) -> Bool {
    var metadata = stat()
    guard Darwin.lstat(url.path, &metadata) == 0 else { return false }
    return metadata.st_mode & S_IFMT == S_IFDIR
  }
}
