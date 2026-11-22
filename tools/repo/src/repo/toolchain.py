"""Sequential cross-project engineering operations."""

from dataclasses import dataclass
from pathlib import Path

from .errors import RepoError
from .runners import go, kotlin, node, python, rust

RUNNERS = {"rust": rust, "go": go, "node": node, "python": python, "kotlin": kotlin}


@dataclass(frozen=True)
class Step:
    label: str
    argv: tuple[str, ...]
    cwd: Path
    fail_on_output: bool = False


def run(projects, runner, action, explicit=False):
    plans = []
    skipped = []
    for project in projects:
        if action not in project.capabilities:
            if explicit:
                raise RepoError(f"{project.id} does not support {action}")
            skipped.append({"project": project.id, "reason": "not applicable"})
            runner.out.info(f"{project.id}: {action} not applicable")
            continue
        plans.extend(RUNNERS[project.kind].steps(project, action))
    results = []
    blocked = False
    for step in plans:
        runner.out.info(step.label)
        result = runner.run(step.argv, step.cwd, timeout=1800)
        blocked |= result.missing
        ok = result.ok and not (step.fail_on_output and result.stdout.strip())
        if not ok:
            runner.out.error(f"{step.label}: exited {result.returncode}\n{result.tail()}")
        results.append(
            {
                "label": step.label,
                "ok": bool(ok),
                "exit_code": result.returncode,
                "diagnostic": result.tail() if not ok else "",
            }
        )
        if result.returncode == 130:
            break
    return {"steps": results, "not_applicable": skipped}, (
        130
        if any(s["exit_code"] == 130 for s in results)
        else 3
        if blocked
        else int(any(not s["ok"] for s in results))
    )
