# Python format reference

This file is the agent reference for `--format python` UserAsCode extraction runs. Read it when the driver passes `--format python` (the default).

## Directory layout

```
user_project/
├── .gitignore
├── manifest.py                   # Always-in-context index: DOMAINS + ACTIVE_ALERTS
├── runner.py                     # Runs all constraint modules and prints alerts
├── domains/
│   ├── identity/
│   │   ├── schema.py             # Dataclass definitions
│   │   └── state.py             # Current instances  # source: session_N, YYYY-MM-DD
│   ├── travel/
│   │   ├── schema.py
│   │   └── state.py
│   ├── finance/
│   │   ├── schema.py
│   │   └── state.py
│   └── <domain>/                 # Created as facts accumulate in new life areas
│       ├── schema.py
│       └── state.py
├── constraints/
│   ├── travel_readiness.py       # def check(project) -> list[Alert]
│   └── health_safety.py
└── tests/
    ├── test_identity.py
    └── test_travel.py
```

## `.gitignore`

```
__pycache__/
*.pyc
*.pyo
.DS_Store
.greenbubbles-runs/
```

## `manifest.py` template

```python
# manifest.py — ALWAYS IN AGENT CONTEXT
"""User project manifest. Load this first in every session."""

from datetime import datetime

# One-line summary per domain. Keep under 80 characters.
# Format: "key fact 1; key fact 2 | updated YYYY-MM-DD"
DOMAINS = {
    "identity":      "Name, DOB, passport | updated 2026-01-15",
    "travel":        "2 upcoming trips; passport expires 2026-06-01 | updated 2026-01-20",
    "finance":       "2 accounts; 1 pending transfer | updated 2026-01-10",
    "health":        "Allergies: peanuts; Rx: cetirizine | updated 2025-12-01",
    "vehicles":      "Toyota Camry 2022 | updated 2025-11-20",
    "family":        "Spouse: Alex; Daughter: Maya (6) | updated 2026-01-05",
    "social":        "Weekly tennis, book club | updated 2025-12-15",
    "work":          "Software engineer at Acme; remote | updated 2026-01-18",
    "entertainment": "Sci-fi books; hiking; Netflix | updated 2025-12-20",
}

ACTIVE_ALERTS: list[str] = [
    # Populated by runner.py — do not edit manually
    # "[CRITICAL] travel_readiness: Passport expires 2026-06-01, "
    # "Singapore trip departs 2026-06-15 (only 14 days validity, many destinations require 6 months)",
]

LAST_UPDATED = "2026-01-20T14:30:00+08:00"
```

**Updating the manifest:** After patching any domain state, update the DOMAINS entry for that domain with a fresh summary and today's date. After running `runner.py`, replace the ACTIVE_ALERTS list with the runner's output. Always update LAST_UPDATED.

## `domains/<domain>/schema.py` template

```python
# domains/travel/schema.py
"""Schema definitions for the travel domain."""

from dataclasses import dataclass, field
from datetime import date
from typing import Optional


@dataclass
class PassportInfo:
    number: str                      # Redact middle digits in state: "AB*****67"
    expiry: date
    issuing_country: str
    nationality: str = ""


@dataclass
class Trip:
    destination: str
    departure: date
    return_date: Optional[date]
    booking_refs: list[str] = field(default_factory=list)
    is_international: bool = True
    notes: str = ""


@dataclass
class TravelProfile:
    seat_preference: str = ""        # Free-text preference description
    frequent_flyer: list[str] = field(default_factory=list)
    lounge_access: list[str] = field(default_factory=list)
```

**Schema evolution:** When new facts require new fields, add them to the dataclass with a default value so existing state.py instances remain valid. When a domain needs to be split, create the new domain directory and move the relevant dataclass, updating all imports in state.py.

## `domains/<domain>/state.py` template

```python
# domains/travel/state.py
"""Current travel state. Each instance carries a source comment."""

from datetime import date
from domains.travel.schema import PassportInfo, Trip, TravelProfile

# Passport
passport = PassportInfo(
    number="AB*****67",
    expiry=date(2026, 6, 1),
    issuing_country="US",
    nationality="US",
)
# source: session_003, 2026-01-20

# Upcoming trips
upcoming_trips = [
    Trip(
        destination="Singapore",
        departure=date(2026, 6, 15),
        return_date=date(2026, 6, 22),
        booking_refs=["SQ-1234"],
        is_international=True,
    ),
    # source: session_005, 2026-01-18
]

# Travel preferences
profile = TravelProfile(
    seat_preference="Aisle on flights over 4 hours, window on shorter legs.",
    frequent_flyer=["Singapore Airlines KrisFlyer"],
)
# source: session_001, 2025-11-10
```

**Source comments:** Every instance or significant assignment carries a `# source: session_N, YYYY-MM-DD` comment immediately after the value. When updating a value, update the source comment to reflect the new session and date. When adding a new fact, append it with a new source comment.

**CRUD rules in state.py:**
- **Add:** Append the new instance at the end of the relevant section, with a source comment.
- **Update:** Edit the value in place; update the source comment.
- **Skip:** If the value is identical, leave the file unchanged.
- Never remove facts from state.py unless they are demonstrably stale (the relevant entity no longer exists). Use a periodic revise pass for archival decisions.

## `constraints/<name>.py` template

```python
# constraints/travel_readiness.py
"""Cross-domain constraint: passport validity vs upcoming international trips.

Generated by session_005 on 2026-01-18. Promoted from ad-hoc check.
"""

from collections import namedtuple
from datetime import date

Alert = namedtuple("Alert", ["severity", "constraint", "message"])

PASSPORT_MIN_VALIDITY_DAYS = 180  # Required by most destinations; adjust per destination


def check(project) -> list[Alert]:
    """Check passport validity against all upcoming international trips."""
    alerts: list[Alert] = []
    try:
        from domains.travel import state as travel
    except ImportError:
        return alerts

    passport = getattr(travel, "passport", None)
    upcoming = getattr(travel, "upcoming_trips", [])
    if passport is None:
        return alerts

    today = date.today()
    for trip in upcoming:
        if not getattr(trip, "is_international", False):
            continue
        if trip.departure < today:
            continue  # Past trip
        days_valid = (passport.expiry - trip.departure).days
        if passport.expiry <= trip.departure:
            alerts.append(Alert(
                severity="CRITICAL",
                constraint="travel_readiness",
                message=(
                    f"Passport expires {passport.expiry}, "
                    f"{trip.destination} trip departs {trip.departure} "
                    f"(passport EXPIRED at departure)"
                ),
            ))
        elif days_valid < PASSPORT_MIN_VALIDITY_DAYS:
            alerts.append(Alert(
                severity="WARNING",
                constraint="travel_readiness",
                message=(
                    f"Passport expires {passport.expiry}, "
                    f"{trip.destination} trip departs {trip.departure} "
                    f"(only {days_valid} days validity, need {PASSPORT_MIN_VALIDITY_DAYS})"
                ),
            ))
    return alerts
```

**When to write a constraint:** Write one when you notice:
- A time-dependent condition (expiry date vs. future event)
- A cross-domain incompatibility (allergy vs. new medication)
- Conflicting instructions from different sources
- A deadline or threshold that the user should be notified about proactively

**Constraint naming:** Use `<domain>_<condition>.py`, e.g., `travel_readiness.py`, `health_safety.py`, `finance_authorization.py`.

**Alert severities:** `CRITICAL` (user must act before a deadline or harm), `WARNING` (user should be aware), `INFO` (informational cross-domain note).

## `runner.py` template

```python
# runner.py
"""Discover all constraint modules, run check(), and print active alerts.

Run with: python runner.py
Output is used to populate manifest.py:ACTIVE_ALERTS.
"""

import importlib
import sys
from pathlib import Path

PROJECT_ROOT = Path(__file__).parent


def main() -> int:
    constraints_dir = PROJECT_ROOT / "constraints"
    if not constraints_dir.is_dir():
        print("No constraints directory found.")
        return 0

    # Make the project root importable so constraint modules can import domains
    if str(PROJECT_ROOT) not in sys.path:
        sys.path.insert(0, str(PROJECT_ROOT))

    alerts = []
    for path in sorted(constraints_dir.glob("*.py")):
        if path.name.startswith("_"):
            continue
        module_name = f"constraints.{path.stem}"
        try:
            module = importlib.import_module(module_name)
            check_fn = getattr(module, "check", None)
            if check_fn is None:
                continue
            result = check_fn(None)  # project arg reserved for future use
            for alert in result or []:
                alerts.append(f"[{alert.severity}] {alert.constraint}: {alert.message}")
        except Exception as exc:  # noqa: BLE001
            print(f"  constraint {path.stem} failed: {exc}", file=sys.stderr)

    if alerts:
        print(f"{len(alerts)} active alert(s):")
        for alert in alerts:
            print(f"  {alert}")
    else:
        print("No active alerts.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
```

## `tests/test_<domain>.py` template

```python
# tests/test_travel.py
"""Invariant tests for the travel domain.

Generated by session_005 on 2026-01-18.
Run with: python -m pytest tests/
"""

import sys
from pathlib import Path
from datetime import date

# Make the project root importable
sys.path.insert(0, str(Path(__file__).parent.parent))

from domains.travel import state as travel
from domains.travel.schema import PassportInfo, Trip


def test_passport_has_expiry():
    assert hasattr(travel, "passport"), "passport must be defined in travel state"
    assert isinstance(travel.passport.expiry, date), "passport.expiry must be a date"


def test_upcoming_trips_are_list():
    assert hasattr(travel, "upcoming_trips"), "upcoming_trips must be defined"
    assert isinstance(travel.upcoming_trips, list)


def test_no_past_departure_in_upcoming():
    """Upcoming trips should not have departure dates in the past."""
    today = date.today()
    for trip in travel.upcoming_trips:
        # Allow a grace period of 7 days for trips that just departed
        assert trip.departure >= date(today.year, today.month, 1), (
            f"Trip to {trip.destination} ({trip.departure}) looks stale — "
            "move it to an archive if it has passed"
        )
```

**When to generate tests:** Write one test module per domain when you first create that domain. Focus on invariants: required fields exist, types are correct, no obviously stale data. Add specific tests when a constraint reveals an invariant worth enforcing permanently.

## Workflow summary (Python format)

1. Run `memory next` → review pages → extract facts.
2. For each touched domain: read schema.py + state.py, diff, patch state.py.
3. Run `git diff HEAD` in user_project — verify only expected changes appear.
4. Run `python -m pytest tests/` if tests exist.
5. Write or update any new constraints in `constraints/`.
6. Run `python runner.py` and capture output.
7. Update `manifest.py` DOMAINS summaries and ACTIVE_ALERTS from runner output.
8. Run `memory commit --format python`.
9. Run `memory status`.
10. (Driver handles): `git -C <user_project> add -A && git -C <user_project> commit -m "memory update: ..."`.

## Revise pass (Python format)

During a holistic revise pass (`personal-memory-parallel.py revise`), the agent:

1. Reads the full manifest and all domain state.
2. Identifies opportunities: domains that could be split, schemas that need new fields, stale state that should be archived, constraints that are no longer relevant.
3. Applies schema evolution: add fields with defaults, rename with a migration comment, split domains into subdirectories.
4. Archives stale state: move old facts to an `archive/` section at the bottom of state.py with an `# archived: YYYY-MM-DD` comment.
5. Prunes constraints: remove checks for events that have passed or conditions that no longer apply.
6. Cross-domain reference audit: verify all cross-domain references in constraints are still valid.
7. Regenerates manifest.py.
8. Commits with message: `periodic revision: <summary>`.
