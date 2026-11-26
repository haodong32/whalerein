"""Half-open interval math for read exposure around an active Watch.

Touching endpoints yield zero overlap. Summed reader time may exceed the
union, so both are recorded and never conflated. Pure module.
"""

from __future__ import annotations


def interval(start_ms, finish_ms):
    """Normalised half-open interval, or ``None`` for missing observations."""
    if start_ms is None or finish_ms is None:
        return None
    if finish_ms < start_ms:
        return None
    return (float(start_ms), float(finish_ms))


def overlap_ms(read, watch):
    """Length of ``read`` clipped to ``watch``; zero when they only touch."""
    if read is None or watch is None:
        return 0.0
    start = max(read[0], watch[0])
    end = min(read[1], watch[1])
    return round(max(0.0, end - start), 6)


def merge_union(intervals):
    """Total length covered by a set of half-open intervals."""
    ordered = sorted(
        (item for item in intervals if item is not None), key=lambda item: item[0]
    )
    if not ordered:
        return 0.0
    total = 0.0
    current_start, current_end = ordered[0]
    for start, end in ordered[1:]:
        if start > current_end:
            total += current_end - current_start
            current_start, current_end = start, end
        else:
            current_end = max(current_end, end)
    total += current_end - current_start
    return round(total, 6)


def exposure(reads, watch):
    """Summarise per-read and union exposure for one attempt.

    ``reads`` are mappings with ``started_at_ms``/``finished_at_ms`` or the
    pre-normalised ``started_ms``/``finished_ms``. Missing observations are
    reported as unknown, never as zero exposure.
    """
    unknown = 0
    overlapping = 0
    intervals = []
    summed = 0.0
    for read in reads:
        start = read.get("started_at_ms", read.get("started_ms"))
        finish = read.get("finished_at_ms", read.get("finished_ms"))
        current = interval(start, finish)
        if current is None or watch is None:
            unknown += 1
            continue
        value = overlap_ms(current, watch)
        if value > 0:
            overlapping += 1
        summed += value
        intervals.append(current)
    union = merge_union(intervals)
    return {
        "read_count": len(reads),
        "overlapping_reads": overlapping,
        "unknown_reads": unknown,
        "summed_overlap_ms": round(summed, 6),
        "union_ms": union,
    }


def sufficient(summary, required_ms):
    """Structural exposure gate used only to label the accepted criterion."""
    if required_ms is None:
        return summary["overlapping_reads"] > 0
    return summary["overlapping_reads"] > 0 and summary["union_ms"] >= required_ms
