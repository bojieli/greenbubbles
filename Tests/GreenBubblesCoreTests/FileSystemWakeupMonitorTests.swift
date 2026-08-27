import Dispatch
import Foundation
import Testing

@testable import GreenBubblesCore

@Suite("FileSystemWakeupMonitorTests")
struct FileSystemWakeupMonitorTests {
  @Test
  func mapsKernelFlagsWithoutTreatingThemAsMessages() {
    let reasons = FileSystemWakeupMonitor.reasons(for: [.write, .rename, .extend])
    #expect(reasons == [.write, .rename, .extend])
  }

  @Test
  func observesDirectoryActivityWithRedactedRoot() throws {
    let root = FileManager.default.temporaryDirectory.appending(
      path: "greenbubbles-wakeup-\(UUID().uuidString)",
      directoryHint: .isDirectory
    )
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
    defer { try? FileManager.default.removeItem(at: root) }

    let signal = DispatchSemaphore(value: 0)
    let monitor = FileSystemWakeupMonitor()
    try monitor.start(roots: [root]) { event in
      if event.root.path == nil, event.reasons.contains(.write) {
        signal.signal()
      }
    }
    defer { monitor.stop() }

    try Data("wake".utf8).write(to: root.appending(path: "change"))
    #expect(signal.wait(timeout: .now() + 3) == .success)
  }
}
