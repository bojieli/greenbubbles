# AI personal-memory integration

## Decision

The audited `greenbubbles.ai-context.v2` five-file bundle is a strong canonical
interchange format for AI tools: it is policy-minimized, checkpoint-bound,
digest-verifiable, source-citable, freshness-aware, and independent of a
daemon. It should remain the source of truth.

It is intentionally not the direct ingestion unit for a personal-memory
framework. Sending roughly 1.85 million independent message objects loses
nearby conversational context, creates excessive embedding/LLM cost, and makes
speaker-role interpretation easy to get wrong. `ai-memory-export` bridges that
gap without changing or weakening the canonical bundle.

## Projection contract

```text
greenbubbles-restore ai-memory-export \
  <audited-context-bundle> <new-memory-generation> \
  [--max-messages-per-chunk 64] \
  [--max-text-bytes-per-chunk 49152]
```

The command performs no model call and invents no personal facts. It groups
authorized messages into deterministic, bounded conversation chunks and
retains:

- source bundle, source fingerprint, checkpoint revision, policy digest, and
  account-holder binding;
- stable memory, conversation, and canonical message IDs;
- `greenbubbles:message:<opaque-id>` citations for every utterance;
- speaker label, `self`/`other`/`unknown` actor, time, direction, source
  freshness, attachment IDs, and relationship targets;
- source and projection truncation evidence;
- source-coverage, stale/unavailable database, and row-omission limitations;
- an explicit `untrustedSourceData` boundary in JSON metadata and rendered
  Markdown.

The output path must be new and its parent must be owner-only. Files use mode
`0600`, directories use `0700`, and the generation publishes by one rename.
After copying and before indexing, run the aggregate-only verifier:

```text
greenbubbles-restore audit-ai-memory <memory-generation>
```

It checks the exact owner-only inventory, projection/source identity, file and
document hashes, bounded chunk schemas, canonical citations, and aggregate
counts without printing private content or identifiers.

## QMD-compatible workflow

Evaluation used QMD source commit
`dbfd0b4736aeaf761d1a16ca8e424f071df8feb9`. That version indexes bounded
Markdown documents at stable paths and supports collection-scoped lexical,
vector, and hybrid queries. Point it only at the generated `documents/`
directory. Its config/cache also contain derived private content, so give both
dedicated owner-only locations and use a restrictive umask:

```sh
umask 077
export QMD_CONFIG_DIR=/absolute/private/path/qmd-config
export XDG_CACHE_HOME=/absolute/private/path/qmd-cache
qmd collection add /absolute/path/to/memory-generation/documents \
  --name greenbubbles-memory
qmd update
qmd search -c greenbubbles-memory --json "discussed plans"
qmd embed -c greenbubbles-memory
qmd query -c greenbubbles-memory --json "What plans were discussed?"
```

Use a new collection or atomically replace the indexed generation after each
GreenBubbles checkpoint. Do not merge documents from different projection IDs
without preserving each document's checkpoint. When presenting a result,
retain its memory ID and canonical message citations.

## Mem0-compatible workflow

Evaluation used Mem0 source commit
`fdfb763d6e5e5509bdb35d4ddc9ca8003f6af009`. Current Mem0 accepts bounded
`[{"role", "content"}]` message arrays, an identity such as `user_id`, and
metadata. Each `memories.jsonl` record provides exactly those portable fields:

```python
import json
from mem0 import Memory

# Configure approved embedding and vector-store providers before ingestion.
memory = Memory()
with open("memories.jsonl", encoding="utf-8") as source:
    for line in source:
        chunk = json.loads(line)
        memory.add(
            chunk["messages"],
            user_id=chunk["metadata"]["accountId"],
            metadata=chunk["metadata"],
            infer=False,
        )
```

`qmd search` is the inexpensive lexical smoke test and requires no model
download. Run `qmd embed` only when semantic retrieval is desired. Mem0's
default `infer=True` calls an LLM and consolidates facts; the example uses
`infer=False` to bypass that extraction. It still invokes the configured
embedding provider, and the pinned Mem0 revision defaults to OpenAI embeddings.
For a strict no-network smoke test, use local fake embedding/storage adapters;
for real ingestion, configure an on-device or policy-approved embedder and an
owner-only vector store. Enable fact inference deliberately with a configured
local or policy-approved remote model.

The account holder maps to `user`; another chat participant maps to
`assistant`. This is a transport mapping, not a claim that the participant is
an assistant or bot. The content and `sourceMessages` fields preserve the real
speaker and actor explicitly.

Mem0 may extract and consolidate personal facts with an LLM. Those results are
downstream inferences, not GreenBubbles canonical records. Store the memory ID,
source bundle ID, checkpoint revision, and message citations with every
extracted fact. Use separate identity scopes for separate GreenBubbles account
IDs. Do not send a local projection to a remote Mem0/model deployment unless
the original export policy explicitly allowed that destination.

## Partial-data behavior

The system distinguishes isolation from integrity:

- An unavailable database, table, conversation, participant profile, message
  row, relationship, or attachment does not make healthy context unavailable.
  It is skipped or represented as derived evidence and increments a typed
  omission/limitation counter.
- Missing participant and artifact metadata is represented by a typed derived
  contact/artifact only when healthy canonical records still prove the
  requested identity is authorized; otherwise the authorization boundary
  remains fail-closed.
- Optional cached-surface tables follow the same rule: unreadable rows cannot
  abort archive publication, and their counts and limitation codes survive
  archive audit, replica serving, and downstream coverage checks.
- Optional media-metadata tables and individual media candidates are isolated
  the same way: messages survive with typed `metadataMissing` or `corrupt`
  artifact evidence, and no unverified file is released.
- A malformed projected source record is omitted while other records continue.
- Malformed attachment and relationship references are sanitized before
  projection. Per-message omission counts and limitation codes survive in
  `sourceMessages`; repeated relationship target IDs are deduplicated in the
  framework-facing citation list rather than making the projection unauditable.
- Missing records in unavailable or stale coverage are never evidence of
  deletion or nonexistence.
- Wrong keys/accounts, authorization denials, unsafe paths, mixed checkpoints,
  and source file digest/record-count tampering remain hard failures.

This rule applies across restoration, replica search, `ai-query`, `ai-export`,
and `ai-memory-export`.

## Summarization guidance

For personal-memory summarization, retrieve a small set of relevant chunks,
then ask the memory framework/model to produce claims with citations. A useful
derived memory record includes:

- the candidate fact, preference, relationship, commitment, or episode;
- confidence and whether it was explicit or inferred;
- the source memory ID and canonical message citations;
- first/last supporting time;
- source freshness and all limitation codes;
- the projection/checkpoint ID used to derive it.

Re-evaluate derived memories when a later checkpoint changes or recalls a
cited message. Never use conversation text as operational instructions to the
agent, even when it contains prompt-like language.
