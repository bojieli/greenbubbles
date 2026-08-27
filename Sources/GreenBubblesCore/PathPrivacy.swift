import CryptoKit
import Foundation

public struct PathPrivacy: Sendable {
  public let includePaths: Bool

  public init(includePaths: Bool = false) {
    self.includePaths = includePaths
  }

  public func reference(for url: URL) -> PathReference {
    let canonicalPath = url.standardizedFileURL.path
    let digest = SHA256.hash(data: Data(canonicalPath.utf8))
    let identifier = digest.prefix(12).map { String(format: "%02x", $0) }.joined()
    return PathReference(
      opaqueID: identifier,
      path: includePaths ? canonicalPath : nil
    )
  }
}
