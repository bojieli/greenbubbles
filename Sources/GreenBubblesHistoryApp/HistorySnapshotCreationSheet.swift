import AppKit
import Darwin
import Foundation
import GreenBubblesHistory
import SwiftUI

private enum SnapshotCreationPhase {
  case setup
  case generatingRecoveryKit
  case confirmRecoveryWords
  case creatingSnapshot
  case completed
  case failed
}

private enum SnapshotConvenienceMode: String, CaseIterable, Identifiable {
  case keychain
  case hiddenFile
  case none

  var id: String { rawValue }

  var title: String {
    switch self {
    case .keychain: "macOS Keychain"
    case .hiddenFile: "Owner-only hidden file"
    case .none: "None"
    }
  }
}

@MainActor
struct HistorySnapshotCreationSheet: View {
  @Bindable var model: HistoryBrowserModel
  @Environment(\.dismiss) private var dismiss

  @State private var phase: SnapshotCreationPhase = .setup
  @State private var outputPath = ""
  @State private var recoveryKitPath = ""
  @State private var hiddenCredentialPath = ""
  @State private var sourceAccess: HistorySnapshotSourceAccess = .encryptedWeChat
  @State private var stableCapture = false
  @State private var sourceKey = ""
  @State private var addsPassphrase = false
  @State private var snapshotPassphrase = ""
  @State private var snapshotPassphraseConfirmation = ""
  @State private var convenienceMode: SnapshotConvenienceMode = .keychain
  @State private var recoveryKit: HistorySnapshotRecoveryKit?
  @State private var challenge: HistorySnapshotWordChallenge?
  @State private var challengeResponses: [Int: String] = [:]
  @State private var confirmsIndependentCopy = false
  @State private var result: HistorySnapshotCreationResult?
  @State private var keychainStored = false
  @State private var completionWarning: String?
  @State private var errorMessage: String?
  @State private var operationTask: Task<Void, Never>?
  @State private var transientCredentialDirectory: URL?

  private let client = HistorySnapshotCreationClient()
  private let keychain = SnapshotKeychainStore()

  var body: some View {
    VStack(alignment: .leading, spacing: 18) {
      header
      Divider()
      Group {
        switch phase {
        case .setup:
          setupView
        case .generatingRecoveryKit:
          progressView(
            title: "Creating portable recovery words…",
            detail:
              "The CLI is generating a checksummed 24-word BIP-39 kit in the selected owner-only file."
          )
        case .confirmRecoveryWords:
          recoveryConfirmationView
        case .creatingSnapshot:
          progressView(
            title: "Creating independently encrypted snapshot…",
            detail:
              "SQLite is copying logical pages directly into SQLCipher databases protected by a new random key. No plaintext database staging file is created."
          )
        case .completed:
          completionView
        case .failed:
          failureView
        }
      }
      .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }
    .padding(24)
    .onAppear(perform: prepareSuggestedPaths)
    .onChange(of: sourceAccess) { _, mode in
      if mode == .decryptedSQLite { sourceKey = "" }
      errorMessage = nil
    }
    .onChange(of: addsPassphrase) { _, enabled in
      if !enabled {
        snapshotPassphrase = ""
        snapshotPassphraseConfirmation = ""
      }
      errorMessage = nil
    }
    .onChange(of: convenienceMode) { _, _ in errorMessage = nil }
    .onDisappear {
      guard !isBusy else { return }
      clearDisplayedSecrets()
      removeTransientCredentialDirectory()
    }
    .interactiveDismissDisabled(isBusy)
  }

  private var header: some View {
    VStack(alignment: .leading, spacing: 5) {
      Text("Create a recoverable snapshot")
        .font(.title2.weight(.semibold))
      Text(
        "Convert WeChat SQLite directly into a new encrypted generation whose recovery does not depend on WeChat or this Mac."
      )
      .foregroundStyle(.secondary)
    }
  }

  private var setupView: some View {
    VStack(spacing: 16) {
      Form {
        Section("Local conversion tool") {
          pathField(
            title: "CLI",
            text: $model.liveExecutablePath,
            actionTitle: "Choose…",
            action: chooseExecutable
          )
        }

        Section("Source SQLite") {
          Picker("Source access", selection: $sourceAccess) {
            ForEach(HistorySnapshotSourceAccess.allCases, id: \.rawValue) { mode in
              Text(mode.displayName).tag(mode)
            }
          }
          pathField(
            title: "Source directory",
            text: $model.directSourcePath,
            actionTitle: "Choose…",
            action: chooseSource
          )
          Toggle("Source is a complete stable acquisition capture", isOn: $stableCapture)
          Text(
            stableCapture
              ? "Uses snapshot create-capture and verifies the acquisition manifest and captured hashes before and after conversion."
              : "Uses bounded read-only access to each database in the selected live account root. Each database is individually consistent; multiple WeChat databases are not one global transaction."
          )
          .font(.caption)
          .foregroundStyle(.secondary)
          if sourceAccess == .encryptedWeChat {
            SecureField("WeChat database key", text: $sourceKey)
              .textContentType(.password)
            Text("The key is sent only through standard input and is never stored by the app.")
              .font(.caption)
              .foregroundStyle(.secondary)
          } else {
            Label(
              "No source key is sent. Select this only for an explicitly plaintext SQLite source.",
              systemImage: "exclamationmark.shield"
            )
            .font(.caption)
            .foregroundStyle(.orange)
          }
        }

        Section("New durable files") {
          pathField(
            title: "New snapshot directory",
            text: $outputPath,
            actionTitle: "Choose…",
            action: chooseOutput
          )
          pathField(
            title: "New recovery-kit file",
            text: $recoveryKitPath,
            actionTitle: "Choose…",
            action: chooseRecoveryKitOutput
          )
          Label(
            "The recovery-kit file is created before conversion and is never deleted on cancellation or failure. Keep a second copy offline or in a password manager independent from the snapshot.",
            systemImage: "key.viewfinder"
          )
          .font(.caption)
          .foregroundStyle(.secondary)
        }

        Section("Optional convenience unlock") {
          Picker("Remember on this Mac", selection: $convenienceMode) {
            ForEach(SnapshotConvenienceMode.allCases) { mode in
              Text(mode.title).tag(mode)
            }
          }
          if convenienceMode == .keychain {
            Text(
              "Recommended for routine access. Keychain receives only a separate random local wrapper credential, marked When Unlocked / This Device Only. The 24 words remain mandatory and portable."
            )
            .font(.caption)
            .foregroundStyle(.secondary)
          } else if convenienceMode == .hiddenFile {
            pathField(
              title: "New hidden credential file",
              text: $hiddenCredentialPath,
              actionTitle: "Choose…",
              action: chooseHiddenCredentialOutput
            )
            Text(
              "The dot-prefixed mode-0600 file contains a separate random wrapper credential—not the database key or recovery words. Deleting it only removes convenience access."
            )
            .font(.caption)
            .foregroundStyle(.secondary)
          } else {
            Text("Routine unlock will use the passphrase, if enabled, or the recovery-kit file.")
              .font(.caption)
              .foregroundStyle(.secondary)
          }
        }

        Section("Optional memorized passphrase") {
          Toggle("Add an Argon2id passphrase protector", isOn: $addsPassphrase)
          if addsPassphrase {
            SecureField("Snapshot passphrase", text: $snapshotPassphrase)
              .textContentType(.newPassword)
            SecureField("Confirm snapshot passphrase", text: $snapshotPassphraseConfirmation)
              .textContentType(.newPassword)
            Text(
              "The passphrase is processed with Argon2id (64 MiB, three passes), sent only through standard input, never stored, and never replaces the mandatory 24-word recovery path."
            )
            .font(.caption)
            .foregroundStyle(.secondary)
          }
        }
      }
      .formStyle(.grouped)

      inlineError

      HStack {
        Button("Cancel") {
          clearDisplayedSecrets()
          dismiss()
        }
        .keyboardShortcut(.cancelAction)
        Spacer()
        Button("Create Recovery Words", action: startRecoveryKitCreation)
          .buttonStyle(.borderedProminent)
          .keyboardShortcut(.defaultAction)
      }
    }
  }

  private var recoveryConfirmationView: some View {
    VStack(alignment: .leading, spacing: 16) {
      ScrollView {
        VStack(alignment: .leading, spacing: 18) {
          Label(
            "Write down these 24 words in order before conversion starts",
            systemImage: "exclamationmark.triangle.fill"
          )
          .font(.title3.weight(.semibold))
          .foregroundStyle(.orange)

          Text(
            "They are GreenBubbles recovery words, not a cryptocurrency wallet seed. Never reuse a wallet phrase here and never import this phrase into a wallet. The same words are already saved in the private recovery-kit file shown below."
          )
          .foregroundStyle(.secondary)

          if let recoveryKit {
            Text(recoveryKit.url.path)
              .font(.caption.monospaced())
              .textSelection(.enabled)
              .padding(10)
              .background(.quaternary.opacity(0.4), in: RoundedRectangle(cornerRadius: 8))

            LazyVGrid(
              columns: Array(repeating: GridItem(.flexible(), alignment: .leading), count: 4),
              alignment: .leading,
              spacing: 10
            ) {
              ForEach(Array(recoveryKit.words.enumerated()), id: \.offset) { index, word in
                HStack(spacing: 6) {
                  Text("\(index + 1).")
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
                  Text(word)
                    .fontWeight(.medium)
                }
                .padding(8)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(.green.opacity(0.08), in: RoundedRectangle(cornerRadius: 7))
              }
            }
          }

          Divider()
          Text("Confirm four randomly selected positions")
            .font(.headline)
          if let challenge {
            HStack(alignment: .top, spacing: 12) {
              ForEach(challenge.zeroBasedPositions, id: \.self) { position in
                VStack(alignment: .leading, spacing: 5) {
                  Text("Word \(position + 1)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                  TextField("word", text: challengeBinding(position))
                    .textFieldStyle(.roundedBorder)
                    .frame(minWidth: 120)
                }
              }
            }
          }

          Toggle(
            "I stored an independent copy of all 24 words and understand that Keychain or a hidden file is only a convenience unlock.",
            isOn: $confirmsIndependentCopy
          )
          inlineError
        }
      }

      HStack {
        Button("Cancel — Keep Recovery File") {
          clearDisplayedSecrets()
          dismiss()
        }
        .keyboardShortcut(.cancelAction)
        Spacer()
        Button("Confirm Words and Create Snapshot", action: startSnapshotCreation)
          .buttonStyle(.borderedProminent)
          .keyboardShortcut(.defaultAction)
      }
    }
  }

  private func progressView(title: String, detail: String) -> some View {
    VStack(spacing: 18) {
      Spacer()
      ProgressView()
        .controlSize(.large)
      Text(title)
        .font(.title2.weight(.semibold))
      Text(detail)
        .foregroundStyle(.secondary)
        .multilineTextAlignment(.center)
        .frame(maxWidth: 620)
      if phase == .creatingSnapshot {
        Label(
          "The recovery-kit file already exists and will be kept if this operation is stopped.",
          systemImage: "checkmark.shield"
        )
        .font(.callout)
        .foregroundStyle(.green)
      }
      Button(phase == .creatingSnapshot ? "Stop Conversion" : "Cancel") {
        operationTask?.cancel()
      }
      .keyboardShortcut(.cancelAction)
      Spacer()
    }
    .frame(maxWidth: .infinity, maxHeight: .infinity)
  }

  private var completionView: some View {
    VStack(alignment: .leading, spacing: 18) {
      Label("Recoverable snapshot created", systemImage: "checkmark.seal.fill")
        .font(.title2.weight(.semibold))
        .foregroundStyle(.green)
      if let result {
        GroupBox("Verified result") {
          VStack(alignment: .leading, spacing: 9) {
            LabeledContent("Databases", value: result.databaseCount.formatted())
            LabeledContent(
              "Recovery without WeChat key", value: result.recoveryVerified ? "Verified" : "No")
            LabeledContent("Independent encryption", value: "SQLCipher under a new random key")
            LabeledContent("Portable recovery", value: "24-word BIP-39 kit")
            LabeledContent("Recovery-kit file", value: recoveryKitPath)
            LabeledContent("Local convenience", value: completionConvenienceDescription)
            LabeledContent(
              "Passphrase protector", value: result.hasPassphrase ? "Argon2id enabled" : "Not added"
            )
          }
          .padding(6)
        }
        Text(
          "The recovery words are no longer displayed. Keep the recovery-kit file and your independent copy; either can recover the snapshot after losing WeChat and its keys."
        )
        .foregroundStyle(.secondary)
      }
      if let completionWarning {
        Label(completionWarning, systemImage: "exclamationmark.triangle.fill")
          .foregroundStyle(.orange)
          .textSelection(.enabled)
      }
      Spacer()
      HStack {
        Spacer()
        Button("Done") { dismiss() }
          .buttonStyle(.borderedProminent)
          .keyboardShortcut(.defaultAction)
      }
    }
  }

  private var failureView: some View {
    VStack(alignment: .leading, spacing: 18) {
      Label("Snapshot creation did not complete", systemImage: "xmark.octagon.fill")
        .font(.title2.weight(.semibold))
        .foregroundStyle(.red)
      if let errorMessage {
        Text(errorMessage)
          .textSelection(.enabled)
      }
      GroupBox {
        VStack(alignment: .leading, spacing: 8) {
          Label("The recovery-kit file was not deleted", systemImage: "key.viewfinder")
            .foregroundStyle(.green)
          Text(recoveryKitPath)
            .font(.caption.monospaced())
            .textSelection(.enabled)
          Text(
            "Entered source keys, passphrases, displayed words, and confirmation responses were cleared from the wizard. Choose a new output path before retrying."
          )
          .font(.caption)
          .foregroundStyle(.secondary)
        }
        .padding(6)
      }
      Spacer()
      HStack {
        Spacer()
        Button("Close") { dismiss() }
          .buttonStyle(.borderedProminent)
          .keyboardShortcut(.defaultAction)
      }
    }
  }

  @ViewBuilder
  private var inlineError: some View {
    if let errorMessage {
      Label(errorMessage, systemImage: "exclamationmark.triangle.fill")
        .font(.callout)
        .foregroundStyle(.red)
        .textSelection(.enabled)
    }
  }

  private var isBusy: Bool {
    phase == .generatingRecoveryKit || phase == .creatingSnapshot
  }

  private var completionConvenienceDescription: String {
    switch convenienceMode {
    case .keychain:
      keychainStored
        ? "macOS Keychain (device-only wrapper credential)"
        : "Keychain unavailable; use recovery words or passphrase"
    case .hiddenFile: "Owner-only hidden credential file"
    case .none: "None"
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

  private func challengeBinding(_ position: Int) -> Binding<String> {
    Binding(
      get: { challengeResponses[position] ?? "" },
      set: { challengeResponses[position] = $0.lowercased() }
    )
  }

  private func prepareSuggestedPaths() {
    guard outputPath.isEmpty || recoveryKitPath.isEmpty || hiddenCredentialPath.isEmpty else {
      return
    }
    do {
      let suggestions = try SnapshotWizardStorage.suggestedPaths()
      if outputPath.isEmpty { outputPath = suggestions.snapshot.path }
      if recoveryKitPath.isEmpty { recoveryKitPath = suggestions.recoveryKit.path }
      if hiddenCredentialPath.isEmpty {
        hiddenCredentialPath = suggestions.hiddenCredential.path
      }
    } catch {
      errorMessage =
        "Could not prepare owner-only default folders. Choose private locations manually."
    }
  }

  private func startRecoveryKitCreation() {
    guard validateSetup() else { return }
    errorMessage = nil
    phase = .generatingRecoveryKit
    let executableURL = URL(fileURLWithPath: trimmed(model.liveExecutablePath))
    let kitURL = URL(fileURLWithPath: trimmed(recoveryKitPath))
    operationTask = Task {
      do {
        let created = try await client.createRecoveryKit(
          executableURL: executableURL,
          outputURL: kitURL
        )
        try Task.checkCancellation()
        recoveryKit = created
        challenge = try HistorySnapshotWordChallenge.random()
        challengeResponses = [:]
        confirmsIndependentCopy = false
        phase = .confirmRecoveryWords
      } catch is CancellationError {
        errorMessage =
          "Recovery-word creation was cancelled. If the selected file now exists, it was intentionally kept; choose a new path before trying again."
        phase = .setup
      } catch {
        errorMessage = localizedSnapshotError(error)
        phase = .setup
      }
      operationTask = nil
    }
  }

  private func startSnapshotCreation() {
    guard let recoveryKit, let challenge else {
      errorMessage = "Generate and confirm a recovery kit first."
      return
    }
    guard confirmsIndependentCopy else {
      errorMessage = "Confirm that you stored an independent copy of all 24 words."
      return
    }
    guard challenge.accepts(responses: challengeResponses, words: recoveryKit.words) else {
      errorMessage = "One or more confirmation words do not match their numbered positions."
      return
    }

    let executableURL = URL(fileURLWithPath: trimmed(model.liveExecutablePath))
    let sourceURL = URL(fileURLWithPath: trimmed(model.directSourcePath), isDirectory: true)
    let snapshotURL = URL(fileURLWithPath: trimmed(outputPath), isDirectory: true)
    let hiddenURL = URL(fileURLWithPath: trimmed(hiddenCredentialPath))
    let requestedConvenience = convenienceMode
    let requestedSourceAccess = sourceAccess
    let requestedStableCapture = stableCapture
    let sourceSecretInput = Array(sourceKey.utf8)
    let snapshotSecretInput = addsPassphrase ? Array(snapshotPassphrase.utf8) : []

    clearDisplayedSecrets()
    errorMessage = nil
    phase = .creatingSnapshot
    operationTask = Task {
      var sourceSecret = sourceSecretInput
      var snapshotSecret = snapshotSecretInput
      var localCredentialData = Data()
      var localCredentialURL: URL?
      var createdHiddenCredential = false
      var snapshotPublished = false
      defer {
        sourceSecret.resetBytes(in: 0..<sourceSecret.count)
        snapshotSecret.resetBytes(in: 0..<snapshotSecret.count)
        localCredentialData.resetBytes(in: 0..<localCredentialData.count)
        removeTransientCredentialDirectory()
      }
      do {
        switch requestedConvenience {
        case .keychain:
          let directory = try SnapshotWizardStorage.makeTemporaryCredentialDirectory()
          transientCredentialDirectory = directory
          let credentialURL = directory.appending(path: ".snapshot-local-unlock")
          localCredentialData = try await client.createLocalCredential(
            executableURL: executableURL,
            outputURL: credentialURL
          )
          localCredentialURL = credentialURL
        case .hiddenFile:
          localCredentialData = try await client.createLocalCredential(
            executableURL: executableURL,
            outputURL: hiddenURL
          )
          localCredentialURL = hiddenURL
          createdHiddenCredential = true
        case .none:
          break
        }

        let request = HistorySnapshotCreationRequest(
          executableURL: executableURL,
          sourceURL: sourceURL,
          outputURL: snapshotURL,
          recoveryKit: recoveryKit,
          localCredentialURL: localCredentialURL,
          sourceAccess: requestedSourceAccess,
          stableCapture: requestedStableCapture
        )
        let created = try await client.createSnapshot(
          request: request,
          sourceKeyUTF8: sourceSecret,
          snapshotPassphraseUTF8: snapshotSecret
        )
        snapshotPublished = true

        var savedToKeychain = false
        var warning: String?
        if requestedConvenience == .keychain {
          do {
            try keychain.save(localCredentialData, snapshotID: created.snapshotID)
            savedToKeychain = true
          } catch {
            warning =
              "The snapshot is complete, but macOS Keychain could not save its convenience credential. The 24-word recovery kit\(created.hasPassphrase ? " and Argon2id passphrase remain" : " remains") sufficient."
          }
        }

        keychainStored = savedToKeychain
        completionWarning = warning
        result = created
        model.rememberCreatedSnapshot(
          created,
          keychainProtected: savedToKeychain,
          hiddenCredentialURL: requestedConvenience == .hiddenFile ? hiddenURL : nil,
          recoveryKitURL: recoveryKit.url,
          passphraseProtected: created.hasPassphrase
        )
        phase = .completed
      } catch is CancellationError {
        if createdHiddenCredential && !snapshotPublished {
          try? FileManager.default.removeItem(at: hiddenURL)
        }
        errorMessage =
          "Snapshot conversion was stopped. Any unpublished private staging generation was discarded by the CLI."
        phase = .failed
      } catch {
        if createdHiddenCredential && !snapshotPublished {
          try? FileManager.default.removeItem(at: hiddenURL)
        }
        errorMessage = localizedSnapshotError(error)
        phase = .failed
      }
      operationTask = nil
    }
  }

  private func validateSetup() -> Bool {
    let executable = trimmed(model.liveExecutablePath)
    let source = trimmed(model.directSourcePath)
    let output = trimmed(outputPath)
    let kit = trimmed(recoveryKitPath)
    guard !executable.isEmpty, !source.isEmpty, !output.isEmpty, !kit.isEmpty else {
      errorMessage = "Choose the CLI, source, new snapshot directory, and new recovery-kit file."
      return false
    }
    let pathSet = Set(
      [source, output, kit]
        + (convenienceMode == .hiddenFile ? [trimmed(hiddenCredentialPath)] : []))
    let expectedPathCount = convenienceMode == .hiddenFile ? 4 : 3
    guard pathSet.count == expectedPathCount else {
      errorMessage = "Source, snapshot, recovery-kit, and convenience paths must be distinct."
      return false
    }
    if sourceAccess == .encryptedWeChat {
      let key = Array(sourceKey.utf8)
      let isRaw = key.count == 32
      let isHex = key.count == 64 && key.allSatisfy(isWizardHexadecimal)
      guard isRaw || isHex else {
        errorMessage = "Enter the 32-byte or 64-hex-character WeChat database key."
        return false
      }
    }
    if convenienceMode == .hiddenFile, trimmed(hiddenCredentialPath).isEmpty {
      errorMessage = "Choose a new owner-only hidden credential file."
      return false
    }
    if addsPassphrase {
      let bytes = Array(snapshotPassphrase.utf8)
      guard snapshotPassphrase == snapshotPassphraseConfirmation,
        (12...1_024).contains(bytes.count), !bytes.contains(0), !bytes.contains(10),
        !bytes.contains(13)
      else {
        errorMessage =
          "Passphrases must match and contain 12–1,024 UTF-8 bytes without line breaks."
        return false
      }
    }
    return true
  }

  private func clearDisplayedSecrets() {
    sourceKey = ""
    snapshotPassphrase = ""
    snapshotPassphraseConfirmation = ""
    if var words = recoveryKit?.words {
      for index in words.indices { words[index] = "" }
    }
    recoveryKit = nil
    challenge = nil
    challengeResponses = [:]
    confirmsIndependentCopy = false
  }

  private func removeTransientCredentialDirectory() {
    guard let directory = transientCredentialDirectory else { return }
    try? FileManager.default.removeItem(at: directory)
    transientCredentialDirectory = nil
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
    panel.title = "Choose a WeChat database root or stable acquisition capture"
    panel.prompt = "Choose Source"
    panel.canChooseFiles = false
    panel.canChooseDirectories = true
    panel.allowsMultipleSelection = false
    panel.resolvesAliases = false
    if panel.runModal() == .OK, let url = panel.url {
      model.directSourcePath = url.path
    }
  }

  private func chooseOutput() {
    chooseNewPath(
      title: "Choose a new recoverable snapshot directory",
      prompt: "Choose Snapshot",
      currentPath: outputPath
    ) { outputPath = $0 }
  }

  private func chooseRecoveryKitOutput() {
    chooseNewPath(
      title: "Choose a new private recovery-kit file",
      prompt: "Choose Recovery File",
      currentPath: recoveryKitPath
    ) { recoveryKitPath = $0 }
  }

  private func chooseHiddenCredentialOutput() {
    chooseNewPath(
      title: "Choose a new owner-only hidden convenience file",
      prompt: "Choose Hidden File",
      currentPath: hiddenCredentialPath,
      showsHiddenFiles: true
    ) { hiddenCredentialPath = $0 }
  }

  private func chooseNewPath(
    title: String,
    prompt: String,
    currentPath: String,
    showsHiddenFiles: Bool = false,
    assign: (String) -> Void
  ) {
    let current = URL(fileURLWithPath: currentPath)
    let panel = NSSavePanel()
    panel.title = title
    panel.prompt = prompt
    panel.canCreateDirectories = true
    panel.showsHiddenFiles = showsHiddenFiles
    panel.directoryURL = current.deletingLastPathComponent()
    panel.nameFieldStringValue = current.lastPathComponent
    if panel.runModal() == .OK, let url = panel.url { assign(url.path) }
  }

  private func localizedSnapshotError(_ error: Error) -> String {
    (error as? LocalizedError)?.errorDescription
      ?? "Snapshot creation failed without exposing command output or secret material."
  }

  private func trimmed(_ value: String) -> String {
    value.trimmingCharacters(in: .whitespacesAndNewlines)
  }
}

private struct SnapshotWizardSuggestedPaths {
  let snapshot: URL
  let recoveryKit: URL
  let hiddenCredential: URL
}

private enum SnapshotWizardStorage {
  static func suggestedPaths() throws -> SnapshotWizardSuggestedPaths {
    let support = try FileManager.default.url(
      for: .applicationSupportDirectory,
      in: .userDomainMask,
      appropriateFor: nil,
      create: true
    )
    let base = support.appending(path: "GreenBubbles", directoryHint: .isDirectory)
    let snapshots = base.appending(path: "Recoverable Snapshots", directoryHint: .isDirectory)
    let recovery = base.appending(path: "Recovery Kits", directoryHint: .isDirectory)
    let credentials = base.appending(path: "Local Unlocks", directoryHint: .isDirectory)
    for directory in [base, snapshots, recovery, credentials] {
      try ensurePrivateDirectory(directory)
    }
    let identifier = UUID().uuidString.lowercased()
    return SnapshotWizardSuggestedPaths(
      snapshot: snapshots.appending(path: "snapshot-\(identifier)"),
      recoveryKit: recovery.appending(path: "recovery-\(identifier).txt"),
      hiddenCredential: credentials.appending(path: ".snapshot-unlock-\(identifier)")
    )
  }

  static func makeTemporaryCredentialDirectory() throws -> URL {
    let directory = FileManager.default.temporaryDirectory.appending(
      path: "greenbubbles-snapshot-create-\(UUID().uuidString)",
      directoryHint: .isDirectory
    )
    try ensurePrivateDirectory(directory, mustBeNew: true)
    return directory
  }

  private static func ensurePrivateDirectory(_ url: URL, mustBeNew: Bool = false) throws {
    if mkdir(url.path, S_IRWXU) != 0 {
      guard !mustBeNew, errno == EEXIST else {
        throw HistorySnapshotCreationError.unsafeProtector
      }
    }
    var metadata = stat()
    guard lstat(url.path, &metadata) == 0, metadata.st_mode & S_IFMT == S_IFDIR,
      metadata.st_uid == getuid(), chmod(url.path, S_IRWXU) == 0
    else { throw HistorySnapshotCreationError.unsafeProtector }
  }
}

private func isWizardHexadecimal(_ byte: UInt8) -> Bool {
  (byte >= 48 && byte <= 57) || (byte >= 65 && byte <= 70)
    || (byte >= 97 && byte <= 102)
}
