# Arbiter agent instructions

Arbiter is a local, single-user resource lease broker and collision scanner for concurrent coding agents. It is a CLI, not a service and not an MCP server.

## Non-negotiables

- **CLI first.** Every capability is a subcommand with human output and `--json`. Do not add an MCP server, a background daemon, or a network listener without a measured requirement and an explicit decision record.
- **Daemonless state.** State is a SQLite database in WAL mode. Claims take `BEGIN IMMEDIATE`. Do not add a second source of truth for lease state.
- **Detection is read-only.** Arbiter may report stale git locks, dead worktrees, orphaned processes, and collisions. It must never delete a lock, kill a process, remove a worktree, or `git clean` on its own.
- **Exact worktree claims.** `worktree:` claims cover only that checkout. A separate worktree of the same repository must remain usable. Repository-wide claims require an explicit `path:` resource.
- **No ambient refs claim.** Wrapped sessions do not hold a shared refs lease. Ref mutation is claimed for the duration of the git command that performs it. This avoids self-upgrade deadlocks.
- **Branch verification before commit.** A commit through `arbiter git` must compare the current branch to the branch recorded in the agent's worktree lease and refuse on mismatch. An intentional `arbiter git checkout` updates that recorded branch; an external flip does not.
- **No silent global changes.** Installing the git shim is opt-in. Do not make Arbiter the default `git` for a machine in an automated installer.
- **No GitHub Actions.** Verification runs locally. Never add a workflow under `.github/workflows`.

## Honesty rules

- A claim obtained through `arbiter run` or `arbiter git` is enforced at that boundary. Anything else is advisory plus detection. Do not describe advisory claims as preventing writes.
- `arbiter scan` findings are heuristics derived from `ps`, `lsof`, `git worktree list`, git lock files, and launchd state. State what was observed, not what is certain.
- A worker's green tests are not integration acceptance. A scan with zero findings does not prove no collision; it proves no detectable collision in the scanned roots.

## Layout and ownership

| Area | Files | Contract |
|---|---|---|
| Resource algebra | `src/resource.rs` | Parsing, canonicalization, overlap/conflict rules |
| Lease store | `src/store.rs` | SQLite schema, atomic claim/release, waiters, reaping, events |
| Execution wrapper | `src/supervisor.rs` | Claim before spawn, heartbeat, signals, release |
| Identity | `src/identity.rs` | Agent/session resolution and process ancestry |
| Git guard | `src/gitguard.rs` | Operation classification, branch verification, shim |
| Live inspection | `src/scan.rs` | Worktrees, agents, ports, services, locks, findings |
| Service protocol | `src/service.rs` | Idle gate, kickstart, new-PID and health verification |
| CLI | `src/cli.rs`, `src/main.rs` | Command surface, JSON output, exit codes |
| Verification | `scripts/check.py` | Local receipt-producing checks |
| Plans and receipts | `docs/`, `evidence/` | Architecture, integrations, dated evidence |

One owner per area. If a change crosses areas, state the interface before editing.

## Verification

```bash
just quick    # cargo fmt --check + clippy -D warnings
just check    # quick + cargo test + script tests, writes a receipt
```

Every non-trivial change gets a dated entry in `docs/architecture.md` and a receipt in `build/reports/` or `evidence/`. The receipt states what the run proves and what it does not.

Required checks for any change to resource or store semantics: unit tests for overlap/mode/capacity, integration tests for cross-process conflict and release, and a test that a dead holder is reaped.

## Work record format

A durable record includes: objective, exclusions, source revision and dirty state, commands with exit codes, observed results, receipt path, and non-claims. Conversation summaries are not receipts.

## Current limitations

- Machine-local leases only; no cross-host coordination.
- No kernel enforcement, FUSE interception, or filesystem write blocking.
- Agent attribution for unwrapped sessions is heuristic (process ancestry plus cwd).
- The launchd restart path is macOS-specific and tested with a fake launcher in the suite, not against live services.
- Git shim behavior under unusual global git options (`--git-dir`, custom `GIT_DIR`) is not exhaustively covered.
