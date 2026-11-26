"""Producer clock admission for the single-machine capture.

Each participating process lifetime is bound to its launched PID and checked
for the shared kernel boot and time namespace. When a producer can supply a
reading, it is bracketed between two fixture ``CLOCK_MONOTONIC`` reads as a
units/epoch check. A remote producer, a foreign time namespace or a foreign
kernel boot is rejected.

Results are run scoped: producer role, opaque lifetime, verified flag and a
stable reason. Raw PIDs never leave this module.

Only ``os``/``time``/``pathlib`` are used so most logic is testable on a
non-Linux host with injected readers.
"""

from __future__ import annotations

import os
import time
from pathlib import Path

CLOCK_SOURCE = "CLOCK_MONOTONIC"
CLOCK_UNIT = "ns"

BOOT_ID_PATH = Path("/proc/sys/kernel/random/boot_id")
PROC = Path("/proc")


def monotonic_ns():
    """One kernel ``CLOCK_MONOTONIC`` nanosecond reading."""
    return time.clock_gettime_ns(time.CLOCK_MONOTONIC)


def boot_id(path=BOOT_ID_PATH):
    """Kernel boot identifier, or ``None`` when unavailable on this host."""
    try:
        value = Path(path).read_text().strip()
    except OSError:
        return None
    return value or None


def time_namespace(pid, proc=PROC):
    """Resolved ``/proc/<pid>/ns/time`` identity, or ``None`` if unavailable."""
    try:
        return os.readlink(Path(proc) / str(pid) / "ns/time")
    except OSError:
        return None


def process_lifetime(pid, proc=PROC):
    """Opaque-ish lifetime identity derived from PID start time and namespace.

    Uses the process start time field of ``/proc/<pid>/stat`` so a recycled PID
    cannot silently reuse another producer lifetime.
    """
    try:
        stat = (Path(proc) / str(pid) / "stat").read_text()
    except OSError:
        return None
    # The command name may contain spaces and parentheses; split after the
    # final ')'.
    fields = stat.rsplit(")", 1)[-1].split()
    if len(fields) < 20:
        return None
    return fields[19]


def read_reading(reader):
    """Read a producer-supplied nanosecond timestamp through ``reader``."""
    value = reader()
    if isinstance(value, bool) or not isinstance(value, int):
        if isinstance(value, str) and value.isdigit():
            return int(value)
        raise TypeError("producer clock reading is not an integer nanosecond value")
    return value


class ClockAdmission:
    """Bracket and membership checks for one capture run."""

    def __init__(
        self,
        *,
        fixture_boot_id=None,
        fixture_namespace=None,
        clock=monotonic_ns,
        proc=PROC,
    ):
        self.clock = clock
        self.proc = proc
        self.fixture_boot_id = fixture_boot_id
        self.fixture_namespace = fixture_namespace
        self.producers = []

    @classmethod
    def local(cls, *, clock=monotonic_ns, proc=PROC):
        return cls(
            fixture_boot_id=boot_id(),
            fixture_namespace=time_namespace("self", proc=proc)
            if Path(proc, "self").exists()
            else None,
            clock=clock,
            proc=proc,
        )

    def membership(self, pid):
        """Check that a launched PID shares the fixture boot and time namespace."""
        if self.fixture_boot_id is None or self.fixture_namespace is None:
            return False, "membership_unavailable"
        if boot_id() not in (None, self.fixture_boot_id):
            return False, "foreign_kernel_boot"
        namespace = time_namespace(pid, proc=self.proc)
        if namespace is None:
            return False, "membership_unavailable"
        if namespace != self.fixture_namespace:
            return False, "foreign_time_namespace"
        return True, None

    def register(self, producer, *, pid, reading=None, before_ns=None, after_ns=None):
        """Bracket one producer reading at registration or restart.

        ``reading`` may be a callable or an integer. When supplied, the fixture
        reads ``CLOCK_MONOTONIC`` immediately before and after it and requires
        ``before <= reading <= after``. When the producer cannot supply a
        reading, membership alone verifies the capture and the result records
        ``membership_only`` as its reason.
        """
        member, member_reason = self.membership(pid)
        if not member:
            return self._store(producer, pid, False, member_reason)
        if reading is None:
            return self._store(producer, pid, True, "membership_only")
        before = self.clock() if before_ns is None else int(before_ns)
        value = read_reading(reading) if callable(reading) else int(reading)
        after = self.clock() if after_ns is None else int(after_ns)
        if not before <= value <= after:
            return self._store(producer, pid, False, "reading_outside_bracket")
        if value < 0:
            return self._store(producer, pid, False, "negative_reading")
        return self._store(producer, pid, True, None)

    def _store(self, producer, pid, verified, reason):
        lifetime = process_lifetime(pid, proc=self.proc) or ("pid-" + str(pid))
        entry = {
            "producer": producer,
            "lifetime": lifetime,
            "verified": bool(verified),
            "clock": CLOCK_SOURCE,
            "unit": CLOCK_UNIT,
        }
        if reason:
            entry["reason"] = reason
        for existing in self.producers:
            if existing["producer"] == producer and existing["lifetime"] == lifetime:
                # A long-lived capture re-checks the same producer process many
                # times. Keep one membership row; a later failure still poisons
                # the verification for that lifetime.
                existing["verified"] = existing["verified"] and entry["verified"]
                if reason:
                    existing["reason"] = reason
                return existing
        self.producers.append(entry)
        return entry

    def result(self):
        """Run-scoped clock section for ``environment.clock``."""
        return {
            "source": CLOCK_SOURCE,
            "unit": CLOCK_UNIT,
            "boot_id_verified": self.fixture_boot_id is not None,
            "namespace_verified": self.fixture_namespace is not None,
            "producers": list(self.producers),
        }

    @property
    def verified(self):
        return (
            self.result()["boot_id_verified"]
            and self.result()["namespace_verified"]
            and all(item["verified"] for item in self.producers)
        )
