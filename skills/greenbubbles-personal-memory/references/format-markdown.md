# Markdown format reference

This file is the agent reference for `--format markdown` UserAsCode extraction runs. Read it when the driver passes `--format markdown`.

The Markdown format is simpler than the Python format: no Python dependency, human-editable, and directly readable in any text editor or git viewer. It does not support executable constraints; cross-domain alerts appear as manually maintained bullet items in `manifest.md`.

## Directory layout

```
user_project/
├── .gitignore
├── manifest.md                   # Always-in-context index: domain summaries + active alerts
└── domains/
    ├── identity.md
    ├── travel.md
    ├── finance.md
    ├── health.md
    ├── vehicles.md
    ├── family.md
    ├── social.md
    ├── work.md
    ├── entertainment.md
    └── <domain>.md               # Created as facts accumulate in new life areas
```

## `.gitignore`

```
.DS_Store
.greenbubbles-runs/
```

## `manifest.md` template

```markdown
# Personal Memory Manifest

**Last updated:** 2026-01-20T14:30:00+08:00

## Domains

| Domain | Summary | Updated |
|---|---|---|
| [identity](domains/identity.md) | Name, DOB, passport | 2026-01-20 |
| [travel](domains/travel.md) | 2 upcoming trips; passport expires 2026-06-01 | 2026-01-20 |
| [finance](domains/finance.md) | 2 accounts; 1 pending transfer | 2026-01-10 |
| [health](domains/health.md) | Allergies: peanuts; Rx: cetirizine | 2025-12-01 |
| [vehicles](domains/vehicles.md) | Toyota Camry 2022 | 2025-11-20 |
| [family](domains/family.md) | Spouse: Alex; Daughter: Maya (6) | 2026-01-05 |
| [social](domains/social.md) | Weekly tennis, book club | 2025-12-15 |
| [work](domains/work.md) | Software engineer at Acme; remote | 2026-01-18 |
| [entertainment](domains/entertainment.md) | Sci-fi books; hiking; Netflix | 2025-12-20 |

## Active Alerts

<!-- Update manually when cross-domain issues are identified. -->
<!-- Format: - [SEVERITY] constraint: description -->
- [WARNING] travel_readiness: Passport expires 2026-06-01, Singapore trip departs 2026-06-15 (only 14 days validity)

## Coverage

Corpus: `corpus-v2`  
Sessions committed: 12  
Messages committed: 446,435
```

**Updating the manifest:** After patching any domain file, update the corresponding row in the Domains table (new summary, updated date). After patching all domains, update the Active Alerts section with any newly identified cross-domain issues. Always update the Last updated timestamp.

**Active Alerts (Markdown format):** Since Markdown format has no executable constraint runner, alerts are maintained manually. When you notice a cross-domain issue during extraction (e.g., a passport expiry date conflicts with an upcoming trip), add a bullet to the Active Alerts section. When the issue is resolved or no longer relevant, remove or cross out the bullet.

## `domains/<domain>.md` template

```markdown
# Travel Domain

## Schema

<!-- Ontology: concepts and relationships this domain tracks -->
- **PassportInfo**: number (string, redacted), expiry (date), issuing_country, nationality
- **Trip**: destination, departure (date), return_date (date), booking_refs (list), is_international (bool), notes
- **TravelProfile**: seat_preference (string), frequent_flyer (list)

## State

<!-- One entry per fact, deduplicated. Never add duplicate fields.
     Format: - **Field**: value  *(source: session_N, YYYY-MM-DD)* -->

- **PassportNumber**: AB*****67  *(source: session_003, 2026-01-20)*
- **PassportExpiry**: 2026-06-01  *(source: session_003, 2026-01-20)*
- **PassportIssuingCountry**: US  *(source: session_003, 2026-01-20)*
- **UpcomingTrip_Singapore**: departs 2026-06-15, returns 2026-06-22, booking SQ-1234  *(source: session_005, 2026-01-18)*
- **SeatPreference**: Aisle on flights over 4 hours, window on shorter legs  *(source: session_001, 2025-11-10)*
- **FrequentFlyer**: Singapore Airlines KrisFlyer  *(source: session_002, 2025-11-25)*

## History

<!-- Ordered log of changes. Never delete history entries. Append only. -->
- 2026-01-20 (session_003): Passport renewed, new number AB*****67, new expiry 2026-06-01
- 2026-01-18 (session_005): Added Singapore trip, departs 2026-06-15
```

## Deduplication rules for Markdown

These rules are strictly enforced. Violating them creates corrupt state.

### Updating a `## State` line in place

When a fact already in `## State` changes value:

1. Find the exact line: `- **FieldName**: old_value  *(source: session_N, old_date)*`
2. Replace it with: `- **FieldName**: new_value  *(source: session_M, new_date)*`
3. Do not add a second line for the same field. The State section has exactly one line per field.

Example — passport renewed:

Before:
```
- **PassportExpiry**: 2025-02-18  *(source: session_001, 2024-10-15)*
```

After (value changed, source updated):
```
- **PassportExpiry**: 2026-06-01  *(source: session_003, 2026-01-20)*
```

And append to History:
```
- 2026-01-20 (session_003): Passport renewed, new expiry 2026-06-01 (was 2025-02-18)
```

### Adding a new `## State` entry

When a fact does not yet appear in `## State`:

1. Verify by scanning every existing line in the State section. If any line starts with `- **FieldName**:` for the same field, do not add — update instead.
2. Append the new line at the end of the State section, before the `## History` heading.
3. Use the format: `- **FieldName**: value  *(source: session_N, YYYY-MM-DD)*`

### Unchanged facts

If the field already exists in `## State` with the same value, skip entirely. Do not add a duplicate line or append a History entry.

### Field naming

Use PascalCase for field names. Compound keys (e.g., multiple trips) use `_` separators: `UpcomingTrip_Tokyo`, `UpcomingTrip_Singapore`. When a fact is a list, either use separate lines per item (e.g., one `FrequentFlyer_*` per program) or a comma-separated value field.

### History section

The History section is an immutable, append-only log. Every meaningful state change (new fact, updated value) gets one History entry:

```
- YYYY-MM-DD (session_N): <human-readable description of what changed and why>
```

Never:
- Delete a History entry
- Edit a past History entry
- Combine multiple changes into one entry if they occurred in different sessions

## `## Schema` section

The Schema section is a human-readable ontology for the domain: what concepts it tracks and what fields each concept has. Update it when you add a new field type:

```markdown
## Schema

<!-- Ontology: concepts and relationships this domain tracks -->
- **PassportInfo**: number (string, redacted), expiry (date), issuing_country, nationality
- **Trip**: destination, departure (date), return_date (date), booking_refs (list), is_international (bool), notes
```

The Schema section is not executable — it documents intent. Keep it in sync with what actually appears in `## State`.

## Cross-domain constraints in `manifest.md`

Without an executable constraint runner, cross-domain issues must be identified during extraction and recorded manually. When you notice a cross-domain implication (e.g., health allergy + new medication, passport expiry + upcoming trip), add a bullet to the `## Active Alerts` section of `manifest.md`:

```markdown
## Active Alerts

- [CRITICAL] travel_readiness: Passport expires 2026-06-01, Singapore trip departs 2026-06-15 (only 14 days validity)
- [WARNING] health_safety: Prescribed amoxicillin — check penicillin allergy compatibility
```

Severity levels: `CRITICAL` (must act before deadline), `WARNING` (should be aware), `INFO` (informational).

Remove alerts that are no longer relevant during a revise pass.

## Workflow summary (Markdown format)

1. Run `memory next` → review pages → extract facts.
2. For each touched domain: read `domains/<domain>.md`, identify new/changed/unchanged facts.
3. CRUD-patch `## State` section (update in place, append new, skip unchanged).
4. Append to `## History` for every meaningful change.
5. Update `## Schema` if new field types were added.
6. Run `git diff HEAD` in user_project — verify only expected changes appear.
7. Check for cross-domain implications; update `## Active Alerts` in `manifest.md`.
8. Update `manifest.md` domain table row (summary, date).
9. Run `memory commit --format markdown`.
10. Run `memory status`.
11. (Driver handles): `git -C <user_project> add -A && git -C <user_project> commit -m "memory update: ..."`.

## Revise pass (Markdown format)

During a holistic revise pass (`personal-memory-parallel.py revise`), the agent:

1. Reads the full manifest and all domain files.
2. Identifies: stale State entries that should be archived, schema additions, domains that should be split or merged, outdated Active Alerts.
3. Archives stale state: move old State entries to an `## Archive` section at the bottom of the domain file with an `# archived: YYYY-MM-DD` note.
4. Removes or resolves outdated Active Alerts in manifest.md.
5. Updates the Schema section to reflect any new field types added since last revision.
6. Commits with message: `periodic revision: <summary>`.
