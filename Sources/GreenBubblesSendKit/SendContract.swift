import Foundation

/// The vocabulary shared with the Rust control plane. Every declaration here
/// mirrors `Native/GreenBubblesRestore/src/send_contract.rs`; the encodings are
/// pinned in both languages by `docs/send-canonical-vectors.json`.
public enum SendContract {
  /// Format version of every envelope in this module.
  public static let version: UInt32 = 1
  /// Largest body a single text send may carry.
  public static let maximumBodyBytes = 4_096
  /// Largest search key the addressing step may type.
  public static let maximumSearchKeyBytes = 256
  /// Largest recipient title the recipient gate may compare.
  public static let maximumExpectedTitleBytes = 256
  /// Largest attachment the adapter will stage and send.
  public static let maximumAttachmentBytes: UInt64 = 100 * 1024 * 1024
  /// Largest display file name a staged attachment may carry.
  public static let maximumDisplayFileNameBytes = 255
}

/// The reviewed capabilities. Text carries a body; image and file carry an
/// attachment. Mirrors `ActionCapability` in `action.rs`.
public enum ActionCapability: String, Codable, Sendable, CaseIterable {
  case textSend
  case replySend
  case imageSend
  case fileSend

  /// Whether this capability carries a file rather than text.
  public var carriesAttachment: Bool { self == .imageSend || self == .fileSend }

  /// Whether the client transmits the approved bytes unchanged. False for
  /// images, which the client re-encodes, so no record may call the result a
  /// byte-for-byte match.
  public var preservesBytes: Bool { self != .imageSend }
}

/// One staged local attachment. The control plane copied the approved file into
/// a single-use directory and hashed *that copy*, so this digest describes the
/// exact bytes about to be handed over.
///
/// The helper never reads the file: it writes a *reference* to the pasteboard,
/// so the bytes travel from the filesystem to WeChat without passing through
/// the process that holds the input and capture grants.
public struct ActionAttachment: Codable, Equatable, Sendable {
  public let stagingDirectory: String
  public let stagedPath: String
  public let displayFileName: String
  public let byteCount: UInt64
  public let sha256: String
  public let uniformTypeIdentifier: String

  public init(
    stagingDirectory: String,
    stagedPath: String,
    displayFileName: String,
    byteCount: UInt64,
    sha256: String,
    uniformTypeIdentifier: String
  ) {
    self.stagingDirectory = stagingDirectory
    self.stagedPath = stagedPath
    self.displayFileName = displayFileName
    self.byteCount = byteCount
    self.sha256 = sha256
    self.uniformTypeIdentifier = uniformTypeIdentifier
  }

  /// The containment check is what stops a compromised control plane from
  /// pointing the helper at an arbitrary file: the staged path must be exactly
  /// the approved name inside the directory minted for this one action.
  public func validate() throws(SendFailure) {
    let nameValid =
      !displayFileName.isEmpty
      && displayFileName.utf8.count <= SendContract.maximumDisplayFileNameBytes
      && !displayFileName.contains("/")
      && !displayFileName.contains("\0")
      && displayFileName != "." && displayFileName != ".."
    guard
      nameValid,
      SendDigest.isSHA256Hex(sha256),
      byteCount > 0,
      byteCount <= SendContract.maximumAttachmentBytes,
      !uniformTypeIdentifier.isEmpty,
      stagingDirectory.hasPrefix("/"),
      !stagingDirectory.contains("/.."),
      stagedPath == "\(stagingDirectory)/\(displayFileName)"
    else {
      throw SendFailure(.attachmentInvalid, detail: "the staged attachment is not self-consistent")
    }
  }
}

/// How far the phased rollout has been opened. Only the control plane decides
/// this; the helper receives the decision inside the capability.
public enum SendRolloutStage: String, Codable, Sendable, CaseIterable {
  case dryRun
  case selfSend
  case allowListed

  /// Whether this stage may ever press Return.
  public var permitsReturn: Bool { self != .dryRun }
}

/// The step of the mechanical send skill that a run reached.
public enum SendStage: String, Codable, Sendable, CaseIterable {
  case precheck
  case calibrate
  case address
  case recipientVerify
  case compose
  case contentVerify
  case send
  case sendVerify
}

/// What the helper's own capture proved immediately after Return. Never a
/// delivery verdict: `observedSent` is created only by replica reconciliation
/// in the control plane.
public enum VisualConfirmation: String, Codable, Sendable {
  case notAttempted
  case confirmed
  case unconfirmed
}

/// The user-facing failure taxonomy. Every case keeps the send path closed.
public enum SendFailureCode: String, Codable, Sendable, CaseIterable {
  case grantsMissing
  case wechatNotRunning
  case notLoggedIn
  case unknownBuild
  case calibrationDrift
  case recipientVerifyFailed
  case contentVerifyFailed
  case attachmentInvalid
  case attachmentStagingFailed
  case attachmentDigestMismatch
  case attachmentVerifyFailed
  case attachPanelNotPresented
  case unsupportedAttachmentType
  case sendUnconfirmed
  case engineStall
  case engineUnavailable
  case humanCollision
  case manifestViolation
  case windowNotFound
  case killSwitchEngaged
  case stageNotPermitted
  case configurationInvalid
  case profileInvalid
  case draftInvalid
  case approvalInvalid
  case capabilityExpired
  case capabilityMismatch
  case idempotencyConflict
  case rateLimited
  case circuitOpen
  case outboxBusy
  case reconciliationPending

  /// The one action that resolves this failure.
  public var operatorAction: String {
    switch self {
    case .grantsMissing:
      "Grant Accessibility and Screen Recording to GreenBubblesInputHelper, then re-run the capability probe."
    case .wechatNotRunning: "Launch WeChat and leave it running in the background."
    case .notLoggedIn: "Log in to WeChat on this Mac."
    case .unknownBuild:
      "This macOS/WeChat build pair is not in the signed compatibility matrix."
    case .calibrationDrift:
      "No verified calibration profile is active for this client build; run `send selftest`, and install a signed profile for this WeChat build if it fails."
    case .recipientVerifyFailed:
      "The opened conversation did not match the approved recipient."
    case .contentVerifyFailed: "The composed text did not match the approved body."
    case .attachmentInvalid:
      "The attachment is malformed, too large, or names a path outside its staging directory."
    case .attachmentStagingFailed:
      "The approved file could not be staged; check that it is an owner-only regular file."
    case .attachmentDigestMismatch:
      "The file no longer matches the digest the draft approved; approve a new draft."
    case .attachmentVerifyFailed:
      "The staged attachment's name was not read back from the compose area; nothing was sent."
    case .attachPanelNotPresented:
      "The attach control did not open a file panel; the click was abandoned."
    case .unsupportedAttachmentType:
      "This attachment type is not in the reviewed set for the active calibration profile."
    case .sendUnconfirmed:
      "Return was pressed but the result is unproven; reconcile before retrying."
    case .engineStall: "The input helper stalled and was abandoned."
    case .engineUnavailable: "The input helper is not reachable."
    case .humanCollision: "Real user activity was observed on WeChat; the attempt yielded."
    case .manifestViolation:
      "The helper refused an action outside its WeChat-scoped capability manifest."
    case .windowNotFound: "WeChat's main window was not found on screen."
    case .killSwitchEngaged: "The send path is disabled by the kill switch."
    case .stageNotPermitted: "The rollout stage does not permit sending to this conversation."
    case .configurationInvalid: "The send adapter configuration is invalid."
    case .profileInvalid:
      "The calibration profile is missing, unsigned, expired, or bound to another build."
    case .draftInvalid: "The draft is missing, malformed, stale, or expired."
    case .approvalInvalid: "The approval evidence is missing, malformed, expired, or consumed."
    case .capabilityExpired: "The minted capability expired before it reached the helper."
    case .capabilityMismatch: "The outcome does not match the capability it was given."
    case .idempotencyConflict: "This idempotency key was already reserved."
    case .rateLimited: "The attempt window has no remaining capacity."
    case .circuitOpen: "The circuit breaker is open after consecutive failures."
    case .outboxBusy: "Another attempt is already in flight."
    case .reconciliationPending: "A previous attempt is still awaiting reconciliation."
    }
  }
}

/// A failure that carries its taxonomy code across the helper boundary.
public struct SendFailure: Error, Equatable, Sendable {
  public let code: SendFailureCode
  public let detail: String

  public init(_ code: SendFailureCode, detail: String = "") {
    self.code = code
    self.detail = detail
  }

  public var operatorAction: String { code.operatorAction }
}

/// The single-use, bound action capability handed to the helper. It carries no
/// key, no replica handle, and no policy: the control plane already resolved
/// the recipient, so the helper enforces the recipient gate with nothing but
/// this document and its own capture.
public struct ActionCapabilityEnvelope: Codable, Equatable, Sendable {
  public let formatVersion: UInt32
  public let capabilityID: String
  public let actionID: String
  public let draftID: String
  public let approvalID: String
  public let idempotencyKey: String
  public let accountID: String
  public let conversationID: String
  /// Which reviewed capability this action exercises.
  public let capability: ActionCapability
  public let searchKey: String
  public let expectedTitle: String
  public let body: String
  public let bodySHA256: String
  public let normalizedBodySHA256: String
  public let clientBuildProfileID: String
  public let calibrationProfileID: String
  public let calibrationProfileSHA256: String
  /// Present exactly when `capability` carries an attachment.
  public let attachment: ActionAttachment?
  public let rolloutStage: SendRolloutStage
  public let permitSend: Bool
  public let issuedAtUnixNanoseconds: UInt64
  public let validUntilUnixNanoseconds: UInt64
  public let bindingSHA256: String

  enum CodingKeys: String, CodingKey {
    case formatVersion
    case capabilityID = "capabilityId"
    case actionID = "actionId"
    case draftID = "draftId"
    case approvalID = "approvalId"
    case idempotencyKey
    case accountID = "accountId"
    case conversationID = "conversationId"
    case searchKey
    case expectedTitle
    case body
    case bodySHA256 = "bodySha256"
    case normalizedBodySHA256 = "normalizedBodySha256"
    case clientBuildProfileID = "clientBuildProfileId"
    case calibrationProfileID = "calibrationProfileId"
    case calibrationProfileSHA256 = "calibrationProfileSha256"
    case capability
    case attachment
    case rolloutStage
    case permitSend
    case issuedAtUnixNanoseconds
    case validUntilUnixNanoseconds
    case bindingSHA256 = "bindingSha256"
  }

  public init(
    formatVersion: UInt32,
    capabilityID: String,
    actionID: String,
    draftID: String,
    approvalID: String,
    idempotencyKey: String,
    accountID: String,
    conversationID: String,
    capability: ActionCapability,
    searchKey: String,
    expectedTitle: String,
    body: String,
    bodySHA256: String,
    normalizedBodySHA256: String,
    clientBuildProfileID: String,
    calibrationProfileID: String,
    calibrationProfileSHA256: String,
    attachment: ActionAttachment?,
    rolloutStage: SendRolloutStage,
    permitSend: Bool,
    issuedAtUnixNanoseconds: UInt64,
    validUntilUnixNanoseconds: UInt64,
    bindingSHA256: String
  ) {
    self.formatVersion = formatVersion
    self.capabilityID = capabilityID
    self.actionID = actionID
    self.draftID = draftID
    self.approvalID = approvalID
    self.idempotencyKey = idempotencyKey
    self.accountID = accountID
    self.conversationID = conversationID
    self.capability = capability
    self.searchKey = searchKey
    self.expectedTitle = expectedTitle
    self.body = body
    self.bodySHA256 = bodySHA256
    self.normalizedBodySHA256 = normalizedBodySHA256
    self.clientBuildProfileID = clientBuildProfileID
    self.calibrationProfileID = calibrationProfileID
    self.calibrationProfileSHA256 = calibrationProfileSHA256
    self.attachment = attachment
    self.rolloutStage = rolloutStage
    self.permitSend = permitSend
    self.issuedAtUnixNanoseconds = issuedAtUnixNanoseconds
    self.validUntilUnixNanoseconds = validUntilUnixNanoseconds
    self.bindingSHA256 = bindingSHA256
  }

  /// The exact bytes summarized by `bindingSHA256`, mirroring the Rust
  /// `capability_binding_bytes`.
  public var bindingBytes: Data? {
    var writer = CanonicalWriter(domain: "greenbubbles.send.action-capability.v1")
    writer.number(UInt64(formatVersion))
    writer.text(capabilityID)
    writer.text(actionID)
    writer.text(draftID)
    writer.text(approvalID)
    writer.text(idempotencyKey)
    writer.text(accountID)
    writer.text(conversationID)
    writer.text(capability.rawValue)
    writer.text(searchKey)
    writer.text(expectedTitle)
    writer.text(bodySHA256)
    writer.text(normalizedBodySHA256)
    writer.text(clientBuildProfileID)
    writer.text(calibrationProfileID)
    writer.text(calibrationProfileSHA256)
    writer.flag(attachment != nil)
    if let attachment {
      writer.text(attachment.stagingDirectory)
      writer.text(attachment.stagedPath)
      writer.text(attachment.displayFileName)
      writer.number(attachment.byteCount)
      writer.text(attachment.sha256)
      writer.text(attachment.uniformTypeIdentifier)
    }
    writer.text(rolloutStage.rawValue)
    writer.flag(permitSend)
    writer.number(issuedAtUnixNanoseconds)
    writer.number(validUntilUnixNanoseconds)
    return writer.finish()
  }

  /// Recomputes the binding digest.
  public var computedBindingSHA256: String? {
    bindingBytes.map(SendDigest.sha256Hex)
  }

  /// Structural and temporal validation, performed independently of the
  /// control plane's own check. The helper trusts nothing it is handed.
  public func validate(nowUnixNanoseconds: UInt64) throws(SendFailure) {
    let hexFields = [
      capabilityID, actionID, draftID, approvalID, idempotencyKey, bodySHA256,
      normalizedBodySHA256, calibrationProfileSHA256,
    ]
    let structurallyValid =
      formatVersion == SendContract.version
      && hexFields.allSatisfy(SendDigest.isSHA256Hex)
      && !accountID.isEmpty
      && !conversationID.isEmpty
      && !clientBuildProfileID.isEmpty
      && !calibrationProfileID.isEmpty
      && !searchKey.isEmpty
      && searchKey.utf8.count <= SendContract.maximumSearchKeyBytes
      && !expectedTitle.isEmpty
      && expectedTitle.utf8.count <= SendContract.maximumExpectedTitleBytes
      && body.utf8.count <= SendContract.maximumBodyBytes
      && issuedAtUnixNanoseconds < validUntilUnixNanoseconds
      && SendDigest.sha256Hex(Data(body.utf8)) == bodySHA256
      && SendText.normalizedSHA256(body) == normalizedBodySHA256
      && computedBindingSHA256 == bindingSHA256
      && (!permitSend || rolloutStage.permitsReturn)
    guard structurallyValid else {
      throw SendFailure(
        .capabilityMismatch, detail: "the capability envelope is not self-consistent")
    }
    // A capability carries a body or an attachment, never both and never
    // neither. A caption is a further capability, so an attachment send has no
    // text at all.
    switch (capability.carriesAttachment, attachment) {
    case (false, nil) where !body.isEmpty:
      break
    case (true, .some(let attachment)) where body.isEmpty:
      try attachment.validate()
    default:
      throw SendFailure(
        .capabilityMismatch, detail: "the payload is neither text nor an attachment")
    }
    guard
      nowUnixNanoseconds >= issuedAtUnixNanoseconds,
      nowUnixNanoseconds < validUntilUnixNanoseconds
    else {
      throw SendFailure(.capabilityExpired, detail: "the capability is outside its validity window")
    }
  }
}

/// Body-free evidence from the helper's captures: match decisions and
/// confidences, never recognized text.
public struct HelperGateEvidence: Codable, Equatable, Sendable {
  public var titleConfidencePartsPerMillion: UInt32
  public var titleMatched: Bool
  public var composeMatched: Bool
  /// GATE 2a: the staged attachment's display name was read back.
  public var attachmentNameMatched: Bool
  /// Whether the compose region showed a staged attachment at all.
  public var attachmentStaged: Bool
  /// Whether a send-confirmation sheet was observed and confirmed.
  public var confirmationSheetConfirmed: Bool
  public var composeCleared: Bool
  public var newestOutgoingMatched: Bool
  public var ambiguousSearchResult: Bool
  public var humanActivityObserved: Bool
  public var windowFrameDigest: String
  public var captureCount: UInt32
  public var elapsedMilliseconds: UInt64

  public init(
    titleConfidencePartsPerMillion: UInt32 = 0,
    titleMatched: Bool = false,
    composeMatched: Bool = false,
    attachmentNameMatched: Bool = false,
    attachmentStaged: Bool = false,
    confirmationSheetConfirmed: Bool = false,
    composeCleared: Bool = false,
    newestOutgoingMatched: Bool = false,
    ambiguousSearchResult: Bool = false,
    humanActivityObserved: Bool = false,
    windowFrameDigest: String = String(repeating: "0", count: 64),
    captureCount: UInt32 = 0,
    elapsedMilliseconds: UInt64 = 0
  ) {
    self.titleConfidencePartsPerMillion = titleConfidencePartsPerMillion
    self.titleMatched = titleMatched
    self.composeMatched = composeMatched
    self.attachmentNameMatched = attachmentNameMatched
    self.attachmentStaged = attachmentStaged
    self.confirmationSheetConfirmed = confirmationSheetConfirmed
    self.composeCleared = composeCleared
    self.newestOutgoingMatched = newestOutgoingMatched
    self.ambiguousSearchResult = ambiguousSearchResult
    self.humanActivityObserved = humanActivityObserved
    self.windowFrameDigest = windowFrameDigest
    self.captureCount = captureCount
    self.elapsedMilliseconds = elapsedMilliseconds
  }
}

/// What one `executeSend` run reports back.
public struct HelperSendOutcome: Codable, Equatable, Sendable {
  public let formatVersion: UInt32
  public let capabilityID: String
  public let capabilityBindingSHA256: String
  public let helperVersion: String
  public let engineVersion: String
  public let calibrationProfileID: String
  public let stageReached: SendStage
  public let attempted: Bool
  public let visualConfirmation: VisualConfirmation
  public let failure: SendFailureCode?
  public let evidence: HelperGateEvidence
  public let observedAtUnixNanoseconds: UInt64

  enum CodingKeys: String, CodingKey {
    case formatVersion
    case capabilityID = "capabilityId"
    case capabilityBindingSHA256 = "capabilityBindingSha256"
    case helperVersion
    case engineVersion
    case calibrationProfileID = "calibrationProfileId"
    case stageReached
    case attempted
    case visualConfirmation
    case failure
    case evidence
    case observedAtUnixNanoseconds
  }

  public init(
    formatVersion: UInt32 = SendContract.version,
    capabilityID: String,
    capabilityBindingSHA256: String,
    helperVersion: String,
    engineVersion: String,
    calibrationProfileID: String,
    stageReached: SendStage,
    attempted: Bool,
    visualConfirmation: VisualConfirmation,
    failure: SendFailureCode?,
    evidence: HelperGateEvidence,
    observedAtUnixNanoseconds: UInt64
  ) {
    self.formatVersion = formatVersion
    self.capabilityID = capabilityID
    self.capabilityBindingSHA256 = capabilityBindingSHA256
    self.helperVersion = helperVersion
    self.engineVersion = engineVersion
    self.calibrationProfileID = calibrationProfileID
    self.stageReached = stageReached
    self.attempted = attempted
    self.visualConfirmation = visualConfirmation
    self.failure = failure
    self.evidence = evidence
    self.observedAtUnixNanoseconds = observedAtUnixNanoseconds
  }
}

/// The helper's read-only preflight answer. It is the only way the control
/// plane learns about TCC grants and live client state, because the control
/// plane deliberately holds no input or capture grants of its own.
public struct HelperCapabilityStatus: Codable, Equatable, Sendable {
  public var formatVersion: UInt32
  public var helperVersion: String
  public var engineVersion: String
  public var accessibilityGranted: Bool
  public var screenRecordingGranted: Bool
  public var wechatRunning: Bool
  public var wechatLoggedIn: Bool
  public var wechatBundleIdentifier: String
  public var wechatMarketingVersion: String
  public var wechatBuild: String
  public var macosBuild: String
  public var macosMajor: UInt32
  public var mainWindowFound: Bool
  public var activeCalibrationProfileID: String
  public var engineHealthy: Bool
  public var boundedManifestScope: [String]
  public var observedAtUnixNanoseconds: UInt64

  enum CodingKeys: String, CodingKey {
    case formatVersion
    case helperVersion
    case engineVersion
    case accessibilityGranted
    case screenRecordingGranted
    case wechatRunning
    case wechatLoggedIn
    case wechatBundleIdentifier
    case wechatMarketingVersion
    case wechatBuild
    case macosBuild
    case macosMajor
    case mainWindowFound
    case activeCalibrationProfileID = "activeCalibrationProfileId"
    case engineHealthy
    case boundedManifestScope
    case observedAtUnixNanoseconds
  }

  public init(
    formatVersion: UInt32 = SendContract.version,
    helperVersion: String,
    engineVersion: String,
    accessibilityGranted: Bool,
    screenRecordingGranted: Bool,
    wechatRunning: Bool,
    wechatLoggedIn: Bool,
    wechatBundleIdentifier: String,
    wechatMarketingVersion: String,
    wechatBuild: String,
    macosBuild: String,
    macosMajor: UInt32,
    mainWindowFound: Bool,
    activeCalibrationProfileID: String,
    engineHealthy: Bool,
    boundedManifestScope: [String],
    observedAtUnixNanoseconds: UInt64
  ) {
    self.formatVersion = formatVersion
    self.helperVersion = helperVersion
    self.engineVersion = engineVersion
    self.accessibilityGranted = accessibilityGranted
    self.screenRecordingGranted = screenRecordingGranted
    self.wechatRunning = wechatRunning
    self.wechatLoggedIn = wechatLoggedIn
    self.wechatBundleIdentifier = wechatBundleIdentifier
    self.wechatMarketingVersion = wechatMarketingVersion
    self.wechatBuild = wechatBuild
    self.macosBuild = macosBuild
    self.macosMajor = macosMajor
    self.mainWindowFound = mainWindowFound
    self.activeCalibrationProfileID = activeCalibrationProfileID
    self.engineHealthy = engineHealthy
    self.boundedManifestScope = boundedManifestScope
    self.observedAtUnixNanoseconds = observedAtUnixNanoseconds
  }

  /// The first blocking reason the live environment gives, in the order the
  /// operator should fix them.
  public var blockingFailure: SendFailureCode? {
    if formatVersion != SendContract.version || helperVersion.isEmpty || engineVersion.isEmpty {
      return .configurationInvalid
    }
    if !accessibilityGranted || !screenRecordingGranted { return .grantsMissing }
    if !engineHealthy { return .engineUnavailable }
    if !wechatRunning { return .wechatNotRunning }
    if !wechatLoggedIn { return .notLoggedIn }
    if !mainWindowFound { return .windowNotFound }
    if boundedManifestScope.isEmpty || boundedManifestScope.contains(where: \.isEmpty) {
      return .manifestViolation
    }
    return nil
  }
}

/// The result of one calibration self-test: locate and focus the search box,
/// confirm by capture, and never send.
public struct CalibrationSelfTestReport: Codable, Equatable, Sendable {
  public let formatVersion: UInt32
  public let calibrationProfileID: String
  public let passed: Bool
  public let searchBoxFocused: Bool
  public let titleConfidencePartsPerMillion: UInt32
  public let windowFrameDigest: String
  public let driftReport: [String]
  public let failure: SendFailureCode?
  public let observedAtUnixNanoseconds: UInt64

  enum CodingKeys: String, CodingKey {
    case formatVersion
    case calibrationProfileID = "calibrationProfileId"
    case passed
    case searchBoxFocused
    case titleConfidencePartsPerMillion
    case windowFrameDigest
    case driftReport
    case failure
    case observedAtUnixNanoseconds
  }

  public init(
    formatVersion: UInt32 = SendContract.version,
    calibrationProfileID: String,
    passed: Bool,
    searchBoxFocused: Bool,
    titleConfidencePartsPerMillion: UInt32,
    windowFrameDigest: String,
    driftReport: [String],
    failure: SendFailureCode?,
    observedAtUnixNanoseconds: UInt64
  ) {
    self.formatVersion = formatVersion
    self.calibrationProfileID = calibrationProfileID
    self.passed = passed
    self.searchBoxFocused = searchBoxFocused
    self.titleConfidencePartsPerMillion = titleConfidencePartsPerMillion
    self.windowFrameDigest = windowFrameDigest
    self.driftReport = driftReport
    self.failure = failure
    self.observedAtUnixNanoseconds = observedAtUnixNanoseconds
  }
}
