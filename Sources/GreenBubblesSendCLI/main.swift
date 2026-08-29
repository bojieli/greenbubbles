import AppKit
import Foundation
import GreenBubblesSendKit
import ServiceManagement

/// `greenbubbles-send` — the control plane's client for the input helper, and
/// the user's onboarding and lifecycle command.
///
/// The Rust control plane spawns this executable with one subcommand, writes a
/// JSON request on standard input, and reads a JSON response from standard
/// output under its own watchdog. This process therefore performs no input and
/// holds no grants: it is a thin, auditable bridge to the helper's XPC surface,
/// which is where the powerful grants live.

enum SendClientError: Error, CustomStringConvertible {
  case usage(String)
  case helper(String)
  case timedOut
  case malformedRequest
  case malformedResponse
  case lifecycle(String)

  var description: String {
    switch self {
    case .usage(let detail): "usage: \(detail)"
    case .helper(let code): "helper refused the request: \(code)"
    case .timedOut: SendFailureCode.engineStall.rawValue
    case .malformedRequest: "the request on standard input is not valid JSON for this subcommand"
    case .malformedResponse: "the helper returned a response this client cannot decode"
    case .lifecycle(let detail): detail
    }
  }
}

/// One bounded call to the helper. The reply is delivered on a background
/// queue, so the wait is a semaphore rather than a run loop.
final class HelperClient {
  private let machServiceName: String
  private let timeout: TimeInterval

  init(machServiceName: String, timeout: TimeInterval) {
    self.machServiceName = machServiceName
    self.timeout = timeout
  }

  private func connect() -> NSXPCConnection {
    let connection = NSXPCConnection(machServiceName: machServiceName, options: [])
    connection.remoteObjectInterface = NSXPCInterface(with: GreenBubblesInputHelperProtocol.self)
    connection.resume()
    return connection
  }

  func call(
    _ invoke: (GreenBubblesInputHelperProtocol, @escaping @Sendable (Data?, String?) -> Void) ->
      Void
  ) throws -> Data {
    let connection = connect()
    defer { connection.invalidate() }
    let semaphore = DispatchSemaphore(value: 0)
    let box = ReplyBox()
    guard
      let proxy = connection.remoteObjectProxyWithErrorHandler({ error in
        box.store(
          nil, "\(SendFailureCode.engineUnavailable.rawValue):\(error.localizedDescription)")
        semaphore.signal()
      }) as? GreenBubblesInputHelperProtocol
    else {
      throw SendClientError.helper(SendFailureCode.engineUnavailable.rawValue)
    }
    invoke(proxy) { payload, failure in
      box.store(payload, failure)
      semaphore.signal()
    }
    guard semaphore.wait(timeout: .now() + timeout) == .success else {
      throw SendClientError.timedOut
    }
    if let failure = box.failure { throw SendClientError.helper(failure) }
    guard let payload = box.payload else { throw SendClientError.malformedResponse }
    return payload
  }
}

/// Carries one reply across the XPC callback boundary.
final class ReplyBox: @unchecked Sendable {
  private let lock = NSLock()
  private var storedPayload: Data?
  private var storedFailure: String?

  func store(_ payload: Data?, _ failure: String?) {
    lock.lock()
    defer { lock.unlock() }
    storedPayload = payload
    storedFailure = failure
  }

  var payload: Data? {
    lock.lock()
    defer { lock.unlock() }
    return storedPayload
  }

  var failure: String? {
    lock.lock()
    defer { lock.unlock() }
    return storedFailure
  }
}

func option(_ name: String, in arguments: [String]) -> String? {
  guard let index = arguments.firstIndex(of: name), index + 1 < arguments.count else { return nil }
  let value = arguments[index + 1]
  return value.hasPrefix("--") ? nil : value
}

func readStandardInput(limit: Int = 1_048_576) throws -> Data {
  let data = FileHandle.standardInput.readDataToEndOfFile()
  guard data.count <= limit else { throw SendClientError.malformedRequest }
  return data
}

func emit(_ data: Data) {
  FileHandle.standardOutput.write(data)
  FileHandle.standardOutput.write(Data("\n".utf8))
}

let usage = """
  usage:
    greenbubbles-send capability-status --mach-service <name> [--timeout-milliseconds <n>]
    greenbubbles-send calibration-selftest --mach-service <name> [--timeout-milliseconds <n>]  < signed-profile.json
    greenbubbles-send execute-send --mach-service <name> [--timeout-milliseconds <n>]          < capability.json
    greenbubbles-send onboarding --mach-service <name> [--open]
    greenbubbles-send install-helper | uninstall-helper | helper-status

  The first three subcommands are the control plane's dispatcher protocol: one
  JSON request on standard input, one JSON response on standard output. This
  client performs no input of its own and holds no Accessibility or Screen
  Recording grant; those live only in GreenBubblesInputHelper.
  """

func run() throws {
  let arguments = Array(CommandLine.arguments.dropFirst())
  guard let subcommand = arguments.first else { throw SendClientError.usage(usage) }
  let machService = option("--mach-service", in: arguments) ?? SendHelperIdentity.machServiceName
  let timeout = Double(option("--timeout-milliseconds", in: arguments) ?? "") ?? 45_000
  let client = HelperClient(machServiceName: machService, timeout: timeout / 1_000)

  switch subcommand {
  case "capability-status":
    _ = try? readStandardInput()
    emit(try client.call { proxy, reply in proxy.capabilityStatus(reply: reply) })
  case "calibration-selftest":
    let request = try readStandardInput()
    guard (try? SendCodec.decode(SignedCalibrationProfile.self, from: request)) != nil else {
      throw SendClientError.malformedRequest
    }
    emit(
      try client.call { proxy, reply in
        proxy.runCalibrationSelfTest(signedProfile: request, reply: reply)
      })
  case "execute-send":
    let request = try readStandardInput()
    guard let capability = try? SendCodec.decode(ActionCapabilityEnvelope.self, from: request)
    else {
      throw SendClientError.malformedRequest
    }
    // The client re-validates before spending a helper call, so a malformed or
    // expired capability never reaches the process that holds the grants.
    try capability.validate(
      nowUnixNanoseconds: UInt64(Date().timeIntervalSince1970 * 1_000_000_000))
    emit(try client.call { proxy, reply in proxy.executeSend(capability: request, reply: reply) })
  case "onboarding":
    let payload = try client.call { proxy, reply in proxy.capabilityStatus(reply: reply) }
    let status = try SendCodec.decode(HelperCapabilityStatus.self, from: payload)
    let plan = OnboardingPlan.make(from: status)
    for step in plan.steps {
      let mark = step.granted ? "granted" : "MISSING"
      print("[\(mark)] \(step.title)")
      print("         \(step.rationale)")
      if !step.granted {
        for instruction in step.instructions { print("         - \(instruction)") }
        print("         \(step.settingsURL.absoluteString)")
        if arguments.contains("--open") {
          NSWorkspace.shared.open(step.settingsURL)
        }
      }
    }
    if let blocked = plan.sendPathBlockedBy {
      print("send path closed: \(blocked.rawValue) — \(blocked.operatorAction)")
    } else {
      print("send path prerequisites satisfied")
    }
  case "install-helper", "uninstall-helper", "helper-status":
    try manageHelper(subcommand)
  case "--help", "-h", "help":
    print(usage)
  default:
    throw SendClientError.usage(usage)
  }
}

/// Registers, unregisters, or reports the managed login item. The helper is
/// always started by the application through `SMAppService` — never by the user
/// and never by a downloaded installer.
func manageHelper(_ subcommand: String) throws {
  let service = SMAppService.agent(plistName: SendHelperIdentity.launchAgentPlistName)
  switch subcommand {
  case "install-helper":
    do {
      try service.register()
    } catch {
      throw SendClientError.lifecycle(
        "could not register the input helper agent: \(error.localizedDescription). "
          + "This requires the packaged, signed application bundle."
      )
    }
    print("registered \(SendHelperIdentity.bundleIdentifier) (status: \(describe(service.status)))")
  case "uninstall-helper":
    do {
      try service.unregister()
    } catch {
      throw SendClientError.lifecycle(
        "could not unregister the input helper: \(error.localizedDescription)"
      )
    }
    print("unregistered \(SendHelperIdentity.bundleIdentifier)")
    print(
      "revoke the two grants with: tccutil reset Accessibility "
        + "\(SendHelperIdentity.bundleIdentifier) && tccutil reset ScreenCapture "
        + "\(SendHelperIdentity.bundleIdentifier)"
    )
  default:
    print("\(SendHelperIdentity.bundleIdentifier): \(describe(service.status))")
  }
}

func describe(_ status: SMAppService.Status) -> String {
  switch status {
  case .notRegistered: "not registered"
  case .enabled: "enabled"
  case .requiresApproval: "requires approval in System Settings › General › Login Items"
  case .notFound: "not found in this application bundle"
  @unknown default: "unknown"
  }
}

do {
  try run()
} catch {
  FileHandle.standardError.write(Data("\(error)\n".utf8))
  exit(2)
}
