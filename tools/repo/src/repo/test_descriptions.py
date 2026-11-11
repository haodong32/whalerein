"""Readable test descriptions with explicit provenance and missing-detail flags."""

import re

from .output import redact


def describe(case):
    """Preserve authored guarantees; never present name expansion as authored intent."""
    if case.get("description_zh"):
        case["description_zh"] = redact(case["description_zh"])
    behavior = case.get("behavior", {})
    if behavior.get("ensures"):
        case["description"] = redact(behavior["ensures"])
        case["description_source"] = "behavior"
    elif case.get("description") and case.get("description_source") == "docstring":
        case["description"] = redact(case["description"])
    else:
        # Parameter payloads can contain qualified IDs of their own. Only the
        # native identity before the first parameter bracket contains scope separators.
        identity = case["id"]
        native_id, separator, parameters = identity.partition("[")
        name = case.get("name") or native_id.rsplit("::", 1)[-1] + separator + parameters
        name = re.sub(r"^(?:test_|Test(?=[A-Z])|test(?=[A-Z]))", "", name)
        name = re.sub(r"([a-z0-9])([A-Z])", r"\1 \2", name)
        name = re.sub(r"([A-Z])([A-Z][a-z])", r"\1 \2", name)
        case["description"] = redact(name.replace("_", " ").strip() or case["id"])
        case["description_source"] = "native_name"
    case["description_needs_review"] = case["description_source"] == "native_name"
    return case
