#!/usr/bin/env python3
"""Run and verify the Route A core-library coverage gates.

The GUI and renderer adapters are exercised by their normal tests and native
smoke jobs, but are not part of this headless coverage gate.  The gate covers
the production library where protocol, recording, project, Compare, and rules
semantics live.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
CORE_FILES = {
    "src/compare/mod.rs": 90.0,
    "src/live/protocol.rs": 90.0,
    "src/live/recording.rs": 90.0,
    "src/project.rs": 90.0,
}
OVERALL_LINE_GATE = 75.0


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="scope-coverage-") as directory:
        report_path = Path(directory) / "coverage.json"
        command = [
            "cargo",
            "llvm-cov",
            "--locked",
            "--lib",
            "--json",
            "--summary-only",
            "--output-path",
            str(report_path),
        ]
        print("==>", " ".join(command))
        try:
            subprocess.run(command, cwd=ROOT, check=True)
        except FileNotFoundError:
            print(
                "cargo-llvm-cov is required; install cargo-llvm-cov 0.8.7 and "
                "llvm-tools-preview first.",
                file=sys.stderr,
            )
            return 2
        except subprocess.CalledProcessError as error:
            return error.returncode

        report = json.loads(report_path.read_text(encoding="utf-8"))
        files = report["data"][0]["files"]
        by_path = {Path(entry["filename"]).as_posix(): entry for entry in files}

        def find(path: str) -> dict:
            suffix = f"/{path}"
            for filename, entry in by_path.items():
                if filename == path or filename.endswith(suffix):
                    return entry
            raise KeyError(path)

        total = report["data"][0]["totals"]["lines"]["percent"]
        print(f"core library line coverage: {total:.2f}% (gate {OVERALL_LINE_GATE:.2f}%)")
        failures: list[str] = []
        if total < OVERALL_LINE_GATE:
            failures.append(f"core library total {total:.2f}% < {OVERALL_LINE_GATE:.2f}%")

        for path, gate in CORE_FILES.items():
            percent = find(path)["summary"]["lines"]["percent"]
            print(f"{path}: {percent:.2f}% (gate {gate:.2f}%)")
            if percent < gate:
                failures.append(f"{path} {percent:.2f}% < {gate:.2f}%")

        output = os.environ.get("SCOPE_COVERAGE_REPORT")
        if output:
            destination = Path(output)
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
            print(f"coverage report: {destination}")

        if failures:
            print("Coverage gate failed:", file=sys.stderr)
            for failure in failures:
                print(f"- {failure}", file=sys.stderr)
            return 1
        print("Coverage gates passed")
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
