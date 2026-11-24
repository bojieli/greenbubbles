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
/// The scan lists `~/Library/Containers` once, then walks the fixed path below
/// each entry one component at a time with `openat(O_DIRECTORY|O_NOFOLLOW)`
/// relative to the directory opened before it, so a component swapped for a
/// symbolic link between checks cannot redirect the walk. Only the verified
/// `xwechat_files` directory is listed, through its own descriptor, and each
/// account in it is checked the same way. It never lists or reads anything
/// else inside another app's container (which can raise macOS privacy
/// prompts). Any error is treated as "not found".
struct WeChatAppContainerScanner: Sendable {
  static let standardBundleIdentifier = "com.tencent.xinWeChat"
  static let dataSubpath = ["Data", "Documents", "xwechat_files"]

  /// An account directory in a verified `xwechat_files`: a real directory
  /// whose `db_storage` is a real directory.
  struct Account: Equatable, Sendable {
    let root: URL
    let databaseRoot: URL
    let attachmentRoot: URL?
    let isReadable: Bool
  }

  struct Container: Equatable, Sendable {
    /// The container directory name, which macOS sets to the app's bundle ID.
    let bundleIdentifier: String
    let containerRoot: URL
    let xwechatFilesRoot: URL
    let identity: FileIdentity
    let accounts: [Account]
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
  /// when listing fails, including when `~/Library/Containers` is
  /// search-only and is opened with `O_SEARCH` instead). The standard name is
  /// probed even when its entry is hidden, as releases before 0.11.0 always
  /// checked that fixed path; hidden entries only drop out of the listing. Candidates that are the same `xwechat_files`
  /// directory (device and inode) are reported once, which also covers
  /// case-insensitive volumes.
  ///
  /// When a differently cased standard entry is the same directory as the
  /// hard-coded standard path, the hard-coded path is kept for the container
  /// URLs, as earlier versions used; only `bundleIdentifier` takes the
  /// on-disk spelling.
  func containers(fileManager: FileManager = .default) -> [Container] {
    // When Containers can't be listed (search-only), a search-only
    // descriptor still lets the standard name below be probed directly.
    var containersDirectory = open(containersRoot.path, O_RDONLY | O_DIRECTORY | O_CLOEXEC)
    if containersDirectory < 0 {
      containersDirectory = open(containersRoot.path, O_SEARCH | O_DIRECTORY | O_CLOEXEC)
    }
    guard containersDirectory >= 0 else { return [] }
    defer { close(containersDirectory) }
    let names = Self.entryNames(ofDirectory: containersDirectory)
    let isStandard = { (name: String) in
      name.caseInsensitiveCompare(Self.standardBundleIdentifier) == .orderedSame
    }
    // The exact standard name is probed after any listed spellings of it, so
    // a listed spelling that is the same directory wins and the exact name
    // only adds a container the listing left out (hidden, or not listable).
    let standardNames = names.filter(isStandard).sorted()
    let ordered =
      standardNames
      + (standardNames.contains(Self.standardBundleIdentifier)
        ? [] : [Self.standardBundleIdentifier])
      + names.filter { !isStandard($0) }.sorted()
    let canonicalIdentity = verifiedContainer(
      named: Self.standardBundleIdentifier, in: containersDirectory, listAccounts: false)?
      .identity

    var seen = Set<FileIdentity>()
    return ordered.compactMap { name in
      // The exact standard name adds nothing when a listed spelling already
      // reached that directory; skip it before its accounts are listed.
      if name == Self.standardBundleIdentifier, let canonicalIdentity,
        seen.contains(canonicalIdentity)
      {
        return nil
      }
      guard
        let container = verifiedContainer(
          named: name, in: containersDirectory, listAccounts: true),
        seen.insert(container.identity).inserted
      else { return nil }
      guard container.identity == canonicalIdentity, name != Self.standardBundleIdentifier
      else { return container }
      let canonical = makeURLs(named: Self.standardBundleIdentifier)
      return Container(
        bundleIdentifier: container.bundleIdentifier,
        containerRoot: canonical.containerRoot,
        xwechatFilesRoot: canonical.xwechatFilesRoot,
        identity: container.identity,
        accounts: container.accounts
      )
    }
  }

  struct FileIdentity: Hashable, Sendable {
    let device: dev_t
    let inode: ino_t
  }

  /// Identity of the directory a path resolves to (`stat`, following links).
  static func targetIdentity(_ url: URL) -> FileIdentity? {
    var metadata = stat()
    guard fstatat(AT_FDCWD, url.path, &metadata, 0) == 0 else { return nil }
    return FileIdentity(device: metadata.st_dev, inode: metadata.st_ino)
  }

  /// Identity of the directory entry itself (`lstat`); nil for a symbolic
  /// link, anything that isn't a directory, or any error.
  static func entryIdentity(_ url: URL) -> FileIdentity? {
    var metadata = stat()
    guard Darwin.lstat(url.path, &metadata) == 0, metadata.st_mode & S_IFMT == S_IFDIR else {
      return nil
    }
    return FileIdentity(device: metadata.st_dev, inode: metadata.st_ino)
  }

  /// `lstat` only: a symbolic link, a missing entry, or any error is rejected.
  static func isRealDirectory(_ url: URL) -> Bool {
    var metadata = stat()
    guard Darwin.lstat(url.path, &metadata) == 0 else { return false }
    return metadata.st_mode & S_IFMT == S_IFDIR
  }

  /// Opens `name` below `directory` as a directory, refusing symbolic links.
  static func openDirectory(_ name: String, in directory: Int32, forListing: Bool = false)
    -> Int32
  {
    guard !name.isEmpty, !name.contains("/"), name != ".", name != ".." else { return -1 }
    let access = forListing ? O_RDONLY : O_SEARCH
    return openat(directory, name, access | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
  }

  /// True when `name` below `directory` is a real directory (not a link).
  static func isRealDirectory(_ name: String, in directory: Int32) -> Bool {
    var metadata = stat()
    guard fstatat(directory, name, &metadata, AT_SYMLINK_NOFOLLOW) == 0 else { return false }
    return metadata.st_mode & S_IFMT == S_IFDIR
  }

  /// Visible entry names of an open directory, read through a duplicate of
  /// its descriptor. Like Foundation's `.skipsHiddenFiles`, names starting
  /// with `.` and entries flagged `UF_HIDDEN` are left out.
  static func entryNames(ofDirectory directory: Int32) -> [String] {
    let duplicate = dup(directory)
    guard duplicate >= 0 else { return [] }
    guard let stream = fdopendir(duplicate) else {
      close(duplicate)
      return []
    }
    defer { closedir(stream) }
    rewinddir(stream)
    var names: [String] = []
    while let entry = readdir(stream) {
      let name = withUnsafeBytes(of: entry.pointee.d_name) { bytes in
        String(decoding: bytes.prefix(Int(entry.pointee.d_namlen)), as: UTF8.self)
      }
      guard !name.hasPrefix(".") else { continue }
      var metadata = stat()
      if fstatat(directory, name, &metadata, AT_SYMLINK_NOFOLLOW) == 0,
        metadata.st_flags & UInt32(UF_HIDDEN) != 0
      {
        continue
      }
      names.append(name)
    }
    return names
  }

  /// The path the file system reports for an open directory, in the form
  /// Foundation's directory listing reports (links resolved, on-disk case,
  /// `/private` dropped where the given path did not have it).
  static func reportedPath(ofDirectory directory: Int32, given: URL) -> URL {
    var buffer = [CChar](repeating: 0, count: Int(MAXPATHLEN))
    guard fcntl(directory, F_GETPATH, &buffer) != -1 else { return given.standardizedFileURL }
    var path = String(
      decoding: buffer.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }, as: UTF8.self)
    let givenPath = given.standardizedFileURL.path
    if path.hasPrefix("/private/"), !givenPath.hasPrefix("/private/") {
      path.removeFirst("/private".count)
    }
    return URL(fileURLWithPath: path, isDirectory: true).standardizedFileURL
  }

  private func verifiedContainer(
    named name: String, in containersDirectory: Int32, listAccounts: Bool
  )
    -> Container?
  {
    var descriptors: [Int32] = []
    defer { for descriptor in descriptors { close(descriptor) } }
    var current = containersDirectory
    let components = [name] + Self.dataSubpath
    for (index, component) in components.enumerated() {
      let next = Self.openDirectory(
        component, in: current, forListing: index == components.count - 1)
      guard next >= 0 else { return nil }
      descriptors.append(next)
      current = next
    }
    var metadata = stat()
    guard fstat(current, &metadata) == 0, metadata.st_mode & S_IFMT == S_IFDIR else {
      return nil
    }
    let urls = makeURLs(named: name)
    return Container(
      bundleIdentifier: name,
      containerRoot: urls.containerRoot,
      xwechatFilesRoot: urls.xwechatFilesRoot,
      identity: FileIdentity(device: metadata.st_dev, inode: metadata.st_ino),
      accounts: listAccounts
        ? Self.accounts(inFiles: current, at: urls.xwechatFilesRoot) : []
    )
  }

  /// Accounts in an open, verified `xwechat_files` directory.
  static func accounts(inFiles files: Int32, at url: URL) -> [Account] {
    let base = reportedPath(ofDirectory: files, given: url)
    return entryNames(ofDirectory: files).sorted().compactMap { name in
      let account = openDirectory(name, in: files)
      guard account >= 0 else { return nil }
      defer { close(account) }
      guard isRealDirectory("db_storage", in: account) else { return nil }
      var hasAttachments = false
      let messages = openDirectory("msg", in: account)
      if messages >= 0 {
        hasAttachments = isRealDirectory("attach", in: messages)
        close(messages)
      }
      let root = base.appending(path: name, directoryHint: .isDirectory).standardizedFileURL
      return Account(
        root: root,
        databaseRoot: root.appending(path: "db_storage", directoryHint: .isDirectory)
          .standardizedFileURL,
        attachmentRoot: hasAttachments
          ? root.appending(path: "msg/attach", directoryHint: .isDirectory).standardizedFileURL
          : nil,
        isReadable: faccessat(account, "db_storage", R_OK, 0) == 0
      )
    }
  }

  private func makeURLs(named name: String) -> (containerRoot: URL, xwechatFilesRoot: URL) {
    let root = containersRoot.appending(path: name, directoryHint: .isDirectory)
    var files = root
    for component in Self.dataSubpath {
      files = files.appending(path: component, directoryHint: .isDirectory)
    }
    return (root.standardizedFileURL, files.standardizedFileURL)
  }
}
