# Proper-repo polish: wiki pipeline, playbook, guides, release, 2026-10-25

## Objective

Turn the public repository into a polished artifact for the fleet-coordination
use case: a wiki that can be spun up reproducibly, a fleet playbook, the
standard public-repo guides, and a first tagged release.

## Changes

- `scripts/sync_wiki.py` (tested in `scripts/tests/test_sync_wiki.py`) —
  assembles the GitHub wiki from canonical repository sources (Home from the
  README intro, full Architecture and Integrations mirrors, a
  Deadlock-Resistance section extract, a Configuration page from both config
  examples, a generated Evidence index, and Fleet-Playbook from
  `docs/fleet-playbook.md` when present). The wiki stays a generated view;
  the repository is canonical.
- `docs/fleet-playbook.md` — curated from a Qoder-lane draft (below), grounded
  in README/architecture/integrations with per-claim source citations.
- `CONTRIBUTING.md` (procedure: setup, worktree and arbiter discipline,
  required-checks matrix, work-record format) and `SECURITY.md` (scope,
  threat model, private-advisory reporting).
- README: release/license/platform badges, uninstall guidance, and a wiki
  pointer naming `scripts/sync_wiki.py` as the rebuild path.
- Repository settings: `has_wiki=true`; ten topics (agent-fleet, cli,
  concurrent-agents, coordination, developer-tools, launchd, local-first,
  rust, sqlite, worktrees).
- Release `v0.1.0` on the pushed main, with an unsigned aarch64-apple-darwin
  tarball and its sha256 attached.

Commits: `b451249` (polish + playbook + guides) and the evidence commit on
this page. Pushes fast-forwarded `main` through `arbiter git`.

## Qoder lane

One fresh DeepSeek-Flash attempt (`qodercli -p -m dfmodel --reasoning-effort
max --tools ""`, offline, read-only; first of the day's 20-attempt budget)
drafted the wiki Home, the fleet playbook, and a polish audit from the three
canonical docs. Parent verification: every mechanism claim, incident, count,
and command in the draft traces to the provided sources; the draft correctly
labeled "twelve CCR workers" as an example rather than a validated limit.
The playbook was integrated with structural edits only (title, preamble,
heading levels). Audit adoption: two findings adopted (release asset,
uninstall guidance); the configuration-reference gap is partially addressed
by the wiki Configuration page; the remaining audit items (sample command
output, `arbiter doctor` documentation, Linux service-restart path, upgrade
guidance) are accepted gaps recorded here, not defects.

## Verification

- `just check` passed in the polish worktree (fmt, clippy -D warnings, cargo
  test, script tests; receipt `passed=True`), including the seven new
  sync-wiki unit tests.
- Release page live: https://github.com/RESMP-DEV/arbiter/releases/tag/v0.1.0
  with `arbiter-v0.1.0-aarch64-apple-darwin.tar.gz` (sha256
  `096f381bfd511fa7a976b18b292e9dd7d5c12d6f0fc1bf2522ec64900ecc6326`) and the
  `.sha256` sidecar.
- The unauthenticated repo page renders the topics, the v0.1.0 Latest badge,
  the README badges, and Contributing/Security entries (observed in a
  logged-out browser at 2026-10-25 ~21:40 local).

## Wiki bootstrap state (blocker)

- `has_wiki=true` is set, but GitHub provisions the wiki git repository only
  when the first page is created through the web UI. Probes:
  `git clone git@github.com:RESMP-DEV/arbiter.wiki.git` fails ("Could not
  read from remote repository") and a first push to the wiki remote also
  fails; GitHub has no REST/GraphQL API for wiki content.
- The reachable browser session is signed out of GitHub; credentials were not
  handled. Therefore the one-time bootstrap is a manual step: create any
  first page at https://github.com/RESMP-DEV/arbiter/wiki/_new while signed
  in, then `python3 scripts/sync_wiki.py` replaces it with the full generated
  wiki and all future syncs are a single command.
- DeepWiki (https://deepwiki.com/RESMP-DEV/arbiter) shows "Repository Not
  Indexed"; triggering indexing requires an email/login interaction that was
  not performed unilaterally.

## Non-claims

- The wiki pages are not yet live anywhere (the git wiki awaits bootstrap);
  only the pipeline and its tests are verified.
- The release asset is unsigned and built on this maintainer's Apple Silicon
  machine; it is not a reproducible-build claim.
- Topic and settings state are point-in-time observations of a public page.
