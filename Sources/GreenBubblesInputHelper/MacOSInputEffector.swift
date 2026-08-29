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
    case v = 0x09
    case returnKey = 0x24
    case delete = 0x33
    case escape = 0x35
  }

  private let processIdentifier: pid_t
  private let pasteboard: NSPasteboard
  private var savedPasteboardItems: [String]?
  private let humanIdleThresholdSeconds: TimeInterval

  init(
    processIdentifier: pid_t,
    pasteboard: NSPasteboard = .general,
    humanIdleThresholdSeconds: TimeInterval = 1.5
  ) {
    self.processIdentifier = processIdentifier
    self.pasteboard = pasteboard
    self.humanIdleThresholdSeconds = humanIdleThresholdSeconds
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

  /// True when real hardware input happened within the idle threshold.
  /// Synthesized events are posted from a private event source and to a
  /// specific process, so they never appear in the HID system state this
  /// reads — the check sees the human, not the helper.
  func humanActivityObserved() -> Bool {
    let types: [CGEventType] = [
      .keyDown, .flagsChanged, .leftMouseDown, .rightMouseDown, .scrollWheel,
    ]
    return types.contains { type in
      CGEventSource.secondsSinceLastEventType(.hidSystemState, eventType: type)
        < humanIdleThresholdSeconds
    }
  }

  func settle(milliseconds: UInt64) {
    Thread.sleep(forTimeInterval: Double(milliseconds) / 1_000)
  }
}
