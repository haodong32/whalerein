"""Deterministic module grouping and factual report summaries."""

from collections import Counter


def summarize(scope):
    tests = scope["tests"]
    for case in tests:
        case.setdefault("kind", "unclassified")
        if scope.get("language") == "go":
            case["node_role"] = "case"
            package = case["id"].split("::")[0]
            case["submodule"] = (
                package.split("/internal/")[-1] if "/internal/" in package else package
            )
        else:
            case.setdefault("node_role", "case")
            case.setdefault("submodule", case.get("source", "").rsplit("/", 1)[0] or "General")
    cases = [c for c in tests if c["node_role"] == "case"]
    counts = Counter(c["status"] for c in cases)
    scope["summary"] = {
        "cases": len(cases),
        "containers": sum(c["node_role"] == "container" for c in tests),
        "steps": sum(c["node_role"] == "step" for c in tests),
        "statuses": dict(sorted(counts.items())),
        "authored_descriptions": sum(
            c.get("description_source") in ("behavior", "docstring") for c in cases
        ),
        "name_descriptions": sum(c.get("description_source") == "native_name" for c in cases),
        "missing_descriptions": sum(not c.get("description") for c in cases),
    }
    return scope


def totals(scopes):
    result = {"cases": 0, "containers": 0, "steps": 0, "statuses": {}}
    counts = Counter()
    for scope in scopes:
        summary = summarize(scope)["summary"]
        for key in ("cases", "containers", "steps"):
            result[key] += summary[key]
        counts.update(summary["statuses"])
    result["statuses"] = dict(sorted(counts.items()))
    return result
