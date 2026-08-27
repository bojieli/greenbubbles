import Darwin
import Foundation

public struct PublicArticleRequest: Codable, Equatable, Sendable {
  public let formatVersion: Int
  public let url: URL

  public init(formatVersion: Int = 1, url: URL) {
    self.formatVersion = formatVersion
    self.url = url
  }
}

public struct PublicArticleDocument: Codable, Equatable, Sendable {
  public let formatVersion: Int
  public let sourceURL: URL
  public let finalURL: URL
  public let fetchedAt: Date
  public let title: String?
  public let author: String?
  public let description: String?
  public let bodyText: String
  public let bodyUTF8ByteCount: Int
  public let accessEvidence: PublicArticleAccessEvidence

  public init(
    formatVersion: Int = 1,
    sourceURL: URL,
    finalURL: URL,
    fetchedAt: Date,
    title: String?,
    author: String?,
    description: String?,
    bodyText: String,
    accessEvidence: PublicArticleAccessEvidence
  ) {
    self.formatVersion = formatVersion
    self.sourceURL = sourceURL
    self.finalURL = finalURL
    self.fetchedAt = fetchedAt
    self.title = title
    self.author = author
    self.description = description
    self.bodyText = bodyText
    self.bodyUTF8ByteCount = bodyText.lengthOfBytes(using: .utf8)
    self.accessEvidence = accessEvidence
  }
}

public struct PublicArticleAccessEvidence: Codable, Equatable, Sendable {
  public let robotsChecked: Bool
  public let robotsAllowed: Bool
  public let unauthenticatedRequest: Bool
  public let cookiesDisabled: Bool
  public let redirectsFollowed: Int
  public let completeness: String

  public init(
    robotsChecked: Bool,
    robotsAllowed: Bool,
    unauthenticatedRequest: Bool,
    cookiesDisabled: Bool,
    redirectsFollowed: Int,
    completeness: String = "singlePublicPage"
  ) {
    self.robotsChecked = robotsChecked
    self.robotsAllowed = robotsAllowed
    self.unauthenticatedRequest = unauthenticatedRequest
    self.cookiesDisabled = cookiesDisabled
    self.redirectsFollowed = redirectsFollowed
    self.completeness = completeness
  }
}

public enum PublicArticleError: Error, Equatable, CustomStringConvertible {
  case unsupportedRequestFormat
  case unsafeRequestFile
  case invalidPublicArticleURL
  case transportFailure
  case tooManyRedirects
  case responseTooLarge
  case robotsUnavailable
  case robotsDenied
  case unexpectedStatus(Int)
  case unsupportedContentType
  case invalidTextEncoding
  case authenticationRequired
  case paywallDetected
  case unsupportedDocument
  case extractedArticleTooLarge

  public var description: String {
    switch self {
    case .unsupportedRequestFormat:
      return "The public-article request format is unsupported"
    case .unsafeRequestFile:
      return "The request must be a regular, single-link, owner-only file of at most 16384 bytes"
    case .invalidPublicArticleURL:
      return "Only ordinary HTTPS WeChat public-article URLs are accepted"
    case .transportFailure:
      return "The public page could not be fetched"
    case .tooManyRedirects:
      return "The public page exceeded the redirect limit"
    case .responseTooLarge:
      return "The public page exceeded the response-size limit"
    case .robotsUnavailable:
      return "The site's robots policy could not be established"
    case .robotsDenied:
      return "The site's robots policy does not allow this fetch"
    case .unexpectedStatus(let status):
      return "The public page returned HTTP status \(status)"
    case .unsupportedContentType:
      return "The public page did not return supported HTML"
    case .invalidTextEncoding:
      return "The public page was not valid UTF-8"
    case .authenticationRequired:
      return "The page requires authentication and will not be fetched"
    case .paywallDetected:
      return "The page appears paywalled and will not be parsed"
    case .unsupportedDocument:
      return "The page does not contain a supported public-article document"
    case .extractedArticleTooLarge:
      return "The extracted article exceeded the output-size limit"
    }
  }
}

public enum OwnerOnlyPublicArticleRequestLoader {
  public static let maximumBytes = 16_384

  public static func load(_ url: URL) throws -> PublicArticleRequest {
    let descriptor = Darwin.open(url.path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW)
    guard descriptor >= 0 else { throw PublicArticleError.unsafeRequestFile }
    defer { Darwin.close(descriptor) }
    var metadata = stat()
    guard Darwin.fstat(descriptor, &metadata) == 0,
      metadata.st_mode & S_IFMT == S_IFREG,
      metadata.st_nlink == 1,
      metadata.st_mode & 0o077 == 0,
      metadata.st_size >= 0,
      metadata.st_size <= maximumBytes
    else { throw PublicArticleError.unsafeRequestFile }
    let data =
      try FileHandle(fileDescriptor: descriptor, closeOnDealloc: false).readToEnd()
      ?? Data()
    return try JSONDecoder().decode(PublicArticleRequest.self, from: data)
  }
}

public struct PublicWebResponse: Equatable, Sendable {
  public let statusCode: Int
  public let finalURL: URL
  public let contentType: String?
  public let data: Data
  public let redirectCount: Int

  public init(
    statusCode: Int,
    finalURL: URL,
    contentType: String?,
    data: Data,
    redirectCount: Int = 0
  ) {
    self.statusCode = statusCode
    self.finalURL = finalURL
    self.contentType = contentType
    self.data = data
    self.redirectCount = redirectCount
  }
}

public protocol PublicWebTransport: Sendable {
  func get(_ url: URL, maximumBytes: Int) async throws -> PublicWebResponse
}

public struct PublicArticleFetcher: Sendable {
  public static let maximumPageBytes = 2 * 1_024 * 1_024
  public static let maximumRobotsBytes = 64 * 1_024
  public static let maximumArticleBytes = 512 * 1_024

  private let transport: any PublicWebTransport

  public init(transport: any PublicWebTransport = URLSessionPublicWebTransport()) {
    self.transport = transport
  }

  public func fetch(
    _ request: PublicArticleRequest,
    fetchedAt: Date = Date()
  ) async throws -> PublicArticleDocument {
    guard request.formatVersion == 1,
      let sourceURL = PublicArticleURLPolicy.articleURL(request.url)
    else {
      throw request.formatVersion == 1
        ? PublicArticleError.invalidPublicArticleURL
        : PublicArticleError.unsupportedRequestFormat
    }

    let robotsURL = try PublicArticleURLPolicy.robotsURL(for: sourceURL)
    let robotsResponse: PublicWebResponse
    do {
      robotsResponse = try await transport.get(
        robotsURL,
        maximumBytes: Self.maximumRobotsBytes
      )
    } catch let error as PublicArticleError {
      throw error
    } catch {
      throw PublicArticleError.transportFailure
    }
    guard PublicArticleURLPolicy.isRobotsURL(robotsResponse.finalURL) else {
      throw PublicArticleError.invalidPublicArticleURL
    }
    switch robotsResponse.statusCode {
    case 200:
      guard robotsResponse.contentType?.lowercased().hasPrefix("text/plain") == true else {
        throw PublicArticleError.robotsUnavailable
      }
      guard let text = String(data: robotsResponse.data, encoding: .utf8) else {
        throw PublicArticleError.robotsUnavailable
      }
      let policy = RobotsPolicy(text)
      guard policy.isUsable else { throw PublicArticleError.robotsUnavailable }
      guard policy.allows(path: sourceURL.path, userAgent: "greenbubbles") else {
        throw PublicArticleError.robotsDenied
      }
    case 404, 410:
      break
    case 401, 403:
      throw PublicArticleError.robotsDenied
    default:
      throw PublicArticleError.robotsUnavailable
    }

    let pageResponse: PublicWebResponse
    do {
      pageResponse = try await transport.get(sourceURL, maximumBytes: Self.maximumPageBytes)
    } catch let error as PublicArticleError {
      throw error
    } catch {
      throw PublicArticleError.transportFailure
    }
    guard let finalURL = PublicArticleURLPolicy.articleURL(pageResponse.finalURL) else {
      throw PublicArticleError.invalidPublicArticleURL
    }
    if pageResponse.statusCode == 401 || pageResponse.statusCode == 403 {
      throw PublicArticleError.authenticationRequired
    }
    guard pageResponse.statusCode == 200 else {
      throw PublicArticleError.unexpectedStatus(pageResponse.statusCode)
    }
    guard pageResponse.contentType?.lowercased().hasPrefix("text/html") == true else {
      throw PublicArticleError.unsupportedContentType
    }
    guard let html = String(data: pageResponse.data, encoding: .utf8) else {
      throw PublicArticleError.invalidTextEncoding
    }
    let extracted = try PublicArticleHTMLExtractor().extract(
      html,
      maximumBodyBytes: Self.maximumArticleBytes
    )
    return PublicArticleDocument(
      sourceURL: sourceURL,
      finalURL: finalURL,
      fetchedAt: fetchedAt,
      title: extracted.title,
      author: extracted.author,
      description: extracted.description,
      bodyText: extracted.bodyText,
      accessEvidence: PublicArticleAccessEvidence(
        robotsChecked: true,
        robotsAllowed: true,
        unauthenticatedRequest: true,
        cookiesDisabled: true,
        redirectsFollowed: robotsResponse.redirectCount + pageResponse.redirectCount
      )
    )
  }
}

public final class URLSessionPublicWebTransport: NSObject, PublicWebTransport,
  URLSessionTaskDelegate, @unchecked Sendable
{
  private static let maximumRedirects = 3
  private lazy var session: URLSession = {
    let configuration = URLSessionConfiguration.ephemeral
    configuration.httpCookieAcceptPolicy = .never
    configuration.httpCookieStorage = nil
    configuration.httpShouldSetCookies = false
    configuration.urlCredentialStorage = nil
    configuration.urlCache = nil
    configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
    configuration.timeoutIntervalForRequest = 15
    configuration.timeoutIntervalForResource = 30
    configuration.httpAdditionalHeaders = [
      "Accept": "text/html,text/plain;q=0.9",
      "User-Agent": "GreenBubbles/0.1",
    ]
    return URLSession(configuration: configuration, delegate: self, delegateQueue: nil)
  }()

  public override init() {
    super.init()
  }

  public func get(_ url: URL, maximumBytes: Int) async throws -> PublicWebResponse {
    guard maximumBytes > 0, PublicArticleURLPolicy.hostOnlyURL(url) != nil else {
      throw PublicArticleError.invalidPublicArticleURL
    }
    var current = url
    var redirects = 0
    while true {
      let response = try await getWithoutRedirect(current, maximumBytes: maximumBytes)
      guard [301, 302, 303, 307, 308].contains(response.statusCode) else {
        return PublicWebResponse(
          statusCode: response.statusCode,
          finalURL: current,
          contentType: response.contentType,
          data: response.data,
          redirectCount: redirects
        )
      }
      guard redirects < Self.maximumRedirects,
        let location = response.location,
        let next = URL(string: location, relativeTo: current)?.absoluteURL,
        let validated = PublicArticleURLPolicy.hostOnlyURL(next)
      else {
        throw redirects >= Self.maximumRedirects
          ? PublicArticleError.tooManyRedirects
          : PublicArticleError.invalidPublicArticleURL
      }
      redirects += 1
      current = validated
    }
  }

  public func urlSession(
    _ session: URLSession,
    task: URLSessionTask,
    willPerformHTTPRedirection response: HTTPURLResponse,
    newRequest request: URLRequest,
    completionHandler: @escaping (URLRequest?) -> Void
  ) {
    completionHandler(nil)
  }

  private func getWithoutRedirect(
    _ url: URL,
    maximumBytes: Int
  ) async throws -> RawWebResponse {
    var request = URLRequest(url: url)
    request.httpMethod = "GET"
    request.httpShouldHandleCookies = false
    do {
      let (bytes, response) = try await session.bytes(for: request)
      guard let response = response as? HTTPURLResponse else {
        throw PublicArticleError.transportFailure
      }
      if response.expectedContentLength > Int64(maximumBytes) {
        throw PublicArticleError.responseTooLarge
      }
      var data = Data()
      data.reserveCapacity(min(maximumBytes, max(0, Int(response.expectedContentLength))))
      for try await byte in bytes {
        guard data.count < maximumBytes else {
          throw PublicArticleError.responseTooLarge
        }
        data.append(byte)
      }
      return RawWebResponse(
        statusCode: response.statusCode,
        contentType: response.value(forHTTPHeaderField: "Content-Type"),
        location: response.value(forHTTPHeaderField: "Location"),
        data: data
      )
    } catch let error as PublicArticleError {
      throw error
    } catch {
      throw PublicArticleError.transportFailure
    }
  }
}

private struct RawWebResponse: Sendable {
  let statusCode: Int
  let contentType: String?
  let location: String?
  let data: Data
}

enum PublicArticleURLPolicy {
  private static let allowedHost = "mp.weixin.qq.com"
  private static let rejectedQueryNames = Set([
    "key", "pass_ticket", "uin", "wx_header", "wxtoken",
  ])

  static func articleURL(_ url: URL) -> URL? {
    guard let validated = hostOnlyURL(url),
      validated.path == "/s" || validated.path.hasPrefix("/s/"),
      !hasRejectedQuery(validated)
    else { return nil }
    return validated
  }

  static func hostOnlyURL(_ url: URL) -> URL? {
    guard var components = URLComponents(url: url, resolvingAgainstBaseURL: false),
      components.scheme?.lowercased() == "https",
      components.host?.lowercased() == allowedHost,
      components.user == nil,
      components.password == nil,
      components.port == nil || components.port == 443
    else { return nil }
    components.scheme = "https"
    components.host = allowedHost
    components.port = nil
    components.fragment = nil
    return components.url
  }

  static func robotsURL(for article: URL) throws -> URL {
    var components = URLComponents()
    components.scheme = "https"
    components.host = allowedHost
    components.path = "/robots.txt"
    guard let url = components.url else {
      throw PublicArticleError.invalidPublicArticleURL
    }
    return url
  }

  static func isRobotsURL(_ url: URL) -> Bool {
    guard let validated = hostOnlyURL(url),
      let components = URLComponents(url: validated, resolvingAgainstBaseURL: false)
    else { return false }
    return components.path == "/robots.txt" && components.query == nil
  }

  private static func hasRejectedQuery(_ url: URL) -> Bool {
    guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false) else {
      return true
    }
    return components.queryItems?.contains {
      rejectedQueryNames.contains($0.name.lowercased())
    } ?? false
  }
}

struct RobotsPolicy {
  struct Rule {
    let allow: Bool
    let pattern: String

    var specificity: Int {
      pattern.filter { $0 != "*" && $0 != "$" }.count
    }

    func matches(_ path: String) -> Bool {
      guard !pattern.isEmpty else { return false }
      let anchoredAtEnd = pattern.hasSuffix("$")
      let raw = anchoredAtEnd ? String(pattern.dropLast()) : pattern
      let pieces = raw.split(separator: "*", omittingEmptySubsequences: false)
        .map { NSRegularExpression.escapedPattern(for: String($0)) }
      let expression = "^" + pieces.joined(separator: ".*") + (anchoredAtEnd ? "$" : "")
      return path.range(of: expression, options: .regularExpression) != nil
    }
  }

  struct Group {
    var agents: [String]
    var rules: [Rule]
  }

  let groups: [Group]
  let isUsable: Bool

  init(_ text: String) {
    var groups: [Group] = []
    var agents: [String] = []
    var rules: [Rule] = []
    var hasRules = false
    var sawNonCommentContent = false

    func appendGroup() {
      guard !agents.isEmpty else { return }
      groups.append(Group(agents: agents, rules: rules))
    }

    for (lineNumber, rawLine) in text.split(
      whereSeparator: \.isNewline
    ).enumerated() {
      var line = rawLine.split(separator: "#", maxSplits: 1).first ?? ""
      if lineNumber == 0, line.first == "\u{feff}" { line.removeFirst() }
      if !line.trimmingCharacters(in: .whitespaces).isEmpty {
        sawNonCommentContent = true
      }
      guard let separator = line.firstIndex(of: ":") else { continue }
      let field = line[..<separator].trimmingCharacters(in: .whitespaces).lowercased()
      let value = line[line.index(after: separator)...]
        .trimmingCharacters(in: .whitespaces)
      switch field {
      case "user-agent":
        if hasRules {
          appendGroup()
          agents = []
          rules = []
          hasRules = false
        }
        agents.append(value.lowercased())
      case "allow",
        "disallow" where !agents.isEmpty:
        hasRules = true
        if !value.isEmpty {
          rules.append(Rule(allow: field == "allow", pattern: value))
        }
      default:
        continue
      }
    }
    appendGroup()
    self.groups = groups
    self.isUsable = !sawNonCommentContent || !groups.isEmpty
  }

  func allows(path: String, userAgent: String) -> Bool {
    let userAgent = userAgent.lowercased()
    let exact = groups.filter { group in
      group.agents.contains { agent in
        agent != "*" && userAgent.contains(agent)
      }
    }
    let selected =
      exact.isEmpty
      ? groups.filter { $0.agents.contains("*") }
      : exact
    let matching = selected.flatMap(\.rules).filter { $0.matches(path) }
    guard let maximum = matching.map(\.specificity).max() else { return true }
    return matching.filter { $0.specificity == maximum }.contains(where: \.allow)
  }
}

struct ExtractedPublicArticle: Equatable {
  let title: String?
  let author: String?
  let description: String?
  let bodyText: String
}

struct PublicArticleHTMLExtractor {
  private static let ignoredElements = Set(["script", "style", "noscript", "svg"])
  private static let blockElements = Set([
    "address", "article", "aside", "blockquote", "br", "div", "figcaption", "figure", "h1",
    "h2", "h3", "h4", "h5", "h6", "header", "hr", "li", "main", "ol", "p", "pre",
    "section", "table", "td", "th", "tr", "ul",
  ])

  func extract(_ html: String, maximumBodyBytes: Int) throws -> ExtractedPublicArticle {
    var scanner = HTMLTagScanner(html)
    var metadata: [String: String] = [:]
    var contentTag: HTMLTag?
    while let tag = scanner.next() {
      if tag.attributes.keys.contains(where: { $0 == "id" || $0 == "class" }) {
        let markers = [tag.attributes["id"], tag.attributes["class"]]
          .compactMap { $0?.lowercased() }
          .joined(separator: " ")
        if markers.contains("paywall") || markers.contains("paid-content")
          || markers.contains("js_pay")
        {
          throw PublicArticleError.paywallDetected
        }
      }
      if tag.name == "meta" {
        let key =
          tag.attributes["property"]?.lowercased()
          ?? tag.attributes["name"]?.lowercased()
        if let key, let content = tag.attributes["content"], metadata[key] == nil {
          metadata[key] = normalizeInline(decodeEntities(content))
        }
      }
      if !tag.isClosing, tag.attributes["id"]?.lowercased() == "js_content" {
        if contentTag == nil { contentTag = tag }
      }
    }
    guard let contentTag,
      let bodyRange = HTMLTagScanner.elementBodyRange(
        in: html,
        openingTag: contentTag
      )
    else { throw PublicArticleError.unsupportedDocument }

    let body = extractText(String(html[bodyRange]))
    guard !body.isEmpty else { throw PublicArticleError.unsupportedDocument }
    guard body.lengthOfBytes(using: .utf8) <= maximumBodyBytes else {
      throw PublicArticleError.extractedArticleTooLarge
    }
    return ExtractedPublicArticle(
      title: nonEmpty(metadata["og:title"] ?? metadata["twitter:title"]),
      author: nonEmpty(metadata["og:article:author"] ?? metadata["author"]),
      description: nonEmpty(metadata["og:description"] ?? metadata["description"]),
      bodyText: body
    )
  }

  private func extractText(_ html: String) -> String {
    var scanner = HTMLTagScanner(html)
    var cursor = html.startIndex
    var ignored: [String] = []
    var output = ""
    while let tag = scanner.next() {
      if ignored.isEmpty, cursor < tag.range.lowerBound {
        output += decodeEntities(String(html[cursor..<tag.range.lowerBound]))
      }
      if tag.isClosing {
        if ignored.last == tag.name {
          ignored.removeLast()
        }
      } else if Self.ignoredElements.contains(tag.name), !tag.isSelfClosing {
        ignored.append(tag.name)
      }
      if ignored.isEmpty {
        if tag.name == "img", let alt = tag.attributes["alt"], !alt.isEmpty {
          output += " [Image: \(decodeEntities(alt))] "
        }
        if Self.blockElements.contains(tag.name) {
          output += "\n"
        }
      }
      cursor = tag.range.upperBound
    }
    if ignored.isEmpty, cursor < html.endIndex {
      output += decodeEntities(String(html[cursor...]))
    }
    return normalizeBlock(output)
  }

  private func normalizeBlock(_ value: String) -> String {
    var lines: [String] = []
    for rawLine in value.replacingOccurrences(of: "\r", with: "\n").split(
      separator: "\n",
      omittingEmptySubsequences: false
    ) {
      let line = normalizeInline(String(rawLine))
      if line.isEmpty {
        if !lines.isEmpty, lines.last != "" { lines.append("") }
      } else {
        lines.append(line)
      }
    }
    while lines.last == "" { lines.removeLast() }
    return lines.joined(separator: "\n")
  }

  private func normalizeInline(_ value: String) -> String {
    var output = ""
    var needsSpace = false
    for character in value {
      if character.isWhitespace {
        needsSpace = !output.isEmpty
      } else {
        if needsSpace { output.append(" ") }
        output.append(character)
        needsSpace = false
      }
    }
    return output.trimmingCharacters(in: .whitespacesAndNewlines)
  }

  private func nonEmpty(_ value: String?) -> String? {
    value.flatMap { $0.isEmpty ? nil : $0 }
  }

  private func decodeEntities(_ value: String) -> String {
    var output = ""
    var index = value.startIndex
    while index < value.endIndex {
      guard value[index] == "&",
        let end = value[index...].firstIndex(of: ";"),
        value.distance(from: index, to: end) <= 16
      else {
        output.append(value[index])
        index = value.index(after: index)
        continue
      }
      let entity = String(value[value.index(after: index)..<end])
      let decoded: String?
      switch entity.lowercased() {
      case "amp": decoded = "&"
      case "lt": decoded = "<"
      case "gt": decoded = ">"
      case "quot": decoded = "\""
      case "apos": decoded = "'"
      case "nbsp": decoded = " "
      default:
        let number: UInt32?
        if entity.lowercased().hasPrefix("#x") {
          number = UInt32(entity.dropFirst(2), radix: 16)
        } else if entity.hasPrefix("#") {
          number = UInt32(entity.dropFirst())
        } else {
          number = nil
        }
        decoded = number.flatMap(UnicodeScalar.init).map(String.init)
      }
      if let decoded {
        output += decoded
        index = value.index(after: end)
      } else {
        output.append(value[index])
        index = value.index(after: index)
      }
    }
    return output
  }
}

struct HTMLTag {
  let name: String
  let attributes: [String: String]
  let isClosing: Bool
  let isSelfClosing: Bool
  let range: Range<String.Index>
}

struct HTMLTagScanner {
  private let html: String
  private var cursor: String.Index

  init(_ html: String, startingAt: String.Index? = nil) {
    self.html = html
    self.cursor = startingAt ?? html.startIndex
  }

  mutating func next() -> HTMLTag? {
    while let start = html[cursor...].firstIndex(of: "<") {
      if html[start...].hasPrefix("<!--") {
        guard let end = html[start...].range(of: "-->")?.upperBound else { return nil }
        cursor = end
        continue
      }
      var index = html.index(after: start)
      var quote: Character?
      while index < html.endIndex {
        let character = html[index]
        if let currentQuote = quote {
          if character == currentQuote { quote = nil }
        } else if character == "\"" || character == "'" {
          quote = character
        } else if character == ">" {
          let end = html.index(after: index)
          cursor = end
          if let tag = parseTag(String(html[html.index(after: start)..<index]), range: start..<end)
          {
            return tag
          }
          break
        }
        index = html.index(after: index)
      }
      if index == html.endIndex { return nil }
    }
    return nil
  }

  static func elementBodyRange(
    in html: String,
    openingTag: HTMLTag
  ) -> Range<String.Index>? {
    guard !openingTag.isClosing, !openingTag.isSelfClosing else { return nil }
    var scanner = HTMLTagScanner(html, startingAt: openingTag.range.upperBound)
    var depth = 1
    while let tag = scanner.next() {
      guard tag.name == openingTag.name else { continue }
      if tag.isClosing {
        depth -= 1
        if depth == 0 { return openingTag.range.upperBound..<tag.range.lowerBound }
      } else if !tag.isSelfClosing {
        depth += 1
      }
    }
    return nil
  }

  private func parseTag(_ raw: String, range: Range<String.Index>) -> HTMLTag? {
    var characters = Array(raw)
    while characters.first?.isWhitespace == true { characters.removeFirst() }
    guard !characters.isEmpty, characters.first != "!", characters.first != "?" else {
      return nil
    }
    let isClosing = characters.first == "/"
    if isClosing { characters.removeFirst() }
    while characters.first?.isWhitespace == true { characters.removeFirst() }
    let isSelfClosing = characters.last == "/"
    if isSelfClosing { characters.removeLast() }

    var offset = 0
    while offset < characters.count,
      !characters[offset].isWhitespace,
      characters[offset] != "/"
    {
      offset += 1
    }
    guard offset > 0 else { return nil }
    let name = String(characters[..<offset]).lowercased()
    var attributes: [String: String] = [:]
    var index = offset
    while index < characters.count {
      while index < characters.count, characters[index].isWhitespace { index += 1 }
      guard index < characters.count else { break }
      let keyStart = index
      while index < characters.count,
        !characters[index].isWhitespace,
        characters[index] != "="
      {
        index += 1
      }
      let key = String(characters[keyStart..<index]).lowercased()
      while index < characters.count, characters[index].isWhitespace { index += 1 }
      var value = ""
      if index < characters.count, characters[index] == "=" {
        index += 1
        while index < characters.count, characters[index].isWhitespace { index += 1 }
        if index < characters.count, characters[index] == "\"" || characters[index] == "'" {
          let quote = characters[index]
          index += 1
          let start = index
          while index < characters.count, characters[index] != quote { index += 1 }
          value = String(characters[start..<index])
          if index < characters.count { index += 1 }
        } else {
          let start = index
          while index < characters.count, !characters[index].isWhitespace { index += 1 }
          value = String(characters[start..<index])
        }
      }
      if !key.isEmpty, attributes[key] == nil { attributes[key] = value }
    }
    return HTMLTag(
      name: name,
      attributes: attributes,
      isClosing: isClosing,
      isSelfClosing: isSelfClosing,
      range: range
    )
  }
}
