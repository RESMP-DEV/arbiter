from __future__ import annotations

import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "check.py"
SPEC = importlib.util.spec_from_file_location("arbiter_check", SCRIPT)
assert SPEC and SPEC.loader
check = importlib.util.module_from_spec(SPEC)
sys.modules["arbiter_check"] = check
SPEC.loader.exec_module(check)


class CheckRunnerTests(unittest.TestCase):
    def test_passing_command_produces_passing_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "build/reports/local").mkdir(parents=True)
            log = root / "build/reports/local/ok.log"
            result = check.run_check(
                "ok", [sys.executable, "-c", "print('fine')"], log, root
            )
            self.assertEqual(result["exit_code"], 0)
            self.assertTrue(log.read_text().splitlines()[0].startswith("$ "))
            receipt = check.build_receipt(root, "local", [result])
            self.assertTrue(receipt["passed"])
            self.assertEqual(receipt["schema_version"], 1)

    def test_failing_command_fails_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            log = root / "build/reports/local/fail.log"
            result = check.run_check(
                "fail", [sys.executable, "-c", "raise SystemExit(9)"], log, root
            )
            self.assertEqual(result["exit_code"], 9)
            receipt = check.build_receipt(root, "local", [result])
            self.assertFalse(receipt["passed"])

    def test_receipt_is_json_serializable(self) -> None:
        receipt = check.build_receipt(Path("/tmp"), "quick", [])
        json.dumps(receipt)
        self.assertFalse(receipt["passed"])
        self.assertIn("scope", receipt)

    def test_modes_have_distinct_coverage(self) -> None:
        quick = {name for name, _ in check.CHECKS["quick"]}
        local = {name for name, _ in check.CHECKS["local"]}
        self.assertTrue(quick < local)
        self.assertIn("test", local)


if __name__ == "__main__":
    unittest.main()
