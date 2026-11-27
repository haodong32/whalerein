"""Bounded fixture-owned outlet for typed, allowlisted case events.

The fixture participates in the capture with the same envelope as the product
producers: one NDJSON record per fact at the boundary the fixture owns. Records
are validated against the manifest before writing, so an accidental forbidden
field or unknown event fails closed at the fixture instead of leaking.
"""

from __future__ import annotations

import os

from benchmark import events as events_module


class Outlet:
    def __init__(self, *, path, run_id, producer, lifetime, manifest):
        if producer not in manifest["producers"]:
            raise ValueError("unknown fixture producer role")
        self.path = path
        self.run_id = run_id
        self.producer = producer
        self.lifetime = lifetime
        self.manifest = manifest
        self.sequence = 0
        self.handle = None

    def __enter__(self):
        directory = os.path.dirname(self.path)
        if directory:
            os.makedirs(directory, exist_ok=True)
        self.handle = open(self.path, "a", encoding="utf-8")
        return self

    def __exit__(self, *_):
        self.close()

    def emit(self, event, ts=None, **data):
        if self.handle is None:
            raise RuntimeError("fixture outlet is not open")
        self.sequence += 1
        record = {
            "v": 1,
            "run": self.run_id,
            "producer": self.producer,
            "lifetime": self.lifetime,
            "seq": self.sequence,
            # A boundary event discovered after the fact still carries the
            # boundary reading, not the drain time.
            "ts": int(ts) if ts is not None else _now_ns(),
            "event": event,
            "data": data,
        }
        clean, dropped = events_module.sanitize(
            record, self.manifest, run_id=self.run_id
        )
        if clean is None:
            raise ValueError(
                "fixture emitted an invalid event " + event + ": " + str(dict(dropped))
            )
        if dropped:
            raise ValueError(
                "fixture event "
                + event
                + " carried undeliverable fields: "
                + str(dict(dropped))
            )
        self.handle.write(events_module.serialise(clean) + "\n")
        return clean

    def flush(self):
        if self.handle is None:
            return
        self.handle.flush()
        try:
            os.fsync(self.handle.fileno())
        except OSError:
            pass

    def close(self):
        if self.handle is None:
            return
        self.flush()
        self.handle.close()
        self.handle = None


def _now_ns():
    import time

    return time.clock_gettime_ns(time.CLOCK_MONOTONIC)
