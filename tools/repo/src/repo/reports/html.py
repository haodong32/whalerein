"""Standalone offline HTML rendering of validated test report facts."""

import html
import json
from pathlib import Path
from string import Template

from .metrics import summarize, totals
from .model import validate


def duration_label(value):
    if not isinstance(value, (int, float)):
        return "—"
    return f"{value:.1f}ms" if value < 1000 else f"{value / 1000:.2f}s"


def render(data):
    """Render supplied facts through packaged daisyUI templates, without collection."""
    validate(data)
    root = Path(__file__).parent
    assets = root / "assets"

    def fill(name, **values):
        return Template((root / "templates" / name).read_text()).substitute(values)

    def esc(value):
        return html.escape(str(value), quote=True)

    labels = {
        "complete": "Complete",
        "incomplete": "Incomplete",
        "unknown": "Unknown",
        "not_collected": "Not collected",
        "passed": "Passed",
        "failed": "Failed",
        "skipped": "Skipped",
        "blocked": "Blocked",
        "not_run": "Not run",
        "interrupted": "Interrupted",
        "todo": "Todo",
        "cancelled": "Cancelled",
    }
    classes = {
        "passed": "badge-success",
        "failed": "badge-error",
        "blocked": "badge-warning",
        "interrupted": "badge-warning",
    }
    groups = {}
    for original in data["scopes"]:
        scope = {**original, "tests": [dict(c) for c in original["tests"]]}
        name = scope.get("module", scope.get("project", "Unknown"))
        group = groups.setdefault(
            name,
            {
                "project": name,
                "module": name,
                "tests": [],
                "coverage": [],
                "completeness": "complete",
                "notes": [],
                "durations": [],
            },
        )
        group["tests"].extend(scope["tests"])
        group["coverage"].extend(scope.get("coverage", []))
        group["notes"].append(scope.get("note", ""))
        group["notes"].extend(scope.get("errors", []))
        group.setdefault("errors", []).extend(scope.get("errors", []))
        group["notes"].extend(
            f"{check['status']}: {check['name']} — {check.get('detail', '')}"
            for check in scope.get("checks", [])
        )
        group["durations"].append(scope.get("duration_ms"))
        if scope.get("completeness") != "complete":
            group["completeness"] = scope.get("completeness", "incomplete")
    sections, summaries, navigation, options = [], [], [], []
    attention = []
    checks = []
    priority = {
        s: i
        for i, s in enumerate(
            (
                "failed",
                "interrupted",
                "cancelled",
                "blocked",
                "skipped",
                "todo",
                "not_run",
                "passed",
            )
        )
    }

    def coverage_value(entry):
        if entry.get("error"):
            return "Measurement failed"
        percent = entry.get("percent")
        return (
            "Not measured"
            if percent is None
            else f"{percent:.2f}% {entry.get('unit', 'unknown unit')}"
        )

    for index, (name, group) in enumerate(groups.items()):
        summarize(group)
        summary = group["summary"]
        anchor = f"module-{index}"
        issue_counts = group["summary"]["statuses"]
        failures = issue_counts.get("failed", 0) + issue_counts.get("interrupted", 0)
        suffix = f" · {failures} failed/interrupted" if failures else ""
        navigation.append(f'<li><a href="#{anchor}">{esc(name + suffix)}</a></li>')
        options.append(f'<option value="{esc(name)}">{esc(name)}</option>')
        counts = summary["statuses"]
        coverage = group["coverage"]
        measured = sum(c.get("percent") is not None and not c.get("error") for c in coverage)
        cov_label = f"{measured}/{len(coverage)} packages measured" if coverage else "Not measured"
        if len(coverage) == 1:
            cov_label = coverage_value(coverage[0])
        elif measured:
            values = [
                c["percent"]
                for c in coverage
                if c.get("percent") is not None and not c.get("error")
            ]
            cov_label = (
                f"{min(values):.2f}–{max(values):.2f}% ({measured}/{len(coverage)} packages)"
            )
        outcome = (
            " · ".join(
                f"{counts.get(key, 0)} {label}"
                for key, label in (
                    ("failed", "failed"),
                    ("skipped", "skipped"),
                    ("blocked", "blocked"),
                )
                if counts.get(key, 0)
            )
            or f"{counts.get('passed', 0)} passed"
        )
        if not group["tests"]:
            outcome = "No results"
        cells = [
            f'<a class="link" href="#{anchor}">{esc(name)}</a>',
            str(summary["cases"]),
            esc(outcome),
            esc(cov_label),
            labels[group["completeness"]],
        ]
        summaries.append("<tr>" + "".join("<td>" + cell + "</td>" for cell in cells) + "</tr>")
        coverage_rows = (
            "".join(
                "<tr><td>"
                + esc(c.get("name", "Unknown"))
                + "</td><td>"
                + esc(coverage_value(c))
                + "</td><td>"
                + esc(str(c.get("threshold", "Not set")))
                + "</td><td>"
                + esc(c.get("measurement_scope", "Not recorded"))
                + "</td></tr>"
                for c in coverage
            )
            or '<tr><td colspan="4">Not measured</td></tr>'
        )
        rows, passed_rows = [], []
        for error in group.get("errors", []):
            checks.append((0, f"<li>{esc(name)}: {esc(error)}</li>"))
        if group["completeness"] != "complete":
            checks.append(
                (
                    3,
                    f'<li><a class="link" href="#{anchor}">{esc(name)}: '
                    f"collection {esc(labels[group['completeness']])}</a></li>",
                )
            )
        if any(c.get("error") for c in coverage):
            checks.append(
                (
                    0,
                    f'<li><a class="link" href="#{anchor}">'
                    f"{esc(name)}: coverage measurement failed</a></li>",
                )
            )
        if any(
            c.get("percent") is not None and not c.get("ok", True) and not c.get("error")
            for c in coverage
        ):
            checks.append(
                (
                    1,
                    f'<li><a class="link" href="#{anchor}">'
                    f"{esc(name)}: coverage threshold not met</a></li>",
                )
            )
        ordered = sorted(group["tests"], key=lambda c: (priority.get(c["status"], 9), c["id"]))
        for case_index, case in enumerate(ordered):
            case_anchor = f"{anchor}-case-{case_index}"
            diagnostics = case.get("diagnostics", [])
            diagnostic = "\n\n".join(
                f"[{item.get('phase', 'test')}]\n{item.get('message', '')}" for item in diagnostics
            )
            reason = case.get("reason") or case.get("skip_reason") or diagnostic
            if not reason:
                reason = (
                    "Reason not captured"
                    if case["status"] in ("skipped", "blocked", "todo")
                    else "Diagnostic not captured"
                )
            if case["status"] in (
                "failed",
                "interrupted",
                "cancelled",
                "blocked",
                "skipped",
                "todo",
            ):
                attention.append(
                    (
                        priority[case["status"]],
                        f'<li class="issue-row"><a class="link" href="#{case_anchor}" '
                        f'data-case-link="{case_anchor}">'
                        f'<span class="badge {classes.get(case["status"], "")}">'
                        f"{labels[case['status']]}</span> "
                        f'<span class="issue-module">{esc(name)}</span><br>'
                        f"{esc(case.get('description') or case['id'])}</a>"
                        f'<p class="metadata">{esc(reason.splitlines()[0][:240])}</p></li>',
                    )
                )
            target = passed_rows if case["status"] == "passed" else rows
            target.append(
                fill(
                    "test-report-case.html",
                    case_anchor=case_anchor,
                    duration=duration_label(case.get("duration_ms"))
                    + (" (partial)" if case.get("duration_complete") is False else ""),
                    duration_value=esc(case.get("duration_ms", -1)),
                    duration_order=case_index,
                    phases=esc(
                        " · ".join(
                            f"{phase}: {duration_label(value)}"
                            for phase, value in case.get("phase_durations_ms", {}).items()
                        )
                    ),
                    reason=esc(reason) if case["status"] != "passed" else "",
                    module=esc(name),
                    raw_status=esc(case["status"]),
                    description=esc(case.get("description") or "No description provided"),
                    id=esc(case["id"]),
                    source=esc(case.get("source", "")),
                    status=labels[case["status"]],
                    status_class=classes.get(case["status"], ""),
                    submodule=esc(case.get("submodule", "Unclassified")),
                    kind=esc(case.get("kind", "unclassified")),
                    role=esc(case.get("node_role", "case")),
                    description_source=esc(case.get("description_source", "Not provided")),
                )
            )
        sections.append(
            fill(
                "test-report-scope.html",
                anchor=anchor,
                expanded="open"
                if any(
                    c["status"] in ("failed", "blocked", "skipped", "interrupted", "cancelled")
                    for c in group["tests"]
                )
                else "",
                outcome=esc(outcome),
                coverage_summary=esc(cov_label),
                project=esc(name),
                count=summary["cases"],
                containers=summary["containers"],
                steps=summary["steps"],
                completeness=labels[group["completeness"]],
                warning=(
                    '<div role="note" class="alert alert-warning">Collection is incomplete. '
                    "Missing tests must not be interpreted as removals.</div>"
                    if group["completeness"] != "complete"
                    else ""
                ),
                authored=summary["authored_descriptions"],
                inferred=summary["name_descriptions"],
                missing=summary["missing_descriptions"],
                duration=(
                    f"{sum(group['durations']) / 1000:.2f}s project wall time"
                    if all(isinstance(d, (float, int)) for d in group["durations"])
                    else "Not measured"
                ),
                coverage_rows=coverage_rows,
                note=esc("\n".join(group["notes"])),
                rows="".join(rows),
                passed_rows="".join(passed_rows),
                passed_count=len(passed_rows),
            )
        )
    summary = totals(list(groups.values()))
    status = summary["statuses"]
    metrics = [
        (
            "Test cases" if data.get("completeness") == "complete" else "Known test cases",
            summary["cases"],
            "Unique native cases and declared scenarios",
        ),
        ("Passed", status.get("passed", 0), "Observed outcomes"),
        ("Failed", status.get("failed", 0), "Observed outcomes"),
        (
            "Skipped / Blocked",
            status.get("skipped", 0) + status.get("blocked", 0),
            "Separate from passing",
        ),
        (
            "Integration cases",
            sum(len(s["tests"]) for s in data["scopes"] if s.get("kind") == "integration"),
            "Included in total",
        ),
        (
            "Unit cases",
            sum(
                1
                for scope in data["scopes"]
                if scope.get("kind") == "unit"
                or scope.get("language") in {"rust", "go", "node", "python"}
                for case in scope["tests"]
                if case.get("node_role", "case") == "case"
            ),
            "Included in total",
        ),
    ]
    stat_filters = {"Passed": "passed", "Failed": "failed", "Skipped / Blocked": "attention"}
    return fill(
        "test-report.html",
        css=(assets / "daisyui.css").read_text(),
        generated=esc(data.get("generated_at", "Not recorded")),
        duration=duration_label(data.get("duration_ms"))
        + (" (partial)" if data.get("duration_complete") is False else ""),
        duration_phases=esc(
            " · ".join(
                f"{phase}: {duration_label(value)}"
                for phase, value in data.get("phase_durations_ms", {}).items()
            )
        ),
        configuration=esc(json.dumps(data.get("configuration", {}), sort_keys=True)),
        completeness=labels.get(data.get("completeness"), "Unknown"),
        attention="".join(item for _, item in sorted(attention, key=lambda pair: pair[0]))
        or "<li>No failures, skips, collection gaps or coverage errors recorded.</li>",
        checks="".join(item for _, item in sorted(checks))
        or "<li>No collection gaps recorded.</li>",
        navigation="".join(navigation),
        summary_rows="".join(summaries),
        sections="".join(sections),
        module_options="".join(options),
        status_options="".join(
            f'<option value="{key}">{value}</option>'
            for key, value in labels.items()
            if key not in ("complete", "incomplete", "unknown", "not_collected")
        ),
        stats="".join(
            fill(
                "test-report-stat.html",
                title=t,
                value=v,
                detail=d,
                actions=(
                    '<div class="stat-actions"><button type="button" '
                    'class="btn btn-soft btn-xs" '
                    f'data-filter="{stat_filters.get(t, "all")}" '
                    f'aria-label="Filter {esc(t)}">View tests</button></div>'
                    if t not in ("Unit cases", "Integration cases")
                    else ""
                ),
            )
            for t, v, d in metrics
        ),
        report_scope=esc(data.get("scope", "partial")),
        license=esc((assets / "daisyui.LICENSE").read_text()),
    )
