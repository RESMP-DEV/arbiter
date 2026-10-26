# Toolbelt boundary scripts (vendored)

Operational copies of the two toolbelt entry points that wire Arbiter into
CCR on the maintainer's machine. The toolbelt repository (`RESMP-DEV/toolbelt`)
is canonical; these are pinned snapshots, refreshed when the upstream changes.

- `ccr-worker-lease` — runs a CCR worker under `arbiter run --auto`, claiming
  the `--cwd` worktree exclusively for the worker's lifetime, and fails closed
  when Arbiter is unavailable (`--unguarded` is the explicit bypass).
- `ccr-service-restart` — maps short service names to launchd labels and
  restarts through `arbiter service restart`: claim, idle gate, new-PID
  check, health verification, audit event.

Provenance: vendored from `RESMP-DEV/toolbelt` at `3f9e57a`, 2026-10-25.
The integration contract is described in
[docs/integrations.md](../../docs/integrations.md). The service map and
defaults reflect the maintainer's machine and are examples, not requirements.
