"""Source emission facts and per-workflow measurement cuts.

Start conditions are workflow specific and named explicitly in the facts:

``first-source-emission``
    Ordinary Watch: the first actual source emission (not subscription,
    readiness or a scheduled timer).
``client-invocation``
    Cold connect: the client invocation timestamp.
``link-restoration``
    Recovery: the observed link restoration. The blackout interval is retained
    separately and the catch-up fact requires a visible correct prefix that
    covers everything emitted by restoration.

Pure module: timestamps are integers in CLOCK_MONOTONIC nanoseconds.
"""

from __future__ import annotations

FIRST_EMISSION = "first-source-emission"
CLIENT_INVOCATION = "client-invocation"
LINK_RESTORATION = "link-restoration"


def milliseconds(value_ns, origin_ns):
    return round((value_ns - origin_ns) / 1_000_000, 3)


def origin(workflow, *, first_emission_ns=None, client_invocation_ns=None, restored_ns=None):
    """Return the named measurement cut for one attempt.

    Returns ``(origin_ns, name)``. A missing required boundary raises: the
    fixture must not invent a zero origin for an unfinished start condition.
    """
    if workflow == "recovery":
        if restored_ns is None:
            raise ValueError("recovery requires an observed link restoration")
        return int(restored_ns), LINK_RESTORATION
    if workflow in ("cold-connect", "cold_connect"):
        if client_invocation_ns is None:
            raise ValueError("cold connect requires the client invocation time")
        return int(client_invocation_ns), CLIENT_INVOCATION
    if first_emission_ns is None:
        raise ValueError("ordinary Watch requires the first actual source emission")
    return int(first_emission_ns), FIRST_EMISSION


def source_series(emission_ns, sizes, origin_ns):
    """Relative source times and UTF-8 sizes.

    ``times_ms`` are relative to the attempt cut, so the first ordinary Watch
    emission is exactly 0.0 ms. Recovery keeps pre-restoration emissions as
    negative offsets instead of discarding them.
    """
    times = [milliseconds(int(value), origin_ns) for value in emission_ns]
    return {"times_ms": times, "sizes": [int(value) for value in sizes]}


def emitted_by(times_ms, sizes, cutoff_ms):
    """Logical source bytes emitted at or before ``cutoff_ms``."""
    return sum(size for at_ms, size in zip(times_ms, sizes) if at_ms <= cutoff_ms)


def catch_up(outputs, times_ms, sizes, restored_ms):
    """First visible correct prefix covering everything emitted by restoration.

    Returns ``None`` until such a record is observed. The returned fact keeps
    only the observed time and the path that established it; it is not a
    verdict about recovery quality.
    """
    target = emitted_by(times_ms, sizes, restored_ms)
    for event in outputs:
        if event.get("at_ms", 0) < restored_ms:
            continue
        if not event.get("correct"):
            continue
        if event.get("prefix_bytes", 0) >= target:
            kind = event.get("kind")
            via = "terminal" if kind == "terminal" else "preview"
            return {"at_ms": event["at_ms"], "via": via, "prefix_bytes": event["prefix_bytes"]}
    return None


def recovery_facts(
    *,
    blackout_start_ns,
    restored_ns,
    origin_ns,
    outputs,
    times_ms,
    sizes,
    deadline_ms,
    watch_ready_ns=None,
    first_update_ns=None,
    timeout=False,
):
    """Raw recovery facts with the blackout kept separate from the cut.

    Only fixture-observable milestones are recorded. Product-owned boundaries
    (subscription readiness, channel and Watch readiness) arrive through typed
    events, so the fixture never fabricates them.
    """
    restored_ms = milliseconds(restored_ns, origin_ns)
    milestones = {
        "blackout_start": {
            "at_ms": milliseconds(blackout_start_ns, origin_ns),
            "state": "observed",
        },
        "link_restored": {"at_ms": 0.0, "state": "observed"},
    }
    if watch_ready_ns is not None:
        milestones["watch_ready"] = {
            "at_ms": milliseconds(watch_ready_ns, origin_ns),
            "state": "observed",
        }
    if first_update_ns is not None:
        milestones["first_update"] = {
            "at_ms": milliseconds(first_update_ns, origin_ns),
            "state": "observed",
            "via": "preview",
        }
    first_visible = next(
        (event for event in outputs if event.get("correct") and event.get("prefix_bytes", 0) > 0),
        None,
    )
    if first_visible is not None:
        milestones["first_visible"] = {
            "at_ms": first_visible["at_ms"],
            "state": "observed",
        }
        milestones["first_update"] = {
            "at_ms": min(
                first_visible["at_ms"],
                milestones.get("first_update", {}).get("at_ms", first_visible["at_ms"]),
            ),
            "state": "observed",
            "via": "preview",
        }
    matched = catch_up(outputs, times_ms, sizes, restored_ms)
    if matched is not None:
        milestones["caught_up"] = {
            "at_ms": matched["at_ms"],
            "state": "observed",
            "via": matched["via"],
        }
    elif timeout:
        milestones["timeout"] = {"at_ms": float(deadline_ms), "state": "observed"}
    return {
        "blackout_start_ms": milestones["blackout_start"]["at_ms"],
        "restored_ms": restored_ms,
        "milestones": milestones,
    }