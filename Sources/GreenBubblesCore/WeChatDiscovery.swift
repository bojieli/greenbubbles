import Foundation

public struct WeChatDiscovery: Sendable {
  private let homeDirectory: URL
  private let privacy: PathPrivacy
  private let additionalApplicationURLs: [URL]

  public init(
    homeDirectory: URL = FileManager.default.homeDirectoryForCurrentUser,
    includePaths: Bool = false,
    additionalApplicationURLs: [URL] = []
  ) {
    self.homeDirectory = homeDirectory.standardizedFileURL
    self.privacy = PathPrivacy(includePaths: includePaths)
    self.additionalApplicationURLs = additionalApplicationURLs
  }

  public func discover() -> DiscoveryReport {
    let fileManager = FileManager.default
    let installations = applicationCandidates()
      .filter { fileManager.fileExists(atPath: $0.path) }
      .map(readInstallation)
      .sorted { $0.location.opaqueID < $1.location.opaqueID }

    let roots = dataRootCandidates(fileManager: fileManager)
      .map { candidate in
        CandidateDataRoot(
          location: privacy.reference(for: candidate.url),
          kind: candidate.kind,
          isReadable: fileManager.isReadableFile(atPath: candidate.url.path),
          containerBundleIdentifier: candidate.containerBundleIdentifier
        )
      }
      .sorted { $0.location.opaqueID < $1.location.opaqueID }

    return DiscoveryReport(
      generatedAt: Date(),
      installations: installations,
      dataRoots: roots
    )
  }

  public func accessibleDataRoots() -> [(url: URL, kind: DataRootKind)] {
    accessibleDataRootsWithOwners().map { ($0.url, $0.kind) }
  }

  /// Readable automatic data roots with the bundle ID of the app container
  /// each belongs to, for inventory reports.
  public func accessibleDataRootsWithOwners()
    -> [(url: URL, kind: DataRootKind, containerBundleIdentifier: String?)]
  {
    let fileManager = FileManager.default
    return dataRootCandidates(fileManager: fileManager)
      .filter { fileManager.isReadableFile(atPath: $0.url.path) }
  }

  private func applicationCandidates() -> [URL] {
    let candidates =
      [
        URL(fileURLWithPath: "/Applications/WeChat.app"),
        URL(fileURLWithPath: "/Applications/微信.app"),
        homeDirectory.appending(path: "Applications/WeChat.app"),
        homeDirectory.appending(path: "Applications/微信.app"),
      ] + additionalApplicationURLs

    return uniqueURLs(candidates)
  }

  private typealias DataRootCandidate = (
    url: URL, kind: DataRootKind, containerBundleIdentifier: String?
  )

  private static let standardContainerNames = ["com.tencent.xinWeChat", "com.tencent.WeChat"]

  private func dataRootCandidates(fileManager: FileManager) -> [DataRootCandidate] {
    let containers = homeDirectory.appending(path: "Library/Containers")
    let scanned = WeChatAppContainerScanner(homeDirectory: homeDirectory)
      .containers(fileManager: fileManager)
    var coveredByStandard = Set<WeChatAppContainerScanner.FileIdentity>()
    var candidates: [DataRootCandidate] = Self.standardContainerNames.map { name in
      let container = containers.appending(path: name)
      // Report the on-disk spelling when a scanned container is this one.
      var bundleIdentifier = name
      // Only a standard path made of real directories covers a scanned root:
      // inventory does not follow symbolic links, so a linked standard path
      // would leave the real directory unvisited.
      let files = container.appending(path: "Data/Documents/xwechat_files")
      let reachable = [
        container, container.appending(path: "Data"),
        container.appending(path: "Data/Documents"), files,
      ].allSatisfy(WeChatAppContainerScanner.isRealDirectory)
      if reachable, let identity = WeChatAppContainerScanner.targetIdentity(files) {
        coveredByStandard.insert(identity)
        if let match = scanned.first(where: { $0.identity == identity }) {
          bundleIdentifier = match.bundleIdentifier
        }
      }
      return (container, .applicationContainer, bundleIdentifier)
    }
    // Other app containers holding WeChat data, such as duplicated app
    // copies. Only their xwechat_files directory is a data root, so automatic
    // inventory never walks other data in that container. A scanned container
    // is skipped only when an existing standard container is the same
    // directory.
    for container in scanned {
      if coveredByStandard.contains(container.identity) { continue }
      candidates.append(
        (container.xwechatFilesRoot, .applicationContainer, container.bundleIdentifier))
    }

    let groupContainers = homeDirectory.appending(path: "Library/Group Containers")
    let groupChildren =
      (try? fileManager.contentsOfDirectory(
        at: groupContainers,
        includingPropertiesForKeys: [.isDirectoryKey],
        options: [.skipsHiddenFiles]
      )) ?? []

    for child in groupChildren where looksLikeWeChatGroupContainer(child) {
      candidates.append((child, .groupContainer, nil))
    }

    var seen = Set<String>()
    return candidates.filter { candidate in
      guard fileManager.fileExists(atPath: candidate.url.path) else { return false }
      return seen.insert(candidate.url.standardizedFileURL.path).inserted
    }
  }

  private func looksLikeWeChatGroupContainer(_ url: URL) -> Bool {
    let name = url.lastPathComponent.lowercased()
    return name.contains("tencent") && (name.contains("wechat") || name.contains("xinwechat"))
  }

  private func readInstallation(at url: URL) -> WeChatInstallation {
    let infoURL = url.appending(path: "Contents/Info.plist")
    var bundleIdentifier: String?
    var version: String?
    var build: String?

    if let data = try? Data(contentsOf: infoURL),
      let value = try? PropertyListSerialization.propertyList(from: data, format: nil),
      let info = value as? [String: Any]
    {
      bundleIdentifier = info["CFBundleIdentifier"] as? String
      version = info["CFBundleShortVersionString"] as? String
      build = info["CFBundleVersion"] as? String
    }

    return WeChatInstallation(
      location: privacy.reference(for: url),
      bundleIdentifier: bundleIdentifier,
      version: version,
      build: build
    )
  }

  private func uniqueURLs(_ urls: [URL]) -> [URL] {
    var seen = Set<String>()
    return urls.filter { seen.insert($0.standardizedFileURL.path).inserted }
  }
}
