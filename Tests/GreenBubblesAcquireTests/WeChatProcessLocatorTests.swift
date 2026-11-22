import Testing

@testable import GreenBubblesAcquire

struct WeChatProcessLocatorTests {
  @Test func emptyProcessListHasNoTarget() throws {
    #expect(try WeChatProcessLocator.uniqueProcessID(in: []) == nil)
  }

  @Test func singleMatchIsUnambiguous() throws {
    #expect(try WeChatProcessLocator.uniqueProcessID(in: [42]) == 42)
  }

  @Test func rejectsAmbiguousMatchesInEitherOrder() {
    for matches: [Int32] in [[42, 43], [43, 42]] {
      #expect(throws: ProcessLocatorError.multipleProcesses) {
        try WeChatProcessLocator.uniqueProcessID(in: matches)
      }
    }
  }
}
