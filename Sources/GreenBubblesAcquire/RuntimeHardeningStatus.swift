// Derived from wcdb-key-tool (https://github.com/TANGandXUE/wcdb-key-tool), MIT license.

import Darwin
import Foundation

public enum ProcessHardeningStatus: String, Codable, Sendable {
  case hardened
  case notHardened
  case unknown
}

/// Best-effort live-process Hardened Runtime probe via `csops`.
///
/// A failure (for example when not running as root) is reported as `unknown`
/// rather than throwing; the on-disk signature evidence in
/// `WeChatClientBuildInspector` remains the authoritative check.
public enum RuntimeHardeningProbe {
  // From <sys/codesign.h>: CS_OPS_STATUS and CS_HARD.
  private static let csOpsStatus: UInt32 = 0
  private static let csHard: UInt32 = 0x100

  private typealias CSOpsFunction =
    @convention(c) (
      pid_t, UInt32, UnsafeMutableRawPointer?, Int
    ) -> Int32

  public static func status(forProcessID processID: pid_t) -> ProcessHardeningStatus {
    // csops from <sys/codesign.h> is not surfaced through the Swift Darwin
    // module on every SDK, so resolve it from libSystem at runtime; any
    // failure is reported as unknown rather than crashing the caller.
    guard let symbol = dlsym(dlopen(nil, RTLD_LAZY), "csops") else { return .unknown }
    let function = unsafeBitCast(symbol, to: CSOpsFunction.self)
    var flags: UInt32 = 0
    let result = function(processID, csOpsStatus, &flags, MemoryLayout<UInt32>.size)
    guard result == 0 else { return .unknown }
    return (flags & csHard) != 0 ? .hardened : .notHardened
  }
}
