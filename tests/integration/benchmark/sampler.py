"""Environment evidence sampling.

Samples the *existing* established MQTT sockets at a low fixed frequency,
together with qdisc deltas, Node runtime/event-loop delay, available Access
runtime counters, and effective CPU/memory use. TCP connect probes remain
startup controls only and never stand in for continuous RTT or bandwidth.

Sampling gaps are recorded explicitly; an idle or unsupported sample is not a
measured zero. Optional OS profiling is reported unsupported when unavailable
and never increases privileges.

Pure parsers are separated from the sampling thread so the parsing and window
logic can be unit-tested on any host.
"""

from __future__ import annotations

import re
import resource
import shutil
import subprocess
import threading
import time

MQTT_PORT = 1883


# ---------------------------------------------------------------------------
# Parsers
# ---------------------------------------------------------------------------


def parse_ss(text):
    """Parse ``ss -tin`` output into bounded per-socket facts."""
    sockets = []
    current = None
    for raw in text.splitlines():
        line = raw.rstrip()
        if not line.strip():
            current = None
            continue
        if line[0].isalpha():
            parts = line.split()
            if len(parts) < 5:
                current = None
                continue
            state = parts[0]
            local = parts[3]
            peer = parts[4]
            current = {
                "state": state,
                "local": local,
                "peer": peer,
                "send_queue_bytes": _int(parts[2]),
                "rtt_us": None,
                "retransmits": None,
                "bytes_acked": None,
            }
            sockets.append(current)
            continue
        if current is None:
            continue
        fields = line.split()
        for index, field in enumerate(fields):
            if field.startswith("rtt:"):
                value = field.split(":", 1)[1].split("/")[0]
                current["rtt_us"] = round(float(value) * 1000) if _float(value) is not None else None
            elif field.startswith("retrans:"):
                value = field.split(":", 1)[1].split("/")[-1]
                current["retransmits"] = _int(value)
            elif field.startswith("bytes_acked:"):
                current["bytes_acked"] = _int(field.split(":", 1)[1])
    return sockets


def mqtt_sockets(sockets, *, port=MQTT_PORT):
    """Keep established sockets that belong to the benchmark MQTT port."""
    result = []
    for socket in sockets:
        if socket.get("state") != "ESTAB":
            continue
        if _port(socket.get("local")) != port and _port(socket.get("peer")) != port:
            continue
        result.append(socket)
    return result


def socket_role(socket, *, host_address, client_address):
    """Classify a socket by the endpoint address that owns the broker peer."""
    for address, role in ((host_address, "host"), (client_address, "client")):
        if not address:
            continue
        if socket.get("local", "").startswith(address + ":") or socket.get(
            "peer", ""
        ).startswith(address + ":"):
            return role
    return "reference"


def socket_identity(socket):
    """Identity that changes when the TCP connection is replaced."""
    return (socket.get("local"), socket.get("peer"))


def parse_metrics(text, *, limit=400):
    """Parse selected Prometheus samples from an Access ``/metrics`` body."""
    result = {}
    count = 0
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if not line.startswith(("go_", "process_", "whalerein_")):
            continue
        name, _, value = line.partition(" ")
        # Labeled series (histograms/summaries) are skipped to bound
        # cardinality; only stable single-valued runtime counters are kept.
        if "{" in name:
            continue
        try:
            number = float(value.split()[0])
        except (ValueError, IndexError):
            continue
        if name in result:
            continue
        result[name] = number
        count += 1
        if count >= limit:
            break
    return result


def parse_cgroup_limits(cpu_max=None, memory_max=None):
    """Effective CPU and memory limits from cgroup v2 files."""
    limits = {"cpus": None, "memory_bytes": None}
    if cpu_max:
        fields = cpu_max.split()
        if len(fields) == 2 and fields[0] != "max":
            try:
                limits["cpus"] = round(int(fields[0]) / int(fields[1]), 3)
            except (ValueError, ZeroDivisionError):
                pass
    if memory_max:
        value = memory_max.strip()
        if value.isdigit():
            limits["memory_bytes"] = int(value)
    return limits




def proc_peak_rss(pid="self"):
    try:
        with open(f"/proc/{pid}/status", "r", encoding="utf-8") as handle:
            text = handle.read()
    except OSError:
        return None
    match = re.search(r"VmHWM:\s+(\d+)\s+kB", text)
    return int(match.group(1)) * 1024 if match else None


def fixture_cpu_ms():
    usage = resource.getrusage(resource.RUSAGE_SELF)
    children = resource.getrusage(resource.RUSAGE_CHILDREN)
    return round(
        (usage.ru_utime + usage.ru_stime + children.ru_utime + children.ru_stime) * 1000,
        3,
    )


def profiler_capability():
    """Optional OS profiler availability; never elevates privileges."""
    for name in ("perf", "py-spy"):
        if shutil.which(name):
            return {"available": True, "tool": name, "reason": None}
    return {"available": False, "tool": None, "reason": "unsupported"}


def sample_gap_ms(previous_ms, current_ms, interval_ms):
    """Observed gap between two samples, or ``None`` when on schedule."""
    if previous_ms is None:
        return None
    gap = current_ms - previous_ms
    if gap > interval_ms * 1.5:
        return round(gap, 3)
    return None


def windows(samples, interval_ms):
    """Collapse per-sample facts into contiguous per-role/generation windows."""
    result = []
    for sample in samples:
        key = (sample["role"], sample["generation"])
        if result and (result[-1]["role"], result[-1]["generation"]) == key:
            window = result[-1]
            window["end_ms"] = sample["at_ms"]
            window["samples"] += 1
            if sample.get("rtt_us") is not None:
                window["rtt_us"] = sample["rtt_us"]
                window["rtt_min_us"] = (
                    sample["rtt_us"]
                    if window.get("rtt_min_us") is None
                    else min(window["rtt_min_us"], sample["rtt_us"])
                )
                window["rtt_max_us"] = (
                    sample["rtt_us"]
                    if window.get("rtt_max_us") is None
                    else max(window["rtt_max_us"], sample["rtt_us"])
                )
            if sample.get("retransmits") is not None:
                window["retransmits"] = sample["retransmits"]
            if sample.get("queue_bytes") is not None:
                window["queue_bytes"] = max(window.get("queue_bytes") or 0, sample["queue_bytes"])
            if sample.get("gap_ms") is not None:
                window["gap_ms"] = round((window.get("gap_ms") or 0) + sample["gap_ms"], 3)
            window["state"] = sample["state"]
            continue
        result.append(
            {
                "role": sample["role"],
                "generation": sample["generation"],
                "start_ms": sample["at_ms"],
                "end_ms": sample["at_ms"],
                "samples": 1,
                "state": sample["state"],
                "interval_ms": interval_ms,
                "rtt_us": sample.get("rtt_us"),
                "rtt_min_us": sample.get("rtt_us"),
                "rtt_max_us": sample.get("rtt_us"),
                "retransmits": sample.get("retransmits"),
                "queue_bytes": sample.get("queue_bytes"),
                "gap_ms": sample.get("gap_ms"),
            }
        )
    return result


def _port(value):
    if not isinstance(value, str) or ":" not in value:
        return None
    try:
        return int(value.rsplit(":", 1)[1])
    except ValueError:
        return None


def _int(value):
    try:
        return int(value)
    except (TypeError, ValueError):
        return None


def _float(value):
    try:
        return float(value)
    except (TypeError, ValueError):
        return None




# ---------------------------------------------------------------------------
# Sampling thread
# ---------------------------------------------------------------------------


class EnvironmentSampler:
    """1 Hz sampler for sockets, qdisc deltas and available runtime counters."""

    def __init__(
        self,
        *,
        interval_ms=1000,
        origin_ns,
        socket_probe=None,
        node_probe=None,
        access_probe=None,
        qdisc_probe=None,
        roles=None,
        addresses=None,
    ):
        self.interval_ms = max(100, int(interval_ms))
        self.origin_ns = origin_ns
        self.socket_probe = socket_probe
        self.node_probe = node_probe
        self.access_probe = access_probe
        self.qdisc_probe = qdisc_probe
        self.roles = roles or {}
        self.addresses = addresses or {}
        self._thread = None
        self._stop = threading.Event()
        self._samples = []
        self._generations = {}
        self._identities = {}
        self._runtime = []
        self._access = []
        self._qdisc = []
        self._errors = []
        self._overhead_start = None
        self._overhead_ms = 0.0
        self._last_ms = None
        self.lock = threading.Lock()

    def start(self):
        if self._thread is not None:
            return self
        self._thread = threading.Thread(target=self._loop, name="bench-environment-sampler", daemon=True)
        self._thread.start()
        return self

    def stop(self):
        self._stop.set()
        if self._thread is not None:
            self._thread.join(timeout=10)
            self._thread = None

    def _now_ms(self):
        return round((time.clock_gettime_ns(time.CLOCK_MONOTONIC) - self.origin_ns) / 1_000_000, 3)

    def _loop(self):
        while not self._stop.is_set():
            started = time.process_time()
            at_ms = self._now_ms()
            try:
                self._sample(at_ms)
                self._last_ms = at_ms
            except Exception as error:  # noqa: BLE001 - sampling never perturbs the case
                reason = type(error).__name__
                if reason not in self._errors:
                    self._errors.append(reason)
            finally:
                self._overhead_ms += (time.process_time() - started) * 1000
            self._stop.wait(self.interval_ms / 1000)

    def _sample(self, at_ms):
        gap = sample_gap_ms(self._last_ms, at_ms, self.interval_ms)
        for sample in self._socket_samples(at_ms):
            sample["gap_ms"] = gap
            with self.lock:
                self._samples.append(sample)
        node = self._call(self.node_probe)
        if node is not None:
            with self.lock:
                self._runtime.append({"start_ms": at_ms, **node})
        access = self._call(self.access_probe)
        if access is not None:
            with self.lock:
                self._access.append({"start_ms": at_ms, "counters": access})
        qdisc = self._call(self.qdisc_probe)
        if qdisc is not None:
            with self.lock:
                self._qdisc.append({"at_ms": at_ms, **qdisc})

    def _call(self, probe):
        if probe is None:
            return None
        try:
            return probe()
        except Exception:  # noqa: BLE001
            return None

    def _socket_samples(self, at_ms):
        if self.socket_probe is None:
            return []
        try:
            sockets = mqtt_sockets(parse_ss(self.socket_probe()))
        except Exception:  # noqa: BLE001
            return []
        samples = []
        seen_roles = set()
        for socket in sockets:
            role = socket_role(
                socket,
                host_address=self.addresses.get("host"),
                client_address=self.addresses.get("client"),
            )
            identity = socket_identity(socket)
            if role in self._identities and self._identities[role] != identity:
                self._generations[role] = self._generations.get(role, 1) + 1
                samples.append(
                    {
                        "role": role,
                        "generation": self._generations[role],
                        "at_ms": at_ms,
                        "state": "replaced",
                        "rtt_us": socket.get("rtt_us"),
                        "retransmits": socket.get("retransmits"),
                        "queue_bytes": socket.get("send_queue_bytes"),
                    }
                )
            self._identities[role] = identity
            generation = self._generations.setdefault(role, 1)
            seen_roles.add(role)
            samples.append(
                {
                    "role": role,
                    "generation": generation,
                    "at_ms": at_ms,
                    "state": "sampled" if socket.get("rtt_us") is not None else "idle",
                    "rtt_us": socket.get("rtt_us"),
                    "retransmits": socket.get("retransmits"),
                    "queue_bytes": socket.get("send_queue_bytes"),
                }
            )
        for role in self.roles.values():
            if role not in seen_roles:
                samples.append(
                    {
                        "role": role,
                        "generation": self._generations.setdefault(role, 1),
                        "at_ms": at_ms,
                        "state": "idle",
                    }
                )
        return samples

    def summary(self, *, access_metrics_available=None, exporter_cpu_ms=None):
        with self.lock:
            samples = list(self._samples)
            runtime = list(self._runtime)
            access = list(self._access)
            qdisc = list(self._qdisc)
        now_ms = round(
            (time.clock_gettime_ns(time.CLOCK_MONOTONIC) - self.origin_ns) / 1_000_000, 3
        )
        result = {
            "interval_ms": self.interval_ms,
            "windows": windows(samples, self.interval_ms),
            "runtime": _close_windows(runtime, now_ms),
            "access": _close_windows(access, now_ms),
            "qdisc": _qdisc_deltas(qdisc),
            "errors": list(self._errors),
            "sampler_cpu_ms": round(self._overhead_ms, 3),
            "profiler": profiler_capability(),
        }
        if access_metrics_available is not None:
            result["access"]["available"] = bool(access_metrics_available)
        if exporter_cpu_ms is not None:
            result["exporter_cpu_ms"] = exporter_cpu_ms
        return result

    @property
    def overhead_ms(self):
        return round(self._overhead_ms, 3)


def _close_windows(entries, now_ms):
    """Give each sampled runtime/access window an explicit end boundary."""
    closed = []
    for index, entry in enumerate(entries):
        end = entries[index + 1]["start_ms"] if index + 1 < len(entries) else now_ms
        closed.append({**entry, "end_ms": end})
    return closed


def _qdisc_deltas(samples):
    """Per-sample qdisc packet/drop deltas alongside the absolute counters."""
    result = []
    previous = None
    for sample in samples:
        delta = None
        if previous is not None:
            delta = {
                "packets": max(0, sample["packets"] - previous["packets"]),
                "dropped": max(0, sample["dropped"] - previous["dropped"]),
            }
        result.append({**sample, "delta": delta})
        previous = sample
    return result


def default_ss_probe():
    """Read established TCP sockets with kernel timer details."""
    result = subprocess.run(
        ["ss", "-H", "-tin", "state", "established"],
        capture_output=True,
        text=True,
        timeout=5,
        check=False,
    )
    if result.returncode != 0:
        raise OSError("ss failed")
    return result.stdout


def default_access_probe(url="http://127.0.0.1:18080/metrics", timeout=2):
    """Fetch the existing Access Prometheus endpoint (no new listener)."""
    import urllib.request

    with urllib.request.urlopen(url, timeout=timeout) as response:
        text = response.read().decode("utf-8", "replace")
    return parse_metrics(text)