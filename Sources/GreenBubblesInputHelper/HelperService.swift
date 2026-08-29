import Foundation
import GreenBubblesSendKit

/// The helper's three-method service. It holds the powerful grants and nothing
/// else: no decryption key, no replica handle, no policy, no message history.
/// Everything it needs to enforce the recipient gate arrives inside the single
/// bound capability the control plane mints.
final class HelperService: NSObject, GreenBubblesInputHelperProtocol, @unchecked Sendable {
  private let targetBundleIdentifier: String
  private let manifest: BoundedCapabilityManifest
  private let trustRoot: SendTrustRoot
  private let pacing: SendPacing
  /// One serial queue owns every mutation and every effector call, so the
  /// helper is single-flight by construction and `activeProfile` needs no
  /// further synchronization.
  private let queue = DispatchQueue(label: "me.greenbubbles.InputHelper.work")
  private var activeProfile: CalibrationProfileBody?

  init(
    targetBundleIdentifier: String = "com.tencent.xinWeChat",
    manifest: BoundedCapabilityManifest = .weChatOnly,
    trustRoot: SendTrustRoot = .pinned,
    pacing: SendPacing = SendPacing()
  ) {
    self.targetBundleIdentifier = targetBundleIdentifier
    self.manifest = manifest
    self.trustRoot = trustRoot
    self.pacing = pacing
  }

  /// The read-only preflight. It probes live rather than trusting any
  /// remembered toggle state, and it never sends.
  func capabilityStatus(reply: @escaping @Sendable (Data?, String?) -> Void) {
    queue.async { [self] in
      respond(reply) { currentStatus() }
    }
  }

  /// The no-send calibration self-test that gates a profile before first use.
  func runCalibrationSelfTest(
    signedProfile: Data,
    reply: @escaping @Sendable (Data?, String?) -> Void
  ) {
    queue.async { [self] in
      respond(reply) {
        let signed = try SendCodec.decode(SignedCalibrationProfile.self, from: signedProfile)
        let verified = try CalibrationProfileVerifier.verify(
          signed,
          trustRoot: trustRoot,
          nowUnixSeconds: UInt64(Date().timeIntervalSince1970)
        )
        guard let target = WeChatTarget.locate(bundleIdentifier: targetBundleIdentifier) else {
          return CalibrationSelfTestReport(
            calibrationProfileID: verified.profile.body.profileID,
            passed: false,
            searchBoxFocused: false,
            titleConfidencePartsPerMillion: 0,
            windowFrameDigest: String(repeating: "0", count: 64),
            driftReport: ["WeChat is not running"],
            failure: .wechatNotRunning,
            observedAtUnixNanoseconds: Self.nowUnixNanoseconds()
          )
        }
        guard verified.profile.body.wechatBuild == target.buildIdentifier else {
          return CalibrationSelfTestReport(
            calibrationProfileID: verified.profile.body.profileID,
            passed: false,
            searchBoxFocused: false,
            titleConfidencePartsPerMillion: 0,
            windowFrameDigest: String(repeating: "0", count: 64),
            driftReport: [
              "profile targets \(verified.profile.body.wechatBuild) but the client is "
                + "\(target.buildIdentifier)"
            ],
            failure: .unknownBuild,
            observedAtUnixNanoseconds: Self.nowUnixNanoseconds()
          )
        }
        let report = skill(for: verified.profile.body, target: target).runCalibrationSelfTest()
        if report.passed { activeProfile = verified.profile.body }
        return report
      }
    }
  }

  /// The whole mechanical send skill, under one bound capability.
  func executeSend(capability: Data, reply: @escaping @Sendable (Data?, String?) -> Void) {
    queue.async { [self] in
      respond(reply) {
        let envelope = try SendCodec.decode(ActionCapabilityEnvelope.self, from: capability)
        try envelope.validate(nowUnixNanoseconds: Self.nowUnixNanoseconds())
        guard let profile = activeProfile,
          profile.profileID == envelope.calibrationProfileID
        else {
          throw SendFailure(
            .calibrationDrift,
            detail: "no verified calibration profile is active for this capability"
          )
        }
        guard let target = WeChatTarget.locate(bundleIdentifier: targetBundleIdentifier) else {
          throw SendFailure(.wechatNotRunning, detail: "WeChat is not running")
        }
        guard target.signedIn else {
          throw SendFailure(.notLoggedIn, detail: "WeChat is not signed in")
        }
        guard profile.wechatBuild == target.buildIdentifier else {
          throw SendFailure(.unknownBuild, detail: "the client build changed since calibration")
        }
        return skill(for: profile, target: target).execute(envelope)
      }
    }
  }

  /// The status this helper reports about itself and its target.
  func currentStatus() -> HelperCapabilityStatus {
    let target = WeChatTarget.locate(bundleIdentifier: targetBundleIdentifier)
    return HelperCapabilityStatus(
      helperVersion: SendHelperIdentity.helperVersion,
      engineVersion: SendHelperIdentity.engineVersion,
      accessibilityGranted: EnvironmentProbe.accessibilityGranted,
      screenRecordingGranted: EnvironmentProbe.screenRecordingGranted,
      wechatRunning: target != nil,
      wechatLoggedIn: target?.signedIn ?? false,
      wechatBundleIdentifier: target?.bundleIdentifier ?? targetBundleIdentifier,
      wechatMarketingVersion: target?.marketingVersion ?? "",
      wechatBuild: target?.buildIdentifier ?? "",
      macosBuild: EnvironmentProbe.macosBuild,
      macosMajor: EnvironmentProbe.macosMajor,
      mainWindowFound: target?.frame != nil,
      activeCalibrationProfileID: activeProfile?.profileID ?? "",
      engineHealthy: true,
      boundedManifestScope: manifest.scopeDescription,
      observedAtUnixNanoseconds: Self.nowUnixNanoseconds()
    )
  }

  private func skill(
    for profile: CalibrationProfileBody,
    target: WeChatTarget
  ) -> MechanicalSendSkill {
    MechanicalSendSkill(
      profile: profile,
      manifest: manifest,
      targetBundleIdentifier: targetBundleIdentifier,
      effector: MacOSInputEffector(processIdentifier: target.processIdentifier),
      perception: MacOSScreenPerception(
        processIdentifier: target.processIdentifier,
        bundleIdentifier: targetBundleIdentifier
      ),
      pacing: pacing,
      helperVersion: SendHelperIdentity.helperVersion,
      engineVersion: SendHelperIdentity.engineVersion,
      clock: { Self.nowUnixNanoseconds() }
    )
  }

  /// Encodes a result, turning any refusal into a machine-readable error the
  /// caller can map onto the failure taxonomy.
  private func respond<Value: Encodable>(
    _ reply: @escaping @Sendable (Data?, String?) -> Void,
    _ work: () throws -> Value
  ) {
    do {
      reply(try SendCodec.encode(work()), nil)
    } catch let failure as SendFailure {
      reply(nil, failure.code.rawValue)
    } catch let denial as SignedArtifactDenial {
      reply(nil, "\(SendFailureCode.profileInvalid.rawValue):\(denial.rawValue)")
    } catch {
      reply(nil, SendFailureCode.engineUnavailable.rawValue)
    }
  }

  static func nowUnixNanoseconds() -> UInt64 {
    UInt64(Date().timeIntervalSince1970 * 1_000_000_000)
  }
}
