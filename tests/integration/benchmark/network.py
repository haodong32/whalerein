"""Shape endpoint traffic inside the benchmark's private loopback namespace."""

from __future__ import annotations

import json
import os
import socket
import subprocess
import time

from repo.benchmark.model import NETWORK_PROBE_COUNT, shaped_links

BROKER_ADDRESS = "127.0.0.1"
HOST_BROKER_ADDRESS = "127.0.0.2"
CLIENT_ADDRESS = "127.0.0.3"
BROKER_PORT = 1883
LINK_ADDRESS = {"host": HOST_BROKER_ADDRESS, "client": CLIENT_ADDRESS}
NETEM_HANDLE = "20:"


def tc(*args):
    return subprocess.check_output(["tc", *args], timeout=5)


def configure(settings):
    if os.environ.get("WHALEREIN_INTEGRATION_CONTAINER") != "1" or {
        name for _, name in socket.if_nameindex()
    } != {"lo"}:
        raise RuntimeError("benchmark network requires a loopback-only container")

    tc("qdisc", "replace", "dev", "lo", "root", "handle", "1:",
       "prio", "bands", "2", "priomap", *["0"] * 16)
    tc("qdisc", "replace", "dev", "lo", "parent", "1:2",
       "handle", NETEM_HANDLE, "netem", *netem_options(settings))

    # Traffic in either direction at the selected endpoint addresses traverses
    # the shaped band once. Access↔broker and fixture traffic use the reference.
    priority = 0
    for link in shaped_links(settings):
        for direction in ("dst", "src"):
            priority += 1
            tc("filter", "add", "dev", "lo", "parent", "1:", "protocol", "ip",
               "priority", str(priority), "u32", "match", "ip", direction,
               f"{LINK_ADDRESS[link]}/32", "flowid", "1:2")


def netem_options(settings):
    profile = ["delay", f"{settings['extra_rtt_ms'] * 500}us"]
    jitter = settings["jitter_ms_per_direction"]
    if jitter:
        profile.extend([f"{jitter}ms", "distribution", "normal"])
    loss = settings["loss_percent_per_direction"]
    profile.extend(["loss", "random", f"{loss}%"])
    return profile


def block_client():
    # The benchmark configures this qdisc for the client endpoint only.
    tc("qdisc", "change", "dev", "lo", "parent", "1:2",
       "handle", NETEM_HANDLE, "netem", "loss", "100%")


def restore_client(settings):
    tc("qdisc", "change", "dev", "lo", "parent", "1:2",
       "handle", NETEM_HANDLE, "netem", *netem_options(settings))


def disconnect_watch():
    selector = ["-H", "-n", "-t", "state", "established",
                "dst", f"{CLIENT_ADDRESS}:{BROKER_PORT}"]
    if not subprocess.check_output(["ss", *selector], timeout=5).strip():
        raise RuntimeError("recovery requires an established Watch connection")
    subprocess.run(["ss", "-K", *selector], capture_output=True, timeout=5, check=True)
    if subprocess.check_output(["ss", *selector], timeout=5).strip():
        raise RuntimeError("the Watch connection survived the forced disconnect")


def counters():
    qdiscs = json.loads(tc("-s", "-j", "qdisc", "show", "dev", "lo"))
    root = next(item for item in qdiscs if item["handle"] == NETEM_HANDLE and item["kind"] == "netem")
    return {"packets": root["packets"], "dropped": root["drops"]}


def probe():
    """Measure public-facing TCP round trips separately from the reference."""
    listeners = {"reference": BROKER_ADDRESS, **LINK_ADDRESS}
    result = {key + "_ms": [] for key in listeners}
    for _ in range(NETWORK_PROBE_COUNT):
        for key, address in listeners.items():
            started = time.monotonic_ns()
            with socket.create_connection((address, BROKER_PORT), timeout=5):
                result[key + "_ms"].append(round((time.monotonic_ns() - started) / 1_000_000, 3))
    return result
