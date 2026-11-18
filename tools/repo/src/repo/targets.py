"""Public command scopes shared by help, validation, and execution."""

from dataclasses import dataclass

from .errors import UsageError
from .integration_targets import LEAVES
from .projects import Project


@dataclass(frozen=True)
class Target:
    name: str
    kind: str
    scope: str
    leaf: bool = True
    project: Project | None = None


INTEGRATION = (
    Target("integration", "integration", "all Linux integration leaves", leaf=False),
    Target("protocol", "integration", "protocol-vectors and protocol-http", leaf=False),
    *(
        Target(name, "integration", f"{len(leaf.paths)} pytest modules")
        for name, leaf in LEAVES.items()
    ),
)
IOS = (
    Target("ios-unit", "ios", "iOS XCTest unit plan"),
    Target("ios-ui", "ios", "iOS XCTest UI plan"),
    Target("ios-ui-smoke", "ios", "six CI UI smoke cases", leaf=False),
    Target("ios-integration", "ios", "iOS stack-backed integration plan"),
)
PROTO = Target("proto", "proto", "generated Go/JS binding consistency")


def project_target_name(action, project_id):
    if project_id == "proto" and action in ("check", "doctor"):
        return "proto-rust"
    return project_id


def available(action, projects):
    result = {}
    for project in projects:
        if action in project.capabilities or action == "doctor":
            name = project_target_name(action, project.id)
            result[name] = Target(name, "unit", f"{project.kind} · {project.rel}", project=project)
    if action in ("test", "doctor"):
        result.update((target.name, target) for target in (*INTEGRATION, *IOS))
    if action in ("check", "doctor"):
        result["proto"] = PROTO
    return result


def resolve(action, name, projects):
    if name is None:
        return None
    try:
        return available(action, projects)[name]
    except KeyError:
        raise UsageError(
            f"unknown target for {action}: {name}; see ./repo {action} --help"
        ) from None


def describe(action, projects):
    from .doctor import specs_for_target

    lines = []
    for target in available(action, projects).values():
        tools = ", ".join(dict.fromkeys(spec.name for spec in specs_for_target(target)))
        lines.append(f"  {target.name:<16} {target.scope} ({tools})")
    return "Targets:\n" + "\n".join(lines) + "\n\n" if lines else ""
