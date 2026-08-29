import AppKit
import GreenBubblesHistory
import SwiftUI

struct HistoryDirectConnectionSheet: View {
  @Bindable var model: HistoryBrowserModel
  @Environment(\.dismiss) private var dismiss
  @State private var secret = ""
  @State private var validationError: String?

  var body: some View {
    VStack(alignment: .leading, spacing: 18) {
      VStack(alignment: .leading, spacing: 5) {
        Text("Browse SQLite history directly")
          .font(.title2.weight(.semibold))
        Text(
          "GreenBubbles runs bounded, read-only queries and returns only the current JSON page. It does not restore the full message archive."
        )
        .foregroundStyle(.secondary)
      }

      Form {
        Section("Local query tool") {
          pathField(
            title: "CLI",
            text: $model.liveExecutablePath,
            actionTitle: "Choose…",
            action: chooseExecutable
          )
        }

        Section("SQLite source") {
          Picker("Access", selection: $model.directAccessMode) {
            ForEach(HistoryDirectAccessMode.allCases, id: \.rawValue) { mode in
              Text(mode.displayName).tag(mode)
            }
          }
          pathField(
            title: "Directory",
            text: $model.directSourcePath,
            actionTitle: "Choose…",
            action: chooseSource
          )
        }

        Section("Authorization") {
          if model.directAccessMode == .decrypted {
            Label(
              "No key is sent. Use this only for an explicitly decrypted SQLite source.",
              systemImage: "exclamationmark.shield"
            )
            .foregroundStyle(.orange)
          } else if model.directAccessMode == .snapshotKeychain {
            Label(
              "The app will retrieve this snapshot's random local unlock credential from macOS Keychain for this session.",
              systemImage: "key.fill"
            )
            Text(
              "Keychain stores only the independent convenience credential—not the snapshot database key, WeChat key, or 24 recovery words. A private temporary file is created only while this history source is open."
            )
            .font(.caption)
            .foregroundStyle(.secondary)
            Label(
              "If this Mac or Keychain entry is lost, the separately stored 24 recovery words still recover the snapshot.",
              systemImage: "externaldrive.badge.checkmark"
            )
            .font(.caption)
            .foregroundStyle(.green)
          } else if model.directAccessMode == .snapshotLocalCredential {
            pathField(
              title: "Local unlock",
              text: $model.directLocalCredentialPath,
              actionTitle: "Choose…",
              action: chooseLocalCredential
            )
            Text(
              "This owner-only local file wraps the snapshot key without containing either that key or the 24 recovery words. Its path—not its contents—may be remembered by the app."
            )
            .font(.caption)
            .foregroundStyle(.secondary)
            Label(
              "This file is convenient access, not a backup. If it is deleted, reopen the snapshot with the separately stored recovery words.",
              systemImage: "externaldrive.badge.exclamationmark"
            )
            .font(.caption)
            .foregroundStyle(.orange)
          } else if model.directAccessMode == .snapshotRecoveryKit {
            pathField(
              title: "Recovery kit",
              text: $model.directRecoveryKitPath,
              actionTitle: "Choose…",
              action: chooseRecoveryKit
            )
            Text(
              "The owner-only file is read locally by the CLI. Its 24 words unwrap the snapshot key; the words and key are never placed in process arguments or app preferences."
            )
            .font(.caption)
            .foregroundStyle(.secondary)
            Label(
              "Keep another copy of the 24 words somewhere independent. This local file is convenient access, not the only backup.",
              systemImage: "exclamationmark.triangle"
            )
            .font(.caption)
            .foregroundStyle(.orange)
          } else {
            SecureField(secretTitle, text: $secret)
              .textContentType(.password)
            Text(secretExplanation)
              .font(.caption)
              .foregroundStyle(.secondary)
          }
        }
      }
      .formStyle(.grouped)

      if let validationError {
        Label(validationError, systemImage: "exclamationmark.triangle.fill")
          .font(.callout)
          .foregroundStyle(.red)
      }

      GroupBox {
        Label(
          "Recoverable snapshots are encrypted directly under an independent recovery key. The live WeChat key is neither copied into the snapshot nor required to verify or query it later.",
          systemImage: "key.horizontal.fill"
        )
        .font(.callout)
        .foregroundStyle(.secondary)
      }

      HStack {
        Button("Cancel") { dismiss() }
          .keyboardShortcut(.cancelAction)
        Spacer()
        Button("Connect", action: connect)
          .buttonStyle(.borderedProminent)
          .keyboardShortcut(.defaultAction)
      }
    }
    .padding(24)
    .onChange(of: model.directAccessMode) { _, mode in
      if mode == .decrypted || mode == .snapshotRecoveryKit
        || mode == .snapshotLocalCredential || mode == .snapshotKeychain
      {
        secret = ""
      }
      validationError = nil
    }
  }

  private var secretTitle: String {
    switch model.directAccessMode {
    case .liveEncrypted: "WeChat database key"
    case .snapshotKeychain: ""
    case .snapshotLocalCredential: ""
    case .snapshotPassphrase: "Snapshot passphrase"
    case .snapshotRecoveryKit: ""
    case .snapshotEncrypted: "Independent snapshot recovery key"
    case .decrypted: ""
    }
  }

  private var secretExplanation: String {
    switch model.directAccessMode {
    case .liveEncrypted:
      "Accepted only through standard input for this connection; it is never placed in process arguments."
    case .snapshotKeychain:
      ""
    case .snapshotLocalCredential:
      ""
    case .snapshotPassphrase:
      "Derived with Argon2id (64 MiB, three passes) and accepted only through standard input. It is retained only in memory while this source is open."
    case .snapshotRecoveryKit:
      ""
    case .snapshotEncrypted:
      "Legacy compatibility only. Use the 32-byte snapshot key or its 64-character hexadecimal form—not the WeChat key."
    case .decrypted:
      ""
    }
  }

  private func pathField(
    title: String,
    text: Binding<String>,
    actionTitle: String,
    action: @escaping () -> Void
  ) -> some View {
    HStack {
      TextField(title, text: text)
        .textFieldStyle(.roundedBorder)
      Button(actionTitle, action: action)
    }
  }

  private func chooseExecutable() {
    let panel = NSOpenPanel()
    panel.title = "Choose greenbubbles"
    panel.prompt = "Choose CLI"
    panel.canChooseFiles = true
    panel.canChooseDirectories = false
    panel.allowsMultipleSelection = false
    panel.resolvesAliases = false
    if panel.runModal() == .OK, let url = panel.url {
      model.liveExecutablePath = url.path
    }
  }

  private func chooseSource() {
    let panel = NSOpenPanel()
    panel.title = "Choose a live account root or recoverable snapshot"
    panel.prompt = "Choose Source"
    panel.canChooseFiles = false
    panel.canChooseDirectories = true
    panel.allowsMultipleSelection = false
    panel.resolvesAliases = false
    if panel.runModal() == .OK, let url = panel.url {
      model.directSourcePath = url.path
    }
  }

  private func chooseRecoveryKit() {
    let panel = NSOpenPanel()
    panel.title = "Choose a private 24-word snapshot recovery kit"
    panel.prompt = "Choose Recovery Kit"
    panel.canChooseFiles = true
    panel.canChooseDirectories = false
    panel.allowsMultipleSelection = false
    panel.resolvesAliases = false
    if panel.runModal() == .OK, let url = panel.url {
      model.directRecoveryKitPath = url.path
    }
  }

  private func chooseLocalCredential() {
    let panel = NSOpenPanel()
    panel.title = "Choose a private local snapshot unlock credential"
    panel.prompt = "Choose Local Unlock"
    panel.canChooseFiles = true
    panel.canChooseDirectories = false
    panel.allowsMultipleSelection = false
    panel.resolvesAliases = false
    if panel.runModal() == .OK, let url = panel.url {
      model.directLocalCredentialPath = url.path
    }
  }

  private func connect() {
    let executablePath = model.liveExecutablePath.trimmingCharacters(in: .whitespacesAndNewlines)
    let sourcePath = model.directSourcePath.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !executablePath.isEmpty, !sourcePath.isEmpty else {
      validationError = "Choose both the local CLI and the SQLite source directory."
      return
    }
    let recoveryKitPath = model.directRecoveryKitPath.trimmingCharacters(
      in: .whitespacesAndNewlines
    )
    let localCredentialPath = model.directLocalCredentialPath.trimmingCharacters(
      in: .whitespacesAndNewlines
    )
    if model.directAccessMode == .snapshotLocalCredential, localCredentialPath.isEmpty {
      validationError = "Choose the private local unlock file for this snapshot."
      return
    }
    if model.directAccessMode == .snapshotRecoveryKit, recoveryKitPath.isEmpty {
      validationError = "Choose the private recovery-kit file for this snapshot."
      return
    }
    if model.directAccessMode == .snapshotPassphrase {
      let passphraseBytes = Array(secret.utf8)
      guard (12...1_024).contains(passphraseBytes.count),
        !passphraseBytes.contains(0), !passphraseBytes.contains(10),
        !passphraseBytes.contains(13)
      else {
        validationError =
          "Enter the snapshot passphrase (12–1,024 UTF-8 bytes, without line breaks)."
        return
      }
    }
    if model.directAccessMode == .liveEncrypted || model.directAccessMode == .snapshotEncrypted,
      secret.isEmpty
    {
      validationError = "Enter the key for the selected encrypted source."
      return
    }
    var key =
      model.directAccessMode == .liveEncrypted || model.directAccessMode == .snapshotEncrypted
        || model.directAccessMode == .snapshotPassphrase
      ? Array(secret.utf8) : []
    let recoveryKitURL =
      model.directAccessMode == .snapshotRecoveryKit
      ? URL(fileURLWithPath: recoveryKitPath) : nil
    let localCredentialURL =
      model.directAccessMode == .snapshotLocalCredential
      ? URL(fileURLWithPath: localCredentialPath) : nil
    secret = ""
    defer { key.resetBytes(in: 0..<key.count) }
    model.connectDirectSource(
      executableURL: URL(fileURLWithPath: executablePath),
      sourceURL: URL(fileURLWithPath: sourcePath, isDirectory: true),
      accessMode: model.directAccessMode,
      keyUTF8: key,
      recoveryKitURL: recoveryKitURL,
      localCredentialURL: localCredentialURL
    )
    dismiss()
  }
}

struct HistoryDirectConnectingView: View {
  let cancel: () -> Void

  var body: some View {
    VStack(spacing: 18) {
      ProgressView()
        .controlSize(.large)
      Text("Opening SQLite history…")
        .font(.title2.weight(.semibold))
      Text(
        "Authenticating the source, measuring its SQLite files, and loading the first bounded conversation page."
      )
      .foregroundStyle(.secondary)
      .multilineTextAlignment(.center)
      .frame(maxWidth: 560)
      Label(
        "No database restoration or bulk JSON conversion is running", systemImage: "bolt.shield"
      )
      .font(.callout)
      .foregroundStyle(.green)
      Button("Cancel", action: cancel)
        .keyboardShortcut(.cancelAction)
    }
    .padding(40)
  }
}

struct HistoryDirectLibraryView: View {
  @Bindable var model: HistoryBrowserModel

  var body: some View {
    NavigationSplitView {
      sidebar
        .navigationSplitViewColumnWidth(min: 180, ideal: 220, max: 280)
    } content: {
      content
        .navigationSplitViewColumnWidth(min: 300, ideal: 380, max: 500)
    } detail: {
      detail
    }
  }

  private var sidebar: some View {
    List(selection: $model.directSection) {
      Section("Direct source") {
        ForEach(HistoryDirectSection.allCases) { section in
          Label(section.title, systemImage: section.systemImage)
            .tag(section)
        }
      }
      if let status = model.directStatus {
        Section("Storage") {
          LabeledContent("Databases", value: status.databaseCount.formatted())
          LabeledContent("SQLite files", value: directByteCount(status.databaseBytes))
          LabeledContent("WAL", value: directByteCount(status.writeAheadLogBytes))
          LabeledContent("Total", value: directByteCount(status.totalSqliteStorageBytes))
        }
      }
    }
    .listStyle(.sidebar)
    .safeAreaInset(edge: .bottom) {
      Button {
        model.presentDirectConnection()
      } label: {
        Label("Open Another…", systemImage: "externaldrive")
      }
      .buttonStyle(.plain)
      .foregroundStyle(.secondary)
      .padding(12)
      .frame(maxWidth: .infinity, alignment: .leading)
      .background(.bar)
    }
  }

  @ViewBuilder
  private var content: some View {
    switch model.directSection ?? .overview {
    case .overview:
      HistoryDirectOverviewContents(model: model)
    case .chats:
      HistoryDirectConversationList(model: model)
    case .search:
      HistoryDirectSearchView(model: model)
    }
  }

  @ViewBuilder
  private var detail: some View {
    switch model.directSection ?? .overview {
    case .overview:
      HistoryDirectOverviewDetail(model: model)
    case .chats:
      if let conversationID = model.directSelectedConversationID {
        HistoryDirectTimelineView(model: model, conversationID: conversationID)
      } else {
        HistoryDirectPlaceholder(
          icon: "bubble.left.and.bubble.right",
          title: "Choose a chat",
          detail: "Only its current bounded page will be requested from SQLite."
        )
      }
    case .search:
      HistoryDirectPlaceholder(
        icon: "text.magnifyingglass",
        title: "Bounded native search",
        detail: "Select a result to request that exact message by its opaque identifier."
      )
    }
  }
}

private struct HistoryDirectOverviewContents: View {
  let model: HistoryBrowserModel

  var body: some View {
    List {
      if let configuration = model.directConfiguration, let status = model.directStatus {
        Section("Access") {
          Label(
            configuration.accessMode.displayName, systemImage: accessIcon(configuration.accessMode))
          LabeledContent("Source identity", value: String(status.source.identity.prefix(16)))
          LabeledContent(
            "Observed",
            value: Date(
              timeIntervalSince1970: Double(status.observedAtUnixMilliseconds) / 1_000
            ).formatted(date: .abbreviated, time: .shortened)
          )
        }
        Section("Bounded state") {
          LabeledContent("Loaded chats", value: model.directConversations.count.formatted())
          LabeledContent(
            "More chats",
            value: model.directConversationCursor == nil ? "No" : "Available"
          )
          Label("Read-only SQLite statements", systemImage: "lock.open.display")
            .foregroundStyle(.green)
        }
      }
      HistoryDirectWarnings(warnings: model.directConversationWarnings)
    }
    .navigationTitle("Overview")
  }

  private func accessIcon(_ mode: HistoryDirectAccessMode) -> String {
    switch mode {
    case .liveEncrypted: "waveform.path.ecg"
    case .snapshotKeychain: "key.fill"
    case .snapshotLocalCredential: "lock.laptopcomputer"
    case .snapshotPassphrase: "ellipsis.rectangle"
    case .snapshotRecoveryKit: "key.viewfinder"
    case .snapshotEncrypted: "externaldrive.badge.checkmark"
    case .decrypted: "exclamationmark.lock.open"
    }
  }
}

private struct HistoryDirectOverviewDetail: View {
  let model: HistoryBrowserModel

  var body: some View {
    ScrollView {
      VStack(alignment: .leading, spacing: 20) {
        VStack(alignment: .leading, spacing: 6) {
          Text("Direct SQLite source")
            .font(.largeTitle.weight(.semibold))
          Text(
            "The database remains the canonical store. GreenBubbles requests small JSON pages and discards them as you close the source."
          )
          .font(.title3)
          .foregroundStyle(.secondary)
        }

        if let consistency = model.directConversationConsistency {
          GroupBox("Current query consistency") {
            VStack(alignment: .leading, spacing: 8) {
              LabeledContent("Guarantee", value: directHumanize(consistency.guarantee))
              LabeledContent("Databases read", value: consistency.databaseCount.formatted())
              LabeledContent(
                "Cross-database atomic",
                value: consistency.crossDatabaseAtomic ? "Yes" : "No"
              )
              if !consistency.crossDatabaseAtomic {
                Label(
                  "WeChat databases are independent SQLite files; this page is not a single transaction across all of them.",
                  systemImage: "info.circle"
                )
                .font(.callout)
                .foregroundStyle(.orange)
              }
            }
            .padding(6)
          }
        }

        if let status = model.directStatus {
          GroupBox("Measured SQLite storage") {
            VStack(alignment: .leading, spacing: 8) {
              LabeledContent("Database files", value: directByteCount(status.databaseBytes))
              LabeledContent("Write-ahead logs", value: directByteCount(status.writeAheadLogBytes))
              LabeledContent("Shared memory", value: directByteCount(status.sharedMemoryBytes))
              LabeledContent(
                "Rollback journals",
                value: directByteCount(status.rollbackJournalBytes)
              )
              Divider()
              LabeledContent(
                "Total on disk",
                value: directByteCount(status.totalSqliteStorageBytes)
              )
              Text(
                "These are the source database and SQLite sidecar bytes only; no JSONL, staging database, or decoded-media expansion is included."
              )
              .font(.caption)
              .foregroundStyle(.secondary)
            }
            .padding(6)
          }

          GroupBox("Largest databases") {
            VStack(alignment: .leading, spacing: 9) {
              ForEach(largestEntries(status)) { entry in
                HStack {
                  Text(entry.relativePath)
                    .lineLimit(1)
                    .truncationMode(.middle)
                  Spacer()
                  Text(directByteCount(directEntryBytes(entry)))
                    .monospacedDigit()
                    .foregroundStyle(.secondary)
                }
              }
            }
            .padding(6)
          }
        }

        if let mode = model.directConfiguration?.accessMode,
          mode == .snapshotKeychain || mode == .snapshotLocalCredential
            || mode == .snapshotPassphrase || mode == .snapshotRecoveryKit
            || mode == .snapshotEncrypted
        {
          GroupBox("Independent recovery") {
            Label(
              independentRecoveryExplanation(mode),
              systemImage: "key.radiowaves.forward.fill"
            )
            .padding(6)
          }
        }
      }
      .padding(28)
      .frame(maxWidth: 940, alignment: .leading)
    }
  }

  private func largestEntries(_ status: HistoryDirectSourceStatus) -> [HistoryDirectDatabaseSize] {
    Array(status.entries.sorted { directEntryBytes($0) > directEntryBytes($1) }.prefix(12))
  }

  private func independentRecoveryExplanation(_ mode: HistoryDirectAccessMode) -> String {
    switch mode {
    case .snapshotKeychain:
      "This snapshot is using a convenience credential from macOS Keychain. Losing that device-only entry does not destroy the backup: the separately stored 24 recovery words still unlock it."
    case .snapshotLocalCredential:
      "This snapshot is using its local convenience protector. Deleting that owner-only file does not destroy the backup: the separately stored 24 recovery words can still unlock it."
    case .snapshotPassphrase:
      "This snapshot is using its optional Argon2id passphrase protector. The mandatory, separately stored 24 recovery words remain the portable recovery path if the passphrase is forgotten."
    case .snapshotRecoveryKit:
      "This snapshot is unlocked by its portable 24-word recovery kit without consulting WeChat. Keep another copy of those words independent from this Mac."
    case .snapshotEncrypted:
      "This legacy snapshot is queried using its raw recovery key without consulting WeChat or the original WeChat database key."
    case .liveEncrypted, .decrypted:
      ""
    }
  }
}

private struct HistoryDirectConversationList: View {
  @Bindable var model: HistoryBrowserModel

  var body: some View {
    VStack(spacing: 0) {
      DirectFilterField(text: $model.conversationFilter, prompt: "Filter loaded chats")
      if let error = model.directConversationError {
        DirectInlineError(message: error)
      }
      List {
        ForEach(model.filteredDirectConversations) { conversation in
          Button {
            model.selectDirectConversation(conversation.id)
          } label: {
            HStack(spacing: 10) {
              Image(systemName: "bubble.left.and.bubble.right.fill")
                .foregroundStyle(.green)
                .frame(width: 30)
              VStack(alignment: .leading, spacing: 4) {
                HStack {
                  Text(conversation.displayName)
                    .font(.body.weight(.medium))
                    .lineLimit(1)
                  Spacer()
                  if let date = conversation.sortDate {
                    Text(date, format: .dateTime.month().day())
                      .font(.caption2)
                      .foregroundStyle(.tertiary)
                  }
                }
                Text(conversation.summary ?? "No decoded summary")
                  .font(.caption)
                  .foregroundStyle(.secondary)
                  .lineLimit(2)
              }
            }
            .padding(.vertical, 4)
            .contentShape(Rectangle())
          }
          .buttonStyle(.plain)
          .listRowBackground(
            model.directSelectedConversationID == conversation.id
              ? Color.accentColor.opacity(0.12) : Color.clear
          )
        }

        if model.directConversationCursor != nil {
          Button {
            model.loadMoreDirectConversations()
          } label: {
            HStack {
              Spacer()
              if model.isLoadingDirectConversations {
                ProgressView().controlSize(.small)
              } else {
                Label("Load next 100 chats", systemImage: "arrow.down.circle")
              }
              Spacer()
            }
          }
          .disabled(model.isLoadingDirectConversations)
        }
      }
      .overlay {
        if model.filteredDirectConversations.isEmpty && !model.isLoadingDirectConversations {
          ContentUnavailableView(
            "No loaded chats",
            systemImage: "bubble.left",
            description: Text("Adjust the filter or load another bounded page.")
          )
        }
      }
    }
    .navigationTitle("Chats")
  }
}

private struct HistoryDirectTimelineView: View {
  @Bindable var model: HistoryBrowserModel
  let conversationID: String

  var body: some View {
    VStack(spacing: 0) {
      HStack {
        VStack(alignment: .leading, spacing: 3) {
          Text(model.selectedDirectConversation?.displayName ?? conversationID)
            .font(.title2.weight(.semibold))
          Text("Bounded, newest-first SQLite pages")
            .font(.caption)
            .foregroundStyle(.secondary)
        }
        Spacer()
        if let consistency = model.directMessageConsistency {
          Label(
            consistency.crossDatabaseAtomic ? "Atomic page" : "Independent DB reads",
            systemImage: consistency.crossDatabaseAtomic ? "checkmark.shield" : "square.stack.3d.up"
          )
          .font(.caption)
          .foregroundStyle(consistency.crossDatabaseAtomic ? .green : .orange)
        }
      }
      .padding(.horizontal, 18)
      .padding(.vertical, 12)
      .background(.bar)

      if let error = model.directMessageError {
        DirectInlineError(message: error)
      }
      if !model.directMessageWarnings.isEmpty {
        HistoryDirectWarnings(warnings: model.directMessageWarnings)
          .padding(.horizontal, 16)
          .padding(.top, 10)
      }

      ScrollView {
        LazyVStack(spacing: 10) {
          if model.directIsSearchContext {
            Button("Return to latest messages") {
              model.returnToLatestDirectMessages()
            }
            .buttonStyle(.bordered)
          } else if model.directMessageCursor != nil {
            Button {
              model.loadOlderDirectMessages()
            } label: {
              if model.isLoadingDirectMessages {
                ProgressView().controlSize(.small)
              } else {
                Label("Load 100 older messages", systemImage: "arrow.up.circle")
              }
            }
            .buttonStyle(.bordered)
          }

          ForEach(Array(model.directMessages.reversed())) { message in
            HistoryDirectMessageBubble(
              message: message,
              highlighted: message.id == model.directHighlightedMessageID
            )
          }

          if model.isLoadingDirectMessages && model.directMessages.isEmpty {
            ProgressView("Requesting bounded page…")
              .padding(40)
          } else if !model.isLoadingDirectMessages && model.directMessages.isEmpty
            && model.directMessageError == nil
          {
            ContentUnavailableView(
              "No messages",
              systemImage: "bubble.left",
              description: Text("This bounded query returned no messages.")
            )
            .padding(40)
          }
        }
        .padding(18)
      }
    }
    .background(Color(nsColor: .textBackgroundColor).opacity(0.45))
  }
}

private struct HistoryDirectMessageBubble: View {
  let message: HistoryDirectMessage
  let highlighted: Bool

  var body: some View {
    HStack {
      VStack(alignment: .leading, spacing: 7) {
        HStack {
          Text(message.senderLabel)
            .font(.caption.weight(.semibold))
            .foregroundStyle(.secondary)
          Spacer()
          Text(message.createdAt, format: .dateTime.year().month().day().hour().minute())
            .font(.caption2.monospacedDigit())
            .foregroundStyle(.tertiary)
        }
        Text(message.displayText)
          .textSelection(.enabled)
        HStack(spacing: 6) {
          Text(directHumanize(message.messageTypeLabel))
          if message.messageSubtype != 0 {
            Text("•")
            Text(directHumanize(message.messageSubtypeLabel))
          }
          if message.contentTruncated {
            Text("• truncated")
              .foregroundStyle(.orange)
          }
        }
        .font(.caption2)
        .foregroundStyle(.secondary)
      }
      .padding(.horizontal, 13)
      .padding(.vertical, 10)
      .frame(maxWidth: 680, alignment: .leading)
      .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 14))
      .overlay {
        RoundedRectangle(cornerRadius: 14)
          .stroke(highlighted ? Color.accentColor : .clear, lineWidth: 3)
      }
      .contextMenu {
        Button("Copy Message Text") {
          NSPasteboard.general.clearContents()
          NSPasteboard.general.setString(message.displayText, forType: .string)
        }
        Button("Copy Opaque Message ID") {
          NSPasteboard.general.clearContents()
          NSPasteboard.general.setString(message.id, forType: .string)
        }
      }
      Spacer(minLength: 70)
    }
  }
}

private struct HistoryDirectSearchView: View {
  @Bindable var model: HistoryBrowserModel

  var body: some View {
    VStack(spacing: 0) {
      DirectFilterField(text: $model.directSearchQuery, prompt: "Search message text")
      if model.isSearchingDirect && model.directSearchResults.isEmpty {
        ProgressView()
          .controlSize(.small)
          .padding(.top, 8)
      }
      if let error = model.directSearchError {
        DirectInlineError(message: error)
      }
      if !model.directSearchWarnings.isEmpty {
        HistoryDirectWarnings(warnings: model.directSearchWarnings)
          .padding(.horizontal, 10)
          .padding(.bottom, 5)
      }
      List {
        ForEach(model.directSearchResults) { hit in
          Button {
            model.openDirectSearchResult(hit)
          } label: {
            VStack(alignment: .leading, spacing: 5) {
              HStack {
                Text(hit.conversationID)
                  .font(.caption.weight(.semibold))
                  .foregroundStyle(.secondary)
                  .lineLimit(1)
                Spacer()
                Text(hit.createdAt, format: .dateTime.year().month().day())
                  .font(.caption2)
                  .foregroundStyle(.tertiary)
              }
              Text(hit.snippet)
                .foregroundStyle(.primary)
                .lineLimit(3)
              Text(
                hit.senderLabel.isEmpty
                  ? directHumanize(hit.messageTypeLabel) : hit.senderLabel
              )
              .font(.caption)
              .foregroundStyle(.secondary)
            }
            .padding(.vertical, 4)
          }
          .buttonStyle(.plain)
        }
        if model.directSearchCursor != nil {
          Button {
            model.loadMoreDirectSearchResults()
          } label: {
            HStack {
              Spacer()
              if model.isSearchingDirect {
                ProgressView().controlSize(.small)
              } else {
                Label("Continue bounded search", systemImage: "arrow.down.circle")
              }
              Spacer()
            }
          }
          .disabled(model.isSearchingDirect)
        }
      }
      .overlay {
        if model.directSearchQuery.isEmpty {
          ContentUnavailableView(
            "Search SQLite history",
            systemImage: "text.magnifyingglass",
            description: Text(
              "The query is sent through standard input to native FTS or bounded read-only source windows."
            )
          )
        } else if !model.isSearchingDirect && model.directSearchResults.isEmpty
          && model.directSearchError == nil && model.directSearchCursor == nil
        {
          ContentUnavailableView.search(text: model.directSearchQuery)
        }
      }
    }
    .navigationTitle("Search")
    .task(id: model.directSearchQuery) {
      await model.performDirectSearch()
    }
  }
}

private struct HistoryDirectWarnings: View {
  let warnings: [HistoryDirectWarning]

  var body: some View {
    if !warnings.isEmpty {
      Section("Warnings") {
        ForEach(warnings) { warning in
          Label {
            VStack(alignment: .leading, spacing: 2) {
              Text(warning.message)
              if let count = warning.count {
                Text("Affected records: \(count.formatted())")
                  .font(.caption2)
                  .foregroundStyle(.secondary)
              }
            }
          } icon: {
            Image(systemName: "exclamationmark.triangle.fill")
              .foregroundStyle(.orange)
          }
        }
      }
    }
  }
}

private struct DirectFilterField: View {
  @Binding var text: String
  let prompt: String

  var body: some View {
    HStack {
      Image(systemName: "magnifyingglass")
        .foregroundStyle(.secondary)
      TextField(prompt, text: $text)
        .textFieldStyle(.plain)
      if !text.isEmpty {
        Button {
          text = ""
        } label: {
          Image(systemName: "xmark.circle.fill")
            .foregroundStyle(.tertiary)
        }
        .buttonStyle(.plain)
      }
    }
    .padding(.horizontal, 10)
    .padding(.vertical, 8)
    .background(.bar)
  }
}

private struct DirectInlineError: View {
  let message: String

  var body: some View {
    Label(message, systemImage: "exclamationmark.triangle.fill")
      .font(.caption)
      .foregroundStyle(.red)
      .textSelection(.enabled)
      .padding(9)
      .frame(maxWidth: .infinity, alignment: .leading)
      .background(.red.opacity(0.07))
  }
}

private struct HistoryDirectPlaceholder: View {
  let icon: String
  let title: String
  let detail: String

  var body: some View {
    ContentUnavailableView(
      title,
      systemImage: icon,
      description: Text(detail)
    )
  }
}

private func directByteCount(_ bytes: UInt64) -> String {
  ByteCountFormatter.string(fromByteCount: Int64(clamping: bytes), countStyle: .file)
}

private func directEntryBytes(_ entry: HistoryDirectDatabaseSize) -> UInt64 {
  [
    entry.databaseBytes,
    entry.writeAheadLogBytes,
    entry.sharedMemoryBytes,
    entry.rollbackJournalBytes,
  ].reduce(0) { partial, value in
    let (sum, overflow) = partial.addingReportingOverflow(value)
    return overflow ? UInt64.max : sum
  }
}

private func directHumanize(_ value: String) -> String {
  guard !value.isEmpty else { return "Unknown" }
  var result = ""
  for character in value {
    if character.isUppercase, !result.isEmpty { result.append(" ") }
    result.append(character)
  }
  return result.replacingOccurrences(of: "_", with: " ").capitalized
}
