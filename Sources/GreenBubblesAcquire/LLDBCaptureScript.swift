// Derived from wcdb-key-tool (https://github.com/TANGandXUE/wcdb-key-tool), MIT license.

import Foundation

public enum LLDBTargetArchitecture: String, Sendable {
  case arm64
  // swift-format-ignore: AlwaysUseLowerCamelCase
  case x86_64

  public static var current: LLDBTargetArchitecture {
    #if arch(arm64)
      return .arm64
    #else
      return .x86_64
    #endif
  }

  /// Register holding `CCKeyDerivationPBKDF`'s `password` argument.
  public var passwordRegister: String {
    switch self {
    case .arm64: return "x1"
    case .x86_64: return "rsi"
    }
  }

  /// Register holding `CCKeyDerivationPBKDF`'s `passwordLen` argument.
  public var lengthRegister: String {
    switch self {
    case .arm64: return "x2"
    case .x86_64: return "rdx"
    }
  }
}

/// Generates the lldb command script whose ordering was validated live against
/// the pinned WeChat build: attach, break on the system `CCKeyDerivationPBKDF`
/// symbol when `passwordLen == 32`, read the 32-byte passphrase from the
/// password register when the breakpoint hits, then detach and quit.
public enum LLDBCaptureScript {
  public static func script(
    processID: pid_t,
    architecture: LLDBTargetArchitecture = .current
  ) -> String {
    """
    settings set target.preload-symbols false
    process attach -p \(processID)
    breakpoint set -n CCKeyDerivationPBKDF -c '$\(architecture.lengthRegister) == 32'
    breakpoint command add 1
    memory read --size 1 --count 32 --format x $\(architecture.passwordRegister)
    detach
    quit
    DONE
    process continue

    """
  }
}
