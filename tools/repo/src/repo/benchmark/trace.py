"""Bounded native Perfetto export for trace-enabled benchmark cases.

The exporter is offline and deterministic: it consumes the same typed case
events and model-derived exemplars that produced the report. It never contacts
a live fixture and never manufactures a span for a missing event. All packets
use the shared kernel CLOCK_MONOTONIC clock id, so the trace has one origin for
every producer track.
"""

from __future__ import annotations

import hashlib
import time
from collections import defaultdict
from pathlib import Path

from google.protobuf.message import DecodeError
from perfetto.protos.perfetto.trace.perfetto_trace_pb2 import Trace
from perfetto.trace_builder.proto_builder import TraceProtoBuilder

from ..errors import RepoError

MONOTONIC_CLOCK_ID = 3
# Perfetto requires every TrackEvent packet to name the sequence that owns its
# incremental state, even when the event carries direct names and track ids.
TRACE_SEQUENCE_ID = 1
CAPTURE_TRACK_NAME = "capture"
EXEMPLAR_TRACK_PREFIX = "exemplar/"
MAX_TRACE_BYTES = 64 * 1024 * 1024
FLOW_DOMAINS = (
    "request",
    "stream",
    "frame",
    "delivery",
    "ordinal",
    "object_op",
    "watch",
    "connection",
    "batch",
)
COUNTER_FIELDS = (
    ("queue_depth", "queue depth"),
    ("queue_bytes", "queue bytes"),
    ("bytes", "bytes"),
    ("wait_ns", "wait ns"),
)


def _uuid(name):
    # Deterministic, nonzero 64-bit track identity.
    value = int.from_bytes(hashlib.blake2b(name.encode(), digest_size=8).digest(), "big")
    return value or 1


def _signed(value):
    return value if value < 1 << 63 else value - (1 << 64)


def _annotation(event):
    annotations = [("event", event["event"]), ("producer", event["producer"])]
    data = event.get("data") or {}
    for key in FLOW_DOMAINS:
        value = data.get(key)
        if isinstance(value, str) and value:
            annotations.append((key, value))
    for key, label in COUNTER_FIELDS:
        value = data.get(key)
        if isinstance(value, int) and value >= 0:
            annotations.append((label, str(value)))
    return annotations


def _mark_sequence(packet):
    packet.trusted_packet_sequence_id = TRACE_SEQUENCE_ID
    return packet


def _add_annotations(packet, annotations, string_type, int_type):
    for name, value in annotations:
        annotation = packet.track_event.debug_annotations.add()
        annotation.name = name
        if type(value) is int:
            annotation.int_value = value
        else:
            annotation.string_value = str(value)


def _event_packet(builder, track_uuid, event, flows):
    packet = _mark_sequence(builder.add_packet())
    packet.timestamp = int(event["ts"])
    packet.timestamp_clock_id = MONOTONIC_CLOCK_ID
    track_event = packet.track_event
    track_event.type = track_event.TYPE_INSTANT
    track_event.track_uuid = track_uuid
    track_event.name = event["event"]
    track_event.flow_ids.extend(flows)
    _add_annotations(packet, _annotation(event), None, None)
    return packet


def _prepare_flows(events):
    """Assign one flow id per joined group, shared by every event in it."""
    groups = defaultdict(list)
    for index, event in enumerate(events):
        data = event.get("data") or {}
        for domain in FLOW_DOMAINS:
            value = data.get(domain)
            if isinstance(value, str) and value:
                groups[(domain, value)].append(index)
    flow_ids = {}
    packet_flows = defaultdict(list)
    next_flow = 1
    for key, indexes in sorted(groups.items(), key=lambda item: (item[0][0], item[0][1])):
        if len(indexes) < 2:
            continue
        flow = next_flow
        next_flow += 1
        flow_ids[key] = flow
        for index in indexes:
            packet_flows[index].append(flow)
    return flow_ids, packet_flows


def _boottime_at(origin_ns):
    """Express the same instant in BOOTTIME so Perfetto can relate the
    TrackEvent default clock domain to the MONOTONIC trace time clock."""
    if not hasattr(time, "CLOCK_BOOTTIME"):
        # Offline tests on non-Linux hosts have no BOOTTIME reading; the
        # relation is only consumed by Perfetto and the events remain MONOTONIC.
        return int(origin_ns)
    now_mono = time.clock_gettime_ns(time.CLOCK_MONOTONIC)
    now_boot = time.clock_gettime_ns(time.CLOCK_BOOTTIME)
    return int(origin_ns) + (now_boot - now_mono)


def export(events, exemplars, path, *, origin_ns=None, run_id="unknown"):
    """Write one deterministic native Perfetto trace.

    `events` are manifest-shaped observation rows; `exemplars` each carry
    `attempt`, `metric`, `reason`, `start_ns` and `end_ns` in the same clock.
    Returns the artifact metadata the case manifest records.
    """
    events = list(events or [])
    exemplars = list(exemplars or [])
    builder = TraceProtoBuilder()
    # One MONOTONIC origin for every track. The snapshot is metadata only; the
    # packets keep absolute kernel readings so direct subtraction stays valid.
    snapshot_packet = _mark_sequence(builder.add_packet())
    snapshot = snapshot_packet.clock_snapshot
    snapshot.primary_trace_clock = MONOTONIC_CLOCK_ID
    origin = int(origin_ns if origin_ns is not None else 0)
    clock = snapshot.clocks.add()
    clock.clock_id = MONOTONIC_CLOCK_ID
    clock.timestamp = origin
    # Perfetto's default TrackEvent clock is BOOTTIME (6). Without a clock
    # snapshot relating 6 to our MONOTONIC (3) trace time, every TrackEvent is
    # reported as an unrelatable clock domain. The pair is sampled at one
    # instant; in a container without suspend the two differ only by the
    # kernel's boot/suspend offset.
    boottime = snapshot.clocks.add()
    boottime.clock_id = 6
    boottime.timestamp = _boottime_at(origin)
    info = _mark_sequence(builder.add_packet())
    info.trace_uuid.msb = _signed(_uuid("run:" + str(run_id)))
    info.trace_uuid.lsb = _signed(_uuid("origin:" + str(origin_ns)))
    info.system_info.tracing_service_version = "whalerein-bench/2"

    tracks = {}
    producers = {event["producer"] for event in events}
    if exemplars:
        producers.add("fixture")
    for role in sorted(producers):
        name = f"producer/{role}"
        tracks[role] = _uuid(name)
        packet = _mark_sequence(builder.add_packet())
        descriptor = packet.track_descriptor
        descriptor.uuid = tracks[role]
        descriptor.name = name
    exemplar_tracks = {}
    for metric in sorted({exemplar["metric"] for exemplar in exemplars}):
        name = EXEMPLAR_TRACK_PREFIX + metric
        exemplar_tracks[metric] = _uuid(name)
        packet = _mark_sequence(builder.add_packet())
        packet.track_descriptor.uuid = exemplar_tracks[metric]
        packet.track_descriptor.name = name

    counter_tracks = {}
    for role in sorted({event["producer"] for event in events}):
        for field, label in COUNTER_FIELDS:
            relevant = (
                event for event in events
                if event["producer"] == role and field in (event.get("data") or {})
            )
            if next(relevant, None) is None:
                continue
            name = f"counter/{role}/{label}"
            uuid = _uuid(name)
            counter_tracks[(role, field)] = uuid
            packet = _mark_sequence(builder.add_packet())
            descriptor = packet.track_descriptor
            descriptor.uuid = uuid
            descriptor.name = name
            descriptor.counter.is_incremental = False

    _, packet_flows = _prepare_flows(events)
    for index, event in enumerate(events):
        _event_packet(builder, tracks[event["producer"]], event, packet_flows.get(index, ()))
        data = event.get("data") or {}
        for (role, field), uuid in counter_tracks.items():
            if role != event["producer"] or field not in data:
                continue
            packet = _mark_sequence(builder.add_packet())
            packet.timestamp = int(event["ts"])
            packet.timestamp_clock_id = MONOTONIC_CLOCK_ID
            packet.track_event.type = packet.track_event.TYPE_COUNTER
            packet.track_event.track_uuid = uuid
            packet.track_event.counter_value = int(data[field])

    for exemplar in exemplars:
        exemplar_uuid = exemplar_tracks[exemplar["metric"]]
        start = int(exemplar["start_ns"])
        end = int(exemplar["end_ns"])
        if end < start:
            raise RepoError("invalid benchmark trace: exemplar range reversed")
        begin = _mark_sequence(builder.add_packet())
        begin.timestamp = start
        begin.timestamp_clock_id = MONOTONIC_CLOCK_ID
        begin.track_event.type = begin.track_event.TYPE_SLICE_BEGIN
        begin.track_event.track_uuid = exemplar_uuid
        begin.track_event.name = f"{exemplar['metric']} [{exemplar['attempt']}]"
        _add_annotations(
            begin,
            [
                ("metric", exemplar["metric"]),
                ("attempt", exemplar["attempt"]),
                ("reason", exemplar.get("reason") or ""),
                ("coverage", exemplar.get("coverage") or ""),
            ],
            None,
            None,
        )
        finish = _mark_sequence(builder.add_packet())
        finish.timestamp = end
        finish.timestamp_clock_id = MONOTONIC_CLOCK_ID
        finish.track_event.type = finish.track_event.TYPE_SLICE_END
        finish.track_event.track_uuid = exemplar_uuid

    data = builder.serialize()
    if len(data) > MAX_TRACE_BYTES:
        raise RepoError("benchmark trace exceeds the declared byte cap")
    target = Path(path)
    target.write_bytes(data)
    return {
        "path": str(target),
        "bytes": len(data),
        "checksum": "sha256:" + hashlib.sha256(data).hexdigest(),
        "clock": "CLOCK_MONOTONIC",
        "tracks": sorted(tracks),
        "events": len(events),
        "exemplars": len(exemplars),
    }


def read(path):
    try:
        return Trace.FromString(Path(path).read_bytes())
    except (OSError, ValueError, DecodeError) as exc:
        raise RepoError("invalid benchmark trace: unreadable native file") from exc


def inspect(path):
    """Decode the native file without Trace Processor for contract checks."""
    trace = read(path)
    tracks = set()
    flow_ids = set()
    timestamps = []
    clock_ids = set()
    slices = 0
    packets = 0
    by_track = {}
    for packet in trace.packet:
        packets += 1
        if packet.HasField("track_descriptor"):
            tracks.add(packet.track_descriptor.name)
        if packet.HasField("timestamp"):
            timestamps.append(packet.timestamp)
            if packet.timestamp_clock_id:
                clock_ids.add(packet.timestamp_clock_id)
        if packet.HasField("track_event"):
            slices += 1
            flow_ids.update(packet.track_event.flow_ids)
            by_track.setdefault(packet.track_event.track_uuid, []).append(packet.timestamp)
    return {
        "packets": packets,
        "tracks": sorted(tracks),
        "slices": slices,
        "flows": len(flow_ids),
        "clock_ids": sorted(clock_ids),
        "first_ns": min(timestamps) if timestamps else None,
        "last_ns": max(timestamps) if timestamps else None,
        "monotonic": all(values == sorted(values) for values in by_track.values()),
    }


def verify(path, *, events=0, exemplars=0, minimum_ns=None, maximum_ns=None):
    """Assert the native file agrees with the canonical facts."""
    summary = inspect(path)
    if summary["clock_ids"] != [MONOTONIC_CLOCK_ID]:
        raise RepoError("benchmark trace does not use one MONOTONIC clock")
    if not summary["monotonic"]:
        raise RepoError("benchmark trace timestamps are not ordered")
    if events and summary["slices"] < events:
        raise RepoError("benchmark trace is missing measured slices")
    if exemplars and not any(
        name.startswith(EXEMPLAR_TRACK_PREFIX) for name in summary["tracks"]
    ):
        raise RepoError("benchmark trace is missing exemplar ranges")
    if minimum_ns is not None and (summary["first_ns"] is None or summary["first_ns"] < minimum_ns):
        raise RepoError("benchmark trace range precedes its capture")
    if maximum_ns is not None and (summary["last_ns"] is None or summary["last_ns"] > maximum_ns):
        raise RepoError("benchmark trace range exceeds its capture")
    return summary