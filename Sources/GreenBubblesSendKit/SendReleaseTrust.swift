import Foundation

/// The release verifying keys pinned into this build.
///
/// The value is empty unless the release pipeline injects it, and an empty set
/// is the safe default: with no release key, no release-signed calibration
/// profile verifies, so the send path cannot leave the dry-run stage. Release
/// builds set `GREENBUBBLES_SEND_RELEASE_PUBLIC_KEYS` (a comma-separated list
/// of 32-byte Ed25519 public keys in hexadecimal), which
/// `scripts/package-send-helper.sh` turns into a generated Swift constant.
public enum SendReleaseTrust {
  /// Keys are pinned at build time. This default deliberately trusts nothing.
  public static let pinnedReleasePublicKeys: [String] = generatedReleasePublicKeys

  /// Overwritten by the release pipeline; never edited by hand.
  static let generatedReleasePublicKeys: [String] = []
}
