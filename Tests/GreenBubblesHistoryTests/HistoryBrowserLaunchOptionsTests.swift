import Foundation
import GreenBubblesHistory
import Testing

@Suite("HistoryBrowserLaunchOptionsTests")
struct HistoryBrowserLaunchOptionsTests {
  private let workingDirectory = URL(
    fileURLWithPath: "/private/tmp/greenbubbles-launch-tests",
    isDirectory: true)

  @Test("accepts an explicit relative bundle directory")
  func acceptsRelativeBundle() throws {
    let options = try HistoryBrowserLaunchOptions(
      arguments: ["--bundle", "exports/context"],
      currentDirectoryURL: workingDirectory
    )
    #expect(
      options.bundleURL?.path
        == "/private/tmp/greenbubbles-launch-tests/exports/context")
  }

  @Test("accepts the equals form and an absolute directory")
  func acceptsEqualsForm() throws {
    let options = try HistoryBrowserLaunchOptions(
      arguments: ["--bundle=/private/tmp/context"],
      currentDirectoryURL: workingDirectory
    )
    #expect(options.bundleURL?.path == "/private/tmp/context")
  }

  @Test("allows a panel-only launch")
  func allowsEmptyArguments() throws {
    let options = try HistoryBrowserLaunchOptions(
      arguments: [],
      currentDirectoryURL: workingDirectory
    )
    #expect(options.bundleURL == nil)
  }

  @Test("rejects missing duplicate and unknown launch options", arguments: [
    ["--bundle"],
    ["--bundle", "one", "--bundle", "two"],
    ["--unknown"],
  ])
  func rejectsInvalidArguments(arguments: [String]) {
    #expect(throws: HistoryBrowserLaunchError.self) {
      try HistoryBrowserLaunchOptions(
        arguments: arguments,
        currentDirectoryURL: workingDirectory
      )
    }
  }

  @Test("normalizes a manifest open event to its bundle")
  func normalizesManifestURL() {
    let manifest = URL(fileURLWithPath: "/private/tmp/context/manifest.json")
    #expect(
      HistoryBrowserLaunchOptions.normalizeOpenedURL(manifest).path
        == "/private/tmp/context")
  }
}
