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

  /// Bundle IDs of the app containers that own the selected accounts (nil
  /// for an account in a group container).
  public func containerBundleIdentifiers(accountID: String? = nil) -> [String?] {
    resolvedAccounts()
      .filter { accountID == nil || $0.matches(accountID: accountID!) }
      .map(\.containerBundleIdentifier)
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
    for source in xwechatFileRoots(fileManager: fileManager) {
      // Account paths as listed through each alias, keyed by account name.
      var aliasChildren: [String: [URL]] = [:]
      for alias in source.aliases {
        let listed =
          (try? fileManager.contentsOfDirectory(
            at: alias, includingPropertiesForKeys: nil, options: [.skipsHiddenFiles])) ?? []
        for child in listed {
          aliasChildren[child.lastPathComponent, default: []].append(child)
        }
      }
      let found =
        source.verifiedAccounts ?? pathCheckedAccounts(in: source.url, fileManager: fileManager)
      for account in found {
        let rootReference = privacy.reference(for: account.root)
        accounts.append(
          ResolvedAccount(
            report: WeChatAccountLocation(
              accountID: rootReference.opaqueID,
              root: rootReference,
              databaseRoot: privacy.reference(for: account.databaseRoot),
              attachmentRoot: account.attachmentRoot.map { privacy.reference(for: $0) },
              isReadable: account.isReadable,
              containerBundleIdentifier: source.owner
            ),
            root: account.root.standardizedFileURL,
            databaseRoot: account.databaseRoot.standardizedFileURL,
            attachmentRoot: account.attachmentRoot,
            containerBundleIdentifier: source.owner,
            aliasAccountIDs: (aliasChildren[account.root.lastPathComponent] ?? [])
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

  /// Accounts in a group container's xwechat_files, checked by path as before.
  private func pathCheckedAccounts(in xwechatRoot: URL, fileManager: FileManager)
    -> [WeChatAppContainerScanner.Account]
  {
    let children =
      (try? fileManager.contentsOfDirectory(
        at: xwechatRoot,
        includingPropertiesForKeys: [.isDirectoryKey],
        options: [.skipsHiddenFiles]
      )) ?? []
    return children.compactMap { root in
      guard
        let values = try? root.resourceValues(forKeys: [.isDirectoryKey, .isSymbolicLinkKey]),
        values.isDirectory == true,
        values.isSymbolicLink != true
      else { return nil }
      let databaseRoot = root.appending(path: "db_storage", directoryHint: .isDirectory)
      guard safeDirectory(databaseRoot) else { return nil }
      let attachmentCandidate = root.appending(path: "msg/attach", directoryHint: .isDirectory)
      return WeChatAppContainerScanner.Account(
        root: root,
        databaseRoot: databaseRoot,
        attachmentRoot: safeDirectory(attachmentCandidate)
          ? attachmentCandidate.standardizedFileURL : nil,
        isReadable: fileManager.isReadableFile(atPath: databaseRoot.path)
      )
    }
  }

  private struct RootSource {
    let url: URL
    /// Bundle ID of the owning app container; nil for a group container.
    let owner: String?
    /// Accounts verified through directory descriptors (app containers).
    let verifiedAccounts: [WeChatAppContainerScanner.Account]?
    var aliases: [URL] = []
  }

  /// `xwechat_files` roots with the bundle ID of the app container that owns
  /// each one (nil for a group container) and the other paths that reach the
  /// same directory. App containers come from `WeChatAppContainerScanner`,
  /// which covers duplicated app copies and verifies their accounts.
  private func xwechatFileRoots(fileManager: FileManager) -> [RootSource] {
    var candidates: [(RootSource, WeChatAppContainerScanner.FileIdentity?)] =
      WeChatAppContainerScanner(homeDirectory: homeDirectory)
      .containers(fileManager: fileManager)
      .map {
        (
          RootSource(
            url: $0.xwechatFilesRoot, owner: $0.bundleIdentifier, verifiedAccounts: $0.accounts),
          $0.identity
        )
      }

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
      candidates.append(
        (
          RootSource(url: root, owner: nil, verifiedAccounts: nil),
          WeChatAppContainerScanner.targetIdentity(root)
        ))
    }

    // App containers come first, so when a group container's xwechat_files
    // leads to the same directory (for example through a symbolic link), the
    // app candidate and its owner metadata are kept and the group path is
    // recorded as an alias.
    var seenPaths = Set<String>()
    var indexByTarget: [WeChatAppContainerScanner.FileIdentity: Int] = [:]
    var roots: [RootSource] = []
    for (source, identity) in candidates {
      guard seenPaths.insert(source.url.standardizedFileURL.path).inserted else { continue }
      if let identity {
        if let index = indexByTarget[identity] {
          roots[index].aliases.append(source.url)
          continue
        }
        indexByTarget[identity] = roots.count
      }
      roots.append(source)
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
