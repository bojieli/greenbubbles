import AppKit
import GreenBubblesHistory
import QuickLookUI
import SwiftUI

struct LiveMediaAccessView: View {
  @Bindable var model: HistoryBrowserModel
  let request: HistoryMediaRequest
  @Environment(\.dismiss) private var dismiss
  @State private var replicaKey = ""
  @State private var isResolving = false
  @State private var progress: HistoryMediaResolutionProgress?
  @State private var media: HistoryVerifiedMedia?
  @State private var errorMessage: String?
  @State private var resolutionTask: Task<Void, Never>?

  var body: some View {
    VStack(spacing: 0) {
      header
      Divider()
      if let media {
        preview(media)
      } else {
        accessForm
      }
    }
    .onDisappear {
      resolutionTask?.cancel()
      replicaKey = ""
      isResolving = false
    }
  }

  private var header: some View {
    HStack {
      VStack(alignment: .leading, spacing: 3) {
        Text(media == nil ? "Verify and preview media" : "Verified media preview")
          .font(.title2.weight(.semibold))
        Text("GreenBubbles rechecks policy, file identity, byte count, and SHA-256 before display.")
          .font(.caption)
          .foregroundStyle(.secondary)
      }
      Spacer()
      Button("Done") { cancelAndDismiss() }
        .keyboardShortcut(.cancelAction)
    }
    .padding(18)
    .background(.bar)
  }

  private var accessForm: some View {
    ScrollView {
      VStack(alignment: .leading, spacing: 18) {
        GroupBox("Artifact") {
          HStack(spacing: 12) {
            Image(systemName: artifactIcon)
              .font(.title2)
              .foregroundStyle(.blue)
              .frame(width: 38, height: 38)
              .background(.blue.opacity(0.12), in: RoundedRectangle(cornerRadius: 9))
            VStack(alignment: .leading, spacing: 3) {
              Text(historyHumanize(request.artifact.detail?.kind ?? "attachment"))
                .font(.headline)
              Text(artifactSummary)
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            Spacer()
          }
          .padding(8)
        }

        GroupBox("Local GreenBubbles access") {
          VStack(spacing: 10) {
            LivePathRow(
              label: "CLI", text: $model.liveExecutablePath,
              choose: model.chooseLiveExecutable)
            LivePathRow(
              label: "Replica", text: $model.liveReplicaPath,
              choose: model.chooseLiveReplica)
            LivePathRow(
              label: "Policy", text: $model.livePolicyPath,
              choose: model.chooseLivePolicy)
            LivePathRow(
              label: "Audit log", text: $model.liveAuditPath,
              choose: model.chooseLiveAudit)
          }
          .padding(8)
        }

        GroupBox("One-time replica key") {
          VStack(alignment: .leading, spacing: 8) {
            SecureField("64 hexadecimal characters", text: $replicaKey)
              .textFieldStyle(.roundedBorder)
              .privacySensitive()
              .disabled(isResolving)
            Label(
              "The key is sent only to greenbubbles-restore over standard input. It is cleared from this form before the request and is not saved in settings, arguments, requests, logs, or previews.",
              systemImage: "key.horizontal"
            )
            .font(.caption)
            .foregroundStyle(.secondary)
          }
          .padding(8)
        }

        if let progress {
          VStack(alignment: .leading, spacing: 6) {
            HStack {
              Text(progressTitle(progress))
              Spacer()
              if progress.totalBytes > 0 {
                Text(progress.fraction, format: .percent.precision(.fractionLength(1)))
                  .monospacedDigit()
              } else {
                Text("60-second timeout")
                  .foregroundStyle(.secondary)
              }
            }
            .font(.callout.weight(.medium))
            if progress.totalBytes > 0 {
              ProgressView(value: progress.fraction)
              Text(
                "\(ByteCountFormatter.string(fromByteCount: Int64(clamping: progress.completedBytes), countStyle: .file)) / \(ByteCountFormatter.string(fromByteCount: Int64(clamping: progress.totalBytes), countStyle: .file))"
              )
              .font(.caption.monospacedDigit())
              .foregroundStyle(.secondary)
            } else {
              ProgressView()
                .controlSize(.small)
            }
          }
        }

        if let errorMessage {
          Label(errorMessage, systemImage: "exclamationmark.triangle.fill")
            .font(.callout)
            .foregroundStyle(.red)
            .textSelection(.enabled)
            .padding(12)
            .background(.red.opacity(0.08), in: RoundedRectangle(cornerRadius: 10))
        }

        HStack {
          Label(
            "No WeChat database key or key-acquisition tool is used.", systemImage: "lock.shield"
          )
          .font(.caption)
          .foregroundStyle(.secondary)
          Spacer()
          Button("Cancel") { cancelAndDismiss() }
          Button {
            resolve()
          } label: {
            if isResolving {
              ProgressView().controlSize(.small)
            } else {
              Label("Verify & Preview", systemImage: "checkmark.shield")
            }
          }
          .buttonStyle(.borderedProminent)
          .disabled(isResolving || replicaKey.isEmpty)
          .keyboardShortcut(.defaultAction)
        }
      }
      .padding(20)
      .frame(maxWidth: 820)
    }
  }

  private func preview(_ media: HistoryVerifiedMedia) -> some View {
    VStack(spacing: 0) {
      HistoryQuickLookView(url: media.previewURL)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
      Divider()
      HStack(spacing: 12) {
        Label("Private verified copy", systemImage: "checkmark.shield.fill")
          .foregroundStyle(.green)
        Text(historyHumanize(media.kind))
        Text("•")
        Text(media.format.uppercased())
        Text("•")
        Text(
          ByteCountFormatter.string(
            fromByteCount: Int64(clamping: media.byteCount), countStyle: .file))
        Spacer()
        Button("Reveal Preview Copy") {
          NSWorkspace.shared.activateFileViewerSelecting([media.previewURL])
        }
        Button("Open in Default App") {
          NSWorkspace.shared.open(media.previewURL)
        }
      }
      .font(.callout)
      .padding(12)
      .background(.bar)
    }
  }

  private func resolve() {
    var keyBytes = Array(replicaKey.utf8)
    replicaKey = ""
    errorMessage = nil
    progress = HistoryMediaResolutionProgress(
      phase: .requestingAuthorization, completedBytes: 0, totalBytes: 0)
    isResolving = true
    resolutionTask?.cancel()
    resolutionTask = Task {
      defer { keyBytes.resetBytes(in: 0..<keyBytes.count) }
      do {
        let configuration = try model.liveMediaConfiguration()
        let result = try await HistoryLiveMediaResolver().resolve(
          conversationID: request.conversationID,
          artifactID: request.artifact.artifactID,
          configuration: configuration,
          replicaKeyUTF8: keyBytes,
          progress: { update in
            Task { @MainActor in
              if self.isResolving { self.progress = update }
            }
          }
        )
        try Task.checkCancellation()
        media = result
        isResolving = false
        resolutionTask = nil
      } catch is CancellationError {
        isResolving = false
        resolutionTask = nil
      } catch {
        errorMessage = String(describing: error)
        isResolving = false
        resolutionTask = nil
      }
    }
  }

  private func cancelAndDismiss() {
    resolutionTask?.cancel()
    resolutionTask = nil
    replicaKey = ""
    isResolving = false
    dismiss()
  }

  private var artifactIcon: String {
    switch request.artifact.detail?.kind {
    case "image", "animatedImage", "thumbnail": "photo.fill"
    case "voice": "waveform"
    case "video": "play.rectangle.fill"
    case "document": "doc.fill"
    default: "paperclip"
    }
  }

  private var artifactSummary: String {
    guard let detail = request.artifact.detail else { return "Metadata unavailable" }
    let file = detail.decoded ?? detail.source
    return [
      historyHumanize(detail.availability),
      file?.format.uppercased(),
      file.map {
        ByteCountFormatter.string(
          fromByteCount: Int64(clamping: $0.byteCount), countStyle: .file)
      },
    ].compactMap { $0 }.joined(separator: " • ")
  }

  private func progressTitle(_ progress: HistoryMediaResolutionProgress) -> String {
    switch progress.phase {
    case .requestingAuthorization: "Requesting policy-scoped access"
    case .verifyingAndCopying: "Verifying and copying media"
    case .ready: "Preview ready"
    }
  }
}

private struct LivePathRow: View {
  let label: String
  @Binding var text: String
  let choose: () -> Void

  var body: some View {
    HStack {
      Text(label)
        .frame(width: 72, alignment: .trailing)
        .foregroundStyle(.secondary)
      TextField(label, text: $text)
        .textFieldStyle(.roundedBorder)
        .privacySensitive()
      Button("Choose…", action: choose)
    }
  }
}

private struct HistoryQuickLookView: NSViewRepresentable {
  let url: URL

  func makeNSView(context: Context) -> QLPreviewView {
    let view = QLPreviewView(frame: .zero, style: .normal)!
    view.autostarts = true
    view.previewItem = url as NSURL
    return view
  }

  func updateNSView(_ view: QLPreviewView, context: Context) {
    view.previewItem = url as NSURL
    view.refreshPreviewItem()
  }
}
