import Foundation

public struct HistoryBrowserLaunchOptions: Equatable, Sendable {
  public let bundleURL: URL?

  /// Parses arguments after the executable name. Bundle contents are not
  /// opened here; `HistoryBundleLoader` remains the single verification gate.
  public init(arguments: [String], currentDirectoryURL: URL) throws {
    guard currentDirectoryURL.isFileURL else {
      throw HistoryBrowserLaunchError.invalidCurrentDirectory
    }

    var rawBundlePath: String?
    var index = 0
    while index < arguments.count {
      let argument = arguments[index]
      let value: String
      if argument == "--bundle" {
        index += 1
        guard index < arguments.count else {
          throw HistoryBrowserLaunchError.missingBundlePath
        }
        value = arguments[index]
      } else if argument.hasPrefix("--bundle=") {
        value = String(argument.dropFirst("--bundle=".count))
      } else {
        throw HistoryBrowserLaunchError.unsupportedArgument
      }

      guard rawBundlePath == nil else {
        throw HistoryBrowserLaunchError.duplicateBundle
      }
      guard !value.isEmpty else {
        throw HistoryBrowserLaunchError.missingBundlePath
      }
      rawBundlePath = value
      index += 1
    }

    guard let rawBundlePath else {
      bundleURL = nil
      return
    }
    let expandedPath = (rawBundlePath as NSString).expandingTildeInPath
    bundleURL = URL(
      fileURLWithPath: expandedPath,
      relativeTo: currentDirectoryURL.standardizedFileURL
    ).standardizedFileURL
  }

  public static func normalizeOpenedURL(_ url: URL) -> URL {
    let standardized = url.standardizedFileURL
    if standardized.lastPathComponent == "manifest.json" {
      return standardized.deletingLastPathComponent()
    }
    return standardized
  }
}

public enum HistoryBrowserLaunchError: Error, Equatable, CustomStringConvertible, Sendable {
  case invalidCurrentDirectory
  case missingBundlePath
  case duplicateBundle
  case unsupportedArgument

  public var description: String {
    switch self {
    case .invalidCurrentDirectory:
      "The history browser could not resolve its working directory."
    case .missingBundlePath:
      "Usage: greenbubbles-history [--bundle <private AI-context directory>]"
    case .duplicateBundle:
      "Open one history bundle at a time."
    case .unsupportedArgument:
      "Usage: greenbubbles-history [--bundle <private AI-context directory>]"
    }
  }
}
