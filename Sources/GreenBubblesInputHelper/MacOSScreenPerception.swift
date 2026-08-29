import CoreGraphics
import Foundation
import GreenBubblesSendKit
import ScreenCaptureKit
import Vision

/// Window capture plus on-device text recognition.
///
/// Capture uses ScreenCaptureKit's window filter, so WeChat can stay
/// backgrounded and occluded throughout — nothing is brought to the front and
/// the user's screen is never disturbed. Recognition uses Apple Vision: first
/// party, public, entirely on device, no network and no model of ours. WeChat's
/// own `wxocr` is deliberately not used; it is a closed internal dylib and
/// loading it would reintroduce exactly the per-build private dependency this
/// design exists to avoid.
final class MacOSScreenPerception: ScreenPerception {
  private let processIdentifier: pid_t
  private let bundleIdentifier: String
  private let captureTimeout: TimeInterval
  private let recognitionLanguages: [String]
  private var captures: UInt32 = 0

  init(
    processIdentifier: pid_t,
    bundleIdentifier: String,
    captureTimeout: TimeInterval = 10,
    recognitionLanguages: [String] = ["en-US", "zh-Hans", "zh-Hant"]
  ) {
    self.processIdentifier = processIdentifier
    self.bundleIdentifier = bundleIdentifier
    self.captureTimeout = captureTimeout
    self.recognitionLanguages = recognitionLanguages
  }

  var captureCount: UInt32 { captures }

  func windowFrame() throws(SendFailure) -> WindowFrame {
    guard let target = WeChatTarget.locate(bundleIdentifier: bundleIdentifier),
      target.processIdentifier == processIdentifier,
      let frame = target.frame
    else {
      throw SendFailure(.windowNotFound, detail: "the client's main window was not found")
    }
    return frame
  }

  func recognizeText(in rect: CGRect) throws(SendFailure) -> RecognizedRegionText {
    let frame = try windowFrame()
    let image = try captureWindow()
    captures &+= 1
    guard let cropped = crop(image, to: rect, window: frame) else {
      throw SendFailure(
        .calibrationDrift,
        detail: "the profile's region falls outside the captured window"
      )
    }
    return try recognize(cropped)
  }

  /// Captures the target window even when it is occluded or behind other
  /// spaces. The async API is bridged with a bounded wait; the helper's caller
  /// owns an independent watchdog, so a hung capture can never wedge a send.
  private func captureWindow() throws(SendFailure) -> CGImage {
    let semaphore = DispatchSemaphore(value: 0)
    let box = CaptureResultBox()
    let processIdentifier = processIdentifier
    Task { @Sendable in
      do {
        let content = try await SCShareableContent.excludingDesktopWindows(
          true,
          onScreenWindowsOnly: false
        )
        let candidates = content.windows.filter { window in
          window.owningApplication?.processID == processIdentifier
            && window.frame.width > 0
            && window.frame.height > 0
        }
        guard
          let window = candidates.max(by: {
            $0.frame.width * $0.frame.height < $1.frame.width * $1.frame.height
          })
        else {
          box.store(.failure(SendFailure(.windowNotFound, detail: "no capturable window")))
          semaphore.signal()
          return
        }
        let configuration = SCStreamConfiguration()
        configuration.width = Int(window.frame.width * 2)
        configuration.height = Int(window.frame.height * 2)
        configuration.showsCursor = false
        configuration.captureResolution = .best
        let filter = SCContentFilter(desktopIndependentWindow: window)
        let image = try await SCScreenshotManager.captureImage(
          contentFilter: filter,
          configuration: configuration
        )
        box.store(.success(image))
      } catch {
        box.store(
          .failure(
            SendFailure(
              .grantsMissing,
              detail: "window capture failed: \(error.localizedDescription)"
            )
          )
        )
      }
      semaphore.signal()
    }
    guard semaphore.wait(timeout: .now() + captureTimeout) == .success else {
      throw SendFailure(.engineStall, detail: "window capture did not complete in time")
    }
    switch box.value {
    case .success(let image): return image
    case .failure(let failure): throw failure
    case nil: throw SendFailure(.engineUnavailable, detail: "window capture produced no result")
    }
  }

  /// Converts a global-point region into image pixels and crops.
  private func crop(_ image: CGImage, to rect: CGRect, window: WindowFrame) -> CGImage? {
    guard window.size.width > 0, window.size.height > 0 else { return nil }
    let scaleX = Double(image.width) / window.size.width
    let scaleY = Double(image.height) / window.size.height
    let local = CGRect(
      x: (rect.origin.x - window.origin.x) * scaleX,
      y: (rect.origin.y - window.origin.y) * scaleY,
      width: rect.size.width * scaleX,
      height: rect.size.height * scaleY
    ).integral
    let bounds = CGRect(x: 0, y: 0, width: image.width, height: image.height)
    let clamped = local.intersection(bounds)
    guard clamped.width >= 1, clamped.height >= 1 else { return nil }
    return image.cropping(to: clamped)
  }

  private func recognize(_ image: CGImage) throws(SendFailure) -> RecognizedRegionText {
    let request = VNRecognizeTextRequest()
    request.recognitionLevel = .accurate
    request.usesLanguageCorrection = false
    request.recognitionLanguages = recognitionLanguages
    let handler = VNImageRequestHandler(cgImage: image, options: [:])
    do {
      try handler.perform([request])
    } catch {
      throw SendFailure(
        .engineUnavailable,
        detail: "text recognition failed: \(error.localizedDescription)"
      )
    }
    let observations = request.results ?? []
    var lines: [String] = []
    var lowestConfidence: Float = 1
    for observation in observations {
      guard let candidate = observation.topCandidates(1).first else { continue }
      let text = candidate.string.trimmingCharacters(in: .whitespacesAndNewlines)
      guard !text.isEmpty else { continue }
      lines.append(text)
      lowestConfidence = min(lowestConfidence, candidate.confidence)
    }
    let confidence = lines.isEmpty ? 0 : UInt32((lowestConfidence * 1_000_000).rounded())
    return RecognizedRegionText(
      text: lines.joined(separator: "\n"),
      confidencePartsPerMillion: min(confidence, CalibrationProfileConstants.partsPerMillion),
      candidateCount: lines.count
    )
  }
}

/// Carries one capture result across the async boundary. The semaphore in
/// `captureWindow` orders the single write before the single read, and the
/// lock makes that ordering explicit to the compiler.
private final class CaptureResultBox: @unchecked Sendable {
  private let lock = NSLock()
  private var stored: Result<CGImage, SendFailure>?

  func store(_ result: Result<CGImage, SendFailure>) {
    lock.lock()
    defer { lock.unlock() }
    stored = result
  }

  var value: Result<CGImage, SendFailure>? {
    lock.lock()
    defer { lock.unlock() }
    return stored
  }
}
