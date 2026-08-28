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

  private var store: HistoryStore?
  private var loadTask: Task<Void, Never>?
  private var timelineTask: Task<Void, Never>?
  private var artifactCache: [String: HistoryArtifact] = [:]
  private var activeLoadID = UUID()
  private var activeTimelineID = UUID()
  private var activeSearchID = UUID()
  private var activeArtifactID = UUID()
  private var pendingStartupBundleURL: URL?
  private var processedStartupBundle = false
  private let mediaSessionURL = FileManager.default.temporaryDirectory.appending(
    path: "greenbubbles-history-media-\(UUID().uuidString)", directoryHint: .isDirectory)

  init(
    arguments: [String] = Array(CommandLine.arguments.dropFirst()),
    currentDirectoryURL: URL = URL(
      fileURLWithPath: FileManager.default.currentDirectoryPath,
      isDirectory: true)
  ) {
    let candidates = [
      Bundle.main.executableURL?.deletingLastPathComponent().appending(
        path: "greenbubbles-restore"),
      URL(fileURLWithPath: FileManager.default.currentDirectoryPath)
        .appending(path: "Native/GreenBubblesRestore/target/debug/greenbubbles-restore"),
    ].compactMap { $0 }
    if let executable = candidates.first(where: {
      FileManager.default.isExecutableFile(atPath: $0.path)
    }) {
      liveExecutablePath = executable.path
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
    try? FileManager.default.removeItem(at: mediaSessionURL)
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

  func resolvedDirection(for message: HistoryMessage) -> String? {
    message.resolvedDirection(
      selfParticipantID: session?.manifest.context.selfParticipantID
    )
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
    if let url = chooseFile(title: "Choose greenbubbles-restore", prompt: "Choose CLI") {
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
