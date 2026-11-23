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
  func containers(fileManager: FileManager = .default) -> [Container] {
    let names =
      (try? fileManager.contentsOfDirectory(atPath: containersRoot.path)) ?? []
    let others =
      names
      .filter { !$0.hasPrefix(".") && $0 != Self.standardBundleIdentifier }
      .sorted()
      .compactMap { safeContainer(named: $0) }
    return (standardContainer(fileManager: fileManager).map { [$0] } ?? []) + others
  }

  /// The standard container keeps its earlier existence check so current
  /// installations are discovered exactly as before.
  private func standardContainer(fileManager: FileManager) -> Container? {
    let container = makeContainer(named: Self.standardBundleIdentifier)
    guard fileManager.fileExists(atPath: container.xwechatFilesRoot.path) else { return nil }
    return container
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
