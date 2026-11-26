"""Ephemeral capture identity and the process environment contract.

The fixture provisions one random 32-byte capture key and one run identity per
run. They are exported only through process environment variables to launched
Core/CLI/Host/DSH/Access processes. The key is never written to a report,
event, log line or artifact name.
"""

from __future__ import annotations

import hashlib
import hmac
import os
import secrets
import uuid

CAPTURE_KEY_BYTES = 32

MODE_ENV = "WHALEREIN_BENCH_MODE"
RUN_ENV = "WHALEREIN_BENCH_RUN_ID"
KEY_ENV = "WHALEREIN_BENCH_CAPTURE_KEY"
OUTLET_ENV = "WHALEREIN_BENCH_OUTLET"
OUTLET_CAPACITY_ENV = "WHALEREIN_BENCH_OUTLET_CAPACITY"
OUTLET_BYTES_ENV = "WHALEREIN_BENCH_OUTLET_BYTES"
CONTRACT_ENV = "WHALEREIN_BENCH_CONTRACT_PATH"

MODES = ("off", "aggregate", "trace")


class Capture:
    """One run's capture identity and outlet layout."""

    def __init__(self, mode="off", *, reports=None, run_id=None, key=None):
        if mode not in MODES:
            raise ValueError("unknown observation mode " + str(mode))
        self.mode = mode
        self.run_id = run_id or ("run-" + uuid.uuid4().hex[:12])
        # The key exists for every mode but is only exported in trace mode;
        # off/aggregate create no outlet.
        self.key = key if key is not None else secrets.token_bytes(CAPTURE_KEY_BYTES)
        if len(self.key) != CAPTURE_KEY_BYTES:
            raise ValueError("capture key must be exactly 32 bytes")
        self.reports = reports

    @property
    def trace(self):
        return self.mode == "trace"

    def token(self, domain, raw):
        """Run-local opaque token matching the product producers' contract."""
        message = (
            b"whalerein/latency-token/v1\x00"
            + domain.encode()
            + b"\x00"
            + str(raw).encode()
        )
        return hmac.new(self.key, message, hashlib.sha256).hexdigest()[:32]

    def outlet_path(self, role, lifetime):
        """NDJSON outlet for one process lifetime inside the private mount."""
        return os.path.join(str(self.reports), "outlet", f"{role}-{lifetime}.ndjson")

    def environment(self, role, lifetime):
        """Environment exported to one launched producer process.

        Only trace mode enables capture, so off/aggregate export nothing (the
        key must not reach a process that cannot use it).
        """
        if self.mode != "trace":
            return {}
        return {
            MODE_ENV: self.mode,
            RUN_ENV: self.run_id,
            KEY_ENV: self.key.hex(),
            OUTLET_ENV: self.outlet_path(role, lifetime),
            OUTLET_CAPACITY_ENV: str(self.outlet_capacity),
            OUTLET_BYTES_ENV: str(self.outlet_bytes),
            CONTRACT_ENV: "/workspace/contracts/latency/observation.json",
        }

    # Bounds are declared once so every producer uses the same handoff limit.
    outlet_capacity = 4096
    outlet_bytes = 32 << 20

    def redact(self, text):
        """Return ``text`` with capture secrets removed (defence in depth)."""
        if not isinstance(text, str):
            return text
        return (
            text.replace(self.key.hex(), "[capture-key]")
            .replace(self.key.hex().upper(), "[capture-key]")
            .replace(self.run_id, "[run]")
        )

    def leaks(self, text):
        """Stable reasons when a secret appears in published text."""
        reasons = []
        if self.key.hex() in text or self.key.hex().upper() in text:
            reasons.append("capture_key_leak")
        if self.run_id in text:
            reasons.append("run_id_leak")
        return reasons
