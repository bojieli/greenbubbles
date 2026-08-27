// Derived from wcdb-key-tool (https://github.com/TANGandXUE/wcdb-key-tool), MIT license.

import Foundation
import GreenBubblesCore

/// Aggregate-only capture outcome. Contains no secret material and no absolute
/// paths unless the owner passed `--include-paths`.
public struct AcquisitionReport: Codable, Equatable, Sendable {
  public let formatVersion: Int
  public let capturedAt: Date
  public let captureDurationSeconds: Double
  public let databaseCount: Int
  public let distinctSaltCount: Int
  public let verifiedSaltCount: Int
  public let clientMarketingVersion: String?
  public let clientBuildVersion: String?
  public let clientReSigned: Bool
  public let databaseRoot: PathReference?
  public let outputWritten: Bool

  public init(
    formatVersion: Int = 1,
    capturedAt: Date,
    captureDurationSeconds: Double,
    databaseCount: Int,
    distinctSaltCount: Int,
    verifiedSaltCount: Int,
    clientMarketingVersion: String?,
    clientBuildVersion: String?,
    clientReSigned: Bool,
    databaseRoot: PathReference?,
    outputWritten: Bool
  ) {
    self.formatVersion = formatVersion
    self.capturedAt = capturedAt
    self.captureDurationSeconds = captureDurationSeconds
    self.databaseCount = databaseCount
    self.distinctSaltCount = distinctSaltCount
    self.verifiedSaltCount = verifiedSaltCount
    self.clientMarketingVersion = clientMarketingVersion
    self.clientBuildVersion = clientBuildVersion
    self.clientReSigned = clientReSigned
    self.databaseRoot = databaseRoot
    self.outputWritten = outputWritten
  }
}
