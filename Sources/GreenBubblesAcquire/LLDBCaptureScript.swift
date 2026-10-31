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
  var algorithmRegister: String {
    switch self {
    case .arm64: return "x0"
    case .x86_64: return "rdi"
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
  var prfRegister: String {
    switch self {
    case .arm64: return "x5"
    case .x86_64: return "r9"
    }
  }

  /// lldb expression for `rounds`.
  ///
  /// arm64 passes the first eight integer arguments in `x0`–`x7`, so `rounds`
  /// (the 7th argument) is `x6`. x86_64's System V ABI has only six integer
  /// registers (`rdi`, `rsi`, `rdx`, `rcx`, `r8`, `r9`); `rounds` is the first
  /// stack argument. At the first instruction the return address is at
  /// `(%rsp)` and that argument is at `8(%rsp)`. It is `unsigned` (4 bytes);
  /// an 8-byte load would include the next stack slot.
  var roundsExpression: String {
    switch self {
    case .arm64: return "$x6"
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

/// Generates the lldb command script. Command registration order matches the
/// live-validated capture: settings, attach, breakpoint, stored breakpoint
/// commands, `DONE`, then `process continue`.
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
  public static func script(
    processID: pid_t,
    architecture: LLDBTargetArchitecture = .current
  ) -> String {
    """
    settings set target.preload-symbols false
    settings set target.skip-prologue false
    process attach -p \(processID)
    breakpoint set -n CCKeyDerivationPBKDF -c '\(architecture.derivationCondition)'
    breakpoint command add 1
    memory read --size 1 --count 32 --format x $\(architecture.passwordRegister)
    continue
    DONE
    process continue

    """
  }
}
