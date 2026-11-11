"""Preserve native pytest phase timing without estimating missing phases."""


def record_phase(case, report):
    phases = case.setdefault("phase_durations_ms", {})
    phases[report.when] = round(report.duration * 1000, 3)
    case["duration_ms"] = round(sum(phases.values()), 3)
    case["duration_complete"] = report.when == "teardown"


def finish_interrupted(cases, started, finished, completed):
    for identity, began in started.items():
        if identity in completed:
            continue
        case = cases[identity]
        case["duration_ms"] = round(max(0, finished - began) * 1000, 3)
        case["duration_complete"] = False
        if case["status"] in ("passed", "not_run"):
            case["status"] = "interrupted"
