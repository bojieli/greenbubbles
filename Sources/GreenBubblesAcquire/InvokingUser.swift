import Darwin
import Foundation

public enum InvokingUserError: Error, Equatable, CustomStringConvertible {
  case inconsistentSudoEnvironment(String)
  case privilegeChangeFailed(operation: String, code: Int32)

  public var description: String {
    switch self {
    case .inconsistentSudoEnvironment(let reason):
      return "Cannot tell which user ran sudo: \(reason)"
    case .privilegeChangeFailed(let operation, let code):
      return "Could not \(operation) (POSIX error \(code)); nothing was read or written"
    }
  }
}

/// The account a capture works for: the user who ran `sudo`, or the current
/// user when the tool was not started through `sudo`.
///
/// Every file the user controls — the query-profile file, `config.toml`, and
/// the key output — is opened through `perform(_:)`, which switches the
/// effective user and groups to this account while running as root. The
/// kernel then applies the user's own permissions, so a path from a
/// user-writable file can never make root create, replace, or hand over a
/// file the user could not already write.
public struct InvokingUser: Equatable, Sendable {
  public let name: String
  public let uid: uid_t
  public let gid: gid_t
  public let homeDirectory: URL

  public init(name: String, uid: uid_t, gid: gid_t, homeDirectory: URL) {
    self.name = name
    self.uid = uid
    self.gid = gid
    self.homeDirectory = homeDirectory
  }

  /// A password-database entry, injectable for tests.
  public struct Account: Equatable, Sendable {
    public let name: String
    public let uid: uid_t
    public let gid: gid_t
    public let homeDirectory: String

    public init(name: String, uid: uid_t, gid: gid_t, homeDirectory: String) {
      self.name = name
      self.uid = uid
      self.gid = gid
      self.homeDirectory = homeDirectory
    }
  }

  /// The user this process works for, from the real process state.
  public static func current() throws -> InvokingUser {
    try resolve(
      effectiveUID: geteuid(),
      effectiveGID: getegid(),
      environment: ProcessInfo.processInfo.environment,
      accountNamed: Self.account(named:),
      currentHome: FileManager.default.homeDirectoryForCurrentUser
    )
  }

  /// The identity policy, separated from process state so it can be tested.
  ///
  /// As root, `SUDO_USER` and `SUDO_UID` must either both be absent (root
  /// itself ran the tool) or both be present and agree with the password
  /// database. Anything else fails closed instead of guessing an owner.
  public static func resolve(
    effectiveUID: uid_t,
    effectiveGID: gid_t,
    environment: [String: String],
    accountNamed: (String) -> Account?,
    currentHome: URL
  ) throws -> InvokingUser {
    let root = InvokingUser(
      name: "root", uid: effectiveUID, gid: effectiveGID, homeDirectory: currentHome)
    guard effectiveUID == 0 else {
      return InvokingUser(
        name: NSUserName(), uid: effectiveUID, gid: effectiveGID, homeDirectory: currentHome)
    }
    let sudoUser = environment["SUDO_USER"]
    let sudoUID = environment["SUDO_UID"]
    switch (sudoUser, sudoUID) {
    case (nil, nil):
      return root
    case (let name?, let uidText?):
      guard !name.isEmpty, let uid = uid_t(uidText), String(uid) == uidText else {
        throw InvokingUserError.inconsistentSudoEnvironment("SUDO_UID is not a user ID")
      }
      guard let account = accountNamed(name) else {
        throw InvokingUserError.inconsistentSudoEnvironment("SUDO_USER is not a known account")
      }
      guard account.uid == uid else {
        throw InvokingUserError.inconsistentSudoEnvironment(
          "SUDO_USER and SUDO_UID name different accounts")
      }
      guard account.homeDirectory.hasPrefix("/") else {
        throw InvokingUserError.inconsistentSudoEnvironment(
          "the account's home directory is not an absolute path")
      }
      if uid == 0 { return root }
      return InvokingUser(
        name: account.name,
        uid: account.uid,
        gid: account.gid,
        homeDirectory: URL(fileURLWithPath: account.homeDirectory, isDirectory: true)
          .standardizedFileURL
      )
    default:
      throw InvokingUserError.inconsistentSudoEnvironment(
        "only one of SUDO_USER and SUDO_UID is set")
    }
  }

  private static func account(named name: String) -> Account? {
    guard let entry = getpwnam(name), let directory = entry.pointee.pw_dir else { return nil }
    return Account(
      name: String(cString: entry.pointee.pw_name),
      uid: entry.pointee.pw_uid,
      gid: entry.pointee.pw_gid,
      homeDirectory: String(cString: directory)
    )
  }

  /// Whether `perform(_:)` changes credentials: only when running as root
  /// for a different account. Otherwise it simply runs the body.
  public var switchesCredentials: Bool { geteuid() == 0 && uid != 0 }

  /// Runs `body` with this user's effective user, group, and supplementary
  /// groups, then restores root.
  ///
  /// The credentials are process-wide, so callers must not run other work
  /// concurrently. If the switch fails, the body never runs and the original
  /// credentials are restored. If restoring fails afterwards, the process
  /// terminates instead of continuing with mixed credentials.
  public func perform<T>(_ body: () throws -> T) throws -> T {
    guard switchesCredentials else { return try body() }
    let saved = try SavedCredentials.capture()
    if initgroups(name, Int32(bitPattern: gid)) != 0 {
      let code = errno
      saved.restoreOrTerminate()
      throw InvokingUserError.privilegeChangeFailed(
        operation: "switch to the invoking user's groups", code: code)
    }
    if setegid(gid) != 0 {
      let code = errno
      saved.restoreOrTerminate()
      throw InvokingUserError.privilegeChangeFailed(
        operation: "switch to the invoking user's group", code: code)
    }
    if seteuid(uid) != 0 {
      let code = errno
      saved.restoreOrTerminate()
      throw InvokingUserError.privilegeChangeFailed(
        operation: "switch to the invoking user", code: code)
    }
    guard geteuid() == uid, getegid() == gid else {
      saved.restoreOrTerminate()
      throw InvokingUserError.privilegeChangeFailed(
        operation: "confirm the switch to the invoking user", code: EPERM)
    }
    defer { saved.restoreOrTerminate() }
    return try body()
  }

  private struct SavedCredentials {
    let egid: gid_t
    let groups: [gid_t]

    // Fails before anything is switched when the current group list cannot
    // be saved exactly, or could not be set back with setgroups.
    static func capture() throws -> SavedCredentials {
      let count = getgroups(0, nil)
      guard count >= 0 else {
        throw InvokingUserError.privilegeChangeFailed(
          operation: "save the current group list", code: errno)
      }
      guard count <= NGROUPS_MAX else {
        throw InvokingUserError.privilegeChangeFailed(
          operation: "save the current group list", code: EINVAL)
      }
      var groups = [gid_t](repeating: 0, count: Int(count))
      if count > 0 {
        let filled = getgroups(count, &groups)
        guard filled == count else {
          throw InvokingUserError.privilegeChangeFailed(
            operation: "save the current group list", code: filled < 0 ? errno : EAGAIN)
        }
      }
      return SavedCredentials(egid: getegid(), groups: groups)
    }

    // Root first: changing the group or the group list needs it. Restores
    // exactly what was saved, or terminates.
    func restoreOrTerminate() {
      guard seteuid(0) == 0 else { terminate("restore the root user") }
      guard setegid(egid) == 0 else { terminate("restore the root group") }
      guard setgroups(Int32(groups.count), groups) == 0 else {
        terminate("restore the root group list")
      }
    }

    private func terminate(_ operation: String) -> Never {
      let message = "fatal: could not \(operation) after acting as the invoking user\n"
      FileHandle.standardError.write(Data(message.utf8))
      Darwin._exit(70)
    }
  }
}
