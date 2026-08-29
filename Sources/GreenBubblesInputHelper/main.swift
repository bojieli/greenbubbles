import Foundation
import GreenBubblesSendKit

/// `GreenBubblesInputHelper` — the only component that holds Accessibility and
/// Screen Recording.
///
/// It is started by the main application as a managed login item, never by the
/// user and never by a downloaded installer. Two modes:
///
/// * no arguments — run the XPC listener that the control plane talks to. Only
///   a peer signed by our team may connect.
/// * `probe` — print the capability status as JSON and exit. Read-only, no
///   send; used by onboarding and by support diagnostics before the Mach
///   service is registered.
let service = HelperService()

/// Accepts only peers that satisfy the pinned code-signing requirement, which
/// is what lets the XPC surface stay high level: peer identity is verified by
/// the platform rather than by a hand-rolled token.
final class HelperListenerDelegate: NSObject, NSXPCListenerDelegate, @unchecked Sendable {
  private let service: HelperService
  private let requirement: String

  init(service: HelperService, requirement: String) {
    self.service = service
    self.requirement = requirement
  }

  func listener(
    _ listener: NSXPCListener,
    shouldAcceptNewConnection connection: NSXPCConnection
  ) -> Bool {
    // The platform enforces the requirement: a peer that does not satisfy it
    // has its connection invalidated rather than being handed the interface.
    connection.setCodeSigningRequirement(requirement)
    connection.exportedInterface = NSXPCInterface(with: GreenBubblesInputHelperProtocol.self)
    connection.exportedObject = service
    connection.resume()
    return true
  }
}

let arguments = Array(CommandLine.arguments.dropFirst())
switch arguments.first {
case "probe":
  let status = service.currentStatus()
  if let data = try? SendCodec.encode(status), let text = String(data: data, encoding: .utf8) {
    print(text)
  } else {
    FileHandle.standardError.write(Data("could not encode the capability status\n".utf8))
    exit(2)
  }
case "onboarding":
  let plan = OnboardingPlan.make(from: service.currentStatus())
  for step in plan.steps where !step.granted {
    print("\(step.title)\n  \(step.rationale)")
    for instruction in step.instructions { print("  - \(instruction)") }
    print("  \(step.settingsURL.absoluteString)")
  }
  print(plan.complete ? "all required grants are present" : "grants are missing")
case nil:
  let teamIdentifier = ProcessInfo.processInfo.environment["GREENBUBBLES_TEAM_IDENTIFIER"] ?? ""
  let delegate = HelperListenerDelegate(
    service: service,
    requirement: SendHelperIdentity.codeSigningRequirement(teamIdentifier: teamIdentifier)
  )
  let listener = NSXPCListener(machServiceName: SendHelperIdentity.machServiceName)
  listener.delegate = delegate
  listener.resume()
  RunLoop.main.run()
default:
  FileHandle.standardError.write(
    Data("usage: greenbubbles-input-helper [probe|onboarding]\n".utf8)
  )
  exit(2)
}
