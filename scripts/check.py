#!/usr/bin/env python3
"""Local verification runner. Writes build/reports/<mode>/receipt.json.

There is no CI by policy; this is the durable verification path.
"""

from __future__ import annotations

import argparse
import json
import platform
import subprocess
import sys
import time
from datetime import UTC, datetime
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

CHECKS: dict[str, list[tuple[str, list[str]]]] = {
    "quick": [
        ("fmt", ["cargo", "fmt", "--check"]),
        ("clippy", ["cargo", "clippy", "--all-targets", "--", "-D", "warnings"]),
    ],
    "local": [
        ("fmt", ["cargo", "fmt", "--check"]),
        ("clippy", ["cargo", "clippy", "--all-targets", "--", "-D", "warnings"]),
        ("test", ["cargo", "test"]),
        (
            "script-tests",
            [sys.executable, "-m", "unittest", "discover", "-s", "scripts/tests", "-v"],
        ),
    ],
}

SCOPE = (
    "Proves formatting, lint, and the Rust/Python test suites on this host. "
    "Does not prove live launchd restart behavior, cross-host coordination, "
    "or enforcement against agents that bypass Arbiter entry points."
)


def source_revision(root: Path) -> tuple[str | None, bool | None]:
    try:
        revision = subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=root, text=True, stderr=subprocess.DEVNULL
        ).strip()
        dirty = bool(
            subprocess.check_output(
                ["git", "status", "--porcelain"], cwd=root, text=True, stderr=subprocess.DEVNULL
            ).strip()
        )
        return revision, dirty
    except (OSError, subprocess.CalledProcessError):
        return None, None


def run_check(name: str, command: list[str], log_path: Path, root: Path) -> dict[str, object]:
    log_path.parent.mkdir(parents=True, exist_ok=True)
    started = time.monotonic()
    with log_path.open("w") as log:
        log.write(f"$ {' '.join(command)}\n")
        log.flush()
        process = subprocess.run(
            command,
            cwd=root,
            stdout=log,
            stderr=subprocess.STDOUT,
            text=True,
            check=False,
        )
    elapsed = round(time.monotonic() - started, 3)
    return {
        "name": name,
        "command": command,
        "exit_code": process.returncode,
        "elapsed_secs": elapsed,
        "log": str(log_path.relative_to(root)),
    }


def build_receipt(
    root: Path, mode: str, checks: list[dict[str, object]], note: str | None = None
) -> dict[str, object]:
    revision, dirty = source_revision(root)
    receipt: dict[str, object] = {
        "schema_version": 1,
        "recorded_at": datetime.now(UTC).isoformat(),
        "platform": platform.platform(),
        "machine": platform.machine(),
        "source_revision": revision,
        "dirty": dirty,
        "mode": mode,
        "scope": SCOPE,
        "checks": checks,
        "passed": bool(checks) and all(check["exit_code"] == 0 for check in checks),
    }
    if note:
        receipt["note"] = note
    return receipt


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--mode", choices=sorted(CHECKS), default="local")
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args()

    root = args.root.resolve()
    report_dir = root / "build/reports" / args.mode
    receipt_path = report_dir / "receipt.json"
    report_dir.mkdir(parents=True, exist_ok=True)
    # An interrupted run must not leave a stale PASS behind.
    receipt_path.unlink(missing_ok=True)

    checks: list[dict[str, object]] = []
    try:
        for name, command in CHECKS[args.mode]:
            log_path = report_dir / f"{name}.log"
            result = run_check(name, command, log_path, root)
            checks.append(result)
            status = "ok" if result["exit_code"] == 0 else f"FAIL ({result['exit_code']})"
            print(f"{name:<14} {status:<12} {result['elapsed_secs']:>7.2f}s  {result['log']}")
            if result["exit_code"] != 0:
                print(f"  see {log_path}")
                break
        receipt = build_receipt(root, args.mode, checks)
    except Exception as error:  # noqa: BLE001 - a startup failure still leaves a receipt
        receipt = build_receipt(root, args.mode, checks, note=f"startup failure: {error}")
        receipt["passed"] = False
        print(f"startup failure: {error}", file=sys.stderr)

    receipt_path.write_text(json.dumps(receipt, indent=2) + "\n")
    if args.json:
        print(json.dumps(receipt, indent=2))
    else:
        print(f"receipt: {receipt_path}  passed={receipt['passed']}")
    return 0 if receipt["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
