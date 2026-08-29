import AppKit
import CoreGraphics
import Foundation
import GreenBubblesSendKit

/// `GreenBubblesInputHelper` — the only component that holds Accessibility and
/// Screen Recording.
///
/// It is started by the main application as a managed login item, never by the
/// user and never by a downloaded installer. Two modes:
///
/// * no arguments — run the XPC listener that the control plane talks to. Only
///   a peer signed by our team may connect.
/// * `probe` — print the capability status as JSON and exit. Read-only, no
///   send; used by onboarding and by support diagnostics before the Mach
///   service is registered.
let service = HelperService()

/// Accepts only peers that satisfy the pinned code-signing requirement, which
/// is what lets the XPC surface stay high level: peer identity is verified by
/// the platform rather than by a hand-rolled token.
final class HelperListenerDelegate: NSObject, NSXPCListenerDelegate, @unchecked Sendable {
  private let service: HelperService
  private let requirement: String

  init(service: HelperService, requirement: String) {
    self.service = service
    self.requirement = requirement
  }

  func listener(
    _ listener: NSXPCListener,
    shouldAcceptNewConnection connection: NSXPCConnection
  ) -> Bool {
    // The platform enforces the requirement: a peer that does not satisfy it
    // has its connection invalidated rather than being handed the interface.
    connection.setCodeSigningRequirement(requirement)
    connection.exportedInterface = NSXPCInterface(with: GreenBubblesInputHelperProtocol.self)
    connection.exportedObject = service
    connection.resume()
    return true
  }
}

let arguments = Array(CommandLine.arguments.dropFirst())
switch arguments.first {
case "probe":
  let status = service.currentStatus()
  if let data = try? SendCodec.encode(status), let text = String(data: data, encoding: .utf8) {
    print(text)
  } else {
    FileHandle.standardError.write(Data("could not encode the capability status\n".utf8))
    exit(2)
  }
case "capture":
  guard arguments.count >= 2 else {
    FileHandle.standardError.write(Data("usage: capture <output.png>\n".utf8))
    exit(2)
  }
  do {
    try SpikeHarness.capture(to: arguments[1], bundleIdentifier: "com.tencent.xinWeChat")
  } catch {
    FileHandle.standardError.write(Data("capture failed: \(error)\n".utf8))
    exit(2)
  }
case "spike":
  func option(_ name: String) -> String? {
    guard let index = arguments.firstIndex(of: name), index + 1 < arguments.count else {
      return nil
    }
    return arguments[index + 1]
  }
  guard let profile = option("--profile"), let capability = option("--capability") else {
    FileHandle.standardError.write(
      Data("usage: spike --profile <file> --capability <file> [--trust-root <file>]\n".utf8)
    )
    exit(2)
  }
  do {
    try SpikeHarness.spike(
      profilePath: profile,
      capabilityPath: capability,
      trustRootPath: option("--trust-root")
    )
  } catch {
    FileHandle.standardError.write(Data("spike failed: \(error)\n".utf8))
    exit(2)
  }
case "collision-probe":
  // Answers one question that decides whether the human-collision guard is
  // usable at all: do our own synthesized events register as hardware input?
  guard let target = WeChatTarget.locate(bundleIdentifier: "com.tencent.xinWeChat") else {
    FileHandle.standardError.write(Data("WeChat is not running\n".utf8))
    exit(2)
  }
  let effector = MacOSInputEffector(processIdentifier: target.processIdentifier)
  func idleSeconds() -> [String: Double] {
    [
      "keyDown": CGEventSource.secondsSinceLastEventType(.hidSystemState, eventType: .keyDown),
      "leftMouseDown": CGEventSource.secondsSinceLastEventType(
        .hidSystemState, eventType: .leftMouseDown),
      "flagsChanged": CGEventSource.secondsSinceLastEventType(
        .hidSystemState, eventType: .flagsChanged),
    ]
  }
  let before = idleSeconds()
  try? effector.press(.escape)
  Thread.sleep(forTimeInterval: 0.3)
  let afterKey = idleSeconds()
  let report: [String: Any] = [
    "beforeIdleSeconds": before,
    "afterSynthesizedKeyIdleSeconds": afterKey,
    "synthesizedEventsCountAsHumanInput": afterKey["keyDown"]! < before["keyDown"]!,
    "humanActivityObserved": effector.humanActivityObserved(),
    "targetIsFrontmost": NSWorkspace.shared.frontmostApplication?.processIdentifier
      == target.processIdentifier,
    "collisionIfTargetWereFrontmost": MacOSInputEffector(
      processIdentifier: target.processIdentifier,
      frontmostProcessIdentifier: { target.processIdentifier }
    ).humanActivityObserved(),
  ]
  let data = try! JSONSerialization.data(withJSONObject: report, options: [.sortedKeys])
  print(String(decoding: data, as: UTF8.self))
case "keys":
  // Diagnostic only: posts a fixed sequence of the five reviewed keys to the
  // target. Used to clean up after a failed addressing attempt and to answer
  // whether a keyboard shortcut can move focus where a click cannot.
  guard let target = WeChatTarget.locate(bundleIdentifier: "com.tencent.xinWeChat") else {
    FileHandle.standardError.write(Data("WeChat is not running\n".utf8))
    exit(2)
  }
  let effector = MacOSInputEffector(processIdentifier: target.processIdentifier)
  guard !effector.humanActivityObserved() else {
    FileHandle.standardError.write(Data("refused: the machine is not idle\n".utf8))
    exit(2)
  }
  // Diagnostic-only escape hatch, deliberately not a reviewed SendKey: answers
  // whether a keyboard shortcut can move focus where a background click cannot.
  if arguments.dropFirst().first == "pasteFileReference", arguments.count >= 3 {
    let effector = MacOSInputEffector(processIdentifier: target.processIdentifier)
    do {
      try effector.writeClipboardFileReference(arguments[2])
      try effector.press(.paste)
    } catch {
      FileHandle.standardError.write(Data("paste failed: \(error)\n".utf8))
      exit(2)
    }
    Thread.sleep(forTimeInterval: 1.0)
    effector.restoreClipboard()
    print("pasted a file reference into whatever holds focus")
    exit(0)
  }
  if arguments.dropFirst().first == "pasteSearchKey" {
    let effector = MacOSInputEffector(processIdentifier: target.processIdentifier)
    try? effector.writeClipboard("File Transfer")
    try? effector.press(.paste)
    Thread.sleep(forTimeInterval: 0.4)
    effector.restoreClipboard()
    print("pasted the search key into whatever holds focus")
    exit(0)
  }
  if arguments.dropFirst().first == "searchShortcut" {
    if let source = CGEventSource(stateID: .privateState) {
      for isDown in [true, false] {
        if let event = CGEvent(keyboardEventSource: source, virtualKey: 0x03, keyDown: isDown) {
          event.flags = .maskCommand
          event.postToPid(target.processIdentifier)
        }
      }
    }
    print("posted the search shortcut")
    exit(0)
  }
  for name in arguments.dropFirst() {
    let key: SendKey? =
      switch name {
      case "selectAll": .selectAll
      case "delete": .delete
      case "escape": .escape
      case "paste": .paste
      case "goToFolder": .goToFolder
      case "returnKey": .returnKey
      default: nil
      }
    guard let key else {
      FileHandle.standardError.write(Data("unsupported key: \(name)\n".utf8))
      exit(2)
    }
    try? effector.press(key)
    Thread.sleep(forTimeInterval: 0.15)
  }
  print("posted \(arguments.count - 1) keys")
case "focus-probe":
  // Answers the question the whole design rests on: can a posted click take
  // keyboard focus? Clicks one named anchor, pastes a marker, and reports what
  // the region reads back.
  func probeOption(_ name: String) -> String? {
    guard let i = arguments.firstIndex(of: name), i + 1 < arguments.count else { return nil }
    return arguments[i + 1]
  }
  guard let profilePath = probeOption("--profile"), let anchorName = probeOption("--anchor") else {
    FileHandle.standardError.write(
      Data("usage: focus-probe --profile <f> --anchor composeBox|searchBox\n".utf8))
    exit(2)
  }
  do {
    let data = try Data(contentsOf: URL(fileURLWithPath: profilePath))
    let signed = try SendCodec.decode(SignedCalibrationProfile.self, from: data)
    guard let target = WeChatTarget.locate(bundleIdentifier: "com.tencent.xinWeChat"),
      let frame = target.frame
    else { throw SendFailure(.wechatNotRunning) }
    let anchors = signed.body.anchors
    let regions = signed.body.ocrRegions
    let (anchor, region) =
      anchorName == "searchBox"
      ? (anchors.searchBox, regions.search) : (anchors.composeBox, regions.compose)
    let effector = MacOSInputEffector(processIdentifier: target.processIdentifier)
    guard !effector.humanActivityObserved() else {
      FileHandle.standardError.write(Data("refused: the machine is not idle\n".utf8))
      exit(2)
    }
    let perception = MacOSScreenPerception(
      processIdentifier: target.processIdentifier,
      bundleIdentifier: "com.tencent.xinWeChat"
    )
    let before = try perception.recognizeText(in: WindowGeometry.rect(region, in: frame))
    try effector.click(at: WindowGeometry.point(anchor, in: frame))
    Thread.sleep(forTimeInterval: 0.4)
    let marker = "GBSPIKE-FOCUS"
    try effector.writeClipboard(marker)
    try effector.press(.paste)
    Thread.sleep(forTimeInterval: 0.8)
    let after = try perception.recognizeText(in: WindowGeometry.rect(region, in: frame))
    effector.restoreClipboard()
    let took = after.text.contains(marker)
    let report: [String: Any] = [
      "anchor": anchorName,
      "textBefore": before.text,
      "textAfter": after.text,
      "clickTookFocus": took,
    ]
    print(
      String(
        decoding: try JSONSerialization.data(withJSONObject: report, options: [.sortedKeys]),
        as: UTF8.self))
  } catch {
    FileHandle.standardError.write(Data("focus probe failed: \(error)\n".utf8))
    exit(2)
  }
case "ocr":
  // Read-only calibration aid: prints what the gates actually read out of one
  // named region of the signed profile.
  func opt(_ name: String) -> String? {
    guard let i = arguments.firstIndex(of: name), i + 1 < arguments.count else { return nil }
    return arguments[i + 1]
  }
  guard let profilePath = opt("--profile"), let regionName = opt("--region") else {
    FileHandle.standardError.write(Data("usage: ocr --profile <f> --region <name>\n".utf8))
    exit(2)
  }
  do {
    let data = try Data(contentsOf: URL(fileURLWithPath: profilePath))
    let signed = try SendCodec.decode(SignedCalibrationProfile.self, from: data)
    guard let target = WeChatTarget.locate(bundleIdentifier: "com.tencent.xinWeChat"),
      let frame = target.frame
    else { throw SendFailure(.wechatNotRunning) }
    let regions = signed.body.ocrRegions
    let region: WindowRelativeRect? =
      switch regionName {
      case "search": regions.search
      case "title": regions.title
      case "compose": regions.compose
      case "newestOutgoing": regions.newestOutgoing
      case "composeAttachment": signed.body.attachments?.composeAttachment
      default: nil
      }
    guard let region else {
      FileHandle.standardError.write(Data("unknown region: \(regionName)\n".utf8))
      exit(2)
    }
    let perception = MacOSScreenPerception(
      processIdentifier: target.processIdentifier,
      bundleIdentifier: "com.tencent.xinWeChat"
    )
    let read = try perception.recognizeText(in: WindowGeometry.rect(region, in: frame))
    let report: [String: Any] = [
      "region": regionName,
      "text": read.text,
      "confidencePartsPerMillion": read.confidencePartsPerMillion,
      "candidateCount": read.candidateCount,
    ]
    print(
      String(
        decoding: try JSONSerialization.data(withJSONObject: report, options: [.sortedKeys]),
        as: UTF8.self))
  } catch {
    FileHandle.standardError.write(Data("ocr failed: \(error)\n".utf8))
    exit(2)
  }
case "onboarding":
  let plan = OnboardingPlan.make(from: service.currentStatus())
  for step in plan.steps where !step.granted {
    print("\(step.title)\n  \(step.rationale)")
    for instruction in step.instructions { print("  - \(instruction)") }
    print("  \(step.settingsURL.absoluteString)")
  }
  print(plan.complete ? "all required grants are present" : "grants are missing")
case nil:
  let teamIdentifier = ProcessInfo.processInfo.environment["GREENBUBBLES_TEAM_IDENTIFIER"] ?? ""
  let delegate = HelperListenerDelegate(
    service: service,
    requirement: SendHelperIdentity.codeSigningRequirement(teamIdentifier: teamIdentifier)
  )
  let listener = NSXPCListener(machServiceName: SendHelperIdentity.machServiceName)
  listener.delegate = delegate
  listener.resume()
  RunLoop.main.run()
default:
  FileHandle.standardError.write(
    Data("usage: greenbubbles-input-helper [probe|onboarding]\n".utf8)
  )
  exit(2)
}
