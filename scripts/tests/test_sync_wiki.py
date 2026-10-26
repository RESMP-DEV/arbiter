from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "sync_wiki.py"
SPEC = importlib.util.spec_from_file_location("arbiter_sync_wiki", SCRIPT)
assert SPEC and SPEC.loader
sync_wiki = importlib.util.module_from_spec(SPEC)
sys.modules["arbiter_sync_wiki"] = sync_wiki
SPEC.loader.exec_module(sync_wiki)


class BuildPagesTests(unittest.TestCase):
    def setUp(self) -> None:
        self.pages = sync_wiki.build_pages()

    def test_home_contains_intro_and_nav(self) -> None:
        home = self.pages["Home.md"]
        self.assertIn("Arbiter", home)
        self.assertIn("[[Architecture]]", home)
        self.assertIn("generated from the repository docs", home)

    def test_sidebar_lists_every_nav_page(self) -> None:
        sidebar = self.pages["_Sidebar.md"]
        for name, _ in sync_wiki.NAV:
            with self.subTest(page=name):
                self.assertIn(f"[[{name}]]", sidebar)

    def test_architecture_page_mirrors_canonical_doc(self) -> None:
        canonical = (sync_wiki.REPO_ROOT / "docs/architecture.md").read_text(
            encoding="utf-8"
        )
        self.assertEqual(self.pages["Architecture.md"], canonical)

    def test_deadlock_page_is_section_extract(self) -> None:
        page = self.pages["Deadlock-Resistance.md"]
        self.assertIn("No coordinator process", page)
        self.assertNotIn("## Ownership", page)

    def test_evidence_index_lists_tracked_receipts(self) -> None:
        evidence = self.pages["Evidence.md"]
        receipts = sorted((sync_wiki.REPO_ROOT / "evidence").glob("*.md"))
        self.assertTrue(receipts)
        for receipt in receipts:
            with self.subTest(receipt=receipt.name):
                self.assertIn(receipt.name, evidence)

    def test_config_page_embeds_examples_as_toml(self) -> None:
        page = self.pages["Configuration.md"]
        self.assertIn("```toml", page)
        self.assertIn("examples/config.toml", page)

    def test_extract_section_missing_raises(self) -> None:
        with self.assertRaises(ValueError):
            sync_wiki._extract_section("# nothing here\n", "Nope")


if __name__ == "__main__":
    unittest.main()
