import Foundation
import GreenBubblesWeb

enum ArticleCLIError: Error, CustomStringConvertible {
  case usage

  var description: String {
    switch self {
    case .usage:
      return "Usage: greenbubbles-public-article <owner-only-request.json>"
    }
  }
}

@main
struct GreenBubblesArticleCLI {
  static func main() async {
    do {
      guard CommandLine.arguments.count == 2 else { throw ArticleCLIError.usage }
      let request = try OwnerOnlyPublicArticleRequestLoader.load(
        URL(fileURLWithPath: CommandLine.arguments[1])
      )
      let document = try await PublicArticleFetcher().fetch(request)
      let encoder = JSONEncoder()
      encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
      encoder.dateEncodingStrategy = .iso8601
      FileHandle.standardOutput.write(try encoder.encode(document))
      FileHandle.standardOutput.write(Data("\n".utf8))
    } catch {
      FileHandle.standardError.write(Data("error: \(error)\n".utf8))
      exit(2)
    }
  }

}
