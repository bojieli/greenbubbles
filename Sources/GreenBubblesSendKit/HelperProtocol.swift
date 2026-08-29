import Foundation

/// Identity and versioning for the privilege-separated input helper.
public enum SendHelperIdentity {
  /// The helper's bundle identifier; TCC entries appear under this name.
  public static let bundleIdentifier = "me.greenbubbles.InputHelper"
  /// The Mach service the helper's XPC listener publishes.
  public static let machServiceName = "me.greenbubbles.InputHelper"
  /// The launchd job the packaged application registers with `SMAppService`.
  /// It lives in `Contents/Library/LaunchAgents` and publishes the Mach
  /// service above; its `Program` is the helper inside
  /// `Contents/Library/LoginItems/GreenBubblesInputHelper.app`, so the TCC
  /// grants attribute to that bundle rather than to a bare executable.
  public static let launchAgentPlistName = "me.greenbubbles.InputHelper.plist"
  /// The helper's own version, reported in every status and outcome.
  public static let helperVersion = "1.0.0"
  /// The version of the first-party effector engine inside the helper.
  public static let engineVersion = "1.0.0"

  /// The code-signing requirement the helper pins on its XPC peers. Only a
  /// binary signed by our team may connect, which is why the XPC surface can be
  /// high level rather than hand-rolling an authentication token.
  ///
  /// The team identifier is injected at packaging time; an unset value yields a
  /// requirement no process satisfies, so an unpackaged development build
  /// refuses every connection rather than accepting any.
  public static func codeSigningRequirement(teamIdentifier: String) -> String {
    guard !teamIdentifier.isEmpty else {
      return "identifier \"never.matches.unconfigured.team\""
    }
    return "anchor apple generic and certificate leaf[subject.OU] = \"\(teamIdentifier)\""
  }
}

/// The helper's XPC surface: three high-level methods, no raw "type anywhere"
/// primitive. The helper constrains an already-minted capability to its bound
/// recipient and content, but does not independently attest the owner approval
/// that preceded it. A peer running as the signed control plane can submit only
/// this high-level envelope and cannot ask the helper to press a key of its
/// choosing.
///
/// Payloads cross as JSON `Data` so both sides use the same Codable models the
/// Rust control plane writes, rather than a second, divergent object graph.
@objc public protocol GreenBubblesInputHelperProtocol {
  /// Read-only preflight for onboarding and `send doctor`.
  func capabilityStatus(reply: @escaping @Sendable (Data?, String?) -> Void)

  /// Locate and focus the search box, confirm by capture, and never send.
  func runCalibrationSelfTest(
    signedProfile: Data,
    reply: @escaping @Sendable (Data?, String?) -> Void
  )

  /// Run the whole mechanical send skill under one bound capability.
  func executeSend(capability: Data, reply: @escaping @Sendable (Data?, String?) -> Void)
}

/// JSON coding shared by the helper, its client, and the Rust control plane.
public enum SendCodec {
  /// Encoder that produces the exact field names the Rust models expect.
  public static func encode<Value: Encodable>(_ value: Value) throws -> Data {
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
    return try encoder.encode(value)
  }

  /// Decoder for a payload received across the helper boundary.
  public static func decode<Value: Decodable>(_ type: Value.Type, from data: Data) throws -> Value {
    try JSONDecoder().decode(type, from: data)
  }
}
