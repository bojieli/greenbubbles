import AppKit
import CoreGraphics
import Foundation
import GreenBubblesSendKit

/// The first-party effector.
///
/// Every event is posted directly to the target process with
/// `CGEvent.postToPid`, a public CoreGraphics entry point. That is what makes
/// the whole design possible without a private framework: the target receives
/// the click or keystroke, while the user's physical cursor never moves, no
/// application is raised, and no Space switches. The spike's methodology is
/// preserved exactly — the mouse only ever *focuses* a box, and the keyboard
/// performs every mutation.
final class MacOSInputEffector: InputEffector {
  private enum VirtualKey: CGKeyCode {
    case a = 0x00
    case g = 0x05
    case v = 0x09
    case returnKey = 0x24
    case delete = 0x33
    case escape = 0x35
  }

  private let processIdentifier: pid_t
  private let pasteboard: NSPasteboard
  private var savedPasteboardItems: [String]?
  private let humanIdleThresholdSeconds: TimeInterval
  private let frontmostProcessIdentifier: @Sendable () -> pid_t?

  init(
    processIdentifier: pid_t,
    pasteboard: NSPasteboard = .general,
    humanIdleThresholdSeconds: TimeInterval = HumanCollisionPolicy.defaultIdleThresholdSeconds,
    frontmostProcessIdentifier: @escaping @Sendable () -> pid_t? = {
      NSWorkspace.shared.frontmostApplication?.processIdentifier
    }
  ) {
    self.processIdentifier = processIdentifier
    self.pasteboard = pasteboard
    self.humanIdleThresholdSeconds = humanIdleThresholdSeconds
    self.frontmostProcessIdentifier = frontmostProcessIdentifier
  }

  func click(at point: CGPoint) throws(SendFailure) {
    guard let source = CGEventSource(stateID: .privateState) else {
      throw SendFailure(.engineUnavailable, detail: "could not create a private event source")
    }
    for type in [CGEventType.leftMouseDown, .leftMouseUp] {
      guard
        let event = CGEvent(
          mouseEventSource: source,
          mouseType: type,
          mouseCursorPosition: point,
          mouseButton: .left
        )
      else {
        throw SendFailure(.engineUnavailable, detail: "could not synthesize a mouse event")
      }
      event.postToPid(processIdentifier)
    }
  }

  func press(_ key: SendKey) throws(SendFailure) {
    let (virtualKey, flags): (VirtualKey, CGEventFlags) =
      switch key {
      case .returnKey: (.returnKey, [])
      case .delete: (.delete, [])
      case .escape: (.escape, [])
      case .selectAll: (.a, .maskCommand)
      case .paste: (.v, .maskCommand)
      case .goToFolder: (.g, [.maskCommand, .maskShift])
      }
    guard let source = CGEventSource(stateID: .privateState) else {
      throw SendFailure(.engineUnavailable, detail: "could not create a private event source")
    }
    for isDown in [true, false] {
      guard
        let event = CGEvent(
          keyboardEventSource: source,
          virtualKey: virtualKey.rawValue,
          keyDown: isDown
        )
      else {
        throw SendFailure(.engineUnavailable, detail: "could not synthesize a keyboard event")
      }
      event.flags = flags
      event.postToPid(processIdentifier)
    }
  }

  /// Puts a reference to one already-staged file on the pasteboard.
  ///
  /// The helper writes a URL, not bytes: it never opens or reads the file, so a
  /// compromised helper gains a path rather than a copy. The path is always the
  /// staged copy the capability named.
  func writeClipboardFileReference(_ path: String) throws(SendFailure) {
    savePasteboardIfNeeded()
    let url = URL(fileURLWithPath: path)
    var isDirectory: ObjCBool = false
    guard FileManager.default.fileExists(atPath: path, isDirectory: &isDirectory),
      !isDirectory.boolValue
    else {
      throw SendFailure(.attachmentStagingFailed, detail: "the staged attachment is not a file")
    }
    pasteboard.clearContents()
    guard pasteboard.writeObjects([url as NSURL]) else {
      throw SendFailure(.engineUnavailable, detail: "the pasteboard refused the file reference")
    }
  }

  /// How many windows the target process currently owns. Used to prove that an
  /// open panel or a confirmation sheet actually appeared before acting on it.
  func targetWindowCount() -> Int {
    let options: CGWindowListOption = [.optionAll, .excludeDesktopElements]
    guard let windows = CGWindowListCopyWindowInfo(options, kCGNullWindowID) as? [[String: Any]]
    else { return 0 }
    return windows.filter { window in
      window[kCGWindowOwnerPID as String] as? pid_t == processIdentifier
    }.count
  }

  func writeClipboard(_ text: String) throws(SendFailure) {
    if savedPasteboardItems == nil {
      savedPasteboardItems =
        pasteboard.pasteboardItems?.compactMap { $0.string(forType: .string) }
        ?? []
    }
    pasteboard.clearContents()
    guard pasteboard.setString(text, forType: .string) else {
      throw SendFailure(.engineUnavailable, detail: "the pasteboard refused the payload")
    }
  }

  /// Snapshots the user's pasteboard once per run, so every exit path can put
  /// it back exactly as it was.
  private func savePasteboardIfNeeded() {
    guard savedPasteboardItems == nil else { return }
    savedPasteboardItems =
      pasteboard.pasteboardItems?.compactMap { $0.string(forType: .string) } ?? []
  }

  /// Puts back whatever the user had copied. The skill calls this on every
  /// exit path, including refusals, so an aborted run leaves no trace.
  func restoreClipboard() {
    guard let saved = savedPasteboardItems else { return }
    pasteboard.clearContents()
    if let first = saved.first {
      pasteboard.setString(first, forType: .string)
    }
    savedPasteboardItems = nil
  }

  /// True when the user is actually working *in the target client*.
  ///
  /// Synthesized events are posted from a private source directly to one
  /// process, so they never appear in the HID system state this reads: the
  /// check sees the human, not the helper. That is measured, not assumed —
  /// `collision-probe` reports `synthesizedEventsCountAsHumanInput: false`.
  ///
  /// Superseded note: true when real hardware input happened within the idle
  /// threshold.
  /// Synthesized events are posted from a private event source and to a
  /// specific process, so they never appear in the HID system state this
  /// reads — the check sees the human, not the helper.
  func humanActivityObserved() -> Bool {
    let types: [CGEventType] = [
      .keyDown, .flagsChanged, .leftMouseDown, .rightMouseDown, .scrollWheel,
    ]
    return HumanCollisionPolicy.mustYield(
      targetIsFrontmost: frontmostProcessIdentifier() == processIdentifier,
      idleSecondsByEventType: types.map { type in
        CGEventSource.secondsSinceLastEventType(.hidSystemState, eventType: type)
      },
      thresholdSeconds: humanIdleThresholdSeconds
    )
  }

  func settle(milliseconds: UInt64) {
    Thread.sleep(forTimeInterval: Double(milliseconds) / 1_000)
  }
}
