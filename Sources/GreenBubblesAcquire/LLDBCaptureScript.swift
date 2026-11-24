// Derived from wcdb-key-tool (https://github.com/TANGandXUE/wcdb-key-tool), MIT license.

import CommonCrypto
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

  /// Register holding `CCKeyDerivationPBKDF`'s `algorithm` argument.
  ///
  /// `algorithm` is a 32-bit `CCPBKDFAlgorithm`. The ABI leaves the upper 32
  /// bits of its 64-bit register unspecified, so the condition compares only
  /// the 32-bit view.
  var algorithmRegister: String {
    switch self {
    case .arm64: return "w0"
    case .x86_64: return "edi"
    }
  }

  /// Register holding `CCKeyDerivationPBKDF`'s `saltLen` argument.
  var saltLengthRegister: String {
    switch self {
    case .arm64: return "x4"
    case .x86_64: return "r8"
    }
  }

  /// Register holding `CCKeyDerivationPBKDF`'s `prf` argument.
  ///
  /// `prf` is a 32-bit `CCPseudoRandomAlgorithm`; only the 32-bit view is
  /// compared.
  var prfRegister: String {
    switch self {
    case .arm64: return "w5"
    case .x86_64: return "r9d"
    }
  }

  /// lldb expression for `rounds`.
  ///
  /// arm64 passes the first eight integer arguments in `x0`–`x7`, so `rounds`
  /// (the 7th argument) is in `x6`. It is a 32-bit `unsigned` and the upper
  /// half of `x6` is unspecified, so the expression reads `w6`. x86_64's
  /// System V ABI has only six integer registers (`rdi`, `rsi`, `rdx`, `rcx`,
  /// `r8`, `r9`); `rounds` is the first stack argument. At the first
  /// instruction the return address is at `(%rsp)` and that argument is at
  /// `8(%rsp)`. It is `unsigned` (4 bytes);
  /// an 8-byte load would include the next stack slot.
  var roundsExpression: String {
    switch self {
    case .arm64: return "$w6"
    case .x86_64: return "*(unsigned int *)($rsp + 8)"
    }
  }

  /// Breakpoint condition for the SQLCipher account-key derivation.
  ///
  /// A length-only check stops on the first unrelated 32-byte
  /// `CCKeyDerivationPBKDF` call. WeChat 4.1.15 and 4.2.0 make such calls
  /// before the account passphrase is derived.
  var derivationCondition: String {
    let call = SQLCipherAccountKeyDerivation.expected
    return [
      "$\(algorithmRegister) == \(call.algorithm)",
      "$\(lengthRegister) == \(call.passwordLength)",
      "$\(saltLengthRegister) == \(call.saltLength)",
      "$\(prfRegister) == \(call.prf)",
      "\(roundsExpression) == \(call.rounds)",
    ].joined(separator: " && ")
  }
}

/// The `CCKeyDerivationPBKDF` arguments SQLCipher 4 uses for the account key:
/// `PBKDF2-HMAC-SHA512(passphrase, 16-byte salt, 256000 rounds, 32-byte key)`.
///
/// Parameter order matches CommonCrypto's `CCKeyDerivationPBKDF`: `algorithm`,
/// `password`, `passwordLen`, `salt`, `saltLen`, `prf`, `rounds`, `derivedKey`,
/// `derivedKeyLen`. `kCCPBKDF2` is 2 and `kCCPRFHmacAlgSHA512` is 5.
struct SQLCipherAccountKeyDerivation: Equatable {
  var algorithm: Int
  var passwordLength: Int
  var saltLength: Int
  var prf: Int
  var rounds: Int

  static let expected = SQLCipherAccountKeyDerivation(
    algorithm: Int(kCCPBKDF2),
    passwordLength: PassphraseSecret.byteCount,
    saltLength: SQLCipherKeyVerifier.saltSize,
    prf: Int(kCCPRFHmacAlgSHA512),
    rounds: Int(SQLCipherKeyVerifier.passphraseRounds)
  )
}

/// Generates the lldb commands, in two phases with a check between them.
/// Command order matches the live-validated capture: settings, attach,
/// breakpoint, stored breakpoint commands, `DONE`, then `process continue`.
///
/// 1. `attach` attaches to the PID and prints its status; lldb then waits on
///    stdin with the process stopped. No breakpoint is set and no passphrase
///    register has been read yet (lldb itself may inspect the process while
///    attaching, as any attach does).
/// 2. The Swift side re-reads that PID's user and start time. Only when they
///    still match the selected process does it send `capture`; otherwise it
///    sends `detach`. Once attached, lldb holds the process's task port, so
///    later commands reach the verified process even if its PID is reused.
///
/// `target.skip-prologue` is off so the breakpoint is the function's first
/// instruction. That is where arm64 argument registers still hold the incoming
/// values and where the x86_64 stack offset above is defined.
///
/// The breakpoint command reads the password and continues. It does not
/// detach. A candidate that fails verification must leave the breakpoint armed
/// for a later call in the same login. The Swift side terminates lldb, which
/// detaches, after a candidate is accepted or the timeout expires.
public enum LLDBCaptureScript {
  /// Phase 1: attach and stop. `process status` prints `Process <pid>
  /// stopped` once the attach has completed, which the capture waits for.
  public static func attach(processID: pid_t) -> String {
    """
    settings set target.preload-symbols false
    settings set target.skip-prologue false
    process attach -p \(processID)
    process status

    """
  }

  /// Phase 2, sent only after the attached process's identity was verified.
  public static func capture(architecture: LLDBTargetArchitecture = .current) -> String {
    """
    breakpoint set -n CCKeyDerivationPBKDF -c '\(architecture.derivationCondition)'
    breakpoint command add 1
    memory read --size 1 --count 32 --format x $\(architecture.passwordRegister)
    continue
    DONE
    process continue

    """
  }

  /// Sent instead of `capture` when the attached process is not the selected
  /// one: release it untouched and end lldb.
  public static let detach = """
    process detach
    quit

    """
}
