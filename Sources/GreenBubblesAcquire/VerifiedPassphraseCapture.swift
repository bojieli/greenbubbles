import Foundation

/// Why a capture ended without a candidate that opens the databases.
public enum CaptureVerificationError: Error, Equatable, CustomStringConvertible {
  /// Every rejected candidate was checked against every distinct salt.
  case noMatch(distinctSaltCount: Int, databaseCount: Int)
  /// The deadline cut a candidate's check short after `checkedSaltCount`
  /// salts, none of which matched; the candidate was neither confirmed nor
  /// ruled out. `fullyRejectedCount` candidates were checked against every
  /// salt and matched none.
  case incomplete(checkedSaltCount: Int, distinctSaltCount: Int, fullyRejectedCount: Int)

  public var description: String {
    switch self {
    case .noMatch(let distinctSaltCount, let databaseCount):
      return "Passphrase verification failed: "
        + CaptureError.verificationFailureDiagnostic(
          distinctSaltCount: distinctSaltCount,
          databaseCount: databaseCount
        )
    case .incomplete(let checkedSaltCount, let distinctSaltCount, let fullyRejectedCount):
      var text =
        "Passphrase verification did not finish: the timeout expired after checking "
        + "\(checkedSaltCount) of \(distinctSaltCount) distinct database salts for a "
        + "captured candidate, so it was neither confirmed nor ruled out."
      if fullyRejectedCount > 0 {
        let earlier =
          fullyRejectedCount == 1
          ? "1 earlier candidate" : "\(fullyRejectedCount) earlier candidates"
        text += " \(earlier) matched none of the salts."
      }
      return text
        + " No output was created or replaced. Run capture again with a longer --timeout."
    }
  }
}

/// The capture command's verify-then-write step: a candidate is accepted
/// only if it opens at least one inventoried database, and the output is
/// written only after `capture` returns such a candidate. Any error leaves
/// the output untouched: an absent file stays absent and an existing one,
/// even with `overwrite`, keeps its contents.
public enum VerifiedPassphraseCapture {
  public struct Written: Equatable, Sendable {
    public let verifiedSaltCount: Int
    public let outcome: SecretOutputWriter.Outcome
  }

  /// `capture` runs the debugger and offers each candidate to the closure
  /// it is given (as `LLDBPassphraseCapture.capture(accept:)` does).
  /// Verification stops between salts once `deadline` passes; a candidate
  /// cut short that way is reported as `incomplete`, not as a mismatch.
  public static func captureAndWrite(
    inventory: DatabaseSaltInventory,
    deadline: Date,
    to output: URL,
    overwrite: Bool,
    as user: InvokingUser?,
    now: @escaping () -> Date = Date.init,
    capture: ((PassphraseSecret) throws -> Bool) throws -> PassphraseSecret
  ) throws -> Written {
    var verifiedSaltCount = 0
    var fullyRejectedCount = 0
    var cutShort: SaltVerification?
    let passphrase: PassphraseSecret
    do {
      passphrase = try capture { secret in
        let tally = try inventory.saltVerification(for: secret) { now() >= deadline }
        verifiedSaltCount = tally.verifiedCount
        if tally.verifiedCount > 0 { return true }
        if tally.isComplete {
          fullyRejectedCount += 1
        } else {
          cutShort = tally
        }
        return false
      }
    } catch CaptureError.noAcceptableCandidate {
      if let cutShort {
        throw CaptureVerificationError.incomplete(
          checkedSaltCount: cutShort.checkedCount,
          distinctSaltCount: cutShort.distinctSaltCount,
          fullyRejectedCount: fullyRejectedCount
        )
      }
      throw CaptureVerificationError.noMatch(
        distinctSaltCount: inventory.distinctSalts.count,
        databaseCount: inventory.entries.count
      )
    }
    // `capture` returns only a candidate the closure accepted.
    assert(verifiedSaltCount > 0)
    let outcome = try SecretOutputWriter().write(
      passphrase,
      to: output,
      overwrite: overwrite,
      as: user
    )
    return Written(verifiedSaltCount: verifiedSaltCount, outcome: outcome)
  }
}
