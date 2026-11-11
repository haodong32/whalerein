"""Native coverage measurements and parsers."""

import json
import re
from dataclasses import dataclass
from typing import Any

THRESHOLD = 90.0


_GO_LINE = re.compile(
    r"^(?P<file>.+):(?P<sl>\d+)\.(?P<sc>\d+),(?P<el>\d+)\.(?P<ec>\d+) "
    r"(?P<stmts>\d+) (?P<count>\d+)\s*$"
)


@dataclass
class CoverageResult:
    name: str
    kind: str
    path: str
    covered: int = 0
    total: int = 0
    percent: float | None = None
    threshold: float = THRESHOLD
    error: str | None = None
    note: str = ""

    @property
    def ok(self) -> bool:
        if self.error:
            return False
        if self.percent is None:
            return True
        return self.percent + 1e-09 >= self.threshold

    def as_dict(self) -> dict[str, Any]:
        data: dict[str, Any] = {
            "name": self.name,
            "kind": self.kind,
            "path": self.path,
            "covered": self.covered,
            "total": self.total,
            "percent": self.percent,
            "threshold": self.threshold,
            "ok": self.ok,
        }
        if self.note:
            data["note"] = self.note
        if self.error:
            data["error"] = self.error
        return data


def extract_json(text: str) -> Any:
    """Parse the first JSON object/array in mixed tool output."""
    text = text.strip()
    if not text:
        raise ValueError("empty output")
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        pass
    start_candidates = [index for index in (text.find("{"), text.find("[")) if index >= 0]
    if not start_candidates:
        raise ValueError("no JSON value in output")
    start = min(start_candidates)
    decoder = json.JSONDecoder()
    try:
        value, _ = decoder.raw_decode(text[start:])
        return value
    except json.JSONDecodeError as exc:
        raise ValueError(f"invalid JSON in tool output: {exc}") from exc


def parse_go_coverprofile(text: str) -> dict[str, dict[str, int]]:
    """Aggregate a Go coverprofile into per-package covered/total statement counts."""
    packages: dict[str, dict[str, int]] = {}
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("mode:"):
            continue
        match = _GO_LINE.match(line)
        if match is None:
            continue
        filename = match.group("file")
        package = filename.rsplit("/", 1)[0] if "/" in filename else filename
        statements = int(match.group("stmts"))
        count = int(match.group("count"))
        entry = packages.setdefault(package, {"covered": 0, "total": 0})
        entry["total"] += statements
        if count > 0:
            entry["covered"] += statements
    return packages


def parse_llvm_cov_json(data: Any) -> tuple[int, int, float]:
    """Return (covered, total, percent) from ``cargo llvm-cov --json``."""
    if not isinstance(data, dict):
        raise ValueError("llvm-cov output is not an object")
    entries = data.get("data")
    if not isinstance(entries, list) or not entries:
        raise ValueError("llvm-cov output has no data entries")
    totals = entries[0].get("totals", {})
    lines = totals.get("lines", {})
    covered = int(lines.get("covered", 0))
    count = int(lines.get("count", 0))
    percent = lines.get("percent")
    if percent is None:
        percent = covered / count * 100.0 if count else 100.0
    return (covered, count, float(percent))
