import CryptoKit
import Foundation

/// Signed calibration profiles and the (macOS build x WeChat build)
/// compatibility matrix, mirroring `send_profile.rs`.
///
/// Both artifacts are data, not code: a WeChat layout change is fixed by
/// shipping a new signed profile rather than rebuilding the helper. They are
/// untrusted input until an Ed25519 signature made by a pinned release key
/// verifies over the canonical encoding, and anything unknown, unsigned,
/// expired, or bound to a different client build fails closed.
public enum CalibrationProfileConstants {
  public static let schemaVersion: UInt32 = 1
  public static let compatibilityMatrixSchemaVersion: UInt32 = 1
  public static let partsPerMillion: UInt32 = 1_000_000
  public static let maximumSignedArtifactBytes = 256 * 1024
  public static let maximumCompatibilityEntryCount = 4_096
}

/// A point inside a window, in integer parts-per-million of its size.
public struct WindowRelativePoint: Codable, Equatable, Sendable {
  public let xPartsPerMillion: UInt32
  public let yPartsPerMillion: UInt32

  public init(xPartsPerMillion: UInt32, yPartsPerMillion: UInt32) {
    self.xPartsPerMillion = xPartsPerMillion
    self.yPartsPerMillion = yPartsPerMillion
  }

  var isValid: Bool {
    xPartsPerMillion <= CalibrationProfileConstants.partsPerMillion
      && yPartsPerMillion <= CalibrationProfileConstants.partsPerMillion
  }
}

/// A rectangle inside a window, in integer parts-per-million.
public struct WindowRelativeRect: Codable, Equatable, Sendable {
  public let xPartsPerMillion: UInt32
  public let yPartsPerMillion: UInt32
  public let widthPartsPerMillion: UInt32
  public let heightPartsPerMillion: UInt32

  public init(
    xPartsPerMillion: UInt32,
    yPartsPerMillion: UInt32,
    widthPartsPerMillion: UInt32,
    heightPartsPerMillion: UInt32
  ) {
    self.xPartsPerMillion = xPartsPerMillion
    self.yPartsPerMillion = yPartsPerMillion
    self.widthPartsPerMillion = widthPartsPerMillion
    self.heightPartsPerMillion = heightPartsPerMillion
  }

  var isValid: Bool {
    let limit = UInt64(CalibrationProfileConstants.partsPerMillion)
    return widthPartsPerMillion > 0
      && heightPartsPerMillion > 0
      && UInt64(xPartsPerMillion) + UInt64(widthPartsPerMillion) <= limit
      && UInt64(yPartsPerMillion) + UInt64(heightPartsPerMillion) <= limit
  }
}

/// The three click targets the mechanical send skill needs. A mouse click only
/// ever focuses one of these; every mutation is performed with the keyboard.
public struct CalibrationAnchors: Codable, Equatable, Sendable {
  public let searchBox: WindowRelativePoint
  public let firstResultRow: WindowRelativePoint
  public let composeBox: WindowRelativePoint

  public init(
    searchBox: WindowRelativePoint,
    firstResultRow: WindowRelativePoint,
    composeBox: WindowRelativePoint
  ) {
    self.searchBox = searchBox
    self.firstResultRow = firstResultRow
    self.composeBox = composeBox
  }
}

/// The three capture regions the on-screen gates read with Apple Vision.
public struct CalibrationOCRRegions: Codable, Equatable, Sendable {
  public let title: WindowRelativeRect
  public let compose: WindowRelativeRect
  public let newestOutgoing: WindowRelativeRect

  public init(
    title: WindowRelativeRect,
    compose: WindowRelativeRect,
    newestOutgoing: WindowRelativeRect
  ) {
    self.title = title
    self.compose = compose
    self.newestOutgoing = newestOutgoing
  }
}

/// What the no-send calibration self-test must observe to pass.
public struct CalibrationSelfTestExpectation: Codable, Equatable, Sendable {
  public let focusIndicator: String
  public let minimumTitleConfidencePartsPerMillion: UInt32

  public init(focusIndicator: String, minimumTitleConfidencePartsPerMillion: UInt32) {
    self.focusIndicator = focusIndicator
    self.minimumTitleConfidencePartsPerMillion = minimumTitleConfidencePartsPerMillion
  }
}

/// The extra anchors and regions an attachment send needs. A profile without
/// this section cannot stage an attachment on that build, which is how
/// "attachments are unavailable until someone measures and signs them" is
/// expressed as data rather than as code.
public struct CalibrationAttachments: Codable, Equatable, Sendable {
  /// The compose-toolbar control that opens the file panel. Used only by the
  /// panel fallback; the pasteboard path needs no anchor at all.
  public let attachControl: WindowRelativePoint
  /// The confirm control on the send-confirmation sheet, when one is raised.
  public let confirmSendButton: WindowRelativePoint
  /// Where a staged attachment's name appears in the compose area.
  public let composeAttachment: WindowRelativeRect
  /// Where the confirmation sheet shows the file it is about to send.
  public let confirmSheet: WindowRelativeRect
  /// Whether this build raises a confirmation sheet at all.
  public let presentsConfirmationSheet: Bool
  /// Whether the compose box accepts a pasted file reference on this build.
  /// A profile that says false forces the panel fallback.
  public let composeAcceptsPastedFile: Bool

  public init(
    attachControl: WindowRelativePoint,
    confirmSendButton: WindowRelativePoint,
    composeAttachment: WindowRelativeRect,
    confirmSheet: WindowRelativeRect,
    presentsConfirmationSheet: Bool,
    composeAcceptsPastedFile: Bool
  ) {
    self.attachControl = attachControl
    self.confirmSendButton = confirmSendButton
    self.composeAttachment = composeAttachment
    self.confirmSheet = confirmSheet
    self.presentsConfirmationSheet = presentsConfirmationSheet
    self.composeAcceptsPastedFile = composeAcceptsPastedFile
  }

  var isValid: Bool {
    attachControl.isValid && confirmSendButton.isValid && composeAttachment.isValid
      && confirmSheet.isValid
  }
}

/// Everything the release key signs.
public struct CalibrationProfileBody: Codable, Equatable, Sendable {
  public let schema: UInt32
  public let profileID: String
  public let wechatBundleIdentifier: String
  public let wechatMarketingVersion: String
  public let wechatBuild: String
  public let clientBuildProfileID: String
  public let macosMajor: UInt32
  public let anchors: CalibrationAnchors
  public let ocrRegions: CalibrationOCRRegions
  public let selftest: CalibrationSelfTestExpectation
  /// Absent until someone has measured this build's attachment surface.
  public let attachments: CalibrationAttachments?
  public let issuedAtUnixSeconds: UInt64
  public let expiresAtUnixSeconds: UInt64

  enum CodingKeys: String, CodingKey {
    case schema
    case profileID = "profileId"
    case wechatBundleIdentifier
    case wechatMarketingVersion
    case wechatBuild
    case clientBuildProfileID = "clientBuildProfileId"
    case macosMajor
    case anchors
    case ocrRegions
    case selftest
    case attachments
    case issuedAtUnixSeconds
    case expiresAtUnixSeconds
  }

  public init(
    schema: UInt32,
    profileID: String,
    wechatBundleIdentifier: String,
    wechatMarketingVersion: String,
    wechatBuild: String,
    clientBuildProfileID: String,
    macosMajor: UInt32,
    anchors: CalibrationAnchors,
    ocrRegions: CalibrationOCRRegions,
    selftest: CalibrationSelfTestExpectation,
    attachments: CalibrationAttachments? = nil,
    issuedAtUnixSeconds: UInt64,
    expiresAtUnixSeconds: UInt64
  ) {
    self.schema = schema
    self.profileID = profileID
    self.wechatBundleIdentifier = wechatBundleIdentifier
    self.wechatMarketingVersion = wechatMarketingVersion
    self.wechatBuild = wechatBuild
    self.clientBuildProfileID = clientBuildProfileID
    self.macosMajor = macosMajor
    self.anchors = anchors
    self.ocrRegions = ocrRegions
    self.selftest = selftest
    self.attachments = attachments
    self.issuedAtUnixSeconds = issuedAtUnixSeconds
    self.expiresAtUnixSeconds = expiresAtUnixSeconds
  }

  /// The exact bytes a release key signs, mirroring
  /// `calibration_profile_signing_bytes`.
  public var signingBytes: Data? {
    var writer = CanonicalWriter(domain: "greenbubbles.send.calibration-profile.v1")
    writer.number(UInt64(schema))
    writer.text(profileID)
    writer.text(wechatBundleIdentifier)
    writer.text(wechatMarketingVersion)
    writer.text(wechatBuild)
    writer.text(clientBuildProfileID)
    writer.number(UInt64(macosMajor))
    Self.append(&writer, "anchor.searchBox", anchors.searchBox)
    Self.append(&writer, "anchor.firstResultRow", anchors.firstResultRow)
    Self.append(&writer, "anchor.composeBox", anchors.composeBox)
    Self.append(&writer, "region.title", ocrRegions.title)
    Self.append(&writer, "region.compose", ocrRegions.compose)
    Self.append(&writer, "region.newestOutgoing", ocrRegions.newestOutgoing)
    writer.text("selftest.focusIndicator")
    writer.text(selftest.focusIndicator)
    writer.number(UInt64(selftest.minimumTitleConfidencePartsPerMillion))
    writer.flag(attachments != nil)
    if let attachments {
      Self.append(&writer, "anchor.attachControl", attachments.attachControl)
      Self.append(&writer, "anchor.confirmSendButton", attachments.confirmSendButton)
      Self.append(&writer, "region.composeAttachment", attachments.composeAttachment)
      Self.append(&writer, "region.confirmSheet", attachments.confirmSheet)
      writer.flag(attachments.presentsConfirmationSheet)
      writer.flag(attachments.composeAcceptsPastedFile)
    }
    writer.number(issuedAtUnixSeconds)
    writer.number(expiresAtUnixSeconds)
    return writer.finish()
  }

  var isStructurallyValid: Bool {
    schema == CalibrationProfileConstants.schemaVersion
      && !profileID.isEmpty
      && profileID.count <= 128
      && !wechatBundleIdentifier.isEmpty
      && !wechatMarketingVersion.isEmpty
      && !wechatBuild.isEmpty
      && !clientBuildProfileID.isEmpty
      && macosMajor >= 10
      && anchors.searchBox.isValid
      && anchors.firstResultRow.isValid
      && anchors.composeBox.isValid
      && ocrRegions.title.isValid
      && ocrRegions.compose.isValid
      && ocrRegions.newestOutgoing.isValid
      && !selftest.focusIndicator.isEmpty
      && selftest.minimumTitleConfidencePartsPerMillion
        <= CalibrationProfileConstants.partsPerMillion
      && (attachments?.isValid ?? true)
      && issuedAtUnixSeconds < expiresAtUnixSeconds
  }

  private static func append(
    _ writer: inout CanonicalWriter,
    _ name: String,
    _ point: WindowRelativePoint
  ) {
    writer.text(name)
    writer.number(UInt64(point.xPartsPerMillion))
    writer.number(UInt64(point.yPartsPerMillion))
  }

  private static func append(
    _ writer: inout CanonicalWriter,
    _ name: String,
    _ rect: WindowRelativeRect
  ) {
    writer.text(name)
    writer.number(UInt64(rect.xPartsPerMillion))
    writer.number(UInt64(rect.yPartsPerMillion))
    writer.number(UInt64(rect.widthPartsPerMillion))
    writer.number(UInt64(rect.heightPartsPerMillion))
  }
}

/// A calibration profile plus its detached hexadecimal Ed25519 signature. The
/// signature is a sibling of the body's fields in the JSON document, matching
/// the Rust `#[serde(flatten)]` representation.
public struct SignedCalibrationProfile: Codable, Equatable, Sendable {
  public let body: CalibrationProfileBody
  public let signature: String

  public init(body: CalibrationProfileBody, signature: String) {
    self.body = body
    self.signature = signature
  }

  private enum SignatureKey: String, CodingKey { case signature }

  public init(from decoder: Decoder) throws {
    body = try CalibrationProfileBody(from: decoder)
    signature = try decoder.container(keyedBy: SignatureKey.self)
      .decode(String.self, forKey: .signature)
  }

  public func encode(to encoder: Encoder) throws {
    try body.encode(to: encoder)
    var container = encoder.container(keyedBy: SignatureKey.self)
    try container.encode(signature, forKey: .signature)
  }
}

/// State of one (macOS build x WeChat build) combination.
public enum CompatibilityState: String, Codable, Sendable {
  case supported
  case unverified
  case blocked

  /// Only `supported` may open the send path.
  public var permitsSend: Bool { self == .supported }
}

/// One row of the compatibility matrix.
public struct CompatibilityEntry: Codable, Equatable, Sendable {
  public let macosBuild: String
  public let macosMajor: UInt32
  public let wechatBuild: String
  public let clientBuildProfileID: String
  public let state: CompatibilityState
  public let calibrationProfileID: String
  public let note: String

  enum CodingKeys: String, CodingKey {
    case macosBuild
    case macosMajor
    case wechatBuild
    case clientBuildProfileID = "clientBuildProfileId"
    case state
    case calibrationProfileID = "calibrationProfileId"
    case note
  }

  public init(
    macosBuild: String,
    macosMajor: UInt32,
    wechatBuild: String,
    clientBuildProfileID: String,
    state: CompatibilityState,
    calibrationProfileID: String,
    note: String
  ) {
    self.macosBuild = macosBuild
    self.macosMajor = macosMajor
    self.wechatBuild = wechatBuild
    self.clientBuildProfileID = clientBuildProfileID
    self.state = state
    self.calibrationProfileID = calibrationProfileID
    self.note = note
  }
}

/// Everything the release key signs for a compatibility matrix.
public struct CompatibilityMatrixBody: Codable, Equatable, Sendable {
  public let schema: UInt32
  public let matrixID: String
  public let issuedAtUnixSeconds: UInt64
  public let expiresAtUnixSeconds: UInt64
  /// The field kill switch. Because the matrix is signed and updatable out of
  /// band, publishing one with this set disables the send path everywhere
  /// without shipping an application update; letting a matrix expire does the
  /// same thing passively.
  public let globalKillSwitchEngaged: Bool
  public let entries: [CompatibilityEntry]

  enum CodingKeys: String, CodingKey {
    case schema
    case matrixID = "matrixId"
    case issuedAtUnixSeconds
    case expiresAtUnixSeconds
    case globalKillSwitchEngaged
    case entries
  }

  public init(
    schema: UInt32,
    matrixID: String,
    issuedAtUnixSeconds: UInt64,
    expiresAtUnixSeconds: UInt64,
    globalKillSwitchEngaged: Bool,
    entries: [CompatibilityEntry]
  ) {
    self.schema = schema
    self.matrixID = matrixID
    self.issuedAtUnixSeconds = issuedAtUnixSeconds
    self.expiresAtUnixSeconds = expiresAtUnixSeconds
    self.globalKillSwitchEngaged = globalKillSwitchEngaged
    self.entries = entries
  }

  /// The exact bytes a release key signs.
  public var signingBytes: Data? {
    var writer = CanonicalWriter(domain: "greenbubbles.send.compatibility-matrix.v1")
    writer.number(UInt64(schema))
    writer.text(matrixID)
    writer.number(issuedAtUnixSeconds)
    writer.number(expiresAtUnixSeconds)
    writer.flag(globalKillSwitchEngaged)
    writer.number(UInt64(entries.count))
    for entry in entries {
      writer.text(entry.macosBuild)
      writer.number(UInt64(entry.macosMajor))
      writer.text(entry.wechatBuild)
      writer.text(entry.clientBuildProfileID)
      writer.text(entry.state.rawValue)
      writer.text(entry.calibrationProfileID)
      writer.text(entry.note)
    }
    return writer.finish()
  }

  var isStructurallyValid: Bool {
    guard
      schema == CalibrationProfileConstants.compatibilityMatrixSchemaVersion,
      !matrixID.isEmpty,
      issuedAtUnixSeconds < expiresAtUnixSeconds,
      !entries.isEmpty,
      entries.count <= CalibrationProfileConstants.maximumCompatibilityEntryCount
    else { return false }
    var previous: (String, String)?
    for entry in entries {
      guard
        !entry.macosBuild.isEmpty,
        !entry.wechatBuild.isEmpty,
        !entry.clientBuildProfileID.isEmpty,
        entry.macosMajor >= 10,
        entry.state != .supported || !entry.calibrationProfileID.isEmpty
      else { return false }
      let key = (entry.macosBuild, entry.wechatBuild)
      if let previous, previous >= key { return false }
      previous = key
    }
    return true
  }
}

/// A compatibility matrix plus its detached hexadecimal Ed25519 signature.
public struct SignedCompatibilityMatrix: Codable, Equatable, Sendable {
  public let body: CompatibilityMatrixBody
  public let signature: String

  private enum SignatureKey: String, CodingKey { case signature }

  public init(body: CompatibilityMatrixBody, signature: String) {
    self.body = body
    self.signature = signature
  }

  public init(from decoder: Decoder) throws {
    body = try CompatibilityMatrixBody(from: decoder)
    signature = try decoder.container(keyedBy: SignatureKey.self)
      .decode(String.self, forKey: .signature)
  }

  public func encode(to encoder: Encoder) throws {
    try body.encode(to: encoder)
    var container = encoder.container(keyedBy: SignatureKey.self)
    try container.encode(signature, forKey: .signature)
  }
}

/// Which key verified a signed artifact. Development keys never unlock a
/// rollout stage that can press Return.
public enum SendTrustTier: String, Codable, Sendable {
  case release
  case development
}

/// Machine-readable reasons a signed send artifact was refused.
public enum SignedArtifactDenial: String, Error, Equatable, Sendable {
  case trustRootEmpty
  case trustRootMalformed
  case schemaUnsupported
  case structurallyInvalid
  case signatureMalformed
  case signatureNotVerified
  case notYetValid
  case expired
  case clientBuildMismatch
  case hostBuildMismatch
  case profileNotInMatrix
  case combinationNotSupported
}

/// Where a verifying key came from. Release keys are pinned into the binary at
/// build time; development keys must be named explicitly.
public struct SendTrustRoot: Codable, Equatable, Sendable {
  public var releasePublicKeys: [String]
  public var developmentPublicKeys: [String]

  public init(releasePublicKeys: [String] = [], developmentPublicKeys: [String] = []) {
    self.releasePublicKeys = releasePublicKeys
    self.developmentPublicKeys = developmentPublicKeys
  }

  /// The release keys pinned into this build. An empty set is the safe
  /// default: without a provisioned release key no release-signed profile
  /// verifies and the send path cannot open.
  public static var pinned: SendTrustRoot {
    let pinned = SendReleaseTrust.pinnedReleasePublicKeys
    return SendTrustRoot(releasePublicKeys: pinned, developmentPublicKeys: [])
  }

  func verifyingKeys(_ tier: SendTrustTier) throws(SignedArtifactDenial)
    -> [Curve25519.Signing.PublicKey]
  {
    let encoded = tier == .release ? releasePublicKeys : developmentPublicKeys
    var keys: [Curve25519.Signing.PublicKey] = []
    for value in encoded {
      guard let raw = Data(hexadecimal: value), raw.count == 32,
        let key = try? Curve25519.Signing.PublicKey(rawRepresentation: raw)
      else { throw SignedArtifactDenial.trustRootMalformed }
      keys.append(key)
    }
    return keys
  }

  func verify(message: Data, signature: String) throws(SignedArtifactDenial) -> SendTrustTier {
    guard let signatureBytes = Data(hexadecimal: signature), signatureBytes.count == 64 else {
      throw SignedArtifactDenial.signatureMalformed
    }
    guard !releasePublicKeys.isEmpty || !developmentPublicKeys.isEmpty else {
      throw SignedArtifactDenial.trustRootEmpty
    }
    for tier in [SendTrustTier.release, .development] {
      for key in try verifyingKeys(tier)
      where key.isValidSignature(signatureBytes, for: message) {
        return tier
      }
    }
    throw SignedArtifactDenial.signatureNotVerified
  }
}

/// The result of verifying a signed calibration profile.
public struct VerifiedCalibrationProfile: Equatable, Sendable {
  public let profile: SignedCalibrationProfile
  public let trustTier: SendTrustTier
  public let canonicalSHA256: String
}

/// The result of verifying a signed compatibility matrix.
public struct VerifiedCompatibilityMatrix: Equatable, Sendable {
  public let matrix: SignedCompatibilityMatrix
  public let trustTier: SendTrustTier
  public let canonicalSHA256: String
}

/// The compatibility decision for one host and client build.
public struct CompatibilityDecision: Equatable, Sendable {
  public let macosBuild: String
  public let wechatBuild: String
  public let state: CompatibilityState
  public let knownCombination: Bool
  /// Copied from the signed matrix so a caller cannot look at a combination
  /// without also seeing the field kill switch.
  public let fieldKillSwitchEngaged: Bool
  public let expectedCalibrationProfileID: String
  public let clientBuildProfileID: String
  public let note: String

  public init(
    macosBuild: String,
    wechatBuild: String,
    state: CompatibilityState,
    knownCombination: Bool,
    fieldKillSwitchEngaged: Bool,
    expectedCalibrationProfileID: String,
    clientBuildProfileID: String,
    note: String
  ) {
    self.macosBuild = macosBuild
    self.wechatBuild = wechatBuild
    self.state = state
    self.knownCombination = knownCombination
    self.fieldKillSwitchEngaged = fieldKillSwitchEngaged
    self.expectedCalibrationProfileID = expectedCalibrationProfileID
    self.clientBuildProfileID = clientBuildProfileID
    self.note = note
  }
}

/// Verification entry points. Every failure mode is an explicit denial; there
/// is no partial acceptance and no "warn but continue" path.
public enum CalibrationProfileVerifier {
  /// Verifies a signed calibration profile against a trust root and the clock.
  public static func verify(
    _ profile: SignedCalibrationProfile,
    trustRoot: SendTrustRoot,
    nowUnixSeconds: UInt64
  ) throws(SignedArtifactDenial) -> VerifiedCalibrationProfile {
    guard profile.body.schema == CalibrationProfileConstants.schemaVersion else {
      throw SignedArtifactDenial.schemaUnsupported
    }
    guard profile.body.isStructurallyValid, let message = profile.body.signingBytes else {
      throw SignedArtifactDenial.structurallyInvalid
    }
    let tier = try trustRoot.verify(message: message, signature: profile.signature)
    guard nowUnixSeconds >= profile.body.issuedAtUnixSeconds else {
      throw SignedArtifactDenial.notYetValid
    }
    guard nowUnixSeconds < profile.body.expiresAtUnixSeconds else {
      throw SignedArtifactDenial.expired
    }
    return VerifiedCalibrationProfile(
      profile: profile,
      trustTier: tier,
      canonicalSHA256: SendDigest.sha256Hex(message)
    )
  }

  /// Verifies a signed compatibility matrix against a trust root and the clock.
  public static func verify(
    _ matrix: SignedCompatibilityMatrix,
    trustRoot: SendTrustRoot,
    nowUnixSeconds: UInt64
  ) throws(SignedArtifactDenial) -> VerifiedCompatibilityMatrix {
    guard matrix.body.schema == CalibrationProfileConstants.compatibilityMatrixSchemaVersion else {
      throw SignedArtifactDenial.schemaUnsupported
    }
    guard matrix.body.isStructurallyValid, let message = matrix.body.signingBytes else {
      throw SignedArtifactDenial.structurallyInvalid
    }
    let tier = try trustRoot.verify(message: message, signature: matrix.signature)
    guard nowUnixSeconds >= matrix.body.issuedAtUnixSeconds else {
      throw SignedArtifactDenial.notYetValid
    }
    guard nowUnixSeconds < matrix.body.expiresAtUnixSeconds else {
      throw SignedArtifactDenial.expired
    }
    return VerifiedCompatibilityMatrix(
      matrix: matrix,
      trustTier: tier,
      canonicalSHA256: SendDigest.sha256Hex(message)
    )
  }

  /// Looks one (macOS build x WeChat build) combination up. An unknown
  /// combination is reported as `unverified`, which never permits a send.
  public static func decision(
    in matrix: VerifiedCompatibilityMatrix,
    macosBuild: String,
    wechatBuild: String
  ) -> CompatibilityDecision {
    guard
      let entry = matrix.matrix.body.entries.first(where: {
        $0.macosBuild == macosBuild && $0.wechatBuild == wechatBuild
      })
    else {
      return CompatibilityDecision(
        macosBuild: macosBuild,
        wechatBuild: wechatBuild,
        state: .unverified,
        knownCombination: false,
        fieldKillSwitchEngaged: matrix.matrix.body.globalKillSwitchEngaged,
        expectedCalibrationProfileID: "",
        clientBuildProfileID: "",
        note: "combination is absent from the signed compatibility matrix"
      )
    }
    return CompatibilityDecision(
      macosBuild: macosBuild,
      wechatBuild: wechatBuild,
      state: entry.state,
      knownCombination: true,
      fieldKillSwitchEngaged: matrix.matrix.body.globalKillSwitchEngaged,
      expectedCalibrationProfileID: entry.calibrationProfileID,
      clientBuildProfileID: entry.clientBuildProfileID,
      note: entry.note
    )
  }

  /// Binds a verified profile to a verified compatibility decision.
  public static func bind(
    profile: VerifiedCalibrationProfile,
    to decision: CompatibilityDecision,
    expectedMacosMajor: UInt32
  ) throws(SignedArtifactDenial) {
    guard
      !decision.fieldKillSwitchEngaged,
      decision.knownCombination,
      decision.state.permitsSend
    else { throw SignedArtifactDenial.combinationNotSupported }
    guard decision.expectedCalibrationProfileID == profile.profile.body.profileID else {
      throw SignedArtifactDenial.profileNotInMatrix
    }
    guard
      decision.clientBuildProfileID == profile.profile.body.clientBuildProfileID,
      decision.wechatBuild == profile.profile.body.wechatBuild
    else { throw SignedArtifactDenial.clientBuildMismatch }
    guard profile.profile.body.macosMajor == expectedMacosMajor else {
      throw SignedArtifactDenial.hostBuildMismatch
    }
  }
}

extension Data {
  /// Decodes an even-length lowercase or uppercase hexadecimal string.
  init?(hexadecimal: String) {
    let characters = Array(hexadecimal)
    guard characters.count % 2 == 0 else { return nil }
    var bytes = [UInt8]()
    bytes.reserveCapacity(characters.count / 2)
    var index = 0
    while index < characters.count {
      guard let byte = UInt8(String(characters[index...index + 1]), radix: 16) else { return nil }
      bytes.append(byte)
      index += 2
    }
    self = Data(bytes)
  }
}
