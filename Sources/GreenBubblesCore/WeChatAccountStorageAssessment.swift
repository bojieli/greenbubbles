import Foundation

public enum AttachmentCandidateKind: String, Codable, CaseIterable, Sendable {
  case image
  case audio
  case video
  case document
  case serializedData
  case configuration
  case unclassified
}

public struct AttachmentCandidateAggregate: Codable, Equatable, Sendable {
  public let kind: AttachmentCandidateKind
  public let fileCount: Int
  public let byteCount: Int64

  public init(kind: AttachmentCandidateKind, fileCount: Int, byteCount: Int64) {
    self.kind = kind
    self.fileCount = fileCount
    self.byteCount = byteCount
  }
}

public struct WeChatAccountStorageEvidence: Codable, Equatable, Sendable {
  public let accountID: String
  public let databaseSetCount: Int
  public let ordinaryMessageShardCandidateCount: Int
  public let businessMessageShardCandidateCount: Int
  public let chatbotMessageStoreCandidateCount: Int
  public let mediaDatabaseCandidateCount: Int
  public let messageResourceStoreCandidatePresent: Bool
  public let contactStoreCandidatePresent: Bool
  public let sessionStoreCandidatePresent: Bool
  public let cachedMomentsStoreCandidatePresent: Bool
  public let attachmentRootPresent: Bool
  public let attachmentCandidateFileCount: Int
  public let attachmentCandidateByteCount: Int64
  public let attachmentCandidates: [AttachmentCandidateAggregate]
  public let skippedSymbolicLinkCount: Int
  public let metadataIssueCount: Int
  public let reachedAttachmentLimit: Bool
  public let attachmentEnumerationCompleteWithinRoot: Bool

  public init(
    accountID: String,
    databaseSetCount: Int,
    ordinaryMessageShardCandidateCount: Int,
    businessMessageShardCandidateCount: Int,
    chatbotMessageStoreCandidateCount: Int,
    mediaDatabaseCandidateCount: Int,
    messageResourceStoreCandidatePresent: Bool,
    contactStoreCandidatePresent: Bool,
    sessionStoreCandidatePresent: Bool,
    cachedMomentsStoreCandidatePresent: Bool,
    attachmentRootPresent: Bool,
    attachmentCandidateFileCount: Int,
    attachmentCandidateByteCount: Int64,
    attachmentCandidates: [AttachmentCandidateAggregate],
    skippedSymbolicLinkCount: Int,
    metadataIssueCount: Int,
    reachedAttachmentLimit: Bool,
    attachmentEnumerationCompleteWithinRoot: Bool
  ) {
    self.accountID = accountID
    self.databaseSetCount = databaseSetCount
    self.ordinaryMessageShardCandidateCount = ordinaryMessageShardCandidateCount
    self.businessMessageShardCandidateCount = businessMessageShardCandidateCount
    self.chatbotMessageStoreCandidateCount = chatbotMessageStoreCandidateCount
    self.mediaDatabaseCandidateCount = mediaDatabaseCandidateCount
    self.messageResourceStoreCandidatePresent = messageResourceStoreCandidatePresent
    self.contactStoreCandidatePresent = contactStoreCandidatePresent
    self.sessionStoreCandidatePresent = sessionStoreCandidatePresent
    self.cachedMomentsStoreCandidatePresent = cachedMomentsStoreCandidatePresent
    self.attachmentRootPresent = attachmentRootPresent
    self.attachmentCandidateFileCount = attachmentCandidateFileCount
    self.attachmentCandidateByteCount = attachmentCandidateByteCount
    self.attachmentCandidates = attachmentCandidates
    self.skippedSymbolicLinkCount = skippedSymbolicLinkCount
    self.metadataIssueCount = metadataIssueCount
    self.reachedAttachmentLimit = reachedAttachmentLimit
    self.attachmentEnumerationCompleteWithinRoot = attachmentEnumerationCompleteWithinRoot
  }
}

public struct WeChatAccountStorageAssessmentConclusions: Codable, Equatable, Sendable {
  public let locallyPersistedAttachmentCandidatesObserved: Bool
  public let databaseOrAttachmentContentRead: Bool
  public let messageAttachmentLinkageProven: Bool
  public let completeConversationAndAttachmentCoverageProven: Bool

  public init(
    locallyPersistedAttachmentCandidatesObserved: Bool,
    databaseOrAttachmentContentRead: Bool = false,
    messageAttachmentLinkageProven: Bool = false,
    completeConversationAndAttachmentCoverageProven: Bool = false
  ) {
    self.locallyPersistedAttachmentCandidatesObserved =
      locallyPersistedAttachmentCandidatesObserved
    self.databaseOrAttachmentContentRead = databaseOrAttachmentContentRead
    self.messageAttachmentLinkageProven = messageAttachmentLinkageProven
    self.completeConversationAndAttachmentCoverageProven =
      completeConversationAndAttachmentCoverageProven
  }
}

public struct WeChatAccountStorageAssessmentReport: Codable, Equatable, Sendable {
  public let reportFormatVersion: Int
  public let generatedAt: Date
  public let accounts: [WeChatAccountStorageEvidence]
  public let conclusions: WeChatAccountStorageAssessmentConclusions

  public init(
    reportFormatVersion: Int = 1,
    generatedAt: Date,
    accounts: [WeChatAccountStorageEvidence],
    conclusions: WeChatAccountStorageAssessmentConclusions
  ) {
    self.reportFormatVersion = reportFormatVersion
    self.generatedAt = generatedAt
    self.accounts = accounts
    self.conclusions = conclusions
  }
}

public struct WeChatAccountStorageAssessor: Sendable {
  private let homeDirectory: URL
  private let maxDepth: Int
  private let maxAttachmentFiles: Int
  private let classifier = ArtifactClassifier()

  public init(
    homeDirectory: URL = FileManager.default.homeDirectoryForCurrentUser,
    maxDepth: Int = 20,
    maxAttachmentFiles: Int = 100_000
  ) {
    self.homeDirectory = homeDirectory.standardizedFileURL
    self.maxDepth = max(0, maxDepth)
    self.maxAttachmentFiles = max(1, maxAttachmentFiles)
  }

  public func assess(accountID: String? = nil) -> WeChatAccountStorageAssessmentReport {
    let accounts = WeChatAccountDiscovery(homeDirectory: homeDirectory).resolvedAccounts()
      .filter { accountID == nil || $0.report.accountID == accountID }
      .map(assess)
      .sorted { $0.accountID < $1.accountID }
    return WeChatAccountStorageAssessmentReport(
      generatedAt: Date(),
      accounts: accounts,
      conclusions: WeChatAccountStorageAssessmentConclusions(
        locallyPersistedAttachmentCandidatesObserved: accounts.contains {
          $0.attachmentCandidateFileCount > 0
        })
    )
  }

  private func assess(_ account: WeChatAccountDiscovery.ResolvedAccount)
    -> WeChatAccountStorageEvidence
  {
    let databaseSets = DatabaseSetPlanner().findDatabaseSets(
      in: [account.databaseRoot],
      maxDepth: maxDepth
    )
    let logicalPaths = databaseSets.map(\.logicalPath)
    let attachment = assessAttachments(root: account.attachmentRoot)
    return WeChatAccountStorageEvidence(
      accountID: account.report.accountID,
      databaseSetCount: databaseSets.count,
      ordinaryMessageShardCandidateCount: logicalPaths.count {
        matchesNumberedStore($0, prefix: "message/message_")
      },
      businessMessageShardCandidateCount: logicalPaths.count {
        matchesNumberedStore($0, prefix: "message/biz_message_")
      },
      chatbotMessageStoreCandidateCount: logicalPaths.count {
        $0 == "chatbot/chatbot_message.db"
      },
      mediaDatabaseCandidateCount: logicalPaths.count {
        matchesNumberedStore($0, prefix: "message/media_")
      },
      messageResourceStoreCandidatePresent: logicalPaths.contains("message/message_resource.db"),
      contactStoreCandidatePresent: logicalPaths.contains("contact/contact.db"),
      sessionStoreCandidatePresent: logicalPaths.contains("session/session.db"),
      cachedMomentsStoreCandidatePresent: logicalPaths.contains("sns/sns.db"),
      attachmentRootPresent: account.attachmentRoot != nil,
      attachmentCandidateFileCount: attachment.fileCount,
      attachmentCandidateByteCount: attachment.byteCount,
      attachmentCandidates: AttachmentCandidateKind.allCases.compactMap { kind in
        guard let value = attachment.counts[kind], value.fileCount > 0 else { return nil }
        return AttachmentCandidateAggregate(
          kind: kind,
          fileCount: value.fileCount,
          byteCount: value.byteCount
        )
      },
      skippedSymbolicLinkCount: attachment.skippedSymbolicLinkCount,
      metadataIssueCount: attachment.metadataIssueCount,
      reachedAttachmentLimit: attachment.reachedLimit,
      attachmentEnumerationCompleteWithinRoot: !attachment.reachedLimit
        && attachment.metadataIssueCount == 0
    )
  }

  private struct AttachmentAssessment {
    var fileCount = 0
    var byteCount: Int64 = 0
    var counts: [AttachmentCandidateKind: (fileCount: Int, byteCount: Int64)] = [:]
    var skippedSymbolicLinkCount = 0
    var metadataIssueCount = 0
    var reachedLimit = false
  }

  private func assessAttachments(root: URL?) -> AttachmentAssessment {
    guard let root else { return AttachmentAssessment() }
    var result = AttachmentAssessment()
    guard
      let enumerator = FileManager.default.enumerator(
        at: root,
        includingPropertiesForKeys: [.isRegularFileKey, .isSymbolicLinkKey, .fileSizeKey],
        options: [.skipsPackageDescendants],
        errorHandler: { _, _ in
          result.metadataIssueCount += 1
          return true
        }
      )
    else {
      result.metadataIssueCount += 1
      return result
    }

    for case let url as URL in enumerator {
      if enumerator.level > maxDepth {
        enumerator.skipDescendants()
        continue
      }
      let values: URLResourceValues
      do {
        values = try url.resourceValues(forKeys: [
          .isRegularFileKey, .isSymbolicLinkKey, .fileSizeKey,
        ])
      } catch {
        result.metadataIssueCount += 1
        continue
      }
      if values.isSymbolicLink == true {
        result.skippedSymbolicLinkCount += 1
        enumerator.skipDescendants()
        continue
      }
      guard values.isRegularFile == true else { continue }
      let bytes = Int64(max(0, values.fileSize ?? 0))
      let kind = attachmentKind(for: url.lastPathComponent)
      result.fileCount += 1
      result.byteCount = addingWithoutOverflow(result.byteCount, bytes)
      let prior = result.counts[kind] ?? (0, 0)
      result.counts[kind] = (
        prior.fileCount + 1,
        addingWithoutOverflow(prior.byteCount, bytes)
      )
      if result.fileCount >= maxAttachmentFiles {
        result.reachedLimit = true
        break
      }
    }
    return result
  }

  private func attachmentKind(for fileName: String) -> AttachmentCandidateKind {
    switch classifier.classify(fileName: fileName) {
    case .image: .image
    case .audio: .audio
    case .video: .video
    case .document: .document
    case .serializedData: .serializedData
    case .configuration: .configuration
    default: .unclassified
    }
  }

  private func matchesNumberedStore(_ logicalPath: String, prefix: String) -> Bool {
    guard logicalPath.hasPrefix(prefix), logicalPath.hasSuffix(".db") else { return false }
    let start = logicalPath.index(logicalPath.startIndex, offsetBy: prefix.count)
    let end = logicalPath.index(logicalPath.endIndex, offsetBy: -3)
    guard start < end else { return false }
    return logicalPath[start..<end].allSatisfy(\.isNumber)
  }

  private func addingWithoutOverflow(_ left: Int64, _ right: Int64) -> Int64 {
    let (value, overflow) = left.addingReportingOverflow(right)
    return overflow ? Int64.max : value
  }
}
