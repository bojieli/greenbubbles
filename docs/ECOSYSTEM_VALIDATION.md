# Ecosystem validation evidence

Evidence recorded on 2026-08-27 and extended on 2026-08-28 uses only synthetic
connector data and tool metadata.

## Primary CLI skill workflow

The `greenbubbles-context` repository skill routes an agent to the one-shot
`ai-query` command and checkpoint-consistent `ai-export` bundle. Integration
tests prove policy-scoped search without a daemon, normalized static
conversations/contacts/messages/artifacts, source coverage on every response,
per-record fresh versus preserved-stale database state, owner-only and atomic
outputs, digest/count manifests, absolute-path/raw-field suppression, read-only
operation enforcement, and monotonic progress completion. This is the primary
agent integration for new development.

## Downstream change consumer

The runnable change consumer and deterministic Markdown projection are tested
end to end as described in `DOWNSTREAM_CONSUMER.md`. The proof covers bootstrap,
idle resume, durable cursor advancement through body-free changes, minimized
record refresh, owner-only outputs, and replacement-replica fail closure.

These results validate the implemented GreenBubbles interfaces. They do not
prove interoperability with every agent host, real-client acquisition latency,
or any Phase 4 action mechanism.
