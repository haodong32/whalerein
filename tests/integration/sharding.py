"""Deterministic shard partition shared by pytest collection and host tests.

Only the standard library is used so this helper is importable both inside the
integration image and from the repository unit tests.
"""

import zlib


def parse_shard(value):
    """Parse INDEX/COUNT (INDEX is 0-based) or raise ValueError."""
    if not isinstance(value, str):
        raise ValueError("--shard must be INDEX/COUNT")
    parts = value.split("/")
    if len(parts) != 2:
        raise ValueError("--shard must be INDEX/COUNT")
    try:
        index = int(parts[0])
        count = int(parts[1])
    except ValueError:
        raise ValueError("--shard INDEX and COUNT must be integers") from None
    if count < 1:
        raise ValueError("--shard COUNT must be positive")
    if not 0 <= index < count:
        raise ValueError("--shard INDEX must be between 0 and COUNT-1")
    return index, count


def bucket(nodeid, count):
    """Stable, deterministic shard number for a pytest node ID."""
    return zlib.crc32(nodeid.encode("utf-8")) % count


def partition(items, index, count, identify=lambda item: getattr(item, "nodeid", item)):
    """Return every item assigned to one shard; no duplicates by construction."""
    return [item for item in items if bucket(identify(item), count) == index]
