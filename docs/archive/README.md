# Archived development record

This directory preserves superseded documents for provenance. It is not a
second documentation set, and nothing in it should be read as a current claim.

Archived documents may contain abandoned mechanisms, measurements that were
later corrected, command-line options that no longer exist, and conclusions
that the implementation went on to contradict. Their original text is retained
so that rejected approaches and mistaken measurements stay auditable.

Current documentation starts at [`../README.md`](../README.md).

## What is here

| Document | Why it was archived |
| --- | --- |
| [`PLAN-2026-08.md`](PLAN-2026-08.md) | The original product and engineering plan, written before the query architecture replaced full restoration. Superseded by [`../ROADMAP.md`](../ROADMAP.md) and [`../ARCHITECTURE.md`](../ARCHITECTURE.md). |
| [`ACQUISITION_FEASIBILITY.md`](ACQUISITION_FEASIBILITY.md) | The study that decided how the database key could be obtained. Its conclusion is now [`../PASSPHRASE_ACQUISITION.md`](../PASSPHRASE_ACQUISITION.md). |
| [`ACTIVE_READ_FEASIBILITY.md`](ACTIVE_READ_FEASIBILITY.md) | Whether the running client could be asked to fetch dynamic content. Not implemented. |
| [`SEND_INTEGRATION_DESIGN.md`](SEND_INTEGRATION_DESIGN.md), [`SEND_PATH_RE_FINDINGS.md`](SEND_PATH_RE_FINDINGS.md), [`SEND_ATTACHMENTS_PLAN.md`](SEND_ATTACHMENTS_PLAN.md) | Design and reverse-engineering notes for the send path. The shipped position is [`../SEND_ADAPTER.md`](../SEND_ADAPTER.md): public builds cannot leave dry-run mode. |
| [`GATE_READINESS.md`](GATE_READINESS.md), [`LOCAL_ACQUISITION_VALIDATION.md`](LOCAL_ACQUISITION_VALIDATION.md), [`ECOSYSTEM_VALIDATION.md`](ECOSYSTEM_VALIDATION.md) | Point-in-time readiness and validation evidence. Current evidence lives in [`../MEASUREMENTS.md`](../MEASUREMENTS.md) and [`../KNOWN_LIMITATIONS.md`](../KNOWN_LIMITATIONS.md). |
| [`AI_DESKTOP_AGENT_HANDOFF.md`](AI_DESKTOP_AGENT_HANDOFF.md) | Working notes from a design session, kept because the alternatives it rejected are still worth knowing. |
