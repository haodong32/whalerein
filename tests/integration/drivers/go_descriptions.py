"""Export source-authored Go test descriptions for the compiled native inventory."""

import json
import re
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


def export(root):
    packages = {
        "store": "access/internal/store",
        "store-support": "access/internal/store/storetest",
        "server": "access/cmd/server",
    }
    output = root / "tests/integration/fixtures"
    for package, directory in packages.items():
        names = (output / f"{package}-cases.txt").read_text().splitlines()
        authored = source_descriptions(root / directory)
        missing = [name for name in names if not authored.get(name)]
        if missing:
            raise ValueError(f"Missing native test descriptions: {package}: {missing}")
        (output / f"{package}-descriptions.json").write_text(
            json.dumps({name: authored[name] for name in names}, indent=2) + "\n"
        )


if __name__ == "__main__":
    export(Path(__file__).resolve().parents[3])
