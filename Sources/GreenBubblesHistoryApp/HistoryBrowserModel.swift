import AppKit
import Foundation
import GreenBubblesHistory
import Observation

enum HistorySection: String, CaseIterable, Identifiable {
  case overview
  case chats
  case contacts
  case search

  var id: String { rawValue }

  var title: String {
    switch self {
    case .overview: "Overview"
    case .chats: "Chats"
    case .contacts: "Contacts"
    case .search: "Search"
    }
  }

  var systemImage: String {
    switch self {
    case .overview: "chart.bar.xaxis"
    case .chats: "bubble.left.and.bubble.right"
    case .contacts: "person.2"
    case .search: "text.magnifyingglass"
    }
  }
}

enum HistoryDirectSection: String, CaseIterable, Identifiable {
  case overview
  case chats
  case search

  var id: String { rawValue }

  var title: String {
    switch self {
    case .overview: "Overview"
    case .chats: "Chats"
    case .search: "Search"
    }
  }

  var systemImage: String {
    switch self {
    case .overview: "externaldrive.badge.checkmark"
    case .chats: "bubble.left.and.bubble.right"
    case .search: "text.magnifyingglass"
    }
  }
}

struct HistoryMediaRequest: Identifiable {
  let conversationID: String
  let artifact: HistoryArtifact
  var id: String { "\(conversationID):\(artifact.artifactID)" }
}

@MainActor
@Observable
final class HistoryBrowserModel {
  var session: HistoryBundleSession?
  var loadProgress: HistoryLoadProgress?
  var isLoading = false
  var libraryError: String?
  var selectedSection: HistorySection? = .overview
  var selectedConversationID: String?
  var selectedContactID: String?
  var conversationFilter = ""
  var contactFilter = ""
  var searchQuery = ""
  var searchResults: [HistoryMessage] = []
  var isSearching = false
  var searchError: String?
  var timelineMessages: [HistoryMessage] = []
  var timelineCursor: HistoryMessageCursor?
  var isLoadingTimeline = false
  var timelineError: String?
  var isSearchContext = false
  var highlightedMessageID: String?
  var showsCoverageDetails = false
  var pendingMediaRequest: HistoryMediaRequest?
  var liveExecutablePath = ""
  var liveReplicaPath = ""
  var livePolicyPath = ""
  var liveAuditPath = ""
  var showsDirectConnection = false
  var showsSnapshotCreation = false
  var directSourcePath = ""
  var directAccessMode: HistoryDirectAccessMode = .liveEncrypted
  var directLocalCredentialPath = ""
  var directRecoveryKitPath = ""
  var directConfiguration: HistoryDirectConfiguration?
  var directStatus: HistoryDirectSourceStatus?
  var directSection: HistoryDirectSection? = .overview
  var directConversations: [HistoryDirectConversation] = []
  var directConversationCursor: String?
  var directConversationWarnings: [HistoryDirectWarning] = []
  var directConversationConsistency: HistoryDirectConsistency?
  var directSelectedConversationID: String?
  var directMessages: [HistoryDirectMessage] = []
  var directMessageCursor: String?
  var directMessageWarnings: [HistoryDirectWarning] = []
  var directMessageConsistency: HistoryDirectConsistency?
  var directSearchQuery = ""
  var directSearchResults: [HistoryDirectSearchHit] = []
  var directSearchCursor: String?
  var directSearchWarnings: [HistoryDirectWarning] = []
  var isDirectConnecting = false
  var isLoadingDirectConversations = false
  var isLoadingDirectMessages = false
  var isSearchingDirect = false
  var directConversationError: String?
  var directMessageError: String?
  var directSearchError: String?
  var directIsSearchContext = false
  var directHighlightedMessageID: String?

  private var store: HistoryStore?
  private var loadTask: Task<Void, Never>?
  private var timelineTask: Task<Void, Never>?
  private var artifactCache: [String: HistoryArtifact] = [:]
  private var activeLoadID = UUID()
  private var activeTimelineID = UUID()
  private var activeSearchID = UUID()
  private var activeArtifactID = UUID()
  private var directLoadTask: Task<Void, Never>?
  private var directConversationTask: Task<Void, Never>?
  private var directMessageTask: Task<Void, Never>?
  private var directSearchTask: Task<Void, Never>?
  private var activeDirectLoadID = UUID()
  private var activeDirectConversationID = UUID()
  private var activeDirectMessageID = UUID()
  private var activeDirectSearchID = UUID()
  @ObservationIgnored private var directKeyUTF8: [UInt8] = []
  @ObservationIgnored private let directClient = HistoryDirectQueryClient()
  @ObservationIgnored private let snapshotKeychain = SnapshotKeychainStore()
  private var pendingStartupBundleURL: URL?
  private var processedStartupBundle = false
  private let mediaSessionURL = FileManager.default.temporaryDirectory.appending(
    path: "greenbubbles-history-media-\(UUID().uuidString)", directoryHint: .isDirectory)
  private let snapshotUnlockSessionURL = FileManager.default.temporaryDirectory.appending(
    path: "greenbubbles-history-unlock-\(UUID().uuidString)", directoryHint: .isDirectory)

  init(
    arguments: [String] = Array(CommandLine.arguments.dropFirst()),
    currentDirectoryURL: URL = URL(
      fileURLWithPath: FileManager.default.currentDirectoryPath,
      isDirectory: true)
  ) {
    let candidates = [
      Bundle.main.executableURL?.deletingLastPathComponent().appending(
        path: "greenbubbles"),
      URL(fileURLWithPath: FileManager.default.currentDirectoryPath)
        .appending(path: "Native/GreenBubblesRestore/target/debug/greenbubbles"),
    ].compactMap { $0 }
    if let executable = candidates.first(where: {
      FileManager.default.isExecutableFile(atPath: $0.path)
    }) {
      liveExecutablePath = executable.path
    }
    let defaults = UserDefaults.standard
    if let rememberedSource = defaults.string(forKey: "history.direct.sourcePath") {
      directSourcePath = rememberedSource
    }
    if let rememberedKit = defaults.string(forKey: "history.direct.recoveryKitPath") {
      directRecoveryKitPath = rememberedKit
    }
    if let rememberedLocal = defaults.string(forKey: "history.direct.localCredentialPath") {
      directLocalCredentialPath = rememberedLocal
    }
    if let rememberedMode = defaults.string(forKey: "history.direct.accessMode"),
      let mode = HistoryDirectAccessMode(rawValue: rememberedMode)
    {
      directAccessMode = mode
    }
    do {
      pendingStartupBundleURL = try HistoryBrowserLaunchOptions(
        arguments: arguments,
        currentDirectoryURL: currentDirectoryURL
      ).bundleURL
    } catch {
      libraryError = String(describing: error)
    }
  }

  deinit {
    directKeyUTF8.resetBytes(in: 0..<directKeyUTF8.count)
    try? FileManager.default.removeItem(at: mediaSessionURL)
    try? FileManager.default.removeItem(at: snapshotUnlockSessionURL)
  }

  var hasOpenHistory: Bool {
    session != nil || directConfiguration != nil
  }

  var isDirectHistoryOpen: Bool {
    directConfiguration != nil && directStatus != nil
  }

  var selectedConversation: HistoryConversation? {
    guard let selectedConversationID else { return nil }
    return session?.conversations.first { $0.conversationID == selectedConversationID }
  }

  var selectedContact: HistoryContact? {
    guard let selectedContactID else { return nil }
    return session?.contacts.first { $0.participantID == selectedContactID }
  }

  var filteredConversations: [HistoryConversation] {
    guard let session else { return [] }
    let query = conversationFilter.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !query.isEmpty else { return session.conversations }
    return session.conversations.filter { conversation in
      conversation.humanLabel.localizedCaseInsensitiveContains(query)
        || conversation.participants.contains {
          $0.displayName.localizedCaseInsensitiveContains(query)
        }
    }
  }

  var filteredContacts: [HistoryContact] {
    guard let session else { return [] }
    let query = contactFilter.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !query.isEmpty else { return session.contacts }
    return session.contacts.filter { contact in
      contact.displayName.localizedCaseInsensitiveContains(query)
        || contact.conversationProfiles.contains {
          $0.conversationLabel.localizedCaseInsensitiveContains(query)
        }
    }
  }

  var filteredDirectConversations: [HistoryDirectConversation] {
    let query = conversationFilter.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !query.isEmpty else { return directConversations }
    return directConversations.filter { conversation in
      conversation.displayName.localizedCaseInsensitiveContains(query)
        || (conversation.summary?.localizedCaseInsensitiveContains(query) == true)
        || (conversation.lastSenderDisplayName?.localizedCaseInsensitiveContains(query) == true)
    }
  }

  var selectedDirectConversation: HistoryDirectConversation? {
    guard let directSelectedConversationID else { return nil }
    return directConversations.first { $0.id == directSelectedConversationID }
  }

  func resolvedDirection(for message: HistoryMessage) -> String? {
    message.resolvedDirection(
      selfParticipantID: session?.manifest.context.selfParticipantID
    )
  }

  func presentDirectConnection() {
    libraryError = nil
    showsDirectConnection = true
  }

  func presentSnapshotCreation() {
    libraryError = nil
    showsSnapshotCreation = true
  }

  func rememberCreatedSnapshot(
    _ result: HistorySnapshotCreationResult,
    keychainProtected: Bool,
    hiddenCredentialURL: URL?,
    recoveryKitURL: URL,
    passphraseProtected: Bool
  ) {
    directSourcePath = result.outputURL.standardizedFileURL.path
    directRecoveryKitPath = recoveryKitURL.standardizedFileURL.path
    if keychainProtected {
      directAccessMode = .snapshotKeychain
    } else if let hiddenCredentialURL {
      directAccessMode = .snapshotLocalCredential
      directLocalCredentialPath = hiddenCredentialURL.standardizedFileURL.path
    } else if passphraseProtected {
      directAccessMode = .snapshotPassphrase
    } else {
      directAccessMode = .snapshotRecoveryKit
    }
    UserDefaults.standard.set(directSourcePath, forKey: "history.direct.sourcePath")
    UserDefaults.standard.set(directAccessMode.rawValue, forKey: "history.direct.accessMode")
    UserDefaults.standard.set(
      directRecoveryKitPath,
      forKey: "history.direct.recoveryKitPath"
    )
  }

  func connectDirectSource(
    executableURL: URL,
    sourceURL: URL,
    accessMode: HistoryDirectAccessMode,
    keyUTF8: [UInt8],
    recoveryKitURL: URL? = nil,
    localCredentialURL: URL? = nil
  ) {
    closeBundle()
    let effectiveLocalCredentialURL: URL?
    if accessMode == .snapshotKeychain {
      do {
        effectiveLocalCredentialURL = try snapshotKeychain.materialize(
          snapshotURL: sourceURL,
          sessionDirectory: snapshotUnlockSessionURL
        )
      } catch {
        libraryError = String(describing: error)
        showsDirectConnection = false
        return
      }
    } else {
      effectiveLocalCredentialURL = localCredentialURL
    }
    liveExecutablePath = executableURL.standardizedFileURL.path
    directSourcePath = sourceURL.standardizedFileURL.path
    directAccessMode = accessMode
    directRecoveryKitPath = recoveryKitURL?.standardizedFileURL.path ?? directRecoveryKitPath
    if accessMode != .snapshotKeychain {
      directLocalCredentialPath =
        localCredentialURL?.standardizedFileURL.path ?? directLocalCredentialPath
    }
    let configuration = HistoryDirectConfiguration(
      executableURL: executableURL,
      sourceURL: sourceURL,
      accessMode: accessMode,
      recoveryKitURL: recoveryKitURL,
      localCredentialURL: effectiveLocalCredentialURL
    )
    let defaults = UserDefaults.standard
    defaults.set(directSourcePath, forKey: "history.direct.sourcePath")
    defaults.set(accessMode.rawValue, forKey: "history.direct.accessMode")
    if let recoveryKitURL {
      defaults.set(
        recoveryKitURL.standardizedFileURL.path,
        forKey: "history.direct.recoveryKitPath"
      )
    }
    if let localCredentialURL, accessMode != .snapshotKeychain {
      defaults.set(
        localCredentialURL.standardizedFileURL.path,
        forKey: "history.direct.localCredentialPath"
      )
    }
    directConfiguration = configuration
    replaceDirectKey(with: keyUTF8)
    showsDirectConnection = false
    isDirectConnecting = true
    libraryError = nil
    let loadID = UUID()
    activeDirectLoadID = loadID
    directLoadTask = Task { [weak self] in
      guard let self else { return }
      var key = directKeyUTF8
      defer { key.resetBytes(in: 0..<key.count) }
      do {
        let status = try await directClient.status(
          configuration: configuration,
          keyUTF8: key
        )
        try Task.checkCancellation()
        let page = try await directClient.conversations(
          configuration: configuration,
          keyUTF8: key,
          limit: 100
        )
        try Task.checkCancellation()
        guard activeDirectLoadID == loadID, directConfiguration == configuration else { return }
        directStatus = status
        directConversations = page.items
        directConversationCursor = page.page.nextCursor
        directConversationWarnings = page.warnings
        directConversationConsistency = page.consistency
        directSection = .overview
        isDirectConnecting = false
      } catch is CancellationError {
        if activeDirectLoadID == loadID { isDirectConnecting = false }
      } catch {
        guard activeDirectLoadID == loadID else { return }
        isDirectConnecting = false
        libraryError = String(describing: error)
        clearDirectState()
      }
    }
  }

  func loadMoreDirectConversations() {
    guard let configuration = directConfiguration, let cursor = directConversationCursor,
      !isLoadingDirectConversations
    else { return }
    directConversationTask?.cancel()
    let requestID = UUID()
    activeDirectConversationID = requestID
    isLoadingDirectConversations = true
    directConversationError = nil
    directConversationTask = Task { [weak self] in
      guard let self else { return }
      var key = directKeyUTF8
      defer { key.resetBytes(in: 0..<key.count) }
      do {
        let page = try await directClient.conversations(
          configuration: configuration,
          keyUTF8: key,
          limit: 100,
          cursor: cursor
        )
        try Task.checkCancellation()
        guard activeDirectConversationID == requestID,
          directConfiguration == configuration,
          directConversationCursor == cursor
        else { return }
        let existing = Set(directConversations.map(\.id))
        directConversations.append(contentsOf: page.items.filter { !existing.contains($0.id) })
        directConversationCursor = page.page.nextCursor
        directConversationWarnings = mergeDirectWarnings(
          directConversationWarnings,
          page.warnings
        )
        directConversationConsistency = page.consistency
        isLoadingDirectConversations = false
      } catch is CancellationError {
        if activeDirectConversationID == requestID { isLoadingDirectConversations = false }
      } catch {
        if activeDirectConversationID == requestID {
          isLoadingDirectConversations = false
          directConversationError = String(describing: error)
        }
      }
    }
  }

  func selectDirectConversation(_ conversationID: String) {
    directSection = .chats
    directSelectedConversationID = conversationID
    loadDirectMessages(conversationID: conversationID, cursor: nil, appending: false)
  }

  func loadOlderDirectMessages() {
    guard let conversationID = directSelectedConversationID,
      let cursor = directMessageCursor,
      !isLoadingDirectMessages,
      !directIsSearchContext
    else { return }
    loadDirectMessages(conversationID: conversationID, cursor: cursor, appending: true)
  }

  func returnToLatestDirectMessages() {
    guard let conversationID = directSelectedConversationID else { return }
    loadDirectMessages(conversationID: conversationID, cursor: nil, appending: false)
  }

  func performDirectSearch() async {
    let query = directSearchQuery.trimmingCharacters(in: .whitespacesAndNewlines)
    let searchID = UUID()
    activeDirectSearchID = searchID
    guard !query.isEmpty, let configuration = directConfiguration else {
      directSearchTask?.cancel()
      directSearchResults = []
      directSearchCursor = nil
      directSearchWarnings = []
      directSearchError = nil
      isSearchingDirect = false
      return
    }
    isSearchingDirect = true
    directSearchError = nil
    var key = directKeyUTF8
    defer { key.resetBytes(in: 0..<key.count) }
    do {
      try await Task.sleep(for: .milliseconds(250))
      try Task.checkCancellation()
      guard activeDirectSearchID == searchID,
        query == directSearchQuery.trimmingCharacters(in: .whitespacesAndNewlines)
      else { return }
      let page = try await directClient.search(
        configuration: configuration,
        keyUTF8: key,
        query: query,
        limit: 100
      )
      try Task.checkCancellation()
      guard activeDirectSearchID == searchID,
        directConfiguration == configuration,
        query == directSearchQuery.trimmingCharacters(in: .whitespacesAndNewlines)
      else { return }
      directSearchResults = page.items
      directSearchCursor = page.page.nextCursor
      directSearchWarnings = page.warnings
      isSearchingDirect = false
    } catch is CancellationError {
      if activeDirectSearchID == searchID { isSearchingDirect = false }
    } catch {
      if activeDirectSearchID == searchID {
        isSearchingDirect = false
        directSearchResults = []
        directSearchCursor = nil
        directSearchError = String(describing: error)
      }
    }
  }

  func loadMoreDirectSearchResults() {
    let query = directSearchQuery.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !query.isEmpty, let configuration = directConfiguration,
      let cursor = directSearchCursor, !isSearchingDirect
    else { return }
    directSearchTask?.cancel()
    let searchID = UUID()
    activeDirectSearchID = searchID
    isSearchingDirect = true
    directSearchError = nil
    directSearchTask = Task { [weak self] in
      guard let self else { return }
      var key = directKeyUTF8
      defer { key.resetBytes(in: 0..<key.count) }
      do {
        let page = try await directClient.search(
          configuration: configuration,
          keyUTF8: key,
          query: query,
          limit: 100,
          cursor: cursor
        )
        try Task.checkCancellation()
        guard activeDirectSearchID == searchID,
          directConfiguration == configuration,
          directSearchCursor == cursor,
          query == directSearchQuery.trimmingCharacters(in: .whitespacesAndNewlines)
        else { return }
        let existing = Set(directSearchResults.map(\.id))
        directSearchResults.append(contentsOf: page.items.filter { !existing.contains($0.id) })
        directSearchCursor = page.page.nextCursor
        directSearchWarnings = mergeDirectWarnings(directSearchWarnings, page.warnings)
        isSearchingDirect = false
      } catch is CancellationError {
        if activeDirectSearchID == searchID { isSearchingDirect = false }
      } catch {
        if activeDirectSearchID == searchID {
          isSearchingDirect = false
          directSearchError = String(describing: error)
        }
      }
    }
  }

  func openDirectSearchResult(_ hit: HistoryDirectSearchHit) {
    guard let configuration = directConfiguration else { return }
    directMessageTask?.cancel()
    let requestID = UUID()
    activeDirectMessageID = requestID
    directSection = .chats
    directSelectedConversationID = hit.conversationID
    directMessages = []
    directMessageCursor = nil
    directMessageWarnings = []
    directMessageError = nil
    directIsSearchContext = true
    directHighlightedMessageID = hit.id
    isLoadingDirectMessages = true
    directMessageTask = Task { [weak self] in
      guard let self else { return }
      var key = directKeyUTF8
      defer { key.resetBytes(in: 0..<key.count) }
      do {
        let resource = try await directClient.message(
          configuration: configuration,
          keyUTF8: key,
          conversationID: hit.conversationID,
          messageID: hit.id
        )
        try Task.checkCancellation()
        guard activeDirectMessageID == requestID,
          directSelectedConversationID == hit.conversationID,
          directConfiguration == configuration
        else { return }
        directMessages = [resource.item]
        directMessageWarnings = resource.warnings
        directMessageConsistency = resource.consistency
        isLoadingDirectMessages = false
      } catch is CancellationError {
        if activeDirectMessageID == requestID { isLoadingDirectMessages = false }
      } catch {
        if activeDirectMessageID == requestID {
          isLoadingDirectMessages = false
          directMessageError = String(describing: error)
        }
      }
    }
  }

  func chooseBundle() {
    let panel = NSOpenPanel()
    panel.title = "Open GreenBubbles AI Context Bundle"
    panel.message =
      "Choose the private directory containing manifest.json and the four JSONL files."
    panel.prompt = "Open History"
    panel.canChooseDirectories = true
    panel.canChooseFiles = false
    panel.allowsMultipleSelection = false
    panel.resolvesAliases = false
    guard panel.runModal() == .OK, let url = panel.url else { return }
    openBundle(url)
  }

  func openStartupBundleIfNeeded() {
    guard !processedStartupBundle else { return }
    processedStartupBundle = true
    guard let pendingStartupBundleURL else { return }
    self.pendingStartupBundleURL = nil
    openBundle(pendingStartupBundleURL)
  }

  func openExternalURLs(_ urls: [URL]) {
    guard urls.count == 1, let url = urls.first else {
      libraryError = "Open one history bundle at a time."
      return
    }
    openBundle(HistoryBrowserLaunchOptions.normalizeOpenedURL(url))
  }

  func openBundle(_ url: URL) {
    clearDirectState()
    showsDirectConnection = false
    loadTask?.cancel()
    timelineTask?.cancel()
    let loadID = UUID()
    activeLoadID = loadID
    activeTimelineID = UUID()
    activeSearchID = UUID()
    activeArtifactID = UUID()
    isLoading = true
    isLoadingTimeline = false
    isSearching = false
    libraryError = nil
    timelineError = nil
    searchError = nil
    loadProgress = nil
    session = nil
    store = nil
    selectedSection = .overview
    selectedConversationID = nil
    selectedContactID = nil
    conversationFilter = ""
    contactFilter = ""
    searchQuery = ""
    searchResults = []
    timelineMessages = []
    timelineCursor = nil
    isSearchContext = false
    highlightedMessageID = nil
    showsCoverageDetails = false
    artifactCache.removeAll()
    pendingMediaRequest = nil
    loadTask = Task { [weak self] in
      guard let self else { return }
      do {
        let indexDirectory = try historyIndexDirectory()
        let loaded = try await HistoryBundleLoader().load(
          bundleURL: url,
          indexDirectory: indexDirectory,
          progress: { [weak self] update in
            Task { @MainActor [weak self] in
              guard let self, self.activeLoadID == loadID, self.isLoading else { return }
              if update.overallFraction >= (self.loadProgress?.overallFraction ?? 0) {
                self.loadProgress = update
              }
            }
          }
        )
        try Task.checkCancellation()
        let openedStore = try HistoryStore(session: loaded)
        guard activeLoadID == loadID else { return }
        session = loaded
        store = openedStore
        selectedSection = .overview
        isLoading = false
        loadProgress = HistoryLoadProgress(
          phase: .ready,
          fileRole: nil,
          completedBytes: loaded.manifest.files.reduce(0) { $0 + $1.byteCount },
          totalBytes: loaded.manifest.files.reduce(0) { $0 + $1.byteCount },
          completedRecords: loaded.manifest.files.reduce(0) { $0 + $1.recordCount },
          totalRecords: loaded.manifest.files.reduce(0) { $0 + $1.recordCount },
          bundleByteCount: loaded.manifest.files.reduce(0) { $0 + $1.byteCount },
          bundleRecordCount: loaded.manifest.files.reduce(0) { $0 + $1.recordCount },
          phaseFraction: 1,
          overallFraction: 1,
          usingCachedIndex: loaded.reusedIndex
        )
      } catch is CancellationError {
        if activeLoadID == loadID { isLoading = false }
      } catch {
        if activeLoadID == loadID {
          isLoading = false
          libraryError = String(describing: error)
        }
      }
    }
  }

  func closeBundle() {
    loadTask?.cancel()
    timelineTask?.cancel()
    activeLoadID = UUID()
    activeTimelineID = UUID()
    activeSearchID = UUID()
    activeArtifactID = UUID()
    isLoading = false
    isLoadingTimeline = false
    isSearching = false
    session = nil
    store = nil
    loadProgress = nil
    libraryError = nil
    selectedConversationID = nil
    selectedContactID = nil
    selectedSection = .overview
    conversationFilter = ""
    contactFilter = ""
    searchQuery = ""
    searchError = nil
    searchResults = []
    timelineCursor = nil
    timelineError = nil
    timelineMessages = []
    isSearchContext = false
    highlightedMessageID = nil
    showsCoverageDetails = false
    artifactCache.removeAll()
    pendingMediaRequest = nil
    clearDirectState()
    showsDirectConnection = false
  }

  func selectConversation(_ conversationID: String, around canonicalID: String? = nil) {
    selectedSection = .chats
    selectedConversationID = conversationID
    selectedContactID = nil
    loadTimeline(conversationID: conversationID, around: canonicalID)
  }

  func selectContact(_ participantID: String) {
    timelineTask?.cancel()
    activeTimelineID = UUID()
    selectedSection = .contacts
    selectedContactID = participantID
    selectedConversationID = nil
    timelineMessages = []
    timelineCursor = nil
    timelineError = nil
    isLoadingTimeline = false
    isSearchContext = false
    highlightedMessageID = nil
  }

  func loadOlderMessages() {
    guard let conversationID = selectedConversationID, let cursor = timelineCursor,
      !isLoadingTimeline, !isSearchContext, let store
    else { return }
    timelineTask?.cancel()
    let timelineID = UUID()
    activeTimelineID = timelineID
    isLoadingTimeline = true
    timelineError = nil
    timelineTask = Task { [weak self] in
      guard let self else { return }
      do {
        let page = try await store.messages(
          conversationID: conversationID, before: cursor, limit: 100)
        guard activeTimelineID == timelineID, selectedConversationID == conversationID else {
          return
        }
        let existing = Set(timelineMessages.map(\.canonicalID))
        timelineMessages.append(
          contentsOf: page.messages.filter { !existing.contains($0.canonicalID) })
        timelineCursor = page.nextCursor
        isLoadingTimeline = false
      } catch {
        guard activeTimelineID == timelineID else { return }
        isLoadingTimeline = false
        if !(error is CancellationError) {
          timelineError = String(describing: error)
        }
      }
    }
  }

  func returnToLatestMessages() {
    guard let selectedConversationID else { return }
    loadTimeline(conversationID: selectedConversationID, around: nil)
  }

  func performSearch() async {
    let searchID = UUID()
    activeSearchID = searchID
    let query = searchQuery.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !query.isEmpty, let store else {
      searchResults = []
      searchError = nil
      isSearching = false
      return
    }
    isSearching = true
    searchError = nil
    do {
      try await Task.sleep(for: .milliseconds(250))
      guard !Task.isCancelled, activeSearchID == searchID,
        query == searchQuery.trimmingCharacters(in: .whitespacesAndNewlines)
      else { return }
      let results = try await store.searchMessages(query: query, limit: 150)
      guard activeSearchID == searchID,
        query == searchQuery.trimmingCharacters(in: .whitespacesAndNewlines)
      else { return }
      searchResults = results
      isSearching = false
    } catch is CancellationError {
      if activeSearchID == searchID { isSearching = false }
    } catch {
      if activeSearchID == searchID {
        isSearching = false
        searchError = String(describing: error)
      }
    }
  }

  func openSearchResult(_ message: HistoryMessage) {
    selectConversation(message.conversationID, around: message.canonicalID)
  }

  func artifact(_ artifactID: String) async throws -> HistoryArtifact? {
    if let cached = artifactCache[artifactID] { return cached }
    guard let store else { return nil }
    let generation = activeArtifactID
    guard let artifact = try await store.artifact(artifactID: artifactID) else { return nil }
    try Task.checkCancellation()
    guard activeArtifactID == generation else { throw CancellationError() }
    artifactCache[artifactID] = artifact
    return artifact
  }

  func requestMediaPreview(_ artifact: HistoryArtifact) {
    let conversationID =
      selectedConversationID.flatMap { selected in
        artifact.conversationIDs.contains(selected) ? selected : nil
      } ?? artifact.conversationIDs.first
    guard let conversationID else { return }
    pendingMediaRequest = HistoryMediaRequest(
      conversationID: conversationID,
      artifact: artifact
    )
  }

  func liveMediaConfiguration() throws -> HistoryLiveMediaConfiguration {
    guard !liveExecutablePath.isEmpty, !liveReplicaPath.isEmpty,
      !livePolicyPath.isEmpty, !liveAuditPath.isEmpty, let context = session?.manifest.context
    else {
      throw HistoryLiveMediaError.invalidConfiguration(
        "choose the CLI, replica, policy, and audit paths")
    }
    return HistoryLiveMediaConfiguration(
      executableURL: URL(fileURLWithPath: liveExecutablePath),
      replicaURL: URL(fileURLWithPath: liveReplicaPath),
      policyURL: URL(fileURLWithPath: livePolicyPath),
      auditURL: URL(fileURLWithPath: liveAuditPath),
      sessionDirectory: mediaSessionURL,
      scratchDirectory: mediaSessionURL.appending(path: "requests", directoryHint: .isDirectory),
      previewDirectory: mediaSessionURL.appending(path: "previews", directoryHint: .isDirectory),
      expectedAccountID: context.accountID,
      expectedReplicaID: context.replicaID,
      expectedSourceFingerprint: context.sourceFingerprint
    )
  }

  func chooseLiveExecutable() {
    if let url = chooseFile(title: "Choose greenbubbles", prompt: "Choose CLI") {
      liveExecutablePath = url.path
    }
  }

  func chooseLiveReplica() {
    if let url = chooseFile(
      title: "Choose encrypted GreenBubbles replica", prompt: "Choose Replica")
    {
      liveReplicaPath = url.path
    }
  }

  func chooseLivePolicy() {
    if let url = chooseFile(title: "Choose GreenBubbles tool policy", prompt: "Choose Policy") {
      livePolicyPath = url.path
    }
  }

  func chooseLiveAudit() {
    let panel = NSSavePanel()
    panel.title = "Choose the private connector audit log"
    panel.prompt = "Use Audit Log"
    panel.nameFieldStringValue = "audit.ndjson"
    panel.canCreateDirectories = true
    if panel.runModal() == .OK, let url = panel.url {
      liveAuditPath = url.path
    }
  }

  private func loadTimeline(conversationID: String, around canonicalID: String?) {
    timelineTask?.cancel()
    let timelineID = UUID()
    activeTimelineID = timelineID
    timelineMessages = []
    timelineCursor = nil
    timelineError = nil
    isLoadingTimeline = true
    isSearchContext = canonicalID != nil
    highlightedMessageID = canonicalID
    timelineTask = Task { [weak self] in
      guard let self, let store else { return }
      do {
        if let canonicalID {
          let messages = try await store.messagesAround(canonicalID: canonicalID)
          guard activeTimelineID == timelineID, selectedConversationID == conversationID else {
            return
          }
          timelineMessages = messages
          timelineCursor = nil
        } else {
          let page = try await store.messages(conversationID: conversationID, limit: 100)
          guard activeTimelineID == timelineID, selectedConversationID == conversationID else {
            return
          }
          timelineMessages = page.messages
          timelineCursor = page.nextCursor
        }
        isLoadingTimeline = false
      } catch is CancellationError {
        if activeTimelineID == timelineID { isLoadingTimeline = false }
      } catch {
        if activeTimelineID == timelineID {
          isLoadingTimeline = false
          timelineError = String(describing: error)
        }
      }
    }
  }

  private func loadDirectMessages(
    conversationID: String,
    cursor: String?,
    appending: Bool
  ) {
    guard let configuration = directConfiguration else { return }
    directMessageTask?.cancel()
    let requestID = UUID()
    activeDirectMessageID = requestID
    if !appending {
      directMessages = []
      directMessageCursor = nil
      directMessageWarnings = []
    }
    directMessageError = nil
    directIsSearchContext = false
    directHighlightedMessageID = nil
    isLoadingDirectMessages = true
    directMessageTask = Task { [weak self] in
      guard let self else { return }
      var key = directKeyUTF8
      defer { key.resetBytes(in: 0..<key.count) }
      do {
        let page = try await directClient.messages(
          configuration: configuration,
          keyUTF8: key,
          conversationID: conversationID,
          limit: 100,
          cursor: cursor
        )
        try Task.checkCancellation()
        guard activeDirectMessageID == requestID,
          directSelectedConversationID == conversationID,
          directConfiguration == configuration
        else { return }
        if appending {
          let existing = Set(directMessages.map(\.id))
          directMessages.append(contentsOf: page.items.filter { !existing.contains($0.id) })
          directMessageWarnings = mergeDirectWarnings(directMessageWarnings, page.warnings)
        } else {
          directMessages = page.items
          directMessageWarnings = page.warnings
        }
        directMessageCursor = page.page.nextCursor
        directMessageConsistency = page.consistency
        isLoadingDirectMessages = false
      } catch is CancellationError {
        if activeDirectMessageID == requestID { isLoadingDirectMessages = false }
      } catch {
        if activeDirectMessageID == requestID {
          isLoadingDirectMessages = false
          directMessageError = String(describing: error)
        }
      }
    }
  }

  private func replaceDirectKey(with key: [UInt8]) {
    directKeyUTF8.resetBytes(in: 0..<directKeyUTF8.count)
    directKeyUTF8.removeAll(keepingCapacity: false)
    directKeyUTF8 = key
  }

  private func clearDirectState() {
    directLoadTask?.cancel()
    directConversationTask?.cancel()
    directMessageTask?.cancel()
    directSearchTask?.cancel()
    activeDirectLoadID = UUID()
    activeDirectConversationID = UUID()
    activeDirectMessageID = UUID()
    activeDirectSearchID = UUID()
    directLoadTask = nil
    directConversationTask = nil
    directMessageTask = nil
    directSearchTask = nil
    directKeyUTF8.resetBytes(in: 0..<directKeyUTF8.count)
    directKeyUTF8.removeAll(keepingCapacity: false)
    directConfiguration = nil
    directStatus = nil
    directSection = .overview
    directConversations = []
    directConversationCursor = nil
    directConversationWarnings = []
    directConversationConsistency = nil
    directSelectedConversationID = nil
    directMessages = []
    directMessageCursor = nil
    directMessageWarnings = []
    directMessageConsistency = nil
    directSearchQuery = ""
    directSearchResults = []
    directSearchCursor = nil
    directSearchWarnings = []
    isDirectConnecting = false
    isLoadingDirectConversations = false
    isLoadingDirectMessages = false
    isSearchingDirect = false
    directConversationError = nil
    directMessageError = nil
    directSearchError = nil
    directIsSearchContext = false
    directHighlightedMessageID = nil
    try? FileManager.default.removeItem(at: snapshotUnlockSessionURL)
  }

  private func mergeDirectWarnings(
    _ existing: [HistoryDirectWarning],
    _ additions: [HistoryDirectWarning]
  ) -> [HistoryDirectWarning] {
    var merged = existing
    for warning in additions {
      if let index = merged.firstIndex(where: { $0.id == warning.id }) {
        merged[index] = warning
      } else {
        merged.append(warning)
      }
    }
    return merged
  }

  private func historyIndexDirectory() throws -> URL {
    guard
      let applicationSupport = FileManager.default.urls(
        for: .applicationSupportDirectory, in: .userDomainMask
      ).first
    else {
      throw HistoryBundleError.indexFailure("Application Support is unavailable")
    }
    return
      applicationSupport
      .appending(path: "GreenBubbles", directoryHint: .isDirectory)
      .appending(path: "HistoryIndexes", directoryHint: .isDirectory)
  }

  private func chooseFile(title: String, prompt: String) -> URL? {
    let panel = NSOpenPanel()
    panel.title = title
    panel.prompt = prompt
    panel.canChooseFiles = true
    panel.canChooseDirectories = false
    panel.allowsMultipleSelection = false
    panel.resolvesAliases = false
    return panel.runModal() == .OK ? panel.url : nil
  }
}
