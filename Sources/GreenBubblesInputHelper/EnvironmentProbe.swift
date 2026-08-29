import AppKit
import ApplicationServices
import CoreGraphics
import Foundation
import GreenBubblesSendKit

/// Live probes of the two TCC grants and of the target client's state.
///
/// Nothing here is remembered: a grant can be revoked at any moment, so the
/// send path re-probes rather than trusting a cached answer. Neither probe can
/// grant anything, and neither prompts: `AXIsProcessTrustedWithOptions` is
/// called with the prompt option off and `CGPreflightScreenCaptureAccess` is
/// documented as non-prompting.
enum EnvironmentProbe {
  /// Whether this process may synthesize input events.
  static var accessibilityGranted: Bool {
    // The prompt option is spelled literally: the framework constant is a
    // mutable global, which Swift 6 concurrency checking rejects, and the key
    // is part of the stable public API.
    let options = ["AXTrustedCheckOptionPrompt": false] as CFDictionary
    return AXIsProcessTrustedWithOptions(options)
  }

  /// Whether this process may capture window contents.
  static var screenRecordingGranted: Bool {
    CGPreflightScreenCaptureAccess()
  }

  /// Asks the system, once, to show the Screen Recording consent prompt. Used
  /// only by the guided onboarding, never on the send path.
  @discardableResult
  static func requestScreenRecordingConsent() -> Bool {
    CGRequestScreenCaptureAccess()
  }

  /// The host's build string, for the compatibility matrix.
  static var macosBuild: String {
    var size = 0
    guard sysctlbyname("kern.osversion", nil, &size, nil, 0) == 0, size > 0 else { return "" }
    var buffer = [UInt8](repeating: 0, count: size)
    guard sysctlbyname("kern.osversion", &buffer, &size, nil, 0) == 0 else { return "" }
    return String(decoding: buffer.prefix(while: { $0 != 0 }), as: UTF8.self)
  }

  /// The host's major version, for the compatibility matrix.
  static var macosMajor: UInt32 {
    UInt32(ProcessInfo.processInfo.operatingSystemVersion.majorVersion)
  }
}

/// Everything the helper knows about the live WeChat client.
struct WeChatTarget {
  let processIdentifier: pid_t
  let bundleIdentifier: String
  let marketingVersion: String
  let buildVersion: String
  let windowNumber: CGWindowID?
  let frame: WindowFrame?

  /// WeChat permits one desktop session, and its signed-out window is a small
  /// login panel. A main window at chat-window size is therefore the most
  /// reliable non-invasive signal that the account is signed in; the helper
  /// deliberately does not read any account data to answer this.
  var signedIn: Bool {
    frame?.isPlausibleMainWindow ?? false
  }

  /// The build string used by the compatibility matrix.
  var buildIdentifier: String {
    marketingVersion.isEmpty || buildVersion.isEmpty
      ? "" : "\(marketingVersion).\(buildVersion)"
  }

  /// Locates the running client, if any. Absence is a normal, reportable state.
  static func locate(bundleIdentifier: String) -> WeChatTarget? {
    guard
      let application = NSRunningApplication.runningApplications(
        withBundleIdentifier: bundleIdentifier
      ).first
    else { return nil }
    let info = Bundle(url: application.bundleURL ?? URL(fileURLWithPath: "/"))?.infoDictionary
    let (windowNumber, frame) = mainWindow(ownedBy: application.processIdentifier)
    return WeChatTarget(
      processIdentifier: application.processIdentifier,
      bundleIdentifier: bundleIdentifier,
      marketingVersion: info?["CFBundleShortVersionString"] as? String ?? "",
      buildVersion: info?["CFBundleVersion"] as? String ?? "",
      windowNumber: windowNumber,
      frame: frame
    )
  }

  /// The process's main chat window.
  ///
  /// Choosing the largest window is not sufficient, and was measured failing:
  /// the client keeps transient untitled shells around, one of which was larger
  /// than the chat window and blank, so the calibration regions read nothing.
  /// The chat window is the one that carries the client's own name, so a titled
  /// window is preferred and size only breaks ties among titled windows.
  ///
  /// Window bounds are readable without Screen Recording, so this works during
  /// onboarding too; only the window *name* needs the capture grant, and its
  /// absence degrades to the old size-based choice rather than failing.
  private static func mainWindow(ownedBy pid: pid_t) -> (CGWindowID?, WindowFrame?) {
    let options: CGWindowListOption = [.optionAll, .excludeDesktopElements]
    guard let windows = CGWindowListCopyWindowInfo(options, kCGNullWindowID) as? [[String: Any]]
    else { return (nil, nil) }
    var titled: (CGWindowID, WindowFrame)?
    var untitled: (CGWindowID, WindowFrame)?
    for window in windows {
      guard
        window[kCGWindowOwnerPID as String] as? pid_t == pid,
        window[kCGWindowLayer as String] as? Int == 0,
        let number = window[kCGWindowNumber as String] as? CGWindowID,
        let bounds = window[kCGWindowBounds as String] as? [String: CGFloat],
        let x = bounds["X"], let y = bounds["Y"],
        let width = bounds["Width"], let height = bounds["Height"],
        width > 0, height > 0
      else { continue }
      let frame = WindowFrame(x: x, y: y, width: width, height: height)
      let area = frame.size.width * frame.size.height
      let named = !((window[kCGWindowName as String] as? String) ?? "").isEmpty
      if named {
        if titled.map({ $0.1.size.width * $0.1.size.height < area }) ?? true {
          titled = (number, frame)
        }
      } else if untitled.map({ $0.1.size.width * $0.1.size.height < area }) ?? true {
        untitled = (number, frame)
      }
    }
    let chosen = titled ?? untitled
    return (chosen?.0, chosen?.1)
  }
}
