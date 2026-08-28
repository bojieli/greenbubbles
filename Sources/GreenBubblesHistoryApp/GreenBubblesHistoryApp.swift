import AppKit
import GreenBubblesHistory
import SwiftUI

@main
struct GreenBubblesHistoryApplication: App {
  @NSApplicationDelegateAdaptor(HistoryApplicationDelegate.self) private var applicationDelegate
  @State private var model = HistoryBrowserModel()

  var body: some Scene {
    WindowGroup("GreenBubbles History") {
      HistoryRootView(model: model)
        .frame(minWidth: 980, minHeight: 680)
    }
    .defaultSize(width: 1_320, height: 860)
    .commands {
      CommandGroup(replacing: .newItem) {
        Button("Open History Bundle…") {
          model.chooseBundle()
        }
        .keyboardShortcut("o")

        Button("Close History") {
          model.closeBundle()
        }
        .disabled(model.session == nil)
        .keyboardShortcut("w", modifiers: [.command, .shift])
      }
    }
  }
}

@MainActor
final class HistoryApplicationDelegate: NSObject, NSApplicationDelegate {
  func applicationDidFinishLaunching(_ notification: Notification) {
    NSApplication.shared.setActivationPolicy(.regular)
    NSApplication.shared.activate(ignoringOtherApps: true)
  }

  func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
    true
  }
}
