# Recoverable authoritative-archive retention

Every successful `replica-publish` or `restore-publish` now maintains a private
publication history next to the handoff. The mode-`0600` history records each
published generation, exact handoff digest, sealed archive identity, canonical
archive path, and quarantine location. It is control-plane state and must stay
outside Git, logs, issues, and model prompts. An existing deployment without a
history begins tracking from the current handoff; unknown older publications
are never guessed.

The history is updated under the same stable handoff lock. If a process stops
after replacing the handoff but before recording the history entry, the next
publisher or retention command appends only that exact current handoff. A
same-generation mismatch, conflicting seal, reused path with changed contents,
or more than 4,096 tracked publications fails closed.

## Quarantine retired generations

Create an owner-only mode-`0700` directory on the same filesystem as the
archives, then run:

```sh
greenbubbles replica-archive-quarantine \
  <private-handoff.json> <private-quarantine-directory> \
  --retain-publications 2
```

The retention count can be larger but can never be less than two. The command
fully verifies every protected archive and always protects the current and
immediately preceding published records. If an older publication reuses an
archive path referenced by a protected record, that physical archive is also
retained. Eligible older archives are seal-verified, atomically renamed into a
deterministic location under the quarantine directory, re-sealed there, and
then recorded in the history. It never deletes an archive.

Renaming on one filesystem is deliberate: it is atomic and lets an interrupted
move be recognized without accepting a partial copy. A retry detects either
the retained or deterministic quarantine location, verifies the complete seal,
and repairs the history. Both locations existing, neither location existing,
cross-filesystem moves, nested archive/quarantine roots, symlinks, changed
files, or non-private permissions fail closed.

A quarantined archive is not usable in place because canonical restoration
reports and artifact evidence intentionally bind the original absolute path.
Do not edit it. Manual deletion is outside this recoverable workflow and loses
the ability to restore that generation.

## Restore a quarantined archive

Restore the physical archive to its exact original canonical path by any
generation that referenced it:

```sh
greenbubbles replica-archive-restore \
  <private-handoff.json> <private-quarantine-directory> \
  --generation <positive-integer>
```

The command verifies the quarantine seal, atomically renames the archive back,
runs the normal authoritative-archive verification at its original location,
and atomically clears the quarantine state for every publication sharing that
archive. A retry after a stop between the rename and history update recognizes
and completes the already restored state.

Both commands return aggregate-only reports without account IDs, source
fingerprints, paths, content, or absolute timestamps. They do not read live
WeChat stores, receive a database passphrase or replica key, modify the replica,
or change any acquisition, action, legal, or real-corpus gate.
