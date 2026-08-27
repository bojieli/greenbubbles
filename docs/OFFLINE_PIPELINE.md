# Audited offline restoration and publication

`restore-publish` is the fail-closed operator boundary between an already
acquired immutable snapshot and the continuous encrypted-replica follower. It
does not discover or open live WeChat stores, acquire a database passphrase,
read process memory, invoke WeChat, or open the encrypted replica.

## Bootstrap publication

The output parent, snapshot, and optional account root are owner-controlled
private locations. An encrypted snapshot passphrase is accepted only through
standard input:

```sh
greenbubbles-restore restore-publish \
  <bootstrap-snapshot> <new-authoritative-archive> <private-handoff.json> \
  --account-root <authorized-account-root> --passphrase-stdin
```

The command requires format-3 acquisition evidence and the exact pinned signed
client build. It prepares the immutable snapshot, restores it, independently
audits every archive ledger and recorded local artifact, and only then publishes
the bootstrap as generation 1 under the handoff lock. An existing handoff is
rejected. The output archive must not already exist.

## Incremental and integrity-scan publication

Every non-bootstrap acquisition must supply both retained sides of its
baseline:

```sh
greenbubbles-restore restore-publish \
  <next-snapshot> <new-authoritative-archive> <private-handoff.json> \
  --previous-snapshot <previous-snapshot> \
  --previous-archive <previous-authoritative-archive> \
  --account-root <authorized-account-root> --passphrase-stdin
```

Before decrypting the next snapshot, the operator independently verifies the
complete acquisition transition, unchanged pinned build, exact changed,
reconciliation, and deleted source-set classifications, the previous archive,
and its baseline fingerprint. An incremental snapshot is restored into an
owner-only temporary fragment, independently audited, merged by source identity
into a new atomic authoritative archive, and audited again. A full integrity
scan is restored directly as a new authoritative archive after the same chain
and previous-archive checks.

The publication generation is derived and incremented while holding the stable
handoff lock. The same compare-and-swap verifies that the supplied previous
archive is still the exact current sealed handoff, so concurrent restorers from
one baseline cannot publish a stale branch. Operators do not choose a
generation. Failed validation never changes the handoff, and incremental
fragment staging is automatically removed. A failure after an output directory
has been created can leave an unpublished partial or authoritative output at
that explicitly new path; inspect or remove that quarantined directory before
retrying with a new output path.

## Text-first and privacy boundary

`--defer-media` preserves the existing text-first behavior. It makes all
messages available with explicit deferred artifact states but cannot claim full
restoration. A later full restoration from an immutable snapshot can publish a
new generation with resolved media.

The command result contains only the acquisition mode, verification verdicts,
generation, media/completion state, and aggregate coverage counts. It omits
account IDs, source fingerprints, local paths, table names, and content. The
handoff remains owner-private because it necessarily contains the authoritative
archive path and source fingerprint. Replica application and its distinct key
remain isolated in `replica-follow`.

This workflow supplies deterministic sequencing and synthetic fault coverage;
it does not satisfy the plan's real disposable-account corpus, semantic/media
coverage, or 60-second p95 evidence gates.
