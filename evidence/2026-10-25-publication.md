# Publication of RESMP-DEV/arbiter as the public fleet-wiring artifact, 2026-10-25

## Objective

Publish the repository publicly with the machine's anti-collision wiring
self-contained: vendored toolbelt boundary scripts, a deadlock-resistance
contract section grounded in the observed incidents, and README framing for
the evidence-first build loop.

## Changes (commit `5150673`, branch `docs/public-fleet-wiring`, fast-forwarded to `main`)

- `contrib/toolbelt/` — pinned snapshots of `ccr-worker-lease` and
  `ccr-service-restart` from `RESMP-DEV/toolbelt` at `3f9e57a`, with a
  provenance README naming the toolbelt repository as canonical.
- `docs/architecture.md` — new "Deadlock resistance" section naming the
  coordination-failure classes the design closes (no coordinator process,
  dead-holder reaping, no upgrade deadlocks, FIFO waiters, exact worktree
  granularity, read-only detection, no cross-concern circular wait) and their
  originating incidents, with an honest-scope paragraph.
- `README.md` — evidence-first framing paragraph (built from observed
  failures, not designed up front) and a pointer to the vendored copies.
- Visibility flipped PRIVATE to PUBLIC via
  `gh repo edit RESMP-DEV/arbiter --visibility public --accept-visibility-change-consequences`
  (exit 0).

## Pre-publication scan

- History credential scan: `git log -p --all | grep -iE` over secret-shaped
  patterns (api key, secret, token, password, `sk-`, `AKIA`, `ghp_`, `xox`,
  private-key blocks) across all 11 commits returned only Rust
  identifier-parsing code (`take_token`), no credential material.
- Confidentiality-terminology grep (Bayesian, BoTorch, qNEHVI, Gaussian
  Process) across `*.md`, `*.rs`, `*.toml`: no matches.
- `examples/this-machine.toml` contains local paths and port numbers, which
  are operational facts of the wiring being published, not credentials.

## Verification

- `just check` in the publication worktree: fmt, clippy, test, script-tests
  all ok; receipt `build/reports/local/receipt.json` `passed=True`.
- Push: `docs/public-fleet-wiring -> main` fast-forward `dc0db7c..5150673`
  through `arbiter git` (worktree and refs claimed for the command duration).
- Unauthenticated access after the flip: repo page HTTP 200, raw README
  HTTP 200, API reports `visibility=public`, `license=MIT`.
- Repo-level GitHub Actions state: `{"enabled": false}` (org policy; verified,
  no change needed).

## Process notes

- The primary checkout was leased by a live CCR worker (pid 72356) during
  this work; all edits happened in `.worktrees/publication` on a separate
  branch, and `main` was fast-forwarded without touching the leased checkout.
- A manual advisory claim on the publication worktree was recorded under the
  heuristic identity `ccr-worker:pid8306` and then blocked both
  `arbiter git commit` and `arbiter release` from this shell's resolved
  identity; it was cleared with `arbiter release <group> --force` (operator
  escape, own claim, own worktree). Recorded as a live demonstration of the
  documented attribution limit: unwrapped-session identity resolution is
  heuristic.

## Non-claims

- Publication does not certify cross-host coordination (still an open choice)
  or any large-N soak result; the deadlock-resistance section states design
  properties and their test coverage, not a measured 100-agent run.
- The vendored copies are a snapshot at toolbelt `3f9e57a`, not a sync; drift
  is managed by the provenance header in `contrib/toolbelt/README.md`.
- This receipt records state at publication time; it does not track future
  visibility or content changes.
