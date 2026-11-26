"""Seeded slot schedule and slot reconciliation.

The schedule is persisted before any producer starts and before the first
attempt is launched. It declares every warmup and measured slot, the arm and
the counterbalanced pair order. The fixture later emits one raw attempt fact
per planned slot; unexecuted slots stay ``not_started`` instead of being
silently absent.

Pure module: no container, product or ``repo`` imports.
"""

from __future__ import annotations

PHASES = ("warmup", "measured")
WATCH_ARM = "watch"
MIXED_ARMS = ("idle", "bulk")
SLOT_STATES = (
    "planned",
    "started",
    "measurement_entered",
    "finished",
    "setup_failed",
    "interrupted",
    "not_started",
)

# Nominal distance between planned slot starts. It only orders the plan; the
# actual wall time of every slot is measured independently and is what the
# attempt facts and the deadline use.
DEFAULT_SLOT_CYCLE_MS = 15_000


def seed_parity(seed):
    """Stable parity used by the counterbalanced pair order.

    Mirrors the model rule without importing the model: a numeric seed uses
    its integer parity, any other seed uses the sum of its code points.
    """
    value = str(seed).strip()
    if value.lstrip("-").isdigit():
        return int(value) % 2
    return sum(ord(char) for char in value) % 2


def first_arm(seed):
    """The arm of order 0 in the first pair of a phase."""
    return MIXED_ARMS[seed_parity(seed)]


def pair_orders(seed, count):
    """Yield the arm order of each successive pair in one phase."""
    first = first_arm(seed)
    other = MIXED_ARMS[1] if first == MIXED_ARMS[0] else MIXED_ARMS[0]
    for index in range(count):
        yield (first, other) if index % 2 == 0 else (other, first)


def plan(configuration):
    """Build the versioned, seeded slot plan for one case.

    ``configuration`` is the run configuration mapping. Measured slot counts
    equal ``attempts`` and warmup slot counts equal ``warmup_attempts``; the
    model rejects any disagreement.
    """
    attempts = int(configuration["attempts"])
    warmups = int(configuration.get("warmup_attempts", 0))
    workflow = configuration["workflow"]
    seed = str(configuration["seed"])
    cycle = float(configuration.get("slot_cycle_ms") or DEFAULT_SLOT_CYCLE_MS)
    slots = []
    ordinal = 0
    for phase, count in (("warmup", warmups), ("measured", attempts)):
        if workflow == "mixed-load":
            if count % 2:
                raise ValueError(
                    "mixed load requires an even number of slots per phase"
                )
            prefix = "warmup" if phase == "warmup" else "pair"
            for pair_index, order in enumerate(pair_orders(seed, count // 2)):
                for position, arm in enumerate(order):
                    slots.append(
                        {
                            "slot": ordinal,
                            "phase": phase,
                            "arm": arm,
                            "pair": f"{prefix}-{pair_index:04d}",
                            "order": position,
                            "planned_start_ms": round(cycle * len(slots), 3),
                        }
                    )
                    ordinal += 1
        else:
            if workflow not in ("watch", "recovery"):
                raise ValueError("unknown workflow " + str(workflow))
            prefix = "warmup" if phase == "warmup" else "slot"
            for index in range(count):
                slots.append(
                    {
                        "slot": ordinal,
                        "phase": phase,
                        "arm": WATCH_ARM,
                        "pair": f"{prefix}-{index:04d}",
                        "order": 0,
                        "planned_start_ms": round(cycle * len(slots), 3),
                    }
                )
                ordinal += 1
    return {"seed": seed, "rule": "alternating", "slots": slots}


def reconcile(schedule, attempts):
    """Compare persisted slots with emitted attempt facts.

    Returns a control-plane result, not a business verdict: ``missing`` lists
    planned slots with no fact, ``duplicates`` lists slots emitted twice,
    ``unknown`` lists facts for slots outside the plan and ``states`` maps
    every planned slot to its emitted status (``not_started`` when absent).
    """
    planned = {slot["slot"]: slot for slot in schedule["slots"]}
    states = {ordinal: "not_started" for ordinal in planned}
    seen = {}
    duplicates = []
    unknown = []
    for attempt in attempts:
        ordinal = attempt.get("slot")
        if ordinal not in planned:
            unknown.append(ordinal)
            continue
        if ordinal in seen:
            duplicates.append(ordinal)
        seen[ordinal] = attempt.get("status", "planned")
        states[ordinal] = seen[ordinal]
    missing = [ordinal for ordinal in planned if ordinal not in seen]
    return {
        "slots": len(planned),
        "emitted": len(seen),
        "missing": missing,
        "duplicates": duplicates,
        "unknown": unknown,
        "states": states,
        "complete": not missing and not duplicates and not unknown,
    }


def ordered_slots(schedule, phase=None):
    """Yield planned slots in execution order, optionally for one phase."""
    for slot in schedule["slots"]:
        if phase is None or slot["phase"] == phase:
            yield slot
