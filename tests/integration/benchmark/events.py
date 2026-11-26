"""Typed outlet validation, allowlisting, deterministic merge and coverage.

Each producer writes append-only NDJSON. The fixture drains those outlets,
validates every record against ``contracts/latency/observation.json``, drops
any field that is not declared for the event (counting it), drops malformed or
foreign-run records, merges in a deterministic order and stops retaining
detail at the declared cap while reserving room for finalization metadata.

Nothing here derives a business verdict. Pure module: the manifest is passed
in, the caller supplies file paths.
"""

from __future__ import annotations

import json
from collections import Counter

EVENT_SCHEMA = 1
DEFAULT_RESERVE_BYTES = 64 << 10


def load_manifest(path):
    with open(path, "r", encoding="utf-8") as handle:
        manifest = json.load(handle)
    for key in ("producers", "header", "events", "coverage_paths", "forbidden_fields"):
        if key not in manifest:
            raise ValueError("latency manifest is missing " + key)
    return manifest


def _is_int(value):
    return isinstance(value, int) and not isinstance(value, bool)


def value_matches(kind, value):
    """Check one manifest type token."""
    if kind == "str":
        return isinstance(value, str)
    if kind == "bool":
        return type(value) is bool
    if kind == "producer":
        return isinstance(value, str)
    if kind == "u64":
        return _is_int(value) and value >= 0
    if kind.startswith("enum:"):
        return isinstance(value, str) and value in kind[len("enum:"):].split(",")
    return False



def sanitize(record, manifest, *, run_id=None):
    """Validate and allowlist one record.

    Returns ``(clean, dropped)`` where ``dropped`` is a Counter of reasons.
    ``clean`` is ``None`` when the record cannot be retained.
    """
    dropped = Counter()
    if not isinstance(record, dict):
        dropped["malformed_records"] += 1
        return None, dropped
    for key in manifest["header"]["required"]:
        if key not in record:
            dropped["malformed_records"] += 1
            return None, dropped
    if record.get("v") != EVENT_SCHEMA:
        dropped["unsupported_version"] += 1
        return None, dropped
    event = record.get("event")
    if not isinstance(event, str) or event not in manifest["events"]:
        dropped["unknown_event"] += 1
        return None, dropped
    producer = record.get("producer")
    if producer not in manifest["producers"]:
        dropped["unknown_producer"] += 1
        return None, dropped
    if producer not in manifest["events"][event]["producers"]:
        dropped["wrong_producer"] += 1
        return None, dropped
    if not isinstance(record.get("lifetime"), str) or not record["lifetime"]:
        dropped["malformed_records"] += 1
        return None, dropped
    if not _is_int(record.get("seq")) or record["seq"] < 0:
        dropped["malformed_records"] += 1
        return None, dropped
    if not _is_int(record.get("ts")) or record["ts"] < 0:
        dropped["malformed_records"] += 1
        return None, dropped
    if run_id is not None and record.get("run") != run_id:
        dropped["foreign_run"] += 1
        return None, dropped
    data = record.get("data")
    if not isinstance(data, dict):
        dropped["malformed_records"] += 1
        return None, dropped
    spec = manifest["events"][event]
    fields = spec["fields"]
    forbidden = set(manifest["forbidden_fields"])
    clean_data = {}
    for name, value in data.items():
        if name in forbidden:
            dropped["forbidden_fields"] += 1
            continue
        if name not in fields:
            dropped["unknown_fields"] += 1
            continue
        clean_data[name] = value
    for name in spec["required"]:
        if name not in clean_data:
            dropped["missing_required"] += 1
            return None, dropped
        if not value_matches(fields[name], clean_data[name]):
            dropped["invalid_type"] += 1
            return None, dropped
    for name, value in clean_data.items():
        if name in spec["required"]:
            continue
        if not value_matches(fields[name], value):
            dropped["invalid_type"] += 1
            return None, dropped
    clean = {
        "v": EVENT_SCHEMA,
        "run": record["run"],
        "producer": producer,
        "lifetime": record["lifetime"],
        "seq": record["seq"],
        "ts": record["ts"],
        "event": event,
        "data": clean_data,
    }
    return clean, dropped


def read_outlet(path, manifest, *, run_id):
    """Read one NDJSON outlet file.

    Returns ``(records, losses, error)``; ``error`` is a stable reason when the
    file could not be fully read, so an interrupted producer flush stays
    visible instead of looking like an empty capture.
    """
    records = []
    losses = Counter()
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as handle:
            for line in handle:
                line = line.strip()
                if not line:
                    continue
                try:
                    value = json.loads(line)
                except ValueError:
                    losses["malformed_lines"] += 1
                    continue
                clean, dropped = sanitize(value, manifest, run_id=run_id)
                losses.update(dropped)
                if clean is None:
                    losses["outlet_records"] += 1
                else:
                    records.append(clean)
    except FileNotFoundError:
        return records, losses, "outlet_missing"
    except OSError:
        return records, losses, "outlet_unreadable"
    return records, losses, None


def event_order(event):
    """Deterministic merge order: timestamp, producer, lifetime, sequence."""
    return (event["ts"], event["producer"], event["lifetime"], event["seq"])


def serialise(event):
    return json.dumps(event, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def merge(outlet_files, manifest, *, run_id, cap_bytes, reserve_bytes=DEFAULT_RESERVE_BYTES):
    """Drain, allowlist and cap every outlet into one deterministic capture.

    ``outlet_files`` maps an outlet path to its producer role. ``cap_bytes`` is
    the whole sidecar budget; ``reserve_bytes`` is held back for coverage,
    loss and finalization metadata. Returns a mapping with retained ``events``,
    aggregate ``losses``, byte accounting, per-path coverage and per-outlet
    status.
    """
    records = []
    losses = Counter()
    outlets = {}
    incomplete = False
    for path, role in sorted(outlet_files.items()):
        found, dropped, error = read_outlet(path, manifest, run_id=run_id)
        for event in found:
            event["_outlet"] = path
        records.extend(found)
        losses.update(dropped)
        outlets[path] = {
            "role": role,
            "records": len(found),
            "reason": error,
        }
        if error:
            incomplete = True
    records.sort(key=event_order)
    budget = max(0, int(cap_bytes) - max(0, int(reserve_bytes)))
    retained = []
    used = 0
    for event in records:
        size = len(serialise(event)) + 1
        if used + size > budget:
            losses["trace_bytes"] += size
            continue
        retained.append(event)
        used += size
    truncated = len(retained) < len(records)
    coverage = coverage_report(retained, manifest, truncated=truncated)
    losses["outlet_records"] += len(records) - len(retained)
    return {
        "events": retained,
        "losses": dict(sorted(losses.items())),
        "bytes": used,
        "cap_bytes": int(cap_bytes),
        "reserve_bytes": int(reserve_bytes),
        "records": len(records),
        "retained": len(retained),
        "truncated": truncated,
        "complete": not incomplete and not truncated,
        "outlets": outlets,
        "coverage": coverage,
    }


def coverage_report(events, manifest, *, truncated=False):
    """Per-path coverage: which mandatory boundaries were actually observed."""
    observed_by_path = {name: set() for name in manifest["coverage_paths"]}
    names = {event["event"] for event in events}
    for path, required in manifest["coverage_paths"].items():
        observed_by_path[path] = sorted(names & set(required))
    result = {}
    for path, required in manifest["coverage_paths"].items():
        observed = observed_by_path[path]
        result[path] = {
            "required": list(required),
            "observed": observed,
            "complete": set(required) <= set(observed) and not truncated,
        }
    return result


def write_events(path, events):
    """Write the merged sidecar and return its byte accounting and checksum."""
    import hashlib

    digest = hashlib.sha256()
    total = 0
    with open(path, "w", encoding="utf-8") as handle:
        for event in events:
            clean = {key: value for key, value in event.items() if key != "_outlet"}
            line = serialise(clean) + "\n"
            handle.write(line)
            digest.update(line.encode())
            total += len(line.encode())
    return {"bytes": total, "checksum": "sha256:" + digest.hexdigest()}