import Foundation
import Testing

@testable import GreenBubblesAcquire

/// Synthetic candidates only: no real process, app, or home directory is read.
struct WeChatProcessSelectionTests {
  static let user: uid_t = 501
  static let otherUser: uid_t = 502
  static let home = URL(fileURLWithPath: "/Users/synthetic-owner")
  static let standard = "com.tencent.xinWeChat"
  static let duplicate = "com.example.WeChatCopy"
  static let standardApp = "/Applications/WeChat.app"
  static let duplicateApp = "/Applications/WeChat Copy.app"

  static func candidate(
    _ processID: pid_t,
    app: String,
    bundle: String?,
    user: uid_t = WeChatProcessSelectionTests.user
  ) -> WeChatProcessCandidate {
    WeChatProcessCandidate(
      processID: processID,
      userID: user,
      executablePath: app + "/Contents/MacOS/WeChat",
      applicationPath: app,
      bundleIdentifier: bundle
    )
  }

  static func root(in bundle: String) -> URL {
    home.appending(
      path: "Library/Containers/\(bundle)/Data/Documents/xwechat_files/account/db_storage")
  }

  static let standardProcess = candidate(42, app: standardApp, bundle: standard)
  static let duplicateProcess = candidate(43, app: duplicateApp, bundle: duplicate)

  // MARK: - Database root ownership

  @Test func mapsAppContainerRootsToTheirBundleIdentifier() {
    #expect(
      DatabaseRootOwner.resolve(
        databaseRoot: Self.root(in: Self.standard), homeDirectory: Self.home)
        == .application(bundleIdentifier: Self.standard))
    #expect(
      DatabaseRootOwner.resolve(
        databaseRoot: Self.root(in: Self.duplicate), homeDirectory: Self.home)
        == .application(bundleIdentifier: Self.duplicate))
    let dotted = Self.home.appending(
      path: "Library/Containers/./\(Self.duplicate)/Data/x/../Documents/db_storage")
    #expect(
      DatabaseRootOwner.resolve(databaseRoot: dotted, homeDirectory: Self.home)
        == .application(bundleIdentifier: Self.duplicate))
  }

  @Test func rootsOutsideAnAppContainerHaveNoKnownOwner() {
    let unknownRoots = [
      "Library/Group Containers/ABC.com.tencent.xinWeChat/xwechat_files/a/db_storage",
      "Library/Containers/\(Self.standard)",
      "Library/Containers/\(Self.standard)/Other/db_storage",
      "backups/xwechat_files/a/db_storage",
    ].map { Self.home.appending(path: $0) }
    for root in unknownRoots {
      #expect(DatabaseRootOwner.resolve(databaseRoot: root, homeDirectory: Self.home) == .unknown)
    }
    let otherHome = URL(fileURLWithPath: "/Users/someone-else")
      .appending(path: "Library/Containers/\(Self.standard)/Data/Documents/db_storage")
    #expect(
      DatabaseRootOwner.resolve(databaseRoot: otherHome, homeDirectory: Self.home) == .unknown)
  }

  // MARK: - Automatic selection

  @Test func preservesZeroAndSingleStandardProcessBehavior() throws {
    let owner = DatabaseRootOwner.application(bundleIdentifier: Self.standard)
    #expect(
      try WeChatProcessSelector.select(candidates: [], userID: Self.user, owner: owner) == nil)
    let target = try WeChatProcessSelector.select(
      candidates: [Self.standardProcess], userID: Self.user, owner: owner)
    #expect(target?.processID == 42)
    #expect(target?.applicationPath == Self.standardApp)
    #expect(target?.bundleIdentifier == Self.standard)
    #expect(target?.userID == Self.user)
  }

  @Test func selectsTheCopyThatOwnsTheRootInEitherProcessOrder() throws {
    for candidates in [
      [Self.standardProcess, Self.duplicateProcess],
      [Self.duplicateProcess, Self.standardProcess],
    ] {
      let duplicateTarget = try WeChatProcessSelector.select(
        candidates: candidates,
        userID: Self.user,
        owner: .application(bundleIdentifier: Self.duplicate)
      )
      #expect(duplicateTarget?.processID == 43)
      let standardTarget = try WeChatProcessSelector.select(
        candidates: candidates,
        userID: Self.user,
        owner: .application(bundleIdentifier: Self.standard)
      )
      #expect(standardTarget?.processID == 42)
    }
  }

  @Test func refusesWhenTheOwningCopyIsNotRunning() {
    #expect(
      throws: ProcessSelectionError.noProcessForDatabaseRoot(
        bundleIdentifier: Self.duplicate, otherProcessCount: 1)
    ) {
      try WeChatProcessSelector.select(
        candidates: [Self.standardProcess],
        userID: Self.user,
        owner: .application(bundleIdentifier: Self.duplicate)
      )
    }
    // A process whose bundle could not be read never matches.
    let unreadable = Self.candidate(44, app: Self.duplicateApp, bundle: nil)
    #expect(
      throws: ProcessSelectionError.noProcessForDatabaseRoot(
        bundleIdentifier: Self.duplicate, otherProcessCount: 1)
    ) {
      try WeChatProcessSelector.select(
        candidates: [unreadable],
        userID: Self.user,
        owner: .application(bundleIdentifier: Self.duplicate)
      )
    }
  }

  @Test func refusesTwoMatchingProcesses() {
    let second = Self.candidate(
      44, app: "/Users/synthetic-owner/Applications/WeChat.app", bundle: Self.standard)
    for candidates in [[Self.standardProcess, second], [second, Self.standardProcess]] {
      #expect(
        throws: ProcessSelectionError.multipleProcessesForDatabaseRoot(
          bundleIdentifier: Self.standard, count: 2)
      ) {
        try WeChatProcessSelector.select(
          candidates: candidates,
          userID: Self.user,
          owner: .application(bundleIdentifier: Self.standard)
        )
      }
    }
  }

  @Test func ignoresOtherUsersProcesses() throws {
    let foreign = Self.candidate(
      40, app: Self.standardApp, bundle: Self.standard, user: Self.otherUser)
    let owner = DatabaseRootOwner.application(bundleIdentifier: Self.standard)
    #expect(
      try WeChatProcessSelector.select(candidates: [foreign], userID: Self.user, owner: owner)
        == nil)
    let target = try WeChatProcessSelector.select(
      candidates: [foreign, Self.standardProcess], userID: Self.user, owner: owner)
    #expect(target?.processID == 42)
    let unknownUser = WeChatProcessCandidate(
      processID: 41, userID: nil, executablePath: nil, applicationPath: Self.standardApp,
      bundleIdentifier: Self.standard)
    #expect(
      try WeChatProcessSelector.select(
        candidates: [unknownUser, Self.standardProcess], userID: Self.user, owner: owner)?
        .processID == 42)
  }

  @Test func requiresAnExplicitProcessWhenTheRootOwnerIsUnknown() throws {
    #expect(throws: ProcessSelectionError.databaseRootNotInAppContainer(processCount: 1)) {
      try WeChatProcessSelector.select(
        candidates: [Self.standardProcess], userID: Self.user, owner: .unknown)
    }
    #expect(
      try WeChatProcessSelector.select(candidates: [], userID: Self.user, owner: .unknown) == nil)
  }

  // MARK: - --pid validation

  @Test func acceptsAnExplicitProcessOfTheOwningCopy() throws {
    let target = try WeChatProcessSelector.validate(
      requestedProcessID: 43,
      candidate: Self.duplicateProcess,
      userID: Self.user,
      owner: .application(bundleIdentifier: Self.duplicate)
    )
    #expect(target.processID == 43)
    #expect(target.bundleIdentifier == Self.duplicate)
    // A group-container root cannot be mapped, so the explicit choice stands.
    let unknown = try WeChatProcessSelector.validate(
      requestedProcessID: 42, candidate: Self.standardProcess, userID: Self.user, owner: .unknown)
    #expect(unknown.processID == 42)
  }

  @Test func rejectsAnExplicitProcessOfAnotherCopy() {
    #expect(
      throws: ProcessSelectionError.processDoesNotOwnDatabaseRoot(
        processID: 42, processBundleIdentifier: Self.standard,
        ownerBundleIdentifier: Self.duplicate)
    ) {
      try WeChatProcessSelector.validate(
        requestedProcessID: 42,
        candidate: Self.standardProcess,
        userID: Self.user,
        owner: .application(bundleIdentifier: Self.duplicate)
      )
    }
  }

  @Test func rejectsMissingForeignAndNonWeChatProcesses() {
    let owner = DatabaseRootOwner.application(bundleIdentifier: Self.standard)
    #expect(throws: ProcessSelectionError.processNotFound(42)) {
      try WeChatProcessSelector.validate(
        requestedProcessID: 42, candidate: nil, userID: Self.user, owner: owner)
    }
    #expect(throws: ProcessSelectionError.processNotFound(42)) {
      try WeChatProcessSelector.validate(
        requestedProcessID: 42, candidate: Self.duplicateProcess, userID: Self.user, owner: owner)
    }
    let foreign = Self.candidate(
      42, app: Self.standardApp, bundle: Self.standard, user: Self.otherUser)
    #expect(throws: ProcessSelectionError.processOwnedByAnotherUser(42)) {
      try WeChatProcessSelector.validate(
        requestedProcessID: 42, candidate: foreign, userID: Self.user, owner: owner)
    }
    let notWeChat = WeChatProcessCandidate(
      processID: 42, userID: Self.user,
      executablePath: "/Applications/Other.app/Contents/MacOS/Other",
      applicationPath: "/Applications/Other.app", bundleIdentifier: "com.example.Other")
    let helper = WeChatProcessCandidate(
      processID: 42, userID: Self.user,
      executablePath: "/usr/local/bin/WeChat", applicationPath: nil,
      bundleIdentifier: nil)
    for candidate in [notWeChat, helper] {
      #expect(throws: ProcessSelectionError.processNotWeChat(42)) {
        try WeChatProcessSelector.validate(
          requestedProcessID: 42, candidate: candidate, userID: Self.user, owner: .unknown)
      }
    }
  }

  // MARK: - Re-attach after a restart

  @Test func reattachFollowsTheSameAppAcrossPIDChanges() throws {
    let selected = try #require(
      try WeChatProcessSelector.select(
        candidates: [Self.duplicateProcess, Self.standardProcess],
        userID: Self.user,
        owner: .application(bundleIdentifier: Self.duplicate)
      ))
    let restarted = Self.candidate(90, app: Self.duplicateApp, bundle: Self.duplicate)
    // The other copy is running with a lower PID; it must never be chosen.
    for candidates in [[Self.standardProcess, restarted], [restarted, Self.standardProcess]] {
      let next = try WeChatProcessSelector.reselect(candidates: candidates, following: selected)
      #expect(next?.processID == 90)
    }
    // While only the other copy runs, the capture keeps waiting.
    #expect(
      try WeChatProcessSelector.reselect(candidates: [Self.standardProcess], following: selected)
        == nil)
  }

  @Test func reattachIgnoresSameBundleElsewhereAndOtherUsers() throws {
    let selected = WeChatProcessTarget(
      processID: 42, userID: Self.user, applicationPath: Self.standardApp,
      bundleIdentifier: Self.standard)
    let movedCopy = Self.candidate(
      50, app: "/Users/synthetic-owner/Applications/WeChat.app", bundle: Self.standard)
    let foreign = Self.candidate(
      51, app: Self.standardApp, bundle: Self.standard, user: Self.otherUser)
    #expect(
      try WeChatProcessSelector.reselect(candidates: [movedCopy, foreign], following: selected)
        == nil)
    let twice = [
      Self.candidate(60, app: Self.standardApp, bundle: Self.standard),
      Self.candidate(61, app: Self.standardApp, bundle: Self.standard),
    ]
    #expect(
      throws: ProcessSelectionError.multipleProcessesForTarget(
        bundleIdentifier: Self.standard, count: 2)
    ) {
      try WeChatProcessSelector.reselect(candidates: twice, following: selected)
    }
  }

  @Test func explicitPIDDisambiguatesTwoInstancesOfTheSameApp() throws {
    let first = Self.candidate(60, app: Self.standardApp, bundle: Self.standard)
    let second = Self.candidate(61, app: Self.standardApp, bundle: Self.standard)
    let selected = try WeChatProcessSelector.validate(
      requestedProcessID: 61,
      candidate: second,
      userID: Self.user,
      owner: .application(bundleIdentifier: Self.standard),
      runningCandidates: [first, second, Self.duplicateProcess]
    )
    #expect(selected.excludedProcessIDs == [60])
    // The first attach keeps the chosen PID in either listing order.
    for candidates in [[first, second], [second, first]] {
      #expect(
        try WeChatProcessSelector.reselect(candidates: candidates, following: selected)?
          .processID == 61)
    }
    // A reused PID now running another copy is not the selected process.
    let reused = Self.candidate(61, app: Self.duplicateApp, bundle: Self.duplicate)
    #expect(
      try WeChatProcessSelector.reselect(candidates: [reused], following: selected) == nil)
  }

  @Test func preexistingSiblingIsNeverTakenAsTheReplacement() throws {
    let sibling = Self.candidate(60, app: Self.standardApp, bundle: Self.standard)
    let chosen = Self.candidate(61, app: Self.standardApp, bundle: Self.standard)
    let selected = try WeChatProcessSelector.validate(
      requestedProcessID: 61,
      candidate: chosen,
      userID: Self.user,
      owner: .application(bundleIdentifier: Self.standard),
      runningCandidates: [sibling, chosen]
    )
    // The chosen instance exited during logout; only the original, unselected
    // sibling remains. Keep waiting instead of attaching to it.
    #expect(try WeChatProcessSelector.reselect(candidates: [sibling], following: selected) == nil)
    // A new process of the same app is the replacement, in either order.
    let replacement = Self.candidate(70, app: Self.standardApp, bundle: Self.standard)
    for candidates in [[sibling, replacement], [replacement, sibling]] {
      let next = try WeChatProcessSelector.reselect(candidates: candidates, following: selected)
      #expect(next?.processID == 70)
      #expect(next?.excludedProcessIDs == [60])
    }
    // The exclusion carries over: if the replacement exits too, the sibling
    // is still not taken.
    let followed = try #require(
      try WeChatProcessSelector.reselect(candidates: [sibling, replacement], following: selected))
    #expect(try WeChatProcessSelector.reselect(candidates: [sibling], following: followed) == nil)
    // Two new processes of the same app are ambiguous.
    let another = Self.candidate(71, app: Self.standardApp, bundle: Self.standard)
    #expect(
      throws: ProcessSelectionError.multipleProcessesForTarget(
        bundleIdentifier: Self.standard, count: 2)
    ) {
      try WeChatProcessSelector.reselect(
        candidates: [sibling, replacement, another], following: selected)
    }
  }

  // MARK: - Helpers

  @Test func findsTheAppBundleOfAMainExecutableOnly() {
    #expect(
      WeChatProcessSelector.applicationPath(
        forExecutablePath: "/Applications/WeChat.app/Contents/MacOS/WeChat")
        == "/Applications/WeChat.app")
    #expect(
      WeChatProcessSelector.applicationPath(
        forExecutablePath: "/Applications/WeChat Copy.app/Contents/MacOS/WeChat")
        == "/Applications/WeChat Copy.app")
    #expect(
      WeChatProcessSelector.applicationPath(
        forExecutablePath:
          "/Applications/WeChat.app/Contents/Frameworks/Helper.app/Contents/MacOS/WeChat")
        == "/Applications/WeChat.app/Contents/Frameworks/Helper.app")
    for path in [
      "/usr/local/bin/WeChat",
      "/Applications/WeChat.app/Contents/Resources/WeChat",
      "/Applications/.app/Contents/MacOS/WeChat",
      "/Contents/MacOS/WeChat",
    ] {
      #expect(WeChatProcessSelector.applicationPath(forExecutablePath: path) == nil)
    }
  }

  @Test func invokingUserComesFromSudoOnlyWhenRunningAsRoot() {
    #expect(
      WeChatProcessLocator.invokingUserID(
        effectiveUserID: 0, realUserID: 0, environment: ["SUDO_UID": "501"]) == 501)
    #expect(
      WeChatProcessLocator.invokingUserID(
        effectiveUserID: 501, realUserID: 501, environment: ["SUDO_UID": "0"]) == 501)
    #expect(
      WeChatProcessLocator.invokingUserID(
        effectiveUserID: 0, realUserID: 0, environment: ["SUDO_UID": "nope"]) == 0)
  }

  @Test func readsBundleIdentifierOnlyWhenTheExecutableMatches() throws {
    let app = FileManager.default.temporaryDirectory.appending(
      path: "greenbubbles-process-tests-\(UUID().uuidString)/WeChat Copy.app",
      directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: app.deletingLastPathComponent()) }
    let contents = app.appending(path: "Contents", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: contents, withIntermediateDirectories: true)
    let info: [String: String] = [
      "CFBundleIdentifier": Self.duplicate,
      "CFBundleExecutable": "WeChat",
    ]
    try PropertyListSerialization.data(fromPropertyList: info, format: .xml, options: 0)
      .write(to: contents.appending(path: "Info.plist"))

    #expect(
      WeChatProcessLocator.bundleIdentifier(applicationPath: app.path, executableName: "WeChat")
        == Self.duplicate)
    #expect(
      WeChatProcessLocator.bundleIdentifier(applicationPath: app.path, executableName: "Other")
        == nil)
    #expect(
      WeChatProcessLocator.bundleIdentifier(
        applicationPath: app.deletingLastPathComponent().appending(path: "Missing.app").path,
        executableName: "WeChat") == nil)
  }
}
