import AppKit
import GreenBubblesHistory
import SwiftUI

struct HistoryRootView: View {
  @Bindable var model: HistoryBrowserModel

  var body: some View {
    Group {
      if model.isLoading {
        HistoryLoadingView(progress: model.loadProgress) {
          model.closeBundle()
        }
      } else if model.session == nil {
        HistoryWelcomeView(errorMessage: model.libraryError) {
          model.chooseBundle()
        }
      } else {
        HistoryLibraryView(model: model)
      }
    }
    .task {
      model.openStartupBundleIfNeeded()
    }
    .dropDestination(for: URL.self) { urls, _ in
      guard let directory = urls.first,
        (try? directory.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) == true
      else { return false }
      model.openBundle(directory)
      return true
    }
  }
}

private struct HistoryWelcomeView: View {
  let errorMessage: String?
  let open: () -> Void

  var body: some View {
    VStack(spacing: 24) {
      Spacer()
      ZStack {
        Circle()
          .fill(.green.gradient.opacity(0.18))
          .frame(width: 112, height: 112)
        Image(systemName: "bubble.left.and.text.bubble.right.fill")
          .font(.system(size: 46, weight: .medium))
          .foregroundStyle(.green)
      }
      VStack(spacing: 8) {
        Text("Browse your GreenBubbles history")
          .font(.largeTitle.weight(.semibold))
        Text(
          "Open an audited AI context bundle to navigate chats, people, relationships, and media."
        )
        .font(.title3)
        .foregroundStyle(.secondary)
        .multilineTextAlignment(.center)
        .frame(maxWidth: 660)
      }
      Button(action: open) {
        Label("Open History Bundle", systemImage: "folder.badge.plus")
          .padding(.horizontal, 10)
          .padding(.vertical, 4)
      }
      .buttonStyle(.borderedProminent)
      .controlSize(.large)

      if let errorMessage {
        Label(errorMessage, systemImage: "exclamationmark.triangle.fill")
          .foregroundStyle(.red)
          .font(.callout)
          .textSelection(.enabled)
          .padding(14)
          .background(.red.opacity(0.08), in: RoundedRectangle(cornerRadius: 12))
          .frame(maxWidth: 680)
      }

      HStack(spacing: 24) {
        WelcomePromise(icon: "lock.shield", text: "Read-only and local")
        WelcomePromise(icon: "checkmark.seal", text: "Hashes verified before display")
        WelcomePromise(icon: "externaldrive.badge.checkmark", text: "Large-history indexing")
      }
      Spacer()
      Text("You can also drag the five-file bundle directory into this window.")
        .font(.footnote)
        .foregroundStyle(.tertiary)
        .padding(.bottom, 24)
    }
    .padding(40)
  }
}

private struct WelcomePromise: View {
  let icon: String
  let text: String

  var body: some View {
    Label(text, systemImage: icon)
      .font(.callout)
      .foregroundStyle(.secondary)
  }
}

private struct HistoryLoadingView: View {
  let progress: HistoryLoadProgress?
  let cancel: () -> Void

  var body: some View {
    VStack(spacing: 22) {
      ProgressView(value: progress?.overallFraction ?? 0)
        .progressViewStyle(.linear)
        .frame(width: 480)
      VStack(spacing: 7) {
        Text(progress.map(phaseTitle) ?? "Preparing history bundle…")
          .font(.title2.weight(.semibold))
        if let progress {
          HStack(spacing: 8) {
            Text(
              "Overall \(progress.overallFraction.formatted(.percent.precision(.fractionLength(1))))"
            )
            Text("•")
            Text(
              "This phase \(progress.phaseFraction.formatted(.percent.precision(.fractionLength(1))))"
            )
          }
          .font(.system(.title3, design: .rounded, weight: .medium))
          .foregroundStyle(.green)
          Text(progressDetail(progress))
            .font(.callout.monospacedDigit())
            .foregroundStyle(.secondary)
          if progress.phase == .finalizingIndex, progress.phaseFraction < 1 {
            ProgressView()
              .controlSize(.small)
          }
          if progress.bundleByteCount > 0 || progress.bundleRecordCount > 0 {
            Label(bundleDetail(progress), systemImage: "shippingbox")
              .font(.footnote.monospacedDigit())
              .foregroundStyle(.secondary)
          }
          if progress.usingCachedIndex {
            Label(
              "Revalidating source files; the verified search index will be reused",
              systemImage: "bolt.fill"
            )
            .font(.footnote)
            .foregroundStyle(.secondary)
          }
        }
      }
      Label(
        "GreenBubbles verifies permissions, schemas, hashes, record counts, and references before showing content.",
        systemImage: "checkmark.shield"
      )
      .font(.footnote)
      .foregroundStyle(.secondary)
      .frame(maxWidth: 560)
      .multilineTextAlignment(.center)
      Button("Cancel", action: cancel)
        .keyboardShortcut(.cancelAction)
    }
    .padding(40)
  }

  private func phaseTitle(_ progress: HistoryLoadProgress) -> String {
    switch progress.phase {
    case .validatingManifest: "Validating bundle manifest"
    case .verifyingConversations: "Verifying conversations"
    case .verifyingContacts: "Verifying contacts"
    case .indexingMessages: progress.usingCachedIndex ? "Verifying messages" : "Indexing messages"
    case .indexingArtifacts: progress.usingCachedIndex ? "Verifying media" : "Indexing media"
    case .finalizingIndex: "Finalizing private search index"
    case .ready: "History is ready"
    }
  }

  private func progressDetail(_ progress: HistoryLoadProgress) -> String {
    if progress.phase == .finalizingIndex {
      return "Checking SQLite and search-index integrity before atomic publication"
    }
    if progress.phase == .validatingManifest {
      return "Validating the bundle inventory, checkpoint, policy, and file evidence"
    }
    let bytes = ByteCountFormatter.string(
      fromByteCount: Int64(clamping: progress.completedBytes), countStyle: .file)
    let totalBytes = ByteCountFormatter.string(
      fromByteCount: Int64(clamping: progress.totalBytes), countStyle: .file)
    let role = progress.fileRole.map(historyHumanize) ?? "Current file"
    return
      "\(role): \(progress.completedRecords.formatted()) / \(progress.totalRecords.formatted()) records  •  \(bytes) / \(totalBytes)"
  }

  private func bundleDetail(_ progress: HistoryLoadProgress) -> String {
    let bytes = ByteCountFormatter.string(
      fromByteCount: Int64(clamping: progress.bundleByteCount), countStyle: .file)
    return "Bundle data: \(bytes) across \(progress.bundleRecordCount.formatted()) records"
  }
}

private struct HistoryLibraryView: View {
  @Bindable var model: HistoryBrowserModel

  var body: some View {
    NavigationSplitView {
      sidebar
        .navigationSplitViewColumnWidth(min: 170, ideal: 205, max: 260)
    } content: {
      contentColumn
        .navigationSplitViewColumnWidth(min: 280, ideal: 340, max: 450)
    } detail: {
      detailColumn
    }
    .sheet(isPresented: $model.showsCoverageDetails) {
      if let context = model.session?.manifest.context {
        CoverageDetailsView(context: context)
          .frame(minWidth: 560, minHeight: 510)
      }
    }
    .sheet(item: $model.pendingMediaRequest) { request in
      LiveMediaAccessView(model: model, request: request)
        .frame(minWidth: 760, minHeight: 620)
    }
  }

  private var sidebar: some View {
    List(selection: $model.selectedSection) {
      Section("Library") {
        ForEach(HistorySection.allCases) { section in
          Label(section.title, systemImage: section.systemImage)
            .tag(section)
        }
      }
      if let session = model.session {
        Section("Source") {
          LabeledContent("Chats", value: session.conversations.count.formatted())
          LabeledContent("Messages", value: session.manifest.exportedMessageCount.formatted())
          HStack {
            Circle()
              .fill(session.manifest.context.sourceCoverageComplete ? .green : .orange)
              .frame(width: 7, height: 7)
            Text(
              session.manifest.context.sourceCoverageComplete
                ? "Complete coverage" : "Partial coverage"
            )
            .font(.caption)
          }
        }
      }
    }
    .listStyle(.sidebar)
    .safeAreaInset(edge: .bottom) {
      Button {
        model.chooseBundle()
      } label: {
        Label("Open Another…", systemImage: "folder")
      }
      .buttonStyle(.plain)
      .foregroundStyle(.secondary)
      .padding(12)
      .frame(maxWidth: .infinity, alignment: .leading)
      .background(.bar)
    }
  }

  @ViewBuilder
  private var contentColumn: some View {
    switch model.selectedSection ?? .overview {
    case .overview:
      OverviewContents(model: model)
    case .chats:
      ConversationListView(model: model)
    case .contacts:
      ContactListView(model: model)
    case .search:
      SearchResultsView(model: model)
    }
  }

  @ViewBuilder
  private var detailColumn: some View {
    switch model.selectedSection ?? .overview {
    case .overview:
      HistoryOverviewView(model: model)
    case .chats:
      if let conversation = model.selectedConversation {
        ConversationTimelineView(model: model, conversation: conversation)
      } else {
        DetailPlaceholder(
          icon: "bubble.left.and.bubble.right",
          title: "Choose a chat",
          detail: "Select a conversation to browse its normalized history."
        )
      }
    case .contacts:
      if let contact = model.selectedContact {
        ContactDetailView(model: model, contact: contact)
      } else {
        DetailPlaceholder(
          icon: "person.crop.circle",
          title: "Choose a contact",
          detail: "Select a person to see their names, roles, and shared chats."
        )
      }
    case .search:
      DetailPlaceholder(
        icon: "text.magnifyingglass",
        title: "Search all authorized messages",
        detail: "Choose a result to open it with surrounding conversation context."
      )
    }
  }
}

private struct OverviewContents: View {
  let model: HistoryBrowserModel

  var body: some View {
    List {
      if let manifest = model.session?.manifest {
        Section("Bundle") {
          Label("Verified and indexed", systemImage: "checkmark.seal.fill")
            .foregroundStyle(.green)
          LabeledContent(
            "Created",
            value: manifest.createdAt.formatted(date: .abbreviated, time: .shortened))
          LabeledContent(
            "Destination", value: manifest.destination == "local" ? "Local" : "Remote model")
          LabeledContent("Checkpoint", value: shortHistoryID(manifest.context.checkpointRevision))
        }
        Section("Data") {
          LabeledContent("Conversations", value: manifest.enabledConversationCount.formatted())
          LabeledContent("Contacts", value: manifest.exportedContactCount.formatted())
          LabeledContent("Messages", value: manifest.exportedMessageCount.formatted())
          LabeledContent("Media records", value: manifest.exportedArtifactCount.formatted())
        }
      }
    }
    .navigationTitle("Overview")
  }
}

private struct ConversationListView: View {
  @Bindable var model: HistoryBrowserModel

  var body: some View {
    VStack(spacing: 0) {
      HistoryFilterField(text: $model.conversationFilter, prompt: "Filter chats")
      List(selection: $model.selectedConversationID) {
        ForEach(model.filteredConversations) { conversation in
          ConversationRow(
            conversation: conversation,
            statistics: model.session?.conversationStatistics[conversation.conversationID]
          )
          .tag(conversation.conversationID)
        }
      }
      .overlay {
        if model.filteredConversations.isEmpty {
          ContentUnavailableView.search(text: model.conversationFilter)
        }
      }
    }
    .navigationTitle("Chats")
    .onChange(of: model.selectedConversationID) { _, value in
      if let value { model.selectConversation(value) }
    }
  }
}

private struct ConversationRow: View {
  let conversation: HistoryConversation
  let statistics: HistoryConversationStatistics?

  var body: some View {
    HStack(spacing: 11) {
      ZStack {
        Circle()
          .fill(iconColor.opacity(0.16))
          .frame(width: 38, height: 38)
        Image(systemName: icon)
          .foregroundStyle(iconColor)
      }
      VStack(alignment: .leading, spacing: 4) {
        HStack {
          Text(conversation.humanLabel)
            .font(.body.weight(.medium))
            .lineLimit(1)
          Spacer()
          if let date = statistics?.latestMessageDate {
            Text(date, format: .dateTime.month().day())
              .font(.caption2)
              .foregroundStyle(.tertiary)
          }
        }
        HStack(spacing: 6) {
          Text("\(statistics?.messageCount ?? 0) messages")
          Text("•")
          Text("\(conversation.participantCount) people")
          if historyIsStale(conversation.sourceDatabaseFreshness) {
            Image(systemName: "clock.badge.exclamationmark")
              .foregroundStyle(.orange)
              .help("Contains records preserved from an unavailable database")
          }
        }
        .font(.caption)
        .foregroundStyle(.secondary)
      }
    }
    .padding(.vertical, 4)
  }

  private var icon: String {
    switch conversation.kind {
    case "group": "person.3.fill"
    case "business": "building.2.fill"
    case "chatbot": "cpu.fill"
    case "system": "gearshape.fill"
    default: "person.fill"
    }
  }

  private var iconColor: Color {
    conversation.kind == "group" ? .blue : .green
  }
}

private struct ContactListView: View {
  @Bindable var model: HistoryBrowserModel

  var body: some View {
    VStack(spacing: 0) {
      HistoryFilterField(text: $model.contactFilter, prompt: "Filter contacts")
      List(selection: $model.selectedContactID) {
        ForEach(model.filteredContacts) { contact in
          HStack(spacing: 10) {
            HistoryAvatarView(name: contact.displayName)
            VStack(alignment: .leading, spacing: 3) {
              Text(contact.displayName)
                .lineLimit(1)
              Text("\(contact.enabledConversationIDs.count) shared chats")
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            Spacer()
            if historyIsStale(contact.sourceDatabaseFreshness) {
              Image(systemName: "clock.badge.exclamationmark")
                .foregroundStyle(.orange)
            }
          }
          .tag(contact.participantID)
        }
      }
      .overlay {
        if model.filteredContacts.isEmpty {
          ContentUnavailableView.search(text: model.contactFilter)
        }
      }
    }
    .navigationTitle("Contacts")
    .onChange(of: model.selectedContactID) { _, value in
      if let value { model.selectContact(value) }
    }
  }
}

private struct SearchResultsView: View {
  @Bindable var model: HistoryBrowserModel

  var body: some View {
    VStack(spacing: 0) {
      HistoryFilterField(text: $model.searchQuery, prompt: "Search message text")
      if model.isSearching {
        ProgressView()
          .controlSize(.small)
          .padding(.top, 8)
      } else if model.searchResults.count == 150 {
        Text("Showing the first 150 ranked matches. Refine the query to narrow the result set.")
          .font(.caption)
          .foregroundStyle(.secondary)
          .padding(.horizontal, 10)
          .padding(.bottom, 4)
      }
      if let error = model.searchError {
        Text(error)
          .font(.caption)
          .foregroundStyle(.red)
          .padding(8)
      }
      List(model.searchResults) { message in
        Button {
          model.openSearchResult(message)
        } label: {
          VStack(alignment: .leading, spacing: 5) {
            HStack {
              Text(message.conversationLabel)
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
              Spacer()
              if let date = message.createdAt {
                Text(date, format: .dateTime.year().month().day())
                  .font(.caption2)
                  .foregroundStyle(.tertiary)
              }
            }
            Text(message.displayText)
              .foregroundStyle(.primary)
              .lineLimit(3)
            if let sender = message.senderDisplayName {
              Text(sender)
                .font(.caption)
                .foregroundStyle(.secondary)
            }
          }
          .padding(.vertical, 4)
        }
        .buttonStyle(.plain)
      }
      .overlay {
        if model.searchQuery.isEmpty {
          ContentUnavailableView(
            "Search history",
            systemImage: "text.magnifyingglass",
            description: Text("Search normalized text, sender names, and conversation labels."))
        } else if !model.isSearching && model.searchResults.isEmpty && model.searchError == nil {
          ContentUnavailableView.search(text: model.searchQuery)
        }
      }
    }
    .navigationTitle("Search")
    .task(id: model.searchQuery) {
      await model.performSearch()
    }
  }
}
