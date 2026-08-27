import Foundation
import Testing

@testable import GreenBubblesWeb

@Suite("PublicArticleTests")
struct PublicArticleTests {
  @Test
  func fetchesOneAllowedUnauthenticatedPublicPage() async throws {
    let articleURL = try #require(URL(string: "https://mp.weixin.qq.com/s/example?from=timeline"))
    let robotsURL = try #require(URL(string: "https://mp.weixin.qq.com/robots.txt"))
    let html = """
      <!doctype html>
      <html><head>
      <meta property="og:title" content="A &amp; B">
      <meta name="author" content="Example Author">
      <meta property="og:description" content="Public summary">
      </head><body>
      <div id="js_content">
        <p>Hello&nbsp;<strong>world</strong>.</p>
        <script>privateNoise()</script>
        <p>Second line <img alt="diagram" src="https://example.invalid/no-fetch"></p>
      </div>
      </body></html>
      """
    let transport = StubTransport(responses: [
      robotsURL.absoluteString: PublicWebResponse(
        statusCode: 200,
        finalURL: robotsURL,
        contentType: "text/plain",
        data: Data("User-agent: *\nDisallow: /private\nAllow: /s".utf8)
      ),
      articleURL.absoluteString: PublicWebResponse(
        statusCode: 200,
        finalURL: articleURL,
        contentType: "text/html; charset=utf-8",
        data: Data(html.utf8),
        redirectCount: 1
      ),
    ])

    let fetchedAt = Date(timeIntervalSince1970: 1_700_000_000)
    let document = try await PublicArticleFetcher(transport: transport).fetch(
      PublicArticleRequest(url: articleURL),
      fetchedAt: fetchedAt
    )

    #expect(document.sourceURL == articleURL)
    #expect(document.finalURL == articleURL)
    #expect(document.fetchedAt == fetchedAt)
    #expect(document.title == "A & B")
    #expect(document.author == "Example Author")
    #expect(document.description == "Public summary")
    #expect(document.bodyText == "Hello world.\n\nSecond line [Image: diagram]")
    #expect(!document.bodyText.contains("privateNoise"))
    #expect(document.bodyUTF8ByteCount == document.bodyText.utf8.count)
    #expect(document.accessEvidence.robotsChecked)
    #expect(document.accessEvidence.robotsAllowed)
    #expect(document.accessEvidence.unauthenticatedRequest)
    #expect(document.accessEvidence.cookiesDisabled)
    #expect(document.accessEvidence.redirectsFollowed == 1)
    #expect(document.accessEvidence.completeness == "singlePublicPage")
    let calls = await transport.recordedCalls()
    #expect(calls.map(\.url) == [robotsURL, articleURL])
    #expect(calls.map(\.maximumBytes) == [64 * 1_024, 2 * 1_024 * 1_024])
  }

  @Test
  func rejectsNonPublicAndCredentialBearingURLsBeforeTransport() async throws {
    for value in [
      "http://mp.weixin.qq.com/s/example",
      "https://example.com/s/example",
      "https://user:pass@mp.weixin.qq.com/s/example",
      "https://mp.weixin.qq.com:444/s/example",
      "https://mp.weixin.qq.com/not-an-article",
      "https://mp.weixin.qq.com/s/example?pass_ticket=private",
      "https://mp.weixin.qq.com/s?__biz=public&key=session-style-value",
    ] {
      let transport = StubTransport(responses: [:])
      let url = try #require(URL(string: value))
      await #expect(throws: PublicArticleError.invalidPublicArticleURL) {
        try await PublicArticleFetcher(transport: transport).fetch(
          PublicArticleRequest(url: url)
        )
      }
      #expect(await transport.recordedCalls().isEmpty)
    }
  }

  @Test
  func enforcesRobotsBeforeRequestingTheArticle() async throws {
    let articleURL = try #require(URL(string: "https://mp.weixin.qq.com/s/blocked"))
    let robotsURL = try #require(URL(string: "https://mp.weixin.qq.com/robots.txt"))
    let transport = StubTransport(responses: [
      robotsURL.absoluteString: PublicWebResponse(
        statusCode: 200,
        finalURL: robotsURL,
        contentType: "text/plain",
        data: Data("User-agent: *\nDisallow: /s".utf8)
      )
    ])

    await #expect(throws: PublicArticleError.robotsDenied) {
      try await PublicArticleFetcher(transport: transport).fetch(
        PublicArticleRequest(url: articleURL)
      )
    }
    #expect(await transport.recordedCalls().map(\.url) == [robotsURL])
  }

  @Test
  func robotsUsesLongestRuleAndExactAgentGroup() {
    let policy = RobotsPolicy(
      """
      User-agent: *
      Disallow: /s
      Allow: /s/public

      User-agent: greenbubbles
      Disallow: /s/private
      Allow: /s/private/one$
      """
    )
    #expect(policy.allows(path: "/s/public", userAgent: "other"))
    #expect(!policy.allows(path: "/s/private/two", userAgent: "greenbubbles"))
    #expect(policy.allows(path: "/s/private/one", userAgent: "greenbubbles"))
  }

  @Test
  func currentPublishedRobotsShapeDeniesPublicArticlePaths() {
    let policy = RobotsPolicy(
      """
      User-Agent: *
      Allow: /$
      Allow: /debug/
      Allow: /qa/
      Allow: /wiki
      Allow: /cgi-bin/loginpage
      Allow: /cgi-bin/wx
      Allow: /webpoc/ruleCenter
      Allow: /miniprogram/landing_page
      Disallow: /
      """
    )
    #expect(!policy.allows(path: "/s/example", userAgent: "greenbubbles"))
    #expect(policy.allows(path: "/", userAgent: "greenbubbles"))
  }

  @Test
  func malformedRobotsCannotBecomeImplicitPermission() async throws {
    let articleURL = try #require(URL(string: "https://mp.weixin.qq.com/s/example"))
    let robotsURL = try #require(URL(string: "https://mp.weixin.qq.com/robots.txt"))
    for body in ["<html>challenge</html>", "Sitemap: https://example.invalid/map"] {
      let transport = StubTransport(responses: [
        robotsURL.absoluteString: PublicWebResponse(
          statusCode: 200,
          finalURL: robotsURL,
          contentType: "text/plain",
          data: Data(body.utf8)
        )
      ])
      await #expect(throws: PublicArticleError.robotsUnavailable) {
        try await PublicArticleFetcher(transport: transport).fetch(
          PublicArticleRequest(url: articleURL)
        )
      }
      #expect(await transport.recordedCalls().map(\.url) == [robotsURL])
    }

    let bomPolicy = RobotsPolicy("\u{feff}User-agent: *\nDisallow: /s")
    #expect(bomPolicy.isUsable)
    #expect(!bomPolicy.allows(path: "/s/example", userAgent: "greenbubbles"))

    let wrongType = StubTransport(responses: [
      robotsURL.absoluteString: PublicWebResponse(
        statusCode: 200,
        finalURL: robotsURL,
        contentType: "text/html",
        data: Data("User-agent: *\nAllow: /s".utf8)
      )
    ])
    await #expect(throws: PublicArticleError.robotsUnavailable) {
      try await PublicArticleFetcher(transport: wrongType).fetch(
        PublicArticleRequest(url: articleURL)
      )
    }

    let redirectedPolicy = StubTransport(responses: [
      robotsURL.absoluteString: PublicWebResponse(
        statusCode: 200,
        finalURL: articleURL,
        contentType: "text/plain",
        data: Data("User-agent: *\nAllow: /s".utf8),
        redirectCount: 1
      )
    ])
    await #expect(throws: PublicArticleError.invalidPublicArticleURL) {
      try await PublicArticleFetcher(transport: redirectedPolicy).fetch(
        PublicArticleRequest(url: articleURL)
      )
    }
  }

  @Test
  func refusesAuthenticationPaywallsAndOversizedExtraction() async throws {
    let articleURL = try #require(URL(string: "https://mp.weixin.qq.com/s/example"))
    let robotsURL = try #require(URL(string: "https://mp.weixin.qq.com/robots.txt"))
    let allowedRobots = PublicWebResponse(
      statusCode: 404,
      finalURL: robotsURL,
      contentType: "text/plain",
      data: Data()
    )
    let authTransport = StubTransport(responses: [
      robotsURL.absoluteString: allowedRobots,
      articleURL.absoluteString: PublicWebResponse(
        statusCode: 401,
        finalURL: articleURL,
        contentType: "text/html",
        data: Data()
      ),
    ])
    await #expect(throws: PublicArticleError.authenticationRequired) {
      try await PublicArticleFetcher(transport: authTransport).fetch(
        PublicArticleRequest(url: articleURL)
      )
    }

    let paywall = "<div class='paywall'><div id='js_content'>preview</div></div>"
    let paywallTransport = StubTransport(responses: [
      robotsURL.absoluteString: allowedRobots,
      articleURL.absoluteString: PublicWebResponse(
        statusCode: 200,
        finalURL: articleURL,
        contentType: "text/html",
        data: Data(paywall.utf8)
      ),
    ])
    await #expect(throws: PublicArticleError.paywallDetected) {
      try await PublicArticleFetcher(transport: paywallTransport).fetch(
        PublicArticleRequest(url: articleURL)
      )
    }

    #expect(throws: PublicArticleError.extractedArticleTooLarge) {
      try PublicArticleHTMLExtractor().extract(
        "<div id='js_content'>too long</div>",
        maximumBodyBytes: 2
      )
    }
  }

  @Test
  func doesNotMistakeSimilarIDsForTheArticleContainer() {
    #expect(throws: PublicArticleError.unsupportedDocument) {
      try PublicArticleHTMLExtractor().extract(
        "<div id='not_js_content'>wrong</div>",
        maximumBodyBytes: 100
      )
    }
  }

  @Test
  func ownerOnlyRequestLoaderRejectsDisclosureLinksAndOversizedInput() throws {
    let root = FileManager.default.temporaryDirectory.appending(
      path: "greenbubbles-public-article-\(UUID().uuidString)",
      directoryHint: .isDirectory
    )
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: root) }
    let request = PublicArticleRequest(
      url: try #require(URL(string: "https://mp.weixin.qq.com/s/example"))
    )
    let privateFile = root.appending(path: "request.json")
    try JSONEncoder().encode(request).write(to: privateFile)
    try FileManager.default.setAttributes(
      [.posixPermissions: 0o600],
      ofItemAtPath: privateFile.path
    )
    #expect(try OwnerOnlyPublicArticleRequestLoader.load(privateFile) == request)

    try FileManager.default.setAttributes(
      [.posixPermissions: 0o644],
      ofItemAtPath: privateFile.path
    )
    #expect(throws: PublicArticleError.unsafeRequestFile) {
      try OwnerOnlyPublicArticleRequestLoader.load(privateFile)
    }
    try FileManager.default.setAttributes(
      [.posixPermissions: 0o600],
      ofItemAtPath: privateFile.path
    )

    let symlink = root.appending(path: "request-link.json")
    try FileManager.default.createSymbolicLink(at: symlink, withDestinationURL: privateFile)
    #expect(throws: PublicArticleError.unsafeRequestFile) {
      try OwnerOnlyPublicArticleRequestLoader.load(symlink)
    }

    let hardlink = root.appending(path: "request-hardlink.json")
    #expect(link(privateFile.path, hardlink.path) == 0)
    #expect(throws: PublicArticleError.unsafeRequestFile) {
      try OwnerOnlyPublicArticleRequestLoader.load(privateFile)
    }

    let oversized = root.appending(path: "oversized.json")
    try Data(repeating: 0x20, count: OwnerOnlyPublicArticleRequestLoader.maximumBytes + 1)
      .write(to: oversized)
    try FileManager.default.setAttributes(
      [.posixPermissions: 0o600],
      ofItemAtPath: oversized.path
    )
    #expect(throws: PublicArticleError.unsafeRequestFile) {
      try OwnerOnlyPublicArticleRequestLoader.load(oversized)
    }
  }

  @Test
  func paywallMarkerAfterArticleContentStillFailsClosed() {
    #expect(throws: PublicArticleError.paywallDetected) {
      try PublicArticleHTMLExtractor().extract(
        "<div id='js_content'>preview</div><div id='js_pay_area'>pay</div>",
        maximumBodyBytes: 100
      )
    }
  }
}

private actor StubTransport: PublicWebTransport {
  struct Call: Equatable, Sendable {
    let url: URL
    let maximumBytes: Int
  }

  private let responses: [String: PublicWebResponse]
  private var calls: [Call] = []

  init(responses: [String: PublicWebResponse]) {
    self.responses = responses
  }

  func get(_ url: URL, maximumBytes: Int) async throws -> PublicWebResponse {
    calls.append(Call(url: url, maximumBytes: maximumBytes))
    guard let response = responses[url.absoluteString] else {
      throw PublicArticleError.transportFailure
    }
    return response
  }

  func recordedCalls() -> [Call] {
    calls
  }
}
