import AppKit
import GreenBubblesHistory
import SwiftUI

struct HistoryOverviewView: View {
  let model: HistoryBrowserModel

  var body: some View {
    ScrollView {
      if let manifest = model.session?.manifest {
        VStack(alignment: .leading, spacing: 22) {
          VStack(alignment: .leading, spacing: 6) {
            Text("History overview")
              .font(.largeTitle.weight(.semibold))
            Text("A verified, policy-scoped view of your local WeChat context.")
              .font(.title3)
              .foregroundStyle(.secondary)
          }
          CoverageBanner(context: manifest.context) {
            model.showsCoverageDetails = true
          }
          LazyVGrid(
            columns: [GridItem(.adaptive(minimum: 190), spacing: 14)], spacing: 14
          ) {
            MetricCard(
              title: "Conversations", value: manifest.enabledConversationCount.formatted(),
              icon: "bubble.left.and.bubble.right.fill", color: .blue)
            MetricCard(
              title: "Contacts", value: manifest.exportedContactCount.formatted(),
              icon: "person.2.fill", color: .purple)
            MetricCard(
              title: "Messages", value: manifest.exportedMessageCount.formatted(),
              icon: "text.bubble.fill", color: .green)
            MetricCard(
              title: "Media references", value: manifest.exportedArtifactCount.formatted(),
              icon: "photo.on.rectangle.angled", color: .orange)
          }
          GroupBox("Database coverage") {
            HStack(spacing: 32) {
              CoverageMetric(label: "Total", value: manifest.context.totalDatabaseCount)
              CoverageMetric(
                label: "Fresh", value: manifest.context.freshDatabaseCount, color: .green)
              CoverageMetric(
                label: "Unavailable", value: manifest.context.unavailableDatabaseCount,
                color: .orange)
              CoverageMetric(
                label: "Preserved stale", value: manifest.context.preservedStaleDatabaseCount,
                color: .yellow)
              Spacer()
            }
            .padding(8)
          }
          GroupBox("Bundle evidence") {
            Grid(alignment: .leading, horizontalSpacing: 28, verticalSpacing: 10) {
              EvidenceRow(
                label: "Created",
                value: manifest.createdAt.formatted(date: .long, time: .shortened))
              EvidenceRow(
                label: "Checkpoint", value: shortHistoryID(manifest.context.checkpointRevision))
              EvidenceRow(
                label: "Client",
                value: historyHumanize(manifest.context.clientBuildCompatibility ?? "unknown"))
              EvidenceRow(
                label: "Scope", value: historyHumanize(manifest.context.archiveScope ?? "unknown"))
              EvidenceRow(
                label: "Index",
                value: model.session?.reusedIndex == true
                  ? "Verified cache reused" : "Built and verified")
            }
            .padding(8)
            .textSelection(.enabled)
          }
        }
        .padding(28)
        .frame(maxWidth: 1_050, alignment: .leading)
      }
    }
    .background(Color(nsColor: .windowBackgroundColor))
  }
}

struct ConversationTimelineView: View {
  @Bindable var model: HistoryBrowserModel
  let conversation: HistoryConversation

  var body: some View {
    VStack(spacing: 0) {
      conversationHeader
      Divider()
      if let context = model.session?.manifest.context,
        !context.sourceCoverageComplete || historyIsStale(conversation.sourceDatabaseFreshness)
      {
        CoverageBanner(context: context, compact: true) {
          model.showsCoverageDetails = true
        }
        .padding(.horizontal, 16)
        .padding(.top, 12)
      }
      if let timelineError = model.timelineError {
        Label(timelineError, systemImage: "exclamationmark.triangle.fill")
          .foregroundStyle(.red)
          .padding()
      }
      timeline
    }
    .background(Color(nsColor: .textBackgroundColor).opacity(0.45))
  }

  private var conversationHeader: some View {
    HStack(spacing: 14) {
      VStack(alignment: .leading, spacing: 3) {
        Text(conversation.humanLabel)
          .font(.title2.weight(.semibold))
        Text(
          "\(conversation.participantCount) participants • \(model.session?.conversationStatistics[conversation.conversationID]?.messageCount ?? 0) messages"
        )
        .font(.caption)
        .foregroundStyle(.secondary)
      }
      Spacer()
      if historyIsStale(conversation.sourceDatabaseFreshness) {
        FreshnessBadge(value: conversation.sourceDatabaseFreshness)
      }
      Menu {
        ForEach(conversation.participants) { participant in
          Text("\(participant.displayName) — \(historyHumanize(participant.role))")
        }
      } label: {
        Label("Participants", systemImage: "person.2")
      }
      .menuStyle(.borderlessButton)
    }
    .padding(.horizontal, 18)
    .padding(.vertical, 12)
    .background(.bar)
  }

  private var timeline: some View {
    ScrollViewReader { proxy in
      ScrollView {
        LazyVStack(spacing: 10) {
          if model.isSearchContext {
            Button("Return to latest messages") {
              model.returnToLatestMessages()
            }
            .buttonStyle(.bordered)
            .padding(.vertical, 8)
          } else if model.timelineCursor != nil {
            Button {
              model.loadOlderMessages()
            } label: {
              if model.isLoadingTimeline {
                ProgressView().controlSize(.small)
              } else {
                Label("Load 100 older messages", systemImage: "arrow.up.circle")
              }
            }
            .buttonStyle(.bordered)
            .padding(.vertical, 8)
          }

          ForEach(Array(model.timelineMessages.reversed())) { message in
            MessageBubble(
              model: model,
              message: message,
              highlighted: message.canonicalID == model.highlightedMessageID
            )
            .id(message.canonicalID)
          }

          if model.isLoadingTimeline && model.timelineMessages.isEmpty {
            ProgressView("Loading messages…")
              .padding(40)
          }
          if !model.isLoadingTimeline && model.timelineMessages.isEmpty
            && model.timelineError == nil
          {
            ContentUnavailableView(
              "No authorized messages",
              systemImage: "bubble.left",
              description: Text(
                "This conversation has no messages inside the bundle's policy window.")
            )
            .padding(40)
          }
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 14)
      }
      .onChange(of: model.timelineMessages.count) { previousCount, currentCount in
        guard currentCount > 0 else { return }
        if let highlighted = model.highlightedMessageID {
          proxy.scrollTo(highlighted, anchor: .center)
        } else if previousCount == 0, let newest = model.timelineMessages.first {
          proxy.scrollTo(newest.canonicalID, anchor: .bottom)
        }
      }
    }
  }
}

private struct MessageBubble: View {
  let model: HistoryBrowserModel
  let message: HistoryMessage
  let highlighted: Bool

  var body: some View {
    HStack {
      if resolvedDirection == "outgoing" { Spacer(minLength: 90) }
      bubbleContent
        .padding(.horizontal, 13)
        .padding(.vertical, 10)
        .frame(maxWidth: 650, alignment: .leading)
        .background(bubbleColor, in: RoundedRectangle(cornerRadius: 14))
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
            NSPasteboard.general.setString(message.canonicalID, forType: .string)
          }
        }
      if resolvedDirection != "outgoing" { Spacer(minLength: 90) }
    }
  }

  private var bubbleContent: some View {
    VStack(alignment: .leading, spacing: 7) {
      senderHeader
      Text(message.displayText)
        .font(.body)
        .textSelection(.enabled)
      if message.payloadSummaryTruncated == true {
        Label("Summary truncated by policy", systemImage: "scissors")
          .font(.caption2)
          .foregroundStyle(.secondary)
      }
      relationshipLinks
      artifactLinks
      messageMetadata
    }
  }

  private var senderHeader: some View {
    HStack(spacing: 7) {
      if let sender = message.senderDisplayName, resolvedDirection != "outgoing" {
        Text(sender)
          .font(.caption.weight(.semibold))
          .foregroundStyle(.secondary)
      }
      if message.sourceDatabaseFreshness == "preservedStale" {
        FreshnessBadge(value: message.sourceDatabaseFreshness)
      }
    }
  }

  private var relationshipLinks: some View {
    ForEach(Array(message.relationships.enumerated()), id: \.offset) { _, relationship in
      RelationshipButton(model: model, message: message, relationship: relationship)
    }
  }

  private var artifactLinks: some View {
    ForEach(Array(message.artifactReferences.enumerated()), id: \.offset) { _, reference in
      ArtifactReferenceView(model: model, reference: reference)
    }
  }

  private var messageMetadata: some View {
    HStack(spacing: 7) {
      if let date = message.createdAt {
        Text(date, format: .dateTime.year().month().day().hour().minute())
      } else {
        Text("Unknown time")
      }
      if let payloadKind = message.payloadKind {
        Text("•")
        Text(historyHumanize(payloadKind))
      }
      if resolvedDirection == "unknown" {
        Text("• direction unknown")
          .foregroundStyle(.orange)
      }
    }
    .font(.caption2.monospacedDigit())
    .foregroundStyle(.tertiary)
  }

  private var bubbleColor: Color {
    switch resolvedDirection {
    case "outgoing": .green.opacity(0.18)
    case "incoming": Color(nsColor: .controlBackgroundColor)
    default: .orange.opacity(0.10)
    }
  }

  private var resolvedDirection: String? {
    model.resolvedDirection(for: message)
  }
}

private struct RelationshipButton: View {
  let model: HistoryBrowserModel
  let message: HistoryMessage
  let relationship: HistoryRelationshipReference

  var body: some View {
    Button {
      if let target = relationship.targetCanonicalID {
        model.selectConversation(message.conversationID, around: target)
      }
    } label: {
      Label(title, systemImage: icon)
    }
    .buttonStyle(.plain)
    .font(.caption)
    .foregroundStyle(relationship.resolved ? Color.secondary : Color.orange)
    .disabled(!relationship.resolved || relationship.targetCanonicalID == nil)
  }

  private var title: String {
    relationship.resolved
      ? historyHumanize(relationship.kind)
      : "Unresolved \(historyHumanize(relationship.kind))"
  }

  private var icon: String {
    relationship.resolved ? "arrowshape.turn.up.left" : "questionmark.diamond"
  }
}

private struct ArtifactReferenceView: View {
  let model: HistoryBrowserModel
  let reference: HistoryArtifactReference
  @State private var artifact: HistoryArtifact?
  @State private var loaded = false
  @State private var loadError: String?

  var body: some View {
    HStack(spacing: 10) {
      Image(systemName: artifactIcon)
        .font(.title2)
        .frame(width: 30)
        .foregroundStyle(artifactColor)
      VStack(alignment: .leading, spacing: 3) {
        Text(artifactTitle)
          .font(.callout.weight(.medium))
        Text(artifactDetail)
          .font(.caption)
          .foregroundStyle(
            artifact?.error == nil && loadError == nil ? Color.secondary : Color.red
          )
          .lineLimit(2)
      }
      Spacer()
      if !loaded {
        ProgressView().controlSize(.small)
      } else if artifact?.detail != nil {
        Image(systemName: "checkmark.shield.fill")
          .foregroundStyle(.green)
          .help("Digest-verified when exported")
        if canPreview, let artifact {
          Button("Preview") {
            model.requestMediaPreview(artifact)
          }
          .buttonStyle(.bordered)
          .controlSize(.small)
        }
      }
    }
    .padding(9)
    .background(.thinMaterial, in: RoundedRectangle(cornerRadius: 9))
    .task(id: reference.artifactID) {
      artifact = nil
      loadError = nil
      loaded = false
      do {
        artifact = try await model.artifact(reference.artifactID)
        loaded = true
      } catch is CancellationError {
        return
      } catch {
        loadError = String(describing: error)
        loaded = true
      }
    }
  }

  private var artifactIcon: String {
    switch artifact?.detail?.kind {
    case "image", "animatedImage", "thumbnail": "photo.fill"
    case "voice": "waveform"
    case "video": "play.rectangle.fill"
    case "document": "doc.fill"
    case "richMedia": "rectangle.stack.fill"
    default: "paperclip"
    }
  }

  private var artifactColor: Color {
    artifact?.error == nil && loadError == nil ? .blue : .red
  }

  private var artifactTitle: String {
    if let detail = artifact?.detail { return historyHumanize(detail.kind) }
    if artifact?.error != nil || loadError != nil { return "Unavailable attachment" }
    return historyHumanize(reference.role)
  }

  private var artifactDetail: String {
    if let loadError { return loadError }
    if let error = artifact?.error { return error.message }
    guard let detail = artifact?.detail else {
      return loaded ? "Artifact metadata is unavailable" : "Loading verified metadata…"
    }
    let file = detail.decoded ?? detail.source
    let size = file.map {
      ByteCountFormatter.string(
        fromByteCount: Int64(clamping: $0.byteCount), countStyle: .file)
    }
    return [
      historyHumanize(detail.availability),
      "Decode: \(historyHumanize(detail.decodeState))",
      file?.format.uppercased(),
      size,
    ]
    .compactMap { $0 }
    .joined(separator: " • ")
  }

  private var canPreview: Bool {
    guard let availability = artifact?.detail?.availability else { return false }
    return availability == "downloaded" || availability == "materializedFromDatabase"
  }
}

struct ContactDetailView: View {
  let model: HistoryBrowserModel
  let contact: HistoryContact

  var body: some View {
    ScrollView {
      VStack(alignment: .leading, spacing: 22) {
        HStack(spacing: 16) {
          HistoryAvatarView(name: contact.displayName, size: 64)
          VStack(alignment: .leading, spacing: 5) {
            Text(contact.displayName)
              .font(.largeTitle.weight(.semibold))
            HStack {
              Text(
                contact.localProfileAvailable
                  ? "Local profile available" : "Derived from conversations")
              FreshnessBadge(value: contact.sourceDatabaseFreshness)
            }
            .font(.callout)
            .foregroundStyle(.secondary)
          }
        }
        GroupBox("Shared conversations") {
          VStack(spacing: 0) {
            ForEach(contact.conversationProfiles) { profile in
              Button {
                model.selectConversation(profile.conversationID)
              } label: {
                HStack {
                  Image(systemName: "bubble.left.fill")
                    .foregroundStyle(.green)
                  VStack(alignment: .leading) {
                    Text(profile.conversationLabel)
                    Text(
                      "Known as \(profile.displayName) • \(historyHumanize(profile.role))"
                    )
                    .font(.caption)
                    .foregroundStyle(.secondary)
                  }
                  Spacer()
                  Image(systemName: "chevron.right")
                    .foregroundStyle(.tertiary)
                }
                .padding(.vertical, 9)
              }
              .buttonStyle(.plain)
              if profile.id != contact.conversationProfiles.last?.id {
                Divider()
              }
            }
          }
          .padding(.horizontal, 8)
        }
        DisclosureGroup("Technical identity") {
          LabeledContent("Opaque participant ID", value: contact.participantID)
            .textSelection(.enabled)
            .padding(.top, 8)
        }
      }
      .padding(28)
      .frame(maxWidth: 860, alignment: .leading)
    }
  }
}

struct CoverageBanner: View {
  let context: HistoryContextHealth
  var compact = false
  var showDetails: (() -> Void)?

  var body: some View {
    HStack(alignment: .top, spacing: 12) {
      Image(
        systemName: context.sourceCoverageComplete
          ? "checkmark.shield.fill" : "exclamationmark.triangle.fill"
      )
      .font(.title3)
      .foregroundStyle(context.sourceCoverageComplete ? .green : .orange)
      VStack(alignment: .leading, spacing: 3) {
        Text(
          context.sourceCoverageComplete
            ? "Source coverage complete" : "History has partial source coverage"
        )
        .font(.callout.weight(.semibold))
        if !compact || !context.sourceCoverageComplete {
          Text(context.coverageNote)
            .font(.caption)
            .foregroundStyle(.secondary)
            .lineLimit(compact ? 2 : nil)
        }
      }
      Spacer()
      if let showDetails {
        Button("Details", action: showDetails)
          .buttonStyle(.borderless)
      }
    }
    .padding(12)
    .background(
      (context.sourceCoverageComplete ? Color.green : Color.orange).opacity(0.09),
      in: RoundedRectangle(cornerRadius: 11)
    )
  }
}

struct CoverageDetailsView: View {
  let context: HistoryContextHealth
  @Environment(\.dismiss) private var dismiss

  var body: some View {
    VStack(alignment: .leading, spacing: 18) {
      HStack {
        Text("Source coverage")
          .font(.title2.weight(.semibold))
        Spacer()
        Button("Done") { dismiss() }
          .keyboardShortcut(.defaultAction)
      }
      CoverageBanner(context: context)
      GroupBox("Databases") {
        Grid(alignment: .leading, horizontalSpacing: 28, verticalSpacing: 10) {
          EvidenceRow(label: "Total", value: context.totalDatabaseCount?.formatted() ?? "Unknown")
          EvidenceRow(label: "Fresh", value: context.freshDatabaseCount?.formatted() ?? "Unknown")
          EvidenceRow(
            label: "Unavailable", value: context.unavailableDatabaseCount?.formatted() ?? "Unknown")
          EvidenceRow(
            label: "Preserved stale",
            value: context.preservedStaleDatabaseCount?.formatted() ?? "Unknown")
        }
        .padding(8)
      }
      if !context.limitationCodes.isEmpty {
        GroupBox("Limitations") {
          VStack(alignment: .leading, spacing: 8) {
            ForEach(context.limitationCodes, id: \.self) { code in
              Label(historyHumanize(code), systemImage: "exclamationmark.circle")
            }
          }
          .frame(maxWidth: .infinity, alignment: .leading)
          .padding(8)
        }
      }
      Text(
        "When a database is unavailable, missing records are not evidence that a message, contact, or attachment was deleted or never existed."
      )
      .font(.footnote)
      .foregroundStyle(.secondary)
      Spacer()
    }
    .padding(24)
  }
}

struct HistoryFilterField: View {
  @Binding var text: String
  let prompt: String

  var body: some View {
    HStack(spacing: 7) {
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
    .padding(.vertical, 7)
    .background(
      Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 8)
    )
    .padding(10)
  }
}

struct HistoryAvatarView: View {
  let name: String
  var size: CGFloat = 38

  var body: some View {
    ZStack {
      Circle().fill(color.gradient)
      Text(initials)
        .font(.system(size: size * 0.36, weight: .semibold, design: .rounded))
        .foregroundStyle(.white)
    }
    .frame(width: size, height: size)
    .accessibilityLabel(name)
  }

  private var initials: String {
    let components = name.split(separator: " ").prefix(2)
    let value = components.compactMap(\.first).map(String.init).joined()
    return value.isEmpty ? "?" : value.uppercased()
  }

  private var color: Color {
    let value = name.utf8.reduce(0) { ($0 &* 31) &+ Int($1) }
    let colors: [Color] = [.blue, .green, .purple, .orange, .pink, .teal]
    return colors[Int(value.magnitude % UInt(colors.count))]
  }
}

private struct MetricCard: View {
  let title: String
  let value: String
  let icon: String
  let color: Color

  var body: some View {
    HStack(spacing: 14) {
      Image(systemName: icon)
        .font(.title2)
        .foregroundStyle(color)
        .frame(width: 36, height: 36)
        .background(color.opacity(0.12), in: RoundedRectangle(cornerRadius: 9))
      VStack(alignment: .leading, spacing: 2) {
        Text(value)
          .font(.title2.monospacedDigit().weight(.semibold))
        Text(title)
          .font(.caption)
          .foregroundStyle(.secondary)
      }
      Spacer()
    }
    .padding(14)
    .background(
      Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 12))
  }
}

private struct CoverageMetric: View {
  let label: String
  let value: Int?
  var color: Color = .primary

  var body: some View {
    VStack(alignment: .leading, spacing: 3) {
      Text(value?.formatted() ?? "—")
        .font(.title2.monospacedDigit().weight(.semibold))
        .foregroundStyle(color)
      Text(label)
        .font(.caption)
        .foregroundStyle(.secondary)
    }
  }
}

private struct EvidenceRow: View {
  let label: String
  let value: String

  var body: some View {
    GridRow {
      Text(label).foregroundStyle(.secondary)
      Text(value)
    }
  }
}

private struct FreshnessBadge: View {
  let value: String

  var body: some View {
    Text(historyHumanize(value))
      .font(.caption2.weight(.semibold))
      .foregroundStyle(historyIsStale(value) ? .orange : .secondary)
      .padding(.horizontal, 6)
      .padding(.vertical, 2)
      .background(
        (historyIsStale(value) ? Color.orange : Color.secondary).opacity(0.12), in: Capsule())
  }
}

struct DetailPlaceholder: View {
  let icon: String
  let title: String
  let detail: String

  var body: some View {
    ContentUnavailableView(title, systemImage: icon, description: Text(detail))
  }
}

func historyIsStale(_ value: String) -> Bool {
  value == "preservedStale" || value == "mixed"
}

func shortHistoryID(_ value: String) -> String {
  value.count > 18 ? "\(value.prefix(9))…\(value.suffix(8))" : value
}

func historyHumanize(_ value: String) -> String {
  guard !value.isEmpty else { return value }
  let spaced = value.replacingOccurrences(
    of: "([a-z0-9])([A-Z])",
    with: "$1 $2",
    options: .regularExpression
  )
  return spaced.replacingOccurrences(of: "_", with: " ").capitalized
}
