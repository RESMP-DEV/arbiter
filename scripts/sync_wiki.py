#!/usr/bin/env python3
"""Publish the repository wiki as a generated view of the canonical docs.

Every wiki page is assembled from files in this repository; the repo is
canonical and the wiki is a derived view. Rebuild and push with:

    python3 scripts/sync_wiki.py [--no-push]

Pages backed by optional sources (docs/fleet-playbook.md) are omitted with a
warning until their source exists. Pushing requires SSH access to the wiki
remote; the first sync initializes an empty wiki repository.
"""

from __future__ import annotations

import argparse
import re
import shutil
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
REPO_BASE = "https://github.com/RESMP-DEV/arbiter"
WIKI_REMOTE = "git@github.com:RESMP-DEV/arbiter.wiki.git"
WIKI_BRANCH = "master"
SYNC_DIR = REPO_ROOT / "build" / "wiki-sync"

NAV = [
    ("Home", "Overview and quick start"),
    ("Architecture", "Design contract and work log"),
    ("Integrations", "Wiring arbiter into coding agents"),
    ("Fleet-Playbook", "Running several agents on one machine"),
    ("Deadlock-Resistance", "The failure classes the design closes"),
    ("Configuration", "Config reference and worked examples"),
    ("Evidence", "Dated evidence receipts"),
]


def _read(rel: str) -> str:
    return (REPO_ROOT / rel).read_text(encoding="utf-8").rstrip() + "\n"


def _extract_section(text: str, heading: str) -> str:
    pattern = re.compile(rf"(?ms)^## {re.escape(heading)}\s*\n(.*?)(?=^## |\Z)")
    match = pattern.search(text)
    if match is None:
        raise ValueError(f"section not found: {heading!r}")
    return match.group(1).strip() + "\n"


def _repo_rev() -> str:
    proc = subprocess.run(
        ["git", "rev-parse", "--short", "HEAD"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
    )
    return proc.stdout.strip() if proc.returncode == 0 else "unknown"


def _intro(readme: str) -> str:
    """README content above the first section heading, minus badge lines."""
    cut = readme.index("\n## ")
    body = "\n".join(
        line for line in readme[:cut].splitlines() if not line.startswith("[![")
    )
    return body.strip() + "\n"


def _nav_block() -> str:
    lines = ["## Pages", ""]
    for name, blurb in NAV:
        lines.append(f"- [[{name}]] — {blurb}")
    lines.append("")
    lines.append(
        "This wiki is generated from the repository docs by "
        "`scripts/sync_wiki.py`; the repository is canonical."
    )
    return "\n".join(lines) + "\n"


def _evidence_index() -> str:
    lines = [
        "# Evidence index",
        "",
        "Dated receipts recording what was done, how it was verified, and "
        "what it does not prove. The canonical files live in the repository "
        f"at [evidence/]({REPO_BASE}/tree/main/evidence); this page is a "
        "generated index of them.",
        "",
    ]
    for path in sorted((REPO_ROOT / "evidence").glob("*.md")):
        title = ""
        for line in path.read_text(encoding="utf-8").splitlines():
            if line.startswith("# "):
                title = line[2:].strip()
                break
        lines.append(
            f"- [{title or path.stem}]({REPO_BASE}/blob/main/evidence/{path.name})"
        )
    return "\n".join(lines).rstrip() + "\n"


def build_pages() -> dict[str, str]:
    """Assemble every wiki page from canonical repository sources."""
    readme = _read("README.md")
    architecture = _read("docs/architecture.md")
    integrations = _read("docs/integrations.md")

    pages = {
        "Home.md": _intro(readme) + "\n" + _nav_block(),
        "_Sidebar.md": "\n".join(
            f"**[[{name}]]**" if name == "Home" else f"[[{name}]]"
            for name, _ in NAV
        ) + "\n",
        "Architecture.md": architecture,
        "Integrations.md": integrations,
        "Deadlock-Resistance.md": (
            "# Deadlock resistance\n"
            "\n"
            "Generated from the `Deadlock resistance` section of "
            f"[docs/architecture.md]({REPO_BASE}/blob/main/docs/architecture.md).\n"
            "\n"
            + _extract_section(architecture, "Deadlock resistance")
        ),
        "Configuration.md": (
            "# Configuration\n"
            "\n"
            "Optional machine config lives at `~/.config/arbiter/config.toml`. "
            "The generic template is "
            f"[examples/config.toml]({REPO_BASE}/blob/main/examples/config.toml); "
            "the maintainer's live config is kept as a worked example at "
            f"[examples/this-machine.toml]({REPO_BASE}/blob/main/examples/this-machine.toml).\n"  # noqa: E501
            "\n"
            "## Generic template\n"
            "\n"
            "```toml\n" + _read("examples/config.toml").rstrip() + "\n```\n"
            "\n"
            "## Worked example (maintainer's machine)\n"
            "\n"
            "```toml\n" + _read("examples/this-machine.toml").rstrip() + "\n```\n"
        ),
        "Evidence.md": _evidence_index(),
    }

    if (REPO_ROOT / "docs" / "fleet-playbook.md").exists():
        pages["Fleet-Playbook.md"] = _read("docs/fleet-playbook.md")
    else:
        print(
            "warning: docs/fleet-playbook.md missing; Fleet-Playbook page omitted",
            file=sys.stderr,
        )
    return pages


def _git(args: list[str], cwd: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True)


def sync(push: bool = True) -> int:
    pages = build_pages()
    if SYNC_DIR.exists():
        shutil.rmtree(SYNC_DIR)
    SYNC_DIR.mkdir(parents=True)

    clone = _git(["clone", WIKI_REMOTE, str(SYNC_DIR)], SYNC_DIR.parent)
    fresh = False
    if clone.returncode != 0:
        init = _git(
            ["init", "--initial-branch", WIKI_BRANCH, str(SYNC_DIR)], SYNC_DIR.parent
        )
        if init.returncode != 0:
            print(init.stderr, file=sys.stderr)
            return 1
        remote = _git(["remote", "add", "origin", WIKI_REMOTE], SYNC_DIR)
        if remote.returncode != 0:
            print(remote.stderr, file=sys.stderr)
            return 1
        fresh = True

    for name, content in pages.items():
        (SYNC_DIR / name).write_text(content, encoding="utf-8")

    if not push:
        print(f"built {len(pages)} pages in {SYNC_DIR} (--no-push)")
        return 0

    _git(["add", "-A"], SYNC_DIR)
    rev = _repo_rev()
    if _git(["status", "--porcelain"], SYNC_DIR).stdout.strip():
        commit = _git(["commit", "-m", f"wiki: sync from repo docs at {rev}"], SYNC_DIR)
        if commit.returncode != 0:
            print(commit.stderr, file=sys.stderr)
            return 1
    else:
        print("wiki already up to date")

    push_args = (
        ["push", "-u", "origin", WIKI_BRANCH] if fresh else ["push", "origin", WIKI_BRANCH]
    )
    pushed = _git(push_args, SYNC_DIR)
    if pushed.returncode != 0:
        print(pushed.stderr, file=sys.stderr)
        return 1
    print(f"pushed {len(pages)} pages (repo rev {rev}) to {WIKI_REMOTE}")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Sync the GitHub wiki from canonical docs.")
    parser.add_argument(
        "--no-push",
        action="store_true",
        help="build pages into build/wiki-sync without committing or pushing",
    )
    args = parser.parse_args()
    return sync(push=not args.no_push)


if __name__ == "__main__":
    raise SystemExit(main())
