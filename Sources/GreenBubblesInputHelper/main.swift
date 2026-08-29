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

/// Carries one reply across the service's callback boundary.
final class ReplyBox: @unchecked Sendable {
  private let lock = NSLock()
  private var storedPayload: Data?
  private var storedFailure: String?

  func store(_ payload: Data?, _ failure: String?) {
    lock.lock()
    defer { lock.unlock() }
    storedPayload = payload
    storedFailure = failure
  }

  var payload: Data? {
    lock.lock()
    defer { lock.unlock() }
    return storedPayload
  }

  var failure: String? {
    lock.lock()
    defer { lock.unlock() }
    return storedFailure
  }
}

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
  #if DEBUG
    // A long-lived helper activates its profile once, in the self-test, and
    // every later status reports it. When the dispatcher spawns a fresh process
    // per call there is nothing to remember, so this development-only option
    // performs the same verification-and-activation before reporting, rather
    // than reporting an activation that never happened.
    if let index = arguments.firstIndex(of: "--profile"), index + 1 < arguments.count,
      let data = try? Data(contentsOf: URL(fileURLWithPath: arguments[index + 1]))
    {
      let gate = DispatchSemaphore(value: 0)
      service.runCalibrationSelfTest(signedProfile: data) { payload, failure in
        if let failure {
          FileHandle.standardError.write(Data("self-test refused: \(failure)\n".utf8))
        } else if let payload {
          FileHandle.standardError.write(
            Data("self-test: \(String(decoding: payload, as: UTF8.self))\n".utf8))
        }
        gate.signal()
      }
      _ = gate.wait(timeout: .now() + 60)
    }
  #endif
  let status = service.currentStatus()
  if let data = try? SendCodec.encode(status), let text = String(data: data, encoding: .utf8) {
    print(text)
  } else {
    FileHandle.standardError.write(Data("could not encode the capability status\n".utf8))
    exit(2)
  }
#if DEBUG
  // Everything from here to `onboarding` drives input or reads screen content
  // *without* a capability, an approval, the outbox, the audit journal, or the
  // XPC peer check. That is acceptable in a development build a person runs
  // deliberately, and unacceptable in a binary that holds Accessibility and
  // Screen Recording, where it would be an unauthenticated bypass of the whole
  // safety architecture. Release builds must not contain it.
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
    guard let profilePath = probeOption("--profile"), let anchorName = probeOption("--anchor")
    else {
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
  case "execute-send-local":
    // Development-only. Runs the real HelperService methods in this process so
    // the control plane's dispatch path can be exercised on a machine where the
    // launchd agent has no TCC grants of its own. It is the same service object
    // the XPC listener exports, called directly rather than over the wire, and it
    // is compiled out of release builds with the other diagnostics.
    func localOption(_ name: String) -> String? {
      guard let i = arguments.firstIndex(of: name), i + 1 < arguments.count else { return nil }
      return arguments[i + 1]
    }
    guard let profilePath = localOption("--profile") else {
      FileHandle.standardError.write(Data("usage: execute-send-local --profile <f>\n".utf8))
      exit(2)
    }
    do {
      let local = HelperService()
      let profileData = try Data(contentsOf: URL(fileURLWithPath: profilePath))
      // The service refuses a capability until a self-test has verified and
      // activated a profile, exactly as it does over XPC.
      let selfTestGate = DispatchSemaphore(value: 0)
      let selfTestBox = ReplyBox()
      local.runCalibrationSelfTest(signedProfile: profileData) { payload, failure in
        selfTestBox.store(payload, failure)
        selfTestGate.signal()
      }
      guard selfTestGate.wait(timeout: .now() + 60) == .success else {
        throw SendFailure(.engineStall, detail: "the calibration self-test did not complete")
      }
      if let failure = selfTestBox.failure {
        FileHandle.standardError.write(Data("self-test refused: \(failure)\n".utf8))
        exit(2)
      }
      let capability = FileHandle.standardInput.readDataToEndOfFile()
      let sendGate = DispatchSemaphore(value: 0)
      let sendBox = ReplyBox()
      local.executeSend(capability: capability) { payload, failure in
        sendBox.store(payload, failure)
        sendGate.signal()
      }
      guard sendGate.wait(timeout: .now() + 120) == .success else {
        throw SendFailure(.engineStall, detail: "the send did not complete")
      }
      if let failure = sendBox.failure {
        FileHandle.standardError.write(Data("send refused: \(failure)\n".utf8))
        exit(2)
      }
      guard let payload = sendBox.payload else {
        throw SendFailure(.engineUnavailable, detail: "no outcome was produced")
      }
      print(String(decoding: payload, as: UTF8.self))
    } catch {
      FileHandle.standardError.write(Data("local send failed: \(error)\n".utf8))
      exit(2)
    }
  case "windows":
    // Read-only: what the locator is choosing between.
    if let target = WeChatTarget.locate(bundleIdentifier: "com.tencent.xinWeChat") {
      let options: CGWindowListOption = [.optionAll, .excludeDesktopElements]
      let windows =
        (CGWindowListCopyWindowInfo(options, kCGNullWindowID) as? [[String: Any]]) ?? []
      for window in windows
      where window[kCGWindowOwnerPID as String] as? pid_t == target.processIdentifier {
        let bounds = window[kCGWindowBounds as String] as? [String: CGFloat] ?? [:]
        let report: [String: Any] = [
          "number": window[kCGWindowNumber as String] as? Int ?? -1,
          "name": window[kCGWindowName as String] as? String ?? "",
          "layer": window[kCGWindowLayer as String] as? Int ?? -1,
          "alpha": window[kCGWindowAlpha as String] as? Double ?? -1,
          "onScreen": window[kCGWindowIsOnscreen as String] as? Bool ?? false,
          "width": bounds["Width"] ?? 0, "height": bounds["Height"] ?? 0,
          "y": bounds["Y"] ?? 0,
        ]
        print(
          String(
            decoding: try! JSONSerialization.data(withJSONObject: report, options: [.sortedKeys]),
            as: UTF8.self))
      }
    }
  case "click-at":
    // Development-only calibration aid: clicks one window-relative point given in
    // parts-per-million, so anchors can be located without inventing a profile.
    func clickOption(_ name: String) -> UInt32? {
      guard let i = arguments.firstIndex(of: name), i + 1 < arguments.count else { return nil }
      return UInt32(arguments[i + 1])
    }
    guard let xPPM = clickOption("--x"), let yPPM = clickOption("--y"),
      let target = WeChatTarget.locate(bundleIdentifier: "com.tencent.xinWeChat"),
      let frame = target.frame
    else {
      FileHandle.standardError.write(Data("usage: click-at --x <ppm> --y <ppm>\n".utf8))
      exit(2)
    }
    do {
      let effector = MacOSInputEffector(processIdentifier: target.processIdentifier)
      let point = WindowGeometry.point(
        WindowRelativePoint(xPartsPerMillion: xPPM, yPartsPerMillion: yPPM),
        in: frame
      )
      try effector.click(at: point)
      print("clicked \(Int(point.x)),\(Int(point.y))")
    } catch {
      FileHandle.standardError.write(Data("click failed: \(error)\n".utf8))
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
#endif
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
  var requirement = SendHelperIdentity.codeSigningRequirement(teamIdentifier: teamIdentifier)
  #if DEBUG
    // Development-only. A Developer-ID build pins an Apple-anchored requirement
    // that no ad-hoc binary can satisfy, which makes the XPC path impossible to
    // exercise outside a signed release. This override exists so it can be
    // exercised, and it is compiled out of release builds along with the other
    // diagnostics, so a shipped helper cannot be downgraded by an environment
    // variable.
    if let override = ProcessInfo.processInfo.environment["GREENBUBBLES_XPC_REQUIREMENT"],
      !override.isEmpty
    {
      FileHandle.standardError.write(
        Data("using a development XPC requirement override\n".utf8))
      requirement = override
    }
  #endif
  let delegate = HelperListenerDelegate(service: service, requirement: requirement)
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
