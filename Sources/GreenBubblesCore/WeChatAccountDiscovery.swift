import Foundation

public struct WeChatAccountLocation: Codable, Equatable, Sendable {
  public let accountID: String
  public let root: PathReference
  public let databaseRoot: PathReference
  public let attachmentRoot: PathReference?
  public let isReadable: Bool
  /// Bundle ID of the app whose sandbox container holds this account, or nil
  /// for data found in a shared group container.
  public let containerBundleIdentifier: String?

  public init(
    accountID: String,
    root: PathReference,
    databaseRoot: PathReference,
    attachmentRoot: PathReference?,
    isReadable: Bool,
    containerBundleIdentifier: String? = nil
  ) {
    self.accountID = accountID
    self.root = root
    self.databaseRoot = databaseRoot
    self.attachmentRoot = attachmentRoot
    self.isReadable = isReadable
    self.containerBundleIdentifier = containerBundleIdentifier
  }
}

public enum DatabaseRootSelectionError: Error, CustomStringConvertible {
  case none
  case ambiguous

  public var description: String {
    switch self {
    case .none:
      return "No WeChat database root was discovered; pass --db-root to select one"
    case .ambiguous:
      return "Multiple WeChat database roots were found; select the intended account with --db-root"
    }
  }
}

public struct WeChatAccountDiscovery: Sendable {
  private let homeDirectory: URL
  private let privacy: PathPrivacy

  public init(
    homeDirectory: URL = FileManager.default.homeDirectoryForCurrentUser,
    includePaths: Bool = false
  ) {
    self.homeDirectory = homeDirectory.standardizedFileURL
    self.privacy = PathPrivacy(includePaths: includePaths)
  }

  public func discover() -> [WeChatAccountLocation] {
    resolvedAccounts().map(\.report)
  }

  public func databaseRoots(accountID: String? = nil) -> [URL] {
    resolvedAccounts()
      .filter { accountID == nil || $0.matches(accountID: accountID!) }
      .map(\.databaseRoot)
  }

  public func singleDatabaseRoot() throws -> URL {
    let roots = databaseRoots()
    guard let first = roots.first else { throw DatabaseRootSelectionError.none }
    guard roots.count == 1 else { throw DatabaseRootSelectionError.ambiguous }
    return first
  }

  public func accountRoot(accountID: String) -> URL? {
    resolvedAccounts().first { $0.matches(accountID: accountID) }?.root
  }

  struct ResolvedAccount: Sendable {
    let report: WeChatAccountLocation
    let root: URL
    let databaseRoot: URL
    let attachmentRoot: URL?
    /// Bundle ID of the owning app container; nil for a group container.
    let containerBundleIdentifier: String?
    /// IDs of the same account reached through another path, such as a group
    /// container linked to this app container's data. Earlier versions may
    /// have reported one of these, so a saved ID keeps selecting the account.
    var aliasAccountIDs: [String] = []

    func matches(accountID: String) -> Bool {
      report.accountID == accountID || aliasAccountIDs.contains(accountID)
    }
  }

  func resolvedAccounts() -> [ResolvedAccount] {
    let fileManager = FileManager.default
    var accounts: [ResolvedAccount] = []
    for (xwechatRoot, containerBundleIdentifier, aliases) in xwechatFileRoots(
      fileManager: fileManager)
    {
      let children =
        (try? fileManager.contentsOfDirectory(
          at: xwechatRoot,
          includingPropertiesForKeys: [.isDirectoryKey],
          options: [.skipsHiddenFiles]
        )) ?? []
      // Account paths as listed through each alias, keyed by account name.
      var aliasChildren: [String: [URL]] = [:]
      for alias in aliases {
        let listed =
          (try? fileManager.contentsOfDirectory(
            at: alias, includingPropertiesForKeys: nil, options: [.skipsHiddenFiles])) ?? []
        for child in listed {
          aliasChildren[child.lastPathComponent, default: []].append(child)
        }
      }
      for root in children {
        guard
          let values = try? root.resourceValues(forKeys: [.isDirectoryKey, .isSymbolicLinkKey]),
          values.isDirectory == true,
          values.isSymbolicLink != true
        else { continue }
        let databaseRoot = root.appending(path: "db_storage", directoryHint: .isDirectory)
        guard safeDirectory(databaseRoot) else { continue }
        let attachmentCandidate = root.appending(path: "msg/attach", directoryHint: .isDirectory)
        let attachmentRoot =
          safeDirectory(attachmentCandidate)
          ? attachmentCandidate.standardizedFileURL : nil
        let rootReference = privacy.reference(for: root)
        accounts.append(
          ResolvedAccount(
            report: WeChatAccountLocation(
              accountID: rootReference.opaqueID,
              root: rootReference,
              databaseRoot: privacy.reference(for: databaseRoot),
              attachmentRoot: attachmentRoot.map { privacy.reference(for: $0) },
              isReadable: fileManager.isReadableFile(atPath: databaseRoot.path),
              containerBundleIdentifier: containerBundleIdentifier
            ),
            root: root.standardizedFileURL,
            databaseRoot: databaseRoot.standardizedFileURL,
            attachmentRoot: attachmentRoot,
            containerBundleIdentifier: containerBundleIdentifier,
            aliasAccountIDs: (aliasChildren[root.lastPathComponent] ?? [])
              .map { privacy.reference(for: $0).opaqueID }
              .filter { $0 != rootReference.opaqueID }
          ))
      }
    }
    var seen = Set<String>()
    return
      accounts
      .filter { seen.insert($0.root.path).inserted }
      .sorted { $0.report.accountID < $1.report.accountID }
  }

  /// `xwechat_files` roots with the bundle ID of the app container that owns
  /// each one (nil for a group container) and the other paths that reach the
  /// same directory. App containers come from `WeChatAppContainerScanner`,
  /// which covers duplicated app copies.
  private func xwechatFileRoots(fileManager: FileManager) -> [(URL, String?, [URL])] {
    var candidates: [(URL, String?)] = WeChatAppContainerScanner(homeDirectory: homeDirectory)
      .containers(fileManager: fileManager)
      .map { ($0.xwechatFilesRoot, $0.bundleIdentifier) }

    let groups = homeDirectory.appending(
      path: "Library/Group Containers", directoryHint: .isDirectory)
    let groupChildren =
      (try? fileManager.contentsOfDirectory(
        at: groups,
        includingPropertiesForKeys: [.isDirectoryKey],
        options: [.skipsHiddenFiles]
      )) ?? []
    for group in groupChildren {
      let name = group.lastPathComponent.lowercased()
      guard name.contains("wechat") || name.contains("xinwechat") else { continue }
      let root = group.appending(path: "xwechat_files", directoryHint: .isDirectory)
      guard fileManager.fileExists(atPath: root.path) else { continue }
      candidates.append((root, nil))
    }

    // App containers come first, so when a group container's xwechat_files
    // leads to the same directory (for example through a symbolic link), the
    // app candidate and its owner metadata are kept and the group path is
    // recorded as an alias.
    var seenPaths = Set<String>()
    var indexByTarget: [WeChatAppContainerScanner.FileIdentity: Int] = [:]
    var roots: [(URL, String?, [URL])] = []
    for (url, owner) in candidates {
      guard seenPaths.insert(url.standardizedFileURL.path).inserted else { continue }
      if let identity = WeChatAppContainerScanner.targetIdentity(url) {
        if let index = indexByTarget[identity] {
          roots[index].2.append(url)
          continue
        }
        indexByTarget[identity] = roots.count
      }
      roots.append((url, owner, []))
    }
    return roots
  }

  private func safeDirectory(_ url: URL) -> Bool {
    guard
      let values = try? url.resourceValues(forKeys: [.isDirectoryKey, .isSymbolicLinkKey])
    else { return false }
    return values.isDirectory == true && values.isSymbolicLink != true
  }
}
