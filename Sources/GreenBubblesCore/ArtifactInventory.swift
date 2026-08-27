import Foundation

public struct InventoryOptions: Sendable {
  public let maxDepth: Int
  public let maxArtifacts: Int
  public let includePaths: Bool

  public init(maxDepth: Int = 10, maxArtifacts: Int = 10_000, includePaths: Bool = false) {
    self.maxDepth = max(0, maxDepth)
    self.maxArtifacts = max(1, maxArtifacts)
    self.includePaths = includePaths
  }
}

public struct ArtifactInventory: Sendable {
  private let options: InventoryOptions
  private let classifier: ArtifactClassifier
  private let privacy: PathPrivacy

  public init(options: InventoryOptions = InventoryOptions()) {
    self.options = options
    self.classifier = ArtifactClassifier()
    self.privacy = PathPrivacy(includePaths: options.includePaths)
  }

  public func inventory(roots: [(url: URL, kind: DataRootKind)]) -> InventoryReport {
    let fileManager = FileManager.default
    var artifacts: [ArtifactMetadata] = []
    var issues: [InventoryIssue] = []
    var reachedLimit = false
    var reportedRoots: [CandidateDataRoot] = []

    for (root, kind) in roots {
      let standardizedRoot = root.standardizedFileURL
      let rootReference = privacy.reference(for: standardizedRoot)
      let readable = fileManager.isReadableFile(atPath: standardizedRoot.path)
      reportedRoots.append(
        CandidateDataRoot(
          location: rootReference,
          kind: kind,
          isReadable: readable
        ))

      guard readable, !reachedLimit else { continue }

      guard
        let enumerator = fileManager.enumerator(
          at: standardizedRoot,
          includingPropertiesForKeys: [
            .isRegularFileKey,
            .isDirectoryKey,
            .isSymbolicLinkKey,
            .fileSizeKey,
            .contentModificationDateKey,
          ],
          options: [.skipsPackageDescendants],
          errorHandler: { url, error in
            let nsError = error as NSError
            issues.append(
              InventoryIssue(
                locationID: privacy.reference(for: url).opaqueID,
                errorDomain: nsError.domain,
                errorCode: nsError.code
              ))
            return true
          }
        )
      else {
        issues.append(
          InventoryIssue(
            locationID: rootReference.opaqueID,
            errorDomain: NSCocoaErrorDomain,
            errorCode: CocoaError.fileReadUnknown.rawValue
          ))
        continue
      }

      for case let fileURL as URL in enumerator {
        if enumerator.level > options.maxDepth {
          enumerator.skipDescendants()
          continue
        }

        let values: URLResourceValues
        do {
          values = try fileURL.resourceValues(forKeys: [
            .isRegularFileKey,
            .isDirectoryKey,
            .isSymbolicLinkKey,
            .fileSizeKey,
            .contentModificationDateKey,
          ])
        } catch {
          let nsError = error as NSError
          issues.append(
            InventoryIssue(
              locationID: privacy.reference(for: fileURL).opaqueID,
              errorDomain: nsError.domain,
              errorCode: nsError.code
            ))
          continue
        }

        if values.isSymbolicLink == true {
          enumerator.skipDescendants()
          continue
        }
        guard values.isRegularFile == true,
          let kind = classifier.classify(fileName: fileURL.lastPathComponent)
        else {
          continue
        }

        artifacts.append(
          ArtifactMetadata(
            location: privacy.reference(for: fileURL),
            rootID: rootReference.opaqueID,
            kind: kind,
            byteCount: values.fileSize.map(Int64.init),
            modifiedAt: values.contentModificationDate
          ))

        if artifacts.count >= options.maxArtifacts {
          reachedLimit = true
          break
        }
      }
    }

    return InventoryReport(
      generatedAt: Date(),
      roots: reportedRoots,
      artifacts: artifacts.sorted { $0.location.opaqueID < $1.location.opaqueID },
      issues: issues,
      reachedArtifactLimit: reachedLimit
    )
  }
}
