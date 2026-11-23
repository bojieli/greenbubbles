import Darwin
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
    user: uid_t = WeChatProcessSelectionTests.user,
    start: UInt64? = 1_000
  ) -> WeChatProcessCandidate {
    WeChatProcessCandidate(
      processID: processID,
      userID: user,
      executablePath: app + "/Contents/MacOS/WeChat",
      applicationPath: app,
      bundleIdentifier: bundle,
      startMicroseconds: start
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

  @Test func canonicalizesTheRootBeforeMappingItsOwner() throws {
    let fileManager = FileManager.default
    let base = fileManager.temporaryDirectory.appending(
      path: "greenbubbles-owner-tests-\(UUID().uuidString)", directoryHint: .isDirectory)
    defer { try? fileManager.removeItem(at: base) }
    let home = base.appending(path: "home", directoryHint: .isDirectory)
    let root = home.appending(
      path: "Library/Containers/\(Self.duplicate)/Data/Documents/xwechat_files/a/db_storage")
    try fileManager.createDirectory(at: root, withIntermediateDirectories: true)
    let expected = DatabaseRootOwner.application(bundleIdentifier: Self.duplicate)
    #expect(DatabaseRootOwner.resolve(databaseRoot: root, homeDirectory: home) == expected)

    // Reached through a link elsewhere.
    let link = base.appending(path: "link")
    try fileManager.createSymbolicLink(at: link, withDestinationURL: root)
    #expect(DatabaseRootOwner.resolve(databaseRoot: link, homeDirectory: home) == expected)

    // Typed in another letter case; only meaningful on a case-insensitive
    // volume, where this path exists.
    let otherCase = home.appending(
      path:
        "library/containers/\(Self.duplicate.lowercased())/data/documents/xwechat_files/a/db_storage"
    )
    if fileManager.fileExists(atPath: otherCase.path) {
      #expect(DatabaseRootOwner.resolve(databaseRoot: otherCase, homeDirectory: home) == expected)
    }
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
    // A process whose bundle could not be read can't be ruled out.
    let unreadable = Self.candidate(44, app: Self.duplicateApp, bundle: nil)
    #expect(throws: ProcessSelectionError.unclassifiedProcesses(count: 1)) {
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
    // A listed process whose user can't be read may be the user's own.
    #expect(throws: ProcessSelectionError.unclassifiedProcesses(count: 1)) {
      try WeChatProcessSelector.select(
        candidates: [unknownUser, Self.standardProcess], userID: Self.user, owner: owner)
    }
  }

  @Test func unclassifiableLiveProcessBlocksAutomaticSelection() {
    // The readable process matches, but the unreadable one might be the copy
    // that owns the root; in either listing order, ask for --pid.
    let unreadable = WeChatProcessCandidate(
      processID: 44, userID: Self.user,
      executablePath: Self.duplicateApp + "/Contents/MacOS/WeChat",
      applicationPath: Self.duplicateApp, bundleIdentifier: nil, startMicroseconds: 1_000)
    for candidates in [[Self.standardProcess, unreadable], [unreadable, Self.standardProcess]] {
      #expect(throws: ProcessSelectionError.unclassifiedProcesses(count: 1)) {
        try WeChatProcessSelector.select(
          candidates: candidates,
          userID: Self.user,
          owner: .application(bundleIdentifier: Self.standard)
        )
      }
    }
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
      applicationPath: "/Applications/Other.app", bundleIdentifier: "com.example.Other",
      startMicroseconds: 1_000)
    let helper = WeChatProcessCandidate(
      processID: 42, userID: Self.user,
      executablePath: "/usr/local/bin/WeChat", applicationPath: nil,
      bundleIdentifier: nil, startMicroseconds: 1_000)
    for candidate in [notWeChat, helper] {
      #expect(throws: ProcessSelectionError.processNotWeChat(42)) {
        try WeChatProcessSelector.validate(
          requestedProcessID: 42, candidate: candidate, userID: Self.user, owner: .unknown)
      }
    }
  }

  // MARK: - Re-attach after a restart

  /// Times in these tests: processes selected at start 1_000, the selected
  /// process seen to exit at 5_000, replacements started at 6_000 or later.
  static let exitSeen: UInt64 = 5_000

  static func reselect(
    _ candidates: [WeChatProcessCandidate],
    _ selected: inout WeChatProcessTarget,
    now: UInt64 = WeChatProcessSelectionTests.exitSeen
  ) throws -> pid_t? {
    try WeChatProcessSelector.reselect(
      candidates: candidates, following: &selected, nowMicroseconds: now)?.processID
  }

  @Test func reattachFollowsTheSameAppAcrossPIDChanges() throws {
    let original = try #require(
      try WeChatProcessSelector.select(
        candidates: [Self.duplicateProcess, Self.standardProcess],
        userID: Self.user,
        owner: .application(bundleIdentifier: Self.duplicate)
      ))
    var selected = original
    // While it runs, the selected process is kept.
    #expect(try Self.reselect([Self.standardProcess, Self.duplicateProcess], &selected) == 43)
    #expect(selected == original)
    // Gone: the exit is recorded and, with only the other copy running
    // (lower PID), the capture keeps waiting.
    #expect(try Self.reselect([Self.standardProcess], &selected) == nil)
    #expect(selected.exitObservedMicroseconds == Self.exitSeen)
    let restarted = Self.candidate(90, app: Self.duplicateApp, bundle: Self.duplicate, start: 6_000)
    for candidates in [[Self.standardProcess, restarted], [restarted, Self.standardProcess]] {
      var following = selected
      #expect(try Self.reselect(candidates, &following, now: 7_000) == 90)
      #expect(following.processID == 90)
      #expect(following.startMicroseconds == 6_000)
      #expect(following.exitObservedMicroseconds == nil)
    }
  }

  @Test func reattachIgnoresSameBundleElsewhereAndOtherUsers() throws {
    var selected = WeChatProcessTarget(
      processID: 42, userID: Self.user, applicationPath: Self.standardApp,
      bundleIdentifier: Self.standard, startMicroseconds: 1_000)
    let movedCopy = Self.candidate(
      50, app: "/Users/synthetic-owner/Applications/WeChat.app", bundle: Self.standard,
      start: 6_000)
    let foreign = Self.candidate(
      51, app: Self.standardApp, bundle: Self.standard, user: Self.otherUser, start: 6_000)
    #expect(try Self.reselect([movedCopy, foreign], &selected) == nil)
    let twice = [
      Self.candidate(60, app: Self.standardApp, bundle: Self.standard, start: 6_000),
      Self.candidate(61, app: Self.standardApp, bundle: Self.standard, start: 6_001),
    ]
    #expect(
      throws: ProcessSelectionError.multipleProcessesForTarget(
        bundleIdentifier: Self.standard, count: 2)
    ) {
      try Self.reselect(twice, &selected, now: 7_000)
    }
  }

  @Test func explicitPIDDisambiguatesTwoInstancesOfTheSameApp() throws {
    let first = Self.candidate(60, app: Self.standardApp, bundle: Self.standard)
    let second = Self.candidate(61, app: Self.standardApp, bundle: Self.standard)
    let original = try WeChatProcessSelector.validate(
      requestedProcessID: 61,
      candidate: second,
      userID: Self.user,
      owner: .application(bundleIdentifier: Self.standard),
      runningCandidates: [first, second, Self.duplicateProcess]
    )
    #expect(
      original.excludedProcesses == [ProcessIdentity(processID: 60, startMicroseconds: 1_000)])
    // The first attach keeps the chosen PID in either listing order.
    for candidates in [[first, second], [second, first]] {
      var selected = original
      #expect(try Self.reselect(candidates, &selected) == 61)
    }
  }

  @Test func preexistingSiblingIsNeverTakenAsTheReplacement() throws {
    let sibling = Self.candidate(60, app: Self.standardApp, bundle: Self.standard)
    let chosen = Self.candidate(61, app: Self.standardApp, bundle: Self.standard)
    var selected = try WeChatProcessSelector.validate(
      requestedProcessID: 61,
      candidate: chosen,
      userID: Self.user,
      owner: .application(bundleIdentifier: Self.standard),
      runningCandidates: [sibling, chosen]
    )
    // The chosen instance exited during logout; only the original, unselected
    // sibling remains. Keep waiting instead of attaching to it.
    #expect(try Self.reselect([sibling], &selected) == nil)
    // A process started after the exit is the replacement, in either order.
    let replacement = Self.candidate(70, app: Self.standardApp, bundle: Self.standard, start: 6_000)
    for candidates in [[sibling, replacement], [replacement, sibling]] {
      var following = selected
      #expect(try Self.reselect(candidates, &following, now: 7_000) == 70)
      #expect(
        following.excludedProcesses == [ProcessIdentity(processID: 60, startMicroseconds: 1_000)])
    }
  }

  @Test func siblingStartedAfterSelectionIsNotTheReplacement() throws {
    var selected = try #require(
      try WeChatProcessSelector.select(
        candidates: [Self.standardProcess],
        userID: Self.user,
        owner: .application(bundleIdentifier: Self.standard)
      ))
    // A second instance of the same app starts after selection, while the
    // selected process still runs; it is not recorded as a sibling.
    let lateSibling = Self.candidate(
      80, app: Self.standardApp, bundle: Self.standard, start: 3_000)
    #expect(try Self.reselect([Self.standardProcess, lateSibling], &selected, now: 3_500) == 42)
    // The selected process exits at logout; the late sibling survives and is
    // not attached, because it started before the exit was observed.
    #expect(try Self.reselect([lateSibling], &selected) == nil)
    #expect(try Self.reselect([lateSibling], &selected, now: 9_000) == nil)
    // A process that starts after the exit is still followed.
    let replacement = Self.candidate(
      81, app: Self.standardApp, bundle: Self.standard, start: 6_000)
    #expect(try Self.reselect([lateSibling, replacement], &selected, now: 9_000) == 81)
  }

  @Test func reusedPIDsAndUnknownStartTimesAreNotTheSelectedProcess() throws {
    var selected = try #require(
      try WeChatProcessSelector.select(
        candidates: [Self.standardProcess],
        userID: Self.user,
        owner: .application(bundleIdentifier: Self.standard)
      ))
    // Same PID, different start time: another process now has that PID.
    let reusedEarly = Self.candidate(42, app: Self.standardApp, bundle: Self.standard, start: 4_000)
    #expect(try Self.reselect([reusedEarly], &selected) == nil)
    // A replacement whose start time can't be read never qualifies.
    let unknownStart = Self.candidate(43, app: Self.standardApp, bundle: Self.standard, start: nil)
    #expect(try Self.reselect([unknownStart], &selected, now: 9_000) == nil)
    // A reused PID that started after the exit is a genuine new process.
    let reusedLate = Self.candidate(42, app: Self.standardApp, bundle: Self.standard, start: 6_000)
    #expect(try Self.reselect([reusedLate], &selected, now: 9_000) == 42)
    #expect(selected.startMicroseconds == 6_000)
    // A PID that now runs another copy is never the selected process.
    var other = try #require(
      try WeChatProcessSelector.select(
        candidates: [Self.standardProcess],
        userID: Self.user,
        owner: .application(bundleIdentifier: Self.standard)
      ))
    let otherCopy = Self.candidate(42, app: Self.duplicateApp, bundle: Self.duplicate, start: 1_000)
    #expect(try Self.reselect([otherCopy], &other) == nil)
  }

  @Test func exitReportedByTheCaptureIsTrustedOverAStaleListing() throws {
    var selected = try #require(
      try WeChatProcessSelector.select(
        candidates: [Self.standardProcess],
        userID: Self.user,
        owner: .application(bundleIdentifier: Self.standard)
      ))
    // lldb reported the exit; a process list taken a moment later can still
    // show the old process. It is not attached again.
    selected.exitObservedMicroseconds = Self.exitSeen
    #expect(try Self.reselect([Self.standardProcess], &selected, now: 5_100) == nil)
  }

  @Test func replacementStartedDuringCleanupIsAccepted() throws {
    var selected = try #require(
      try WeChatProcessSelector.select(
        candidates: [Self.standardProcess],
        userID: Self.user,
        owner: .application(bundleIdentifier: Self.standard)
      ))
    // The capture recorded the exit when it read lldb's report (5_000); lldb
    // cleanup then took a while, and WeChat restarted meanwhile (5_500).
    selected.exitObservedMicroseconds = Self.exitSeen
    let replacement = Self.candidate(
      90, app: Self.standardApp, bundle: Self.standard, start: 5_500)
    #expect(try Self.reselect([replacement], &selected, now: 8_000) == 90)
  }

  @Test func unreadableMetadataOfTheSelectedProcessIsNotAnExit() throws {
    let original = try #require(
      try WeChatProcessSelector.select(
        candidates: [Self.standardProcess],
        userID: Self.user,
        owner: .application(bundleIdentifier: Self.standard)
      ))
    var selected = original
    // Same PID and start time, but its Info.plist could not be read.
    let unreadable = Self.candidate(42, app: Self.standardApp, bundle: nil)
    #expect(try Self.reselect([unreadable], &selected) == nil)
    #expect(selected.exitObservedMicroseconds == nil)
    // Once the metadata is readable again, the same process is attached.
    #expect(try Self.reselect([Self.standardProcess], &selected, now: 9_000) == 42)
    #expect(selected == original)
  }

  @Test func reusedPIDOfAnExcludedSiblingCanBeTheReplacement() throws {
    let sibling = Self.candidate(60, app: Self.standardApp, bundle: Self.standard)
    let chosen = Self.candidate(61, app: Self.standardApp, bundle: Self.standard)
    var selected = try WeChatProcessSelector.validate(
      requestedProcessID: 61,
      candidate: chosen,
      userID: Self.user,
      owner: .application(bundleIdentifier: Self.standard),
      runningCandidates: [sibling, chosen]
    )
    // The sibling and the chosen process are gone; a new process that got
    // the sibling's PID starts after the exit.
    #expect(try Self.reselect([], &selected) == nil)
    let reused = Self.candidate(60, app: Self.standardApp, bundle: Self.standard, start: 6_000)
    #expect(try Self.reselect([reused], &selected, now: 7_000) == 60)
  }

  @Test func replacementWaitsForAConclusiveProcessList() throws {
    var selected = try #require(
      try WeChatProcessSelector.select(
        candidates: [Self.standardProcess],
        userID: Self.user,
        owner: .application(bundleIdentifier: Self.standard)
      ))
    let replacement = Self.candidate(90, app: Self.standardApp, bundle: Self.standard, start: 6_000)
    let unreadable = WeChatProcessCandidate(
      processID: 91, userID: Self.user, executablePath: nil, applicationPath: nil,
      bundleIdentifier: nil, startMicroseconds: 6_500)
    // The selected process is seen gone at 5_000.
    #expect(try Self.reselect([], &selected) == nil)
    #expect(selected.exitObservedMicroseconds == Self.exitSeen)
    // Another live process can't be classified: it might be the real
    // replacement, so wait.
    #expect(try Self.reselect([replacement, unreadable], &selected, now: 7_000) == nil)
    #expect(selected.exitObservedMicroseconds == Self.exitSeen)
    // Once the list is conclusive, the replacement is followed.
    #expect(try Self.reselect([replacement], &selected, now: 8_000) == 90)
  }

  @Test func unverifiableSelectedPIDIsNotAnExit() throws {
    let original = try #require(
      try WeChatProcessSelector.select(
        candidates: [Self.standardProcess],
        userID: Self.user,
        owner: .application(bundleIdentifier: Self.standard)
      ))
    var selected = original
    // proc_pidinfo failed for this poll: user and start time are unknown,
    // although the executable path was read.
    let unverified = WeChatProcessCandidate(
      processID: 42, userID: nil, executablePath: Self.standardApp + "/Contents/MacOS/WeChat",
      applicationPath: Self.standardApp, bundleIdentifier: Self.standard,
      startMicroseconds: nil)
    #expect(try Self.reselect([unverified], &selected) == nil)
    #expect(selected.exitObservedMicroseconds == nil)
    #expect(try Self.reselect([Self.standardProcess], &selected, now: 9_000) == 42)
    #expect(selected == original)
    // Proven different (another start time): that is an exit.
    let reused = Self.candidate(42, app: Self.standardApp, bundle: Self.standard, start: 4_000)
    #expect(try Self.reselect([reused], &selected, now: 9_500) == nil)
    #expect(selected.exitObservedMicroseconds == 9_500)
  }

  // MARK: - Listed PIDs that can't be described

  /// A locator stub: PID 44 is listed by pgrep, but neither proc_pidinfo nor
  /// proc_pidpath returns anything for it, and it is not confirmed gone.
  static func stubCandidates(_ listed: [pid_t], gone: Set<pid_t> = []) -> [WeChatProcessCandidate] {
    WeChatProcessLocator.candidates(
      processIDs: listed,
      describe: { processID in
        [Self.standardProcess, Self.duplicateProcess].first { $0.processID == processID }
      },
      isGone: { gone.contains($0) }
    )
  }

  @Test func unreadableListedPIDStaysAnUnclassifiedCandidate() {
    let candidates = Self.stubCandidates([42, 44])
    #expect(candidates.map(\.processID) == [42, 44])
    #expect(candidates.last?.userID == nil)
    #expect(candidates.last?.bundleIdentifier == nil)
    // Confirmed gone (ESRCH): dropped.
    #expect(Self.stubCandidates([42, 44], gone: [44]).map(\.processID) == [42])
  }

  @Test func unreadableListedPIDBlocksSelectionAndReattach() throws {
    // Preflight: require --pid.
    #expect(throws: ProcessSelectionError.unclassifiedProcesses(count: 1)) {
      try WeChatProcessSelector.select(
        candidates: Self.stubCandidates([42, 44]),
        userID: Self.user,
        owner: .application(bundleIdentifier: Self.standard)
      )
    }
    var selected = try #require(
      try WeChatProcessSelector.select(
        candidates: Self.stubCandidates([42]),
        userID: Self.user,
        owner: .application(bundleIdentifier: Self.standard)
      ))
    // Selected PID listed but unreadable: skip the poll, no exit recorded.
    let unreadableSelected = WeChatProcessLocator.candidates(
      processIDs: [42], describe: { _ in nil }, isGone: { _ in false })
    #expect(try Self.reselect(unreadableSelected, &selected) == nil)
    #expect(selected.exitObservedMicroseconds == nil)
    // Selected gone; a replacement and an unreadable PID: keep waiting.
    #expect(try Self.reselect([], &selected) == nil)
    let replacement = Self.candidate(90, app: Self.standardApp, bundle: Self.standard, start: 6_000)
    let withUnreadable =
      [replacement]
      + WeChatProcessLocator.candidates(
        processIDs: [44], describe: { _ in nil }, isGone: { _ in false })
    #expect(try Self.reselect(withUnreadable, &selected, now: 7_000) == nil)
    #expect(try Self.reselect([replacement], &selected, now: 8_000) == 90)
  }

  // MARK: - Bundle ID case

  @Test func bundleIdentifiersMatchCaseInsensitively() throws {
    let lowerOwner = DatabaseRootOwner.application(bundleIdentifier: Self.standard.lowercased())
    let target = try WeChatProcessSelector.select(
      candidates: [Self.standardProcess, Self.duplicateProcess],
      userID: Self.user,
      owner: lowerOwner
    )
    #expect(target?.processID == 42)
    let explicit = try WeChatProcessSelector.validate(
      requestedProcessID: 42,
      candidate: Self.standardProcess,
      userID: Self.user,
      owner: lowerOwner
    )
    #expect(explicit.processID == 42)
    // Another copy still doesn't match, in any case.
    #expect(
      throws: ProcessSelectionError.processDoesNotOwnDatabaseRoot(
        processID: 43, processBundleIdentifier: Self.duplicate,
        ownerBundleIdentifier: Self.standard.lowercased())
    ) {
      try WeChatProcessSelector.validate(
        requestedProcessID: 43, candidate: Self.duplicateProcess, userID: Self.user,
        owner: lowerOwner)
    }
    // Two processes whose bundle IDs differ only in case are still ambiguous.
    let upper = Self.candidate(
      45, app: "/Users/synthetic-owner/Applications/WeChat.app", bundle: Self.standard.uppercased())
    #expect(
      throws: ProcessSelectionError.multipleProcessesForDatabaseRoot(
        bundleIdentifier: Self.standard.lowercased(), count: 2)
    ) {
      try WeChatProcessSelector.select(
        candidates: [Self.standardProcess, upper], userID: Self.user, owner: lowerOwner)
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

  @Test func infoPlistIsReadOnlyFromInsideTheResolvedApp() throws {
    let fileManager = FileManager.default
    let base = fileManager.temporaryDirectory.appending(
      path: "greenbubbles-process-tests-\(UUID().uuidString)", directoryHint: .isDirectory)
    defer { try? fileManager.removeItem(at: base) }
    let info: [String: String] = [
      "CFBundleIdentifier": Self.duplicate, "CFBundleExecutable": "WeChat",
    ]
    let plist = try PropertyListSerialization.data(
      fromPropertyList: info, format: .xml, options: 0)
    let elsewhere = base.appending(path: "elsewhere", directoryHint: .isDirectory)
    try fileManager.createDirectory(at: elsewhere, withIntermediateDirectories: true)
    try plist.write(to: elsewhere.appending(path: "Info.plist"))

    // Info.plist itself is a symbolic link.
    let linkedPlist = base.appending(path: "LinkedPlist.app/Contents", directoryHint: .isDirectory)
    try fileManager.createDirectory(at: linkedPlist, withIntermediateDirectories: true)
    try fileManager.createSymbolicLink(
      at: linkedPlist.appending(path: "Info.plist"),
      withDestinationURL: elsewhere.appending(path: "Info.plist"))
    // Contents is a symbolic link out of the bundle.
    let linkedContents = base.appending(path: "LinkedContents.app", directoryHint: .isDirectory)
    try fileManager.createDirectory(at: linkedContents, withIntermediateDirectories: true)
    try fileManager.createSymbolicLink(
      at: linkedContents.appending(path: "Contents"), withDestinationURL: elsewhere)
    for app in [linkedPlist.deletingLastPathComponent(), linkedContents] {
      #expect(WeChatProcessLocator.readInfoPlist(applicationPath: app.path) == nil)
      #expect(
        WeChatProcessLocator.bundleIdentifier(applicationPath: app.path, executableName: "WeChat")
          == nil)
    }
    // A real file inside the bundle is read.
    let real = base.appending(path: "Real.app/Contents", directoryHint: .isDirectory)
    try fileManager.createDirectory(at: real, withIntermediateDirectories: true)
    try plist.write(to: real.appending(path: "Info.plist"))
    #expect(
      WeChatProcessLocator.readInfoPlist(applicationPath: real.deletingLastPathComponent().path)
        == plist)
  }

  @Test func bundleIsNotReadForAnotherUsersProcess() throws {
    // The test runner itself: its user and start time come from the kernel.
    let own = try #require(
      WeChatProcessLocator.candidate(forProcessID: getpid(), expectedUserID: getuid()))
    #expect(own.userID == getuid())
    #expect(own.startMicroseconds != nil)
    let foreign = try #require(
      WeChatProcessLocator.candidate(forProcessID: getpid(), expectedUserID: getuid() &+ 1))
    #expect(foreign.bundleIdentifier == nil)
  }

  @Test func pgrepErrorsAreNotAnEmptyListing() throws {
    #expect(
      try WeChatProcessLocator.parsePgrep(status: 0, output: Data("42\n43\n".utf8)) == [42, 43])
    #expect(try WeChatProcessLocator.parsePgrep(status: 1, output: Data()) == [])
    for status: Int32 in [2, 3] {
      #expect(throws: ProcessLocatorError.commandFailed("pgrep")) {
        try WeChatProcessLocator.parsePgrep(status: status, output: Data())
      }
    }
  }

  @Test func failedListingSkipsThePollWithoutChangingExitState() throws {
    let original = try #require(
      try WeChatProcessSelector.select(
        candidates: [Self.standardProcess],
        userID: Self.user,
        owner: .application(bundleIdentifier: Self.standard)
      ))
    var selected = original
    let failed = Result<[WeChatProcessCandidate], Error> {
      throw ProcessLocatorError.commandFailed("pgrep")
    }
    #expect(
      try WeChatProcessSelector.reselect(
        listing: failed, following: &selected, nowMicroseconds: Self.exitSeen) == nil)
    #expect(selected == original)
    // After an exit was recorded, a failed listing keeps it and picks nothing.
    selected.exitObservedMicroseconds = Self.exitSeen
    let recorded = selected
    #expect(
      try WeChatProcessSelector.reselect(
        listing: failed, following: &selected, nowMicroseconds: 9_000)
        == nil)
    #expect(selected == recorded)
    // A successful listing still works.
    let replacement = Self.candidate(90, app: Self.standardApp, bundle: Self.standard, start: 6_000)
    #expect(
      try WeChatProcessSelector.reselect(
        listing: .success([replacement]), following: &selected, nowMicroseconds: 9_000)?.processID
        == 90)
  }

  @Test func processWithoutAStartTimeIsNeverSelectable() {
    let noStart = Self.candidate(42, app: Self.standardApp, bundle: Self.standard, start: nil)
    // Automatic selection rejects it explicitly (asks for --pid).
    #expect(throws: ProcessSelectionError.unclassifiedProcesses(count: 1)) {
      try WeChatProcessSelector.select(
        candidates: [noStart],
        userID: Self.user,
        owner: .application(bundleIdentifier: Self.standard)
      )
    }
    // An explicit --pid is rejected too.
    #expect(throws: ProcessSelectionError.processIdentityUnavailable(42)) {
      try WeChatProcessSelector.validate(
        requestedProcessID: 42, candidate: noStart, userID: Self.user,
        owner: .application(bundleIdentifier: Self.standard))
    }
  }

  @Test func infoPlistFIFOIsRejectedWithoutBlocking() throws {
    let fileManager = FileManager.default
    let base = fileManager.temporaryDirectory.appending(
      path: "greenbubbles-process-tests-\(UUID().uuidString)", directoryHint: .isDirectory)
    defer { try? fileManager.removeItem(at: base) }
    let contents = base.appending(path: "Fifo.app/Contents", directoryHint: .isDirectory)
    try fileManager.createDirectory(at: contents, withIntermediateDirectories: true)
    #expect(mkfifo(contents.appending(path: "Info.plist").path, 0o600) == 0)
    // No writer ever opens this FIFO; a blocking open would hang here.
    #expect(
      WeChatProcessLocator.readInfoPlist(applicationPath: contents.deletingLastPathComponent().path)
        == nil)
  }
}
