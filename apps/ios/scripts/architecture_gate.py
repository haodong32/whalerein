#!/usr/bin/env python3
"""Deterministic architecture gate for the rewritten WhaleRein iOS client.

This script is standard-library only and runs on Linux, so CI can enforce the
design rules without Xcode. It scans the Swift product tree and reports
per-file violations of the architecture in
``openspec/changes/rewrite-ios-client-architecture/design.md``.

Rules
-----
``feature-boundary``
    A Swift file inside ``Features/<A>/`` must not reference a type declared
    inside ``Features/<B>/`` where ``B != A``. The gate builds an in-memory
    index of every type declared under each feature (struct/class/enum/actor/
    protocol/typealias, including nested and extension-independent
    declarations). A reference is a word-boundary occurrence in code with
    comments and string literals stripped. A name declared by both features is
    treated as owned by ``A`` and does not trigger the rule.

``view-service-reference``
    A file under a ``Views/`` directory, or a ``*View.swift`` file under
    ``Features/``, must not reference a service protocol. Service protocols are
    protocol declarations whose name ends in ``Service``, ``Store`` or
    ``Providing``. Concrete classes with those suffixes (for example
    ``AppConfigurationStore``) are not protocols and are not flagged; the
    design rule is about the protocol boundary, not naming style.

``view-cast``
    The same view files must not contain an ``as?`` cast. ``as!`` is not
    checked because the design rule is specifically about optional probes.

``environment-appmodel``
    No Swift file may use ``@Environment(AppModel.self)``, ``@Environment(AppShellModel.self)`` or
    ``@Environment(\\.appModel)``.

``legacy-ui-flag``
    No string literal may start with ``-ui-`` except the exact
    ``-ui-scenario`` flag (or its ``-ui-scenario=<id>`` form). The check reads
    string literal contents, not comments.

``view-size`` / ``model-size``
    A file whose name ends in ``View.swift`` may not exceed 300 lines; a file
    whose name ends in ``Model.swift`` may not exceed 500 lines. Line count
    includes blank and comment lines.

``state-single-type``
    A ``Features/<feature>/*State.swift`` file may hold one primary declared
    type. Heuristic: count top-level declarations (struct/class/enum/actor at
    zero indentation, after stripping comments and string literals). Nested
    types are indented and therefore allowed; extensions and typealiases are
    not counted as primary types; a file with two zero-indentation type
    declarations is a violation. This is deliberately coarse: it is meant to
    keep state files navigable, not to parse Swift.

``debug-isolation``
    A file that references a type declared under a ``Debug/`` directory (or
    the fallback names ``ScenarioCatalog`` and ``StateGallery``) must either
    contain ``#if DEBUG`` or live inside ``Debug/``. The check is static and
    file-level: a reference guarded by ``#if DEBUG`` satisfies the rule even
    if other references in the same file are not guarded.

Allowlist
---------
Legacy violations are tolerated by ``architecture_allowlist.json``. The
allowlist is a JSON object with ``schemaVersion``, ``comment``,
``legacy_files`` and ``entries``; each entry is ``{path, rule, reason}``.
Paths are POSIX paths relative to the scan root (``apps/ios/WhaleRein``).

The enforcement model is intentionally simple and shrinking:

* The gate fails on every current violation whose ``(path, rule)`` pair is not
  listed.
* The gate fails when an allowlist entry's path no longer exists on disk, so a
  deleted legacy file cannot keep its entry.
* The gate fails on a stale entry whose violation disappeared, so fixed files
  must be pruned instead of silently staying allowlisted.
* The gate fails when an entry's path is absent from ``legacy_files``.
* ``legacy_files`` is frozen when the allowlist is created (the set of paths
  that already violated at baseline). ``--update-allowlist`` never modifies
  it and refuses to add an entry whose path is not in it. New files created by
  the rewrite therefore cannot be allowlisted; the gate fails until the new
  code conforms. The rewrite only deletes legacy paths, so the list can only
  shrink.
* ``--update-allowlist`` prunes deleted/stale entries and appends entries for
  existing legacy paths that gained a violation. Run it after each rewrite
  phase; a clean rewrite must eventually leave an empty allowlist.

Modes
-----
``python3 apps/ios/scripts/architecture_gate.py``
    Gate mode: exit 0 only when every current violation is allowlisted and the
    allowlist itself is consistent.
``--report``
    Non-failing report: list current violations and per-rule counts, always
    exit 0.
``--json``
    Emit machine-readable JSON in any mode.
``--root PATH``
    Scan another tree (used by tests and by anyone evaluating a fixture).
``--allowlist PATH``
    Override the allowlist location (used by tests).
``--update-allowlist``
    Create the baseline or shrink/grow it within the frozen ``legacy_files``
    boundary. Exits non-zero without writing when a violation belongs to a
    path that is not a legacy path.

Exit codes: 0 success/report, 1 gate failure or refused update, 2 usage or
I/O error.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterable, Iterator, Sequence

SCHEMA_VERSION = 1
VIEW_MAX_LINES = 300
MODEL_MAX_LINES = 500

RULE_FEATURE_BOUNDARY = "feature-boundary"
RULE_VIEW_SERVICE = "view-service-reference"
RULE_VIEW_CAST = "view-cast"
RULE_ENVIRONMENT_APPMODEL = "environment-appmodel"
RULE_LEGACY_UI_FLAG = "legacy-ui-flag"
RULE_VIEW_SIZE = "view-size"
RULE_MODEL_SIZE = "model-size"
RULE_STATE_SINGLE_TYPE = "state-single-type"
RULE_DEBUG_ISOLATION = "debug-isolation"

RULES = (
    RULE_FEATURE_BOUNDARY,
    RULE_VIEW_SERVICE,
    RULE_VIEW_CAST,
    RULE_ENVIRONMENT_APPMODEL,
    RULE_LEGACY_UI_FLAG,
    RULE_VIEW_SIZE,
    RULE_MODEL_SIZE,
    RULE_STATE_SINGLE_TYPE,
    RULE_DEBUG_ISOLATION,
)

SERVICE_SUFFIXES = ("Service", "Store", "Providing")
DEBUG_FALLBACK_NAMES = ("ScenarioCatalog", "StateGallery")

DEFAULT_REASONS = {
    RULE_FEATURE_BOUNDARY: (
        "Legacy feature boundary leak; the rewrite moves this type into Domain "
        "or deletes it with the feature rewrite."
    ),
    RULE_VIEW_SERVICE: (
        "Legacy view reads a service protocol; rewritten views receive typed "
        "state and explicit model parameters instead."
    ),
    RULE_VIEW_CAST: (
        "Legacy view uses an as? probe; replaced by typed state and FeatureSet "
        "capabilities."
    ),
    RULE_ENVIRONMENT_APPMODEL: (
        "Legacy environment injection of AppModel; removed with the aggregator "
        "model in task 6.1."
    ),
    RULE_LEGACY_UI_FLAG: (
        "Legacy -ui-* launch flag; replaced by the single -ui-scenario surface "
        "in task 6.4."
    ),
    RULE_VIEW_SIZE: "Legacy view over the 300-line budget; split or deleted by the rewrite.",
    RULE_MODEL_SIZE: "Legacy model over the 500-line budget; deleted or split by the rewrite.",
    RULE_STATE_SINGLE_TYPE: "Legacy state file with more than one primary type; split by the rewrite.",
    RULE_DEBUG_ISOLATION: "Legacy Debug reference outside #if DEBUG; moved under Debug/ by task 5.x.",
}

ALLOWLIST_COMMENT = (
    "Shrinking legacy allowlist for architecture_gate.py. Paths are relative to "
    "the scan root (apps/ios/WhaleRein). legacy_files is frozen when this file is "
    "created; entries may only use those paths, so new files can never be "
    "allowlisted. Delete entries as the rewrite removes violations: the gate "
    "fails on missing paths and on stale entries."
)

DECLARATION_RE = re.compile(
    r"(?P<kind>struct|class|enum|actor|protocol)\s+"
    r"(?P<name>[A-Za-z_][A-Za-z0-9_]*)\s*(?=[:<{]|\s+where\b)"
)
TYPEALIAS_RE = re.compile(r"typealias\s+([A-Za-z_][A-Za-z0-9_]*)")
CAST_RE = re.compile(r"\bas\s*\?")
ENVIRONMENT_RE = re.compile(
    r"@Environment\s*\(\s*("
    + "|".join(
        re.escape(target) for target in ("AppModel.self", "AppShellModel.self", "\\.appModel")
    )
    + r")\s*\)"
)


@dataclass
class Violation:
    """One permitted-to-allowlist violation, aggregated per file and rule."""

    rule: str
    path: str
    occurrences: list[str] = field(default_factory=list)

    @property
    def key(self) -> tuple[str, str]:
        return self.path, self.rule

    def to_json(self) -> dict[str, object]:
        return {
            "rule": self.rule,
            "path": self.path,
            "occurrences": list(self.occurrences),
        }


@dataclass
class Declaration:
    name: str
    kind: str
    line: int


@dataclass
class SwiftFile:
    path: str
    text: str
    code: str
    literals: list[tuple[str, int]]
    lines: int


def analyze_swift(text: str) -> tuple[str, list[tuple[str, int]]]:
    """Blank comments and string literals while preserving line numbers.

    The returned code text replaces every comment and literal character with a
    space, but keeps newlines, so line numbers in the result match the source.
    String literal values are collected separately for the launch-flag rule.
    Swift nested block comments are handled. Raw strings (``#"..."#``) are not
    interpreted; the product tree does not use them and the surrounding code is
    still scanned.
    """

    code: list[str] = []
    literals: list[tuple[str, int]] = []
    index = 0
    length = len(text)
    line = 1
    block_depth = 0
    line_comment = False

    while index < length:
        if block_depth:
            if text.startswith("/*", index):
                block_depth += 1
                code.append("  ")
                index += 2
                continue
            if text.startswith("*/", index):
                block_depth -= 1
                code.append("  ")
                index += 2
                continue
            character = text[index]
            if character == "\n":
                line += 1
                code.append("\n")
            else:
                code.append(" ")
            index += 1
            continue

        if line_comment:
            character = text[index]
            if character == "\n":
                line_comment = False
                line += 1
                code.append("\n")
            else:
                code.append(" ")
            index += 1
            continue

        if text.startswith("//", index):
            line_comment = True
            code.append("  ")
            index += 2
            continue

        if text.startswith("/*", index):
            block_depth = 1
            code.append("  ")
            index += 2
            continue

        character = text[index]

        if character == '"':
            if text.startswith('"""', index):
                start_line = line
                code.append("   ")
                cursor = index + 3
                value: list[str] = []
                while cursor < length:
                    if text.startswith('"""', cursor):
                        break
                    if text[cursor] == "\\" and cursor + 1 < length:
                        value.append(text[cursor : cursor + 2])
                        cursor += 2
                        continue
                    if text[cursor] == "\n":
                        line += 1
                        code.append("\n")
                    else:
                        code.append(" ")
                    value.append(text[cursor])
                    cursor += 1
                if cursor < length:
                    code.append("   ")
                    cursor += 3
                literals.append(("".join(value), start_line))
                index = cursor
                continue

            start_line = line
            cursor = index + 1
            value = []
            while cursor < length:
                next_character = text[cursor]
                if next_character == "\\" and cursor + 1 < length:
                    value.append(text[cursor : cursor + 2])
                    cursor += 2
                    continue
                if next_character == '"':
                    cursor += 1
                    break
                if next_character == "\n":
                    # Malformed single-line literal: stop before the newline so
                    # the newline itself is still processed on the next pass.
                    break
                value.append(next_character)
                cursor += 1
            literals.append(("".join(value), start_line))
            code.append(" " * (cursor - index))
            index = cursor
            continue

        if character == "\n":
            line += 1
        code.append(character)
        index += 1

    return "".join(code), literals


def declared_types(code: str) -> Iterator[Declaration]:
    """Yield declarations found in comment/string-free Swift code.

    The expression is heuristic: it requires a declaration keyword followed by
    an identifier and a Swift type-declaration continuation. ``class func`` and
    member names do not match.
    """

    for match in DECLARATION_RE.finditer(code):
        line_number = code.count("\n", 0, match.start()) + 1
        yield Declaration(match.group("name"), match.group("kind"), line_number)

    for match in TYPEALIAS_RE.finditer(code):
        line_number = code.count("\n", 0, match.start()) + 1
        yield Declaration(match.group(1), "typealias", line_number)


def declaration_line_indent(code: str, position: int) -> int:
    """Return the whitespace indentation of the line containing ``position``."""

    line_start = code.rfind("\n", 0, position) + 1
    line_end = code.find("\n", position)
    if line_end == -1:
        line_end = len(code)
    line = code[line_start:line_end]
    return len(line) - len(line.lstrip(" \t"))


def swift_files(root: Path) -> list[Path]:
    """Return every Swift file under ``root`` in deterministic path order."""

    return sorted(
        (path for path in root.rglob("*.swift") if path.is_file()),
        key=lambda path: path.relative_to(root).as_posix(),
    )


def relative_path(root: Path, path: Path) -> str:
    return path.relative_to(root).as_posix()


def feature_of(relative: str) -> str | None:
    """Return the feature name for a path under ``Features/<name>/``."""

    parts = relative.split("/")
    for index, part in enumerate(parts[:-2]):
        if part == "Features":
            return parts[index + 1]
    return None


def is_view_path(relative: str) -> bool:
    parts = relative.split("/")
    if "Views" in parts[:-1]:
        return True
    return feature_of(relative) is not None and parts[-1].endswith("View.swift")


def is_debug_path(relative: str) -> bool:
    return "Debug" in relative.split("/")[:-1]


def is_service_protocol(name: str) -> bool:
    return name.endswith(SERVICE_SUFFIXES)


def is_legacy_ui_flag(value: str) -> bool:
    """Return True only for a disallowed ``-ui-`` launch flag literal."""

    if not value.startswith("-ui-"):
        return False
    return value != "-ui-scenario" and not value.startswith("-ui-scenario=")


def build_type_index(
    files: Sequence[SwiftFile],
) -> dict[str, dict[str, tuple[str, Declaration]]]:
    """Build the generated type index: feature -> name -> (declaring file, decl)."""

    feature_index: dict[str, dict[str, tuple[str, Declaration]]] = {}
    for swift_file in files:
        feature = feature_of(swift_file.path)
        if feature is None:
            continue
        feature_index.setdefault(feature, {})
        for declaration in declared_types(swift_file.code):
            feature_index[feature].setdefault(
                declaration.name, (swift_file.path, declaration)
            )
    return feature_index


def scan(root: Path) -> list[Violation]:
    """Scan the tree and return aggregated violations sorted by path and rule."""

    root = root.resolve()
    files: list[SwiftFile] = []
    for path in swift_files(root):
        relative = relative_path(root, path)
        text = path.read_text(encoding="utf-8", errors="replace")
        code, literals = analyze_swift(text)
        files.append(
            SwiftFile(
                path=relative,
                text=text,
                code=code,
                literals=literals,
                lines=len(text.splitlines()),
            )
        )

    violations: dict[tuple[str, str], Violation] = {}

    def add(rule: str, path: str, detail: str) -> None:
        key = (path, rule)
        violation = violations.get(key)
        if violation is None:
            violation = Violation(rule=rule, path=path)
            violations[key] = violation
        violation.occurrences.append(detail)

    feature_index = build_type_index(files)
    service_protocols: dict[str, tuple[str, int]] = {}
    debug_types: dict[str, tuple[str, int]] = {
        name: ("Debug fallback", 0) for name in DEBUG_FALLBACK_NAMES
    }

    for swift_file in files:
        for declaration in declared_types(swift_file.code):
            if declaration.kind == "protocol" and is_service_protocol(
                declaration.name
            ):
                service_protocols.setdefault(
                    declaration.name, (swift_file.path, declaration.line)
                )
            if is_debug_path(swift_file.path):
                debug_types.setdefault(
                    declaration.name, (swift_file.path, declaration.line)
                )

    for swift_file in files:
        relative = swift_file.path
        code = swift_file.code

        # feature-boundary
        own_feature = feature_of(relative)
        if own_feature is not None:
            local_names = feature_index.get(own_feature, {})
            for other_feature, other_types in feature_index.items():
                if other_feature == own_feature:
                    continue
                for name, (declaring_file, declaration) in other_types.items():
                    if name in local_names:
                        continue
                    match = re.search(rf"\b{re.escape(name)}\b", code)
                    if match is None:
                        continue
                    line_number = code.count("\n", 0, match.start()) + 1
                    add(
                        RULE_FEATURE_BOUNDARY,
                        relative,
                        f"line {line_number}: references type {name} declared "
                        f"in Features/{other_feature} "
                        f"({declaring_file}:{declaration.line})",
                    )

        # view-service-reference / view-cast
        if is_view_path(relative):
            for name, (declaring_file, declaring_line) in service_protocols.items():
                match = re.search(rf"\b{re.escape(name)}\b", code)
                if match is not None:
                    line_number = code.count("\n", 0, match.start()) + 1
                    add(
                        RULE_VIEW_SERVICE,
                        relative,
                        f"line {line_number}: references service protocol {name} "
                        f"declared at {declaring_file}:{declaring_line}",
                    )
            for match in CAST_RE.finditer(code):
                line_number = code.count("\n", 0, match.start()) + 1
                add(
                    RULE_VIEW_CAST,
                    relative,
                    f"line {line_number}: uses an as? cast",
                )

        # environment-appmodel
        for match in ENVIRONMENT_RE.finditer(code):
            line_number = code.count("\n", 0, match.start()) + 1
            add(
                RULE_ENVIRONMENT_APPMODEL,
                relative,
                f"line {line_number}: uses {match.group(1)} through the environment",
            )

        # legacy-ui-flag
        for value, literal_line in swift_file.literals:
            stripped_value = value.strip()
            if is_legacy_ui_flag(stripped_value):
                add(
                    RULE_LEGACY_UI_FLAG,
                    relative,
                    f"line {literal_line}: legacy flag literal {stripped_value!r}",
                )

        # file size budgets
        if relative.endswith("View.swift") and swift_file.lines > VIEW_MAX_LINES:
            add(
                RULE_VIEW_SIZE,
                relative,
                f"{swift_file.lines} lines (budget {VIEW_MAX_LINES})",
            )
        if relative.endswith("Model.swift") and swift_file.lines > MODEL_MAX_LINES:
            add(
                RULE_MODEL_SIZE,
                relative,
                f"{swift_file.lines} lines (budget {MODEL_MAX_LINES})",
            )

        # state-single-type
        if own_feature is not None and relative.endswith("State.swift"):
            primary = [
                declaration
                for declaration in declared_types(code)
                if declaration.kind in ("struct", "class", "enum", "actor")
                and declaration_line_indent(
                    code, _declaration_position(code, declaration)
                )
                == 0
            ]
            if len(primary) > 1:
                names = ", ".join(declaration.name for declaration in primary)
                add(
                    RULE_STATE_SINGLE_TYPE,
                    relative,
                    f"{len(primary)} primary declared types: {names}",
                )

        # debug-isolation
        if not is_debug_path(relative) and "#if DEBUG" not in code:
            for name, (declaring_file, declaring_line) in debug_types.items():
                match = re.search(rf"\b{re.escape(name)}\b", code)
                if match is not None:
                    line_number = code.count("\n", 0, match.start()) + 1
                    add(
                        RULE_DEBUG_ISOLATION,
                        relative,
                        f"line {line_number}: references Debug type {name} "
                        f"({declaring_file}:{declaring_line}) outside #if DEBUG",
                    )

    return sorted(violations.values(), key=lambda violation: violation.key)


def _declaration_position(code: str, declaration: Declaration) -> int:
    """Find the source position of a declaration on its line.

    The declaration line number is known, so search that line. If the name
    appears more than once, the first match is used; the indentation test then
    treats the line as top-level only when it starts at column zero.
    """

    target_line = declaration.line
    offset = 0
    for _ in range(target_line - 1):
        offset = code.find("\n", offset) + 1
    line_end = code.find("\n", offset)
    if line_end == -1:
        line_end = len(code)
    match = re.search(
        rf"\b{re.escape(declaration.name)}\b", code[offset:line_end]
    )
    if match is None:
        return offset
    return offset + match.start()


def summary_for(violations: Sequence[Violation]) -> dict[str, int]:
    summary = {rule: 0 for rule in RULES}
    for violation in violations:
        summary[violation.rule] = summary.get(violation.rule, 0) + 1
    return summary


def load_allowlist(
    path: Path,
) -> tuple[set[str], dict[tuple[str, str], dict[str, str]], list[dict[str, str]]]:
    """Load the allowlist and return legacy files, entries and schema problems."""

    problems: list[dict[str, str]] = []
    if not path.is_file():
        return set(), {}, problems

    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        return set(), {}, [
            {"kind": "schema", "detail": f"cannot read allowlist: {error}"}
        ]

    if not isinstance(payload, dict):
        return set(), {}, [
            {"kind": "schema", "detail": "allowlist root must be a JSON object"}
        ]

    if payload.get("schemaVersion") != SCHEMA_VERSION:
        problems.append(
            {
                "kind": "schema",
                "detail": f"unsupported schemaVersion {payload.get('schemaVersion')!r}; "
                f"expected {SCHEMA_VERSION}",
            }
        )

    legacy_payload = payload.get("legacy_files")
    if not isinstance(legacy_payload, list) or not all(
        isinstance(item, str) for item in legacy_payload
    ):
        problems.append(
            {
                "kind": "schema",
                "detail": "legacy_files must be an array of path strings",
            }
        )
        legacy_payload = []

    entries_payload = payload.get("entries")
    if not isinstance(entries_payload, list):
        problems.append({"kind": "schema", "detail": "entries must be an array"})
        entries_payload = []

    legacy_files = set(legacy_payload)
    entries: dict[tuple[str, str], dict[str, str]] = {}
    for index, entry in enumerate(entries_payload):
        if not isinstance(entry, dict):
            problems.append(
                {"kind": "schema", "detail": f"entry {index} must be an object"}
            )
            continue
        entry_path = entry.get("path")
        rule = entry.get("rule")
        reason = entry.get("reason")
        if not isinstance(entry_path, str) or not entry_path:
            problems.append(
                {"kind": "schema", "detail": f"entry {index} has no path string"}
            )
            continue
        if not isinstance(rule, str) or not rule:
            problems.append(
                {"kind": "schema", "detail": f"entry {index} has no rule string"}
            )
            continue
        if rule not in RULES:
            problems.append(
                {
                    "kind": "schema",
                    "path": entry_path,
                    "rule": rule,
                    "detail": f"entry {index} uses an unknown rule",
                }
            )
            continue
        if not isinstance(reason, str) or not reason.strip():
            problems.append(
                {
                    "kind": "schema",
                    "path": entry_path,
                    "rule": rule,
                    "detail": f"entry {index} has no reason",
                }
            )
            continue
        key = (entry_path, rule)
        if key in entries:
            problems.append(
                {
                    "kind": "schema",
                    "path": entry_path,
                    "rule": rule,
                    "detail": f"duplicate entry {index}",
                }
            )
            continue
        entries[key] = {"path": entry_path, "rule": rule, "reason": reason}

    return legacy_files, entries, problems


def check_allowlist(
    root: Path,
    violations: Sequence[Violation],
    legacy_files: set[str],
    entries: dict[tuple[str, str], dict[str, str]],
    schema_problems: Sequence[dict[str, str]],
) -> tuple[list[Violation], list[dict[str, str]]]:
    """Return unlisted violations and allowlist consistency problems."""

    current_keys = {violation.key for violation in violations}
    unlisted = [violation for violation in violations if violation.key not in entries]
    problems = list(schema_problems)

    for path, rule in sorted(entries):
        entry = entries[(path, rule)]
        if path not in legacy_files:
            problems.append(
                {
                    "kind": "invalid-entry",
                    "path": path,
                    "rule": rule,
                    "detail": "allowlist entry path is not in the frozen legacy_files",
                }
            )
            continue
        target = root / path
        if not target.is_file():
            problems.append(
                {
                    "kind": "missing-path",
                    "path": path,
                    "rule": rule,
                    "detail": "allowlisted path no longer exists; prune this entry",
                }
            )
            continue
        if (path, rule) not in current_keys:
            problems.append(
                {
                    "kind": "stale-entry",
                    "path": path,
                    "rule": rule,
                    "detail": "the violation is gone; prune this entry",
                }
            )

    return unlisted, problems


def write_allowlist(
    path: Path,
    legacy_files: set[str],
    entries: dict[tuple[str, str], dict[str, str]],
) -> None:
    payload = {
        "schemaVersion": SCHEMA_VERSION,
        "comment": ALLOWLIST_COMMENT,
        "legacy_files": sorted(legacy_files),
        "entries": [
            {
                "path": entries[key]["path"],
                "rule": entries[key]["rule"],
                "reason": entries[key]["reason"],
            }
            for key in sorted(entries)
        ],
    }
    path.write_text(
        json.dumps(payload, indent=2, ensure_ascii=True) + "\n",
        encoding="utf-8",
    )


def update_allowlist(
    root: Path,
    path: Path,
    violations: Sequence[Violation],
) -> tuple[int, dict[str, object], str]:
    """Apply the shrinking update and return exit code, payload and message."""

    legacy_files, entries, schema_problems = load_allowlist(path)
    current_keys = {violation.key for violation in violations}

    if schema_problems:
        message = "Refusing to update: " + "; ".join(
            problem["detail"] for problem in schema_problems
        )
        return 2, {"ok": False, "allowlistProblems": schema_problems}, message

    created = not path.is_file()
    if created:
        legacy_files = {violation.path for violation in violations}

    # Prune entries for deleted files and for violations that no longer exist.
    kept: dict[tuple[str, str], dict[str, str]] = {}
    for key, entry in entries.items():
        file_path = root / entry["path"]
        if not file_path.is_file():
            continue
        if key not in current_keys:
            continue
        kept[key] = entry

    refused = [
        violation
        for violation in violations
        if violation.key not in entries and violation.path not in legacy_files
    ]
    if refused:
        details = ", ".join(f"{v.path} ({v.rule})" for v in refused)
        message = (
            "Refusing to allowlist paths that are not in the frozen legacy_files: "
            f"{details}. New or rewritten files must conform to the architecture."
        )
        return 1, {"ok": False, "refused": [v.to_json() for v in refused]}, message

    for violation in violations:
        if violation.key in kept:
            continue
        if violation.key in entries:
            # Existing legacy entry whose violation is still present.
            kept[violation.key] = entries[violation.key]
            continue
        kept[violation.key] = {
            "path": violation.path,
            "rule": violation.rule,
            "reason": DEFAULT_REASONS.get(
                violation.rule, "Legacy violation tracked until the rewrite removes it."
            ),
        }

    write_allowlist(path, legacy_files, kept)
    payload: dict[str, object] = {
        "ok": True,
        "created": created,
        "allowlisted": len(kept),
        "legacyFiles": len(legacy_files),
        "allowlist": str(path),
    }
    return 0, payload, f"Updated {path} with {len(kept)} allowlist entries."


def gate_payload(
    root: Path,
    allowlist_path: Path,
    violations: Sequence[Violation],
    unlisted: Sequence[Violation],
    problems: Sequence[dict[str, str]],
    allowlisted: int,
    mode: str,
) -> dict[str, object]:
    return {
        "schemaVersion": SCHEMA_VERSION,
        "mode": mode,
        "root": str(root),
        "allowlist": str(allowlist_path),
        "ok": not unlisted and not problems,
        "summary": summary_for(violations),
        "violations": [violation.to_json() for violation in violations],
        "unlisted": [violation.to_json() for violation in unlisted],
        "allowlistProblems": list(problems),
        "allowlistedViolations": allowlisted,
    }


def format_human_report(
    root: Path, violations: Sequence[Violation], mode: str
) -> str:
    summary = summary_for(violations)
    lines = [
        f"Architecture gate report ({mode})",
        f"root: {root}",
        f"violations: {len(violations)}",
    ]
    for rule in RULES:
        lines.append(f"  {rule}: {summary.get(rule, 0)}")
    if not violations:
        lines.append("No violations found.")
    else:
        lines.append("Violating files:")
        for violation in violations:
            lines.append(f"  [{violation.rule}] {violation.path}")
            for occurrence in violation.occurrences:
                lines.append(f"    {occurrence}")
    return "\n".join(lines)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Deterministic architecture gate for the WhaleRein iOS client."
    )
    parser.add_argument(
        "--root",
        help="Swift product tree to scan (default: apps/ios/WhaleRein next to this script).",
    )
    parser.add_argument(
        "--allowlist",
        help="allowlist JSON path (default: architecture_allowlist.json next to this script).",
    )
    parser.add_argument(
        "--report",
        action="store_true",
        help="report current violations without failing.",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="emit machine-readable JSON instead of human-readable text.",
    )
    parser.add_argument(
        "--update-allowlist",
        action="store_true",
        help="create the baseline or shrink/grow it within the frozen legacy files.",
    )
    args = parser.parse_args(argv)

    if args.report and args.update_allowlist:
        parser.error("--report and --update-allowlist cannot be combined")

    script_dir = Path(__file__).resolve().parent
    root = (
        Path(args.root).resolve()
        if args.root
        else (script_dir.parent / "WhaleRein").resolve()
    )
    allowlist_path = (
        Path(args.allowlist).resolve()
        if args.allowlist
        else (script_dir / "architecture_allowlist.json").resolve()
    )

    if not root.is_dir():
        print(f"error: scan root does not exist: {root}", file=sys.stderr)
        return 2

    try:
        violations = scan(root)
    except OSError as error:
        print(f"error: cannot scan {root}: {error}", file=sys.stderr)
        return 2

    if args.report:
        payload = {
            "schemaVersion": SCHEMA_VERSION,
            "mode": "report",
            "root": str(root),
            "ok": True,
            "summary": summary_for(violations),
            "violations": [violation.to_json() for violation in violations],
        }
        if args.json:
            print(json.dumps(payload, indent=2, sort_keys=True))
        else:
            print(format_human_report(root, violations, "non-failing"))
        return 0

    if args.update_allowlist:
        exit_code, update_payload, message = update_allowlist(root, allowlist_path, violations)
        if args.json:
            print(json.dumps(update_payload, indent=2, sort_keys=True))
        else:
            print(message)
        return exit_code

    legacy_files, entries, schema_problems = load_allowlist(allowlist_path)
    unlisted, problems = check_allowlist(
        root, violations, legacy_files, entries, schema_problems
    )
    payload = gate_payload(
        root,
        allowlist_path,
        violations,
        unlisted,
        problems,
        len(entries),
        "gate",
    )

    if args.json:
        print(json.dumps(payload, indent=2, sort_keys=True))
    else:
        if payload["ok"]:
            print("Architecture gate: OK")
            print(f"root: {root}")
            print(f"allowlisted legacy violations: {len(entries)}")
        else:
            print("Architecture gate: FAIL")
            print(f"root: {root}")
            print(f"violations: {len(violations)} (unlisted: {len(unlisted)})")
            if unlisted:
                print("Unlisted violations:")
                for violation in unlisted:
                    print(f"  [{violation.rule}] {violation.path}")
                    for occurrence in violation.occurrences:
                        print(f"    {occurrence}")
            if problems:
                print("Allowlist problems:")
                for problem in problems:
                    location = ""
                    if "path" in problem:
                        location = f" {problem['path']}"
                        if "rule" in problem:
                            location += f" [{problem['rule']}]"
                    print(f"  [{problem['kind']}]{location}: {problem['detail']}")
            print(
                "Fix new code, or run --update-allowlist for existing legacy paths "
                "(new files can never be allowlisted)."
            )

    return 0 if payload["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
