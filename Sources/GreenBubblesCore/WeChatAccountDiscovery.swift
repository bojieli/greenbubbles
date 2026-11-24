import Darwin
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

  /// A selected database root and the bundle ID of the app container that
  /// owns it (nil for an account in a group container).
  public struct OwnedDatabaseRoot: Equatable, Sendable {
    public let databaseRoot: URL
    public let containerBundleIdentifier: String?
  }

  /// The selected database roots together with their owners, from a single
  /// discovery pass, so the owners always describe the roots returned.
  public func ownedDatabaseRoots(accountID: String? = nil) -> [OwnedDatabaseRoot] {
    resolvedAccounts()
      .filter { accountID == nil || $0.matches(accountID: accountID!) }
      .map {
        OwnedDatabaseRoot(
          databaseRoot: $0.databaseRoot, containerBundleIdentifier: $0.containerBundleIdentifier)
      }
  }

  /// True when every owner is the app with `bundleIdentifier`. Bundle IDs are
  /// compared case-insensitively, because discovery reports the container's
  /// on-disk spelling, which can differ in case from the app's Info.plist on a
  /// case-insensitive volume. An unknown owner (nil, such as a group
  /// container) can't be verified and never matches.
  public static func ownersAllMatch(_ owners: [String?], bundleIdentifier: String) -> Bool {
    owners.allSatisfy { owner in
      guard let owner else { return false }
      return owner.caseInsensitiveCompare(bundleIdentifier) == .orderedSame
    }
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
      for account in source.accounts {
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

  private struct RootSource {
    let url: URL
    /// Bundle ID of the owning app container; nil for a group container.
    let owner: String?
    /// Accounts verified through directory descriptors.
    let accounts: [WeChatAppContainerScanner.Account]
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
            url: $0.xwechatFilesRoot, owner: $0.bundleIdentifier, accounts: $0.accounts),
          $0.identity
        )
      }

    candidates += groupContainerRoots().map {
      (RootSource(url: $0.url, owner: nil, accounts: $0.accounts), $0.identity)
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

  /// `xwechat_files` directories in WeChat group containers, walked by
  /// descriptor like the app-container scan: `Group Containers` is listed
  /// once, each matching group and its `xwechat_files` are opened relative to
  /// the directory opened before them, and the identity and the account
  /// listing both come from that one `xwechat_files` descriptor, so a path
  /// swapped between checks cannot make them describe different directories.
  /// Accounts are checked with `WeChatAppContainerScanner.accounts`, which
  /// rejects linked accounts and linked `db_storage`.
  ///
  /// Unlike app containers, a group or its `xwechat_files` may be a symbolic
  /// link, as earlier versions allowed: a group linked to an app container's
  /// data is reported as an alias of that account, so saved IDs keep working.
  private func groupContainerRoots() -> [(
    url: URL, identity: WeChatAppContainerScanner.FileIdentity,
    accounts: [WeChatAppContainerScanner.Account]
  )] {
    let groupsURL = homeDirectory.appending(
      path: "Library/Group Containers", directoryHint: .isDirectory)
    let groups = open(groupsURL.path, O_RDONLY | O_DIRECTORY | O_CLOEXEC)
    guard groups >= 0 else { return [] }
    defer { close(groups) }
    return WeChatAppContainerScanner.entryNames(ofDirectory: groups).sorted().compactMap { name in
      guard name.lowercased().contains("wechat") else { return nil }
      let group = openat(groups, name, O_SEARCH | O_DIRECTORY | O_CLOEXEC)
      guard group >= 0 else { return nil }
      defer { close(group) }
      let files = openat(group, "xwechat_files", O_RDONLY | O_DIRECTORY | O_CLOEXEC)
      guard files >= 0 else { return nil }
      defer { close(files) }
      var metadata = stat()
      guard fstat(files, &metadata) == 0, metadata.st_mode & S_IFMT == S_IFDIR else { return nil }
      let url = groupsURL.appending(path: name, directoryHint: .isDirectory)
        .appending(path: "xwechat_files", directoryHint: .isDirectory).standardizedFileURL
      return (
        url,
        WeChatAppContainerScanner.FileIdentity(device: metadata.st_dev, inode: metadata.st_ino),
        WeChatAppContainerScanner.accounts(inFiles: files, at: url)
      )
    }
  }
}
