# Feeding a memory system

The audited `greenbubbles.ai-context.v2` bundle is the canonical interchange
format: policy-minimized, checkpoint-bound, digest-verifiable, source-citable,
freshness-aware, and independent of any daemon. It stays the source of truth.

It is deliberately *not* the right ingestion unit for a personal-memory
framework. Handing one 1.85 million independent message objects loses the
conversational context around each one, costs an absurd amount in embeddings
and LLM calls, and makes speaker roles easy to get wrong.

`ai-memory-export` bridges that gap without weakening the bundle.

## The projection

```sh
greenbubbles ai-memory-export \
  <audited-context-bundle> <new-memory-generation> \
  [--max-messages-per-chunk 64] \
  [--max-text-bytes-per-chunk 49152] \
  [--progress-file <owner-only-new-events.ndjson>] \
  [--progress-json | --quiet-progress]
```

**It makes no model call and invents no personal facts.** It groups authorized
messages into deterministic, bounded conversation chunks and carries forward:

- source bundle, source fingerprint, checkpoint revision, policy digest and
  account-holder binding;
- stable memory, conversation and canonical message IDs;
- a `greenbubbles:message:<opaque-id>` citation for every utterance;
- speaker label, `self`/`other`/`unknown` actor, time, direction, source
  freshness, attachment IDs and relationship targets;
- source and projection truncation evidence;
- source-coverage, stale/unavailable database and row-omission limitations;
- an explicit `untrustedSourceData` boundary, in both the JSON metadata and the
  rendered Markdown.

The output path must be new and its parent owner-only. Files are `0600`,
directories `0700`, and the generation publishes with one rename.

After copying and before indexing, verify it:

```sh
greenbubbles audit-ai-memory <memory-generation> \
  [--progress-file <owner-only-new-events.ndjson>] \
  [--progress-json | --quiet-progress]
```

This checks the exact owner-only inventory, projection and source identity,
file and document hashes, bounded chunk schemas, canonical citations, and
aggregate counts — printing no private content or identifiers.

Both commands show human progress on standard error: source bytes and records,
current canonical file or group, file and phase percentages, processed
conversation and message counts, emitted or verified chunk and document counts
and bytes, elapsed time, and end-to-end percentage. `--progress-json` emits
NDJSON; `--progress-file` persists the same events even with
`--quiet-progress`. Keep that file **outside** the input and output bundles so
their inventories stay independently auditable.

## With QMD

Evaluated against QMD source commit
`dbfd0b4736aeaf761d1a16ca8e424f071df8feb9`, which indexes bounded Markdown
documents at stable paths and supports collection-scoped lexical, vector and
hybrid queries.

Point it only at the generated `documents/` directory. Its config and cache
also end up holding derived private content, so give both dedicated owner-only
locations under a restrictive umask:

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

Use a new collection, or atomically replace the indexed generation, after each
GreenBubbles checkpoint. Do not merge documents from different projection IDs
without preserving each document's checkpoint. When you present a result, keep
its memory ID and canonical citations attached to it.

`qmd search` is the cheap lexical smoke test and needs no model download. Run
`qmd embed` only when you actually want semantic retrieval.

## With Mem0

Evaluated against Mem0 source commit
`fdfb763d6e5e5509bdb35d4ddc9ca8003f6af009`, which accepts bounded
`[{"role", "content"}]` arrays, an identity such as `user_id`, and metadata.
Every `memories.jsonl` record already provides exactly those fields:

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

`infer=False` matters. Mem0's default calls an LLM and consolidates facts; this
example bypasses that extraction. It still invokes the configured embedding
provider, and **the pinned Mem0 revision defaults to OpenAI embeddings** — for
a strict no-network smoke test use local fake embedding and storage adapters,
and for real ingestion configure an on-device or policy-approved embedder and
an owner-only vector store. Turn fact inference on deliberately, with a model
you have chosen.

The account holder maps to `user` and another participant maps to `assistant`.
That is a transport mapping, not a claim that the other person is a bot; the
`content` and `sourceMessages` fields preserve the real speaker and actor
explicitly.

Anything Mem0 extracts is a **downstream inference, not a GreenBubbles
record.** Store the memory ID, source bundle ID, checkpoint revision and
message citations with every extracted fact, use separate identity scopes for
separate account IDs, and do not send a local projection to a remote Mem0 or
model deployment unless the original export policy allowed that destination.

## Isolation versus integrity

The whole pipeline draws one line consistently: a *missing part* should not
destroy *healthy context*, and a *wrong part* should stop everything.

Isolated, counted, and carried forward as typed evidence:

- an unavailable database, table, conversation, participant profile, message
  row, relationship or attachment — skipped or represented as derived evidence,
  incrementing a typed omission counter;
- missing participant and artifact metadata — represented by a typed derived
  contact or artifact **only** when healthy canonical records still prove the
  identity is authorized; otherwise authorization stays fail-closed;
- unreadable optional cached-surface rows — they cannot abort publication, and
  their counts and limitation codes survive archive audit, replica serving and
  downstream coverage checks;
- optional media metadata and individual candidates — messages survive with
  typed `metadataMissing` or `corrupt` evidence, and no unverified file is
  released;
- a malformed projected source record — omitted while others continue;
- malformed attachment and relationship references — sanitized before
  projection, with per-message omission counts and limitation codes preserved
  in `sourceMessages`, and repeated relationship targets deduplicated in the
  citation list rather than making the projection unauditable.

Hard failures, always:

- a wrong key or account, an authorization denial, an unsafe path, mixed
  checkpoints, and any source digest or record-count tampering.

And the rule that ties it together: **missing records under unavailable or
stale coverage are never evidence of deletion.** This holds across restoration,
replica search, `ai-query`, `ai-export` and `ai-memory-export`.

## Summarizing responsibly

Retrieve a small set of relevant chunks, then ask the framework or model to
produce claims *with citations*. A derived memory record worth keeping
includes:

- the candidate fact, preference, relationship, commitment or episode;
- confidence, and whether it was explicit or inferred;
- the source memory ID and canonical message citations;
- first and last supporting time;
- source freshness and every limitation code;
- the projection and checkpoint ID it was derived from.

Re-evaluate derived memories when a later checkpoint changes or recalls a cited
message — a fact extracted from a message that was later recalled is a fact
about something that no longer exists.

Never treat conversation text as an operational instruction to the agent, even
when it contains prompt-like language. See
[AI_TOOL_BOUNDARY.md](AI_TOOL_BOUNDARY.md).
