"""Export source-authored Go test descriptions for the compiled native inventory."""

import argparse
import json
import re
import subprocess
import tempfile
from pathlib import Path


def source_descriptions(directory):
    descriptions = {}
    for path in sorted(directory.glob("*_test.go")):
        comments = []
        for line in path.read_text().splitlines():
            if line.startswith("//"):
                comments.append(line[2:].strip())
                continue
            match = re.match(r"func (Test\w+)\(", line)
            if match and comments:
                descriptions[match[1]] = " ".join(comments)
            comments = []
    return descriptions


def _test_names(binary):
    output = subprocess.check_output([binary, "-test.list", "^Test"], text=True)
    return {name for name in output.splitlines() if name.startswith("Test")}


def export(root, products):
    packages = {
        "store": "access/internal/store",
        "store-support": "access/internal/store/storetest",
        "external": "access/internal/external",
        "server": "access/cmd/server",
    }
    output = products / "tests/integration/fixtures"
    with tempfile.TemporaryDirectory() as scratch:
        unit = Path(scratch) / "unit-tests"
        for package, directory in packages.items():
            tagged = products / ".verification/bin" / f"{package}-tests"
            if not tagged.exists():
                subprocess.run(
                    ["go", "test", "-c", "-tags=integration", "-o", tagged, f"./{directory}"],
                    cwd=root,
                    check=True,
                )
            unit.unlink(missing_ok=True)
            subprocess.run(
                ["go", "test", "-c", "-o", unit, f"./{directory}"], cwd=root, check=True
            )
            names = sorted(_test_names(tagged) - _test_names(unit))
            authored = source_descriptions(root / directory)
            missing = [name for name in names if not authored.get(name)]
            if missing:
                raise ValueError(f"Missing native test descriptions: {package}: {missing}")
            (output / f"{package}-cases.txt").write_text("\n".join(names))
            (output / f"{package}-descriptions.json").write_text(
                json.dumps({name: authored[name] for name in names}, indent=2) + "\n"
            )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--products", type=Path, required=True)
    export(Path(__file__).resolve().parents[3], parser.parse_args().products)
