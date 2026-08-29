import CoreGraphics
import Foundation
import GreenBubblesSendKit
import ImageIO
import UniformTypeIdentifiers

/// Read-only diagnostics used to answer the calibration questions a signed
/// profile must be measured against.
///
/// Neither entry point can send. `capture` only reads the screen, and `spike`
/// refuses any capability whose `permitSend` is true, so a measurement run can
/// stage and abandon but never press Return.
enum SpikeHarness {
  /// Captures the target window to a PNG so anchors can be measured against a
  /// real frame rather than guessed.
  static func capture(to path: String, bundleIdentifier: String) throws {
    guard let target = WeChatTarget.locate(bundleIdentifier: bundleIdentifier),
      let frame = target.frame
    else {
      throw SendFailure(.wechatNotRunning, detail: "WeChat is not running")
    }
    let perception = MacOSScreenPerception(
      processIdentifier: target.processIdentifier,
      bundleIdentifier: bundleIdentifier
    )
    // Recognizing the whole window forces one full capture through the very
    // path the gates use, so the measurement reflects what they will see.
    let whole = WindowRelativeRect(
      xPartsPerMillion: 0,
      yPartsPerMillion: 0,
      widthPartsPerMillion: CalibrationProfileConstants.partsPerMillion,
      heightPartsPerMillion: CalibrationProfileConstants.partsPerMillion
    )
    _ = try perception.recognizeText(in: WindowGeometry.rect(whole, in: frame))
    guard let image = perception.lastCapturedImage else {
      throw SendFailure(.engineUnavailable, detail: "no image was captured")
    }
    let url = URL(fileURLWithPath: path)
    guard
      let destination = CGImageDestinationCreateWithURL(
        url as CFURL,
        UTType.png.identifier as CFString,
        1,
        nil
      )
    else {
      throw SendFailure(.engineUnavailable, detail: "could not create the image destination")
    }
    CGImageDestinationAddImage(destination, image, nil)
    guard CGImageDestinationFinalize(destination) else {
      throw SendFailure(.engineUnavailable, detail: "could not write the capture")
    }
    let report: [String: Any] = [
      "windowOriginX": frame.origin.x,
      "windowOriginY": frame.origin.y,
      "windowWidth": frame.size.width,
      "windowHeight": frame.size.height,
      "imageWidth": image.width,
      "imageHeight": image.height,
      "output": path,
    ]
    let data = try JSONSerialization.data(withJSONObject: report, options: [.sortedKeys])
    print(String(decoding: data, as: UTF8.self))
  }

  /// Runs the mechanical skill once against a signed profile and a capability,
  /// for measurement only.
  static func spike(profilePath: String, capabilityPath: String, trustRootPath: String?) throws {
    let profileData = try Data(contentsOf: URL(fileURLWithPath: profilePath))
    let signed = try SendCodec.decode(SignedCalibrationProfile.self, from: profileData)
    var trustRoot = SendTrustRoot.pinned
    if let trustRootPath {
      let rootData = try Data(contentsOf: URL(fileURLWithPath: trustRootPath))
      trustRoot = try SendCodec.decode(SendTrustRoot.self, from: rootData)
    }
    let verified = try CalibrationProfileVerifier.verify(
      signed,
      trustRoot: trustRoot,
      nowUnixSeconds: UInt64(Date().timeIntervalSince1970)
    )
    let capabilityData = try Data(contentsOf: URL(fileURLWithPath: capabilityPath))
    let capability = try SendCodec.decode(ActionCapabilityEnvelope.self, from: capabilityData)
    // The spike harness is measurement-only. It must never be the thing that
    // sends a message, whatever the capability says.
    guard !capability.permitSend else {
      throw SendFailure(
        .stageNotPermitted,
        detail: "the spike harness refuses a send-permitting capability"
      )
    }
    try capability.validate(
      nowUnixNanoseconds: UInt64(Date().timeIntervalSince1970 * 1_000_000_000))
    guard let target = WeChatTarget.locate(bundleIdentifier: "com.tencent.xinWeChat"),
      target.signedIn
    else {
      throw SendFailure(.wechatNotRunning, detail: "WeChat is not running or not signed in")
    }
    let skill = MechanicalSendSkill(
      profile: verified.profile.body,
      effector: MacOSInputEffector(processIdentifier: target.processIdentifier),
      perception: MacOSScreenPerception(
        processIdentifier: target.processIdentifier,
        bundleIdentifier: "com.tencent.xinWeChat"
      ),
      helperVersion: SendHelperIdentity.helperVersion,
      engineVersion: SendHelperIdentity.engineVersion,
      clock: { UInt64(Date().timeIntervalSince1970 * 1_000_000_000) }
    )
    let outcome = skill.execute(capability)
    print(String(decoding: try SendCodec.encode(outcome), as: UTF8.self))
  }
}
