import Darwin
import Foundation
import Testing

@testable import GreenBubblesAcquire

struct InvokingUserTests {
  private let alice = InvokingUser.Account(
    name: "alice", uid: 501, gid: 20, homeDirectory: "/Users/alice")
  private let rootHome = URL(fileURLWithPath: "/var/root")

  private func resolve(_ environment: [String: String], euid: uid_t = 0) throws -> InvokingUser {
    try InvokingUser.resolve(
      effectiveUID: euid,
      effectiveGID: 0,
      environment: environment,
      accountNamed: { $0 == "alice" ? alice : nil },
      currentHome: rootHome
    )
  }

  @Test func sudoUserMatchingSudoUIDIsTheInvokingUser() throws {
    let user = try resolve(["SUDO_USER": "alice", "SUDO_UID": "501", "SUDO_GID": "20"])
    #expect(user.name == "alice" && user.uid == 501 && user.gid == 20)
    #expect(user.homeDirectory.path == "/Users/alice")
  }

  @Test(arguments: [
    ["SUDO_USER": "alice", "SUDO_UID": "502"],  // names a different account
    ["SUDO_USER": "alice"],  // SUDO_UID missing
    ["SUDO_UID": "501"],  // SUDO_USER missing
    ["SUDO_USER": "mallory", "SUDO_UID": "501"],  // unknown account
    ["SUDO_USER": "alice", "SUDO_UID": "501x"],
    ["SUDO_USER": "alice", "SUDO_UID": "+501"],
    ["SUDO_USER": "", "SUDO_UID": "501"],
  ])
  func inconsistentSudoEnvironmentFailsClosed(environment: [String: String]) {
    #expect(throws: InvokingUserError.self) { try resolve(environment) }
  }

  @Test func rootWithoutSudoActsForItself() throws {
    let user = try resolve([:])
    #expect(user.uid == 0)
    #expect(user.homeDirectory == rootHome)
  }

  @Test func nonRootIgnoresSudoVariables() throws {
    let user = try resolve(["SUDO_USER": "alice", "SUDO_UID": "999"], euid: 501)
    #expect(user.uid == 501)
    #expect(user.homeDirectory == rootHome)
  }

  @Test func performIsANoOpWhenNotRoot() throws {
    guard geteuid() != 0 else { return }
    let other = InvokingUser(
      name: "nobody", uid: getuid() &+ 1, gid: getgid(), homeDirectory: rootHome)
    #expect(!other.switchesCredentials)
    let seen = try other.perform { (geteuid(), getegid()) }
    #expect(seen.0 == geteuid() && seen.1 == getegid())
    #expect(throws: CancellationError.self) { try other.perform { throw CancellationError() } }
  }
}
