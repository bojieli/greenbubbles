import Foundation
import Testing

@testable import GreenBubblesSendKit

/// Pins the Swift encoders to the same fixture the Rust control plane asserts
/// against. The canonical encodings and the signature scheme are hand-written
/// in two languages; this is what stops them drifting apart silently.
struct CrossLanguageVectorTests {
  private struct Vectors: Decodable {
    struct Signed<Body: Decodable>: Decodable {
      let body: Body
      let canonicalSHA256: String

      enum CodingKeys: String, CodingKey {
        case body
        case canonicalSHA256 = "canonicalSha256"
      }
    }

    struct NormalizationCase: Decodable {
      let input: String
      let normalized: String
      let sha256: String
    }

    struct DevelopmentSigning: Decodable {
      let publicKeyHex: String
      let signedCalibrationProfile: SignedCalibrationProfile
      let signedCompatibilityMatrix: SignedCompatibilityMatrix
    }

    let calibrationProfile: Signed<CalibrationProfileBody>
    let compatibilityMatrix: Signed<CompatibilityMatrixBody>
    let actionCapability: ActionCapabilityEnvelope
    let normalizedText: [NormalizationCase]
    let developmentSigning: DevelopmentSigning
  }

  private func loadVectors() throws -> Vectors {
    let repositoryRoot = URL(fileURLWithPath: #filePath)
      .deletingLastPathComponent()
      .deletingLastPathComponent()
      .deletingLastPathComponent()
    let url = repositoryRoot.appending(path: "docs/send-canonical-vectors.json")
    return try JSONDecoder().decode(Vectors.self, from: Data(contentsOf: url))
  }

  private func developmentTrustRoot(_ vectors: Vectors) -> SendTrustRoot {
    SendTrustRoot(developmentPublicKeys: [vectors.developmentSigning.publicKeyHex])
  }

  @Test("the calibration-profile encoder matches the shared fixture")
  func calibrationProfileEncoding() throws {
    let vectors = try loadVectors()
    let bytes = try #require(vectors.calibrationProfile.body.signingBytes)
    #expect(SendDigest.sha256Hex(bytes) == vectors.calibrationProfile.canonicalSHA256)
  }

  @Test("the compatibility-matrix encoder matches the shared fixture")
  func compatibilityMatrixEncoding() throws {
    let vectors = try loadVectors()
    let bytes = try #require(vectors.compatibilityMatrix.body.signingBytes)
    #expect(SendDigest.sha256Hex(bytes) == vectors.compatibilityMatrix.canonicalSHA256)
  }

  @Test("the action-capability binding matches the shared fixture")
  func actionCapabilityBinding() throws {
    let capability = try loadVectors().actionCapability
    #expect(capability.computedBindingSHA256 == capability.bindingSHA256)
    try capability.validate(nowUnixNanoseconds: capability.issuedAtUnixNanoseconds + 1)
  }

  @Test("the text normalizer matches the shared fixture")
  func textNormalization() throws {
    for testCase in try loadVectors().normalizedText {
      #expect(SendText.normalized(testCase.input) == testCase.normalized)
      #expect(SendText.normalizedSHA256(testCase.input) == testCase.sha256)
    }
  }

  @Test("artifacts signed by the Rust release tooling verify with CryptoKit")
  func rustSignedArtifactsVerify() throws {
    let vectors = try loadVectors()
    let trustRoot = developmentTrustRoot(vectors)
    let profile = try CalibrationProfileVerifier.verify(
      vectors.developmentSigning.signedCalibrationProfile,
      trustRoot: trustRoot,
      nowUnixSeconds: vectors.calibrationProfile.body.issuedAtUnixSeconds + 1
    )
    #expect(profile.trustTier == .development)
    #expect(profile.canonicalSHA256 == vectors.calibrationProfile.canonicalSHA256)

    let matrix = try CalibrationProfileVerifier.verify(
      vectors.developmentSigning.signedCompatibilityMatrix,
      trustRoot: trustRoot,
      nowUnixSeconds: vectors.compatibilityMatrix.body.issuedAtUnixSeconds + 1
    )
    #expect(matrix.trustTier == .development)
    #expect(matrix.canonicalSHA256 == vectors.compatibilityMatrix.canonicalSHA256)

    let decision = CalibrationProfileVerifier.decision(
      in: matrix,
      macosBuild: "25G83",
      wechatBuild: "4.1.13.269579"
    )
    #expect(decision.state.permitsSend)
    #expect(!decision.fieldKillSwitchEngaged)
    try CalibrationProfileVerifier.bind(profile: profile, to: decision, expectedMacosMajor: 26)
  }

  @Test("an unknown or blocked build combination never permits a send")
  func unknownCombinationFailsClosed() throws {
    let vectors = try loadVectors()
    let matrix = try CalibrationProfileVerifier.verify(
      vectors.developmentSigning.signedCompatibilityMatrix,
      trustRoot: developmentTrustRoot(vectors),
      nowUnixSeconds: vectors.compatibilityMatrix.body.issuedAtUnixSeconds + 1
    )
    let unknown = CalibrationProfileVerifier.decision(
      in: matrix,
      macosBuild: "26A1",
      wechatBuild: "4.1.13.269579"
    )
    #expect(unknown.knownCombination == false)
    #expect(unknown.state == .unverified)
    #expect(unknown.state.permitsSend == false)

    let blocked = CalibrationProfileVerifier.decision(
      in: matrix,
      macosBuild: "25G83",
      wechatBuild: "4.1.14.900000"
    )
    #expect(blocked.knownCombination)
    #expect(blocked.state.permitsSend == false)
  }

  @Test("a signed matrix kill switch closes the send path without an application update")
  func fieldKillSwitchClosesTheSendPath() throws {
    let vectors = try loadVectors()
    let original = vectors.developmentSigning.signedCompatibilityMatrix
    let engaged = SignedCompatibilityMatrix(
      body: CompatibilityMatrixBody(
        schema: original.body.schema,
        matrixID: original.body.matrixID,
        issuedAtUnixSeconds: original.body.issuedAtUnixSeconds,
        expiresAtUnixSeconds: original.body.expiresAtUnixSeconds,
        globalKillSwitchEngaged: true,
        entries: original.body.entries
      ),
      signature: original.signature
    )
    // The switch is inside the signature, so flipping it in the field without
    // the release key breaks verification rather than opening the path.
    #expect(throws: SignedArtifactDenial.signatureNotVerified) {
      try CalibrationProfileVerifier.verify(
        engaged,
        trustRoot: developmentTrustRoot(vectors),
        nowUnixSeconds: original.body.issuedAtUnixSeconds + 1
      )
    }
    let decision = CompatibilityDecision(
      macosBuild: "25G83",
      wechatBuild: "4.1.13.269579",
      state: .supported,
      knownCombination: true,
      fieldKillSwitchEngaged: true,
      expectedCalibrationProfileID: vectors.calibrationProfile.body.profileID,
      clientBuildProfileID: vectors.calibrationProfile.body.clientBuildProfileID,
      note: ""
    )
    let profile = try CalibrationProfileVerifier.verify(
      vectors.developmentSigning.signedCalibrationProfile,
      trustRoot: developmentTrustRoot(vectors),
      nowUnixSeconds: vectors.calibrationProfile.body.issuedAtUnixSeconds + 1
    )
    #expect(throws: SignedArtifactDenial.combinationNotSupported) {
      try CalibrationProfileVerifier.bind(profile: profile, to: decision, expectedMacosMajor: 26)
    }
  }

  @Test("the pinned release trust root is empty unless the release pipeline provisions it")
  func pinnedTrustRootIsEmpty() {
    #expect(SendTrustRoot.pinned.releasePublicKeys.isEmpty)
    #expect(SendTrustRoot.pinned.developmentPublicKeys.isEmpty)
  }

  @Test("an empty trust root refuses every signature")
  func emptyTrustRootRefusesEverything() throws {
    let vectors = try loadVectors()
    #expect(throws: SignedArtifactDenial.trustRootEmpty) {
      try CalibrationProfileVerifier.verify(
        vectors.developmentSigning.signedCalibrationProfile,
        trustRoot: SendTrustRoot(),
        nowUnixSeconds: vectors.calibrationProfile.body.issuedAtUnixSeconds + 1
      )
    }
  }

  @Test("tampering with a signed anchor is detected")
  func tamperingIsDetected() throws {
    let vectors = try loadVectors()
    let original = vectors.developmentSigning.signedCalibrationProfile
    let mutated = SignedCalibrationProfile(
      body: CalibrationProfileBody(
        schema: original.body.schema,
        profileID: original.body.profileID,
        wechatBundleIdentifier: original.body.wechatBundleIdentifier,
        wechatMarketingVersion: original.body.wechatMarketingVersion,
        wechatBuild: original.body.wechatBuild,
        clientBuildProfileID: original.body.clientBuildProfileID,
        macosMajor: original.body.macosMajor,
        anchors: CalibrationAnchors(
          searchBox: WindowRelativePoint(
            xPartsPerMillion: original.body.anchors.searchBox.xPartsPerMillion + 1,
            yPartsPerMillion: original.body.anchors.searchBox.yPartsPerMillion
          ),
          firstResultRow: original.body.anchors.firstResultRow,
          composeBox: original.body.anchors.composeBox
        ),
        ocrRegions: original.body.ocrRegions,
        selftest: original.body.selftest,
        issuedAtUnixSeconds: original.body.issuedAtUnixSeconds,
        expiresAtUnixSeconds: original.body.expiresAtUnixSeconds
      ),
      signature: original.signature
    )
    #expect(throws: SignedArtifactDenial.signatureNotVerified) {
      try CalibrationProfileVerifier.verify(
        mutated,
        trustRoot: developmentTrustRoot(vectors),
        nowUnixSeconds: original.body.issuedAtUnixSeconds + 1
      )
    }
  }

  @Test("the profile validity window fails closed on both sides")
  func validityWindowFailsClosed() throws {
    let vectors = try loadVectors()
    let trustRoot = developmentTrustRoot(vectors)
    let profile = vectors.developmentSigning.signedCalibrationProfile
    #expect(throws: SignedArtifactDenial.notYetValid) {
      try CalibrationProfileVerifier.verify(
        profile,
        trustRoot: trustRoot,
        nowUnixSeconds: profile.body.issuedAtUnixSeconds - 1
      )
    }
    #expect(throws: SignedArtifactDenial.expired) {
      try CalibrationProfileVerifier.verify(
        profile,
        trustRoot: trustRoot,
        nowUnixSeconds: profile.body.expiresAtUnixSeconds
      )
    }
  }
}
