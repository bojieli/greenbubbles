import Testing

@testable import GreenBubblesCore

@Suite("WeChatClientBuildFingerprintTests")
struct WeChatClientBuildFingerprintTests {
  @Test
  func parsesPinnedSigningEvidenceWithoutPaths() throws {
    let evidence = try WeChatClientBuildInspector.parseSigningEvidence(
      """
      Executable=/private/application/Contents/MacOS/client
      Identifier=com.example.client
      CodeDirectory v=20500 size=123 flags=0x10000(runtime) hashes=1+1 location=embedded
      CandidateCDHashFull sha256=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
      TeamIdentifier=TEAM123
      """
    )
    #expect(evidence.identifier == "com.example.client")
    #expect(evidence.teamIdentifier == "TEAM123")
    #expect(evidence.codeDirectorySHA256 == String(repeating: "a", count: 64))
    #expect(evidence.hardenedRuntime)
  }

  @Test
  func rejectsIncompleteSigningEvidence() {
    #expect(throws: ClientBuildFingerprintError.malformedSigningEvidence) {
      try WeChatClientBuildInspector.parseSigningEvidence("Identifier=com.example.client")
    }
  }
}
