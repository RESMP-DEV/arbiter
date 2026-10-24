# Arbiter architecture and plan

## Philosophy

Arbiter exists because several agents already share one machine and one set of mutable resources, but each agent's context ends at its own process. The tool adds the smallest shared contract that prevents lost work: named resources, atomic leases, and a live view of what is actually held.

Priority ordering for every decision: **safety and crash correctness > accurate detection > enforceability > simplicity > feature count.** A claim that can be lost on a crash is worse than no claim. A scan that invents conflicts is worse than no scan.

The design is deliberately daemonless. A local SQLite database in WAL mode, written through `BEGIN IMMEDIATE` transactions, provides cross-process atomicity with less operational surface than a launchd daemon and socket. Every command opens the database for milliseconds; there is no process to keep alive and no state to recover after a reboot.

## Ownership

| Boundary | Owner | Contract |
|---|---|---|
| Conflict algebra | `src/resource.rs` | Canonical keys, modifiers, path overlap, port host/proto overlap, shared/exclusive semantics |
| State | `src/store.rs` | Schema, claims, waits, reaping, audit events; the only writer of lease state |
| Process lifetime | `src/supervisor.rs` | Claim-before-spawn, heartbeat, signal forwarding, release-on-exit |
| Identity | `src/identity.rs` | Environment and ancestry resolution; never requires harness cooperation |
| Git | `src/gitguard.rs` | Operation classification, branch verification, worktree-removal guards, optional shim |
| Observation | `src/scan.rs` | Read-only composition of worktrees, processes, ports, services, locks |
| Service restart | `src/service.rs` | Idle gate, kickstart, new-PID and health checks |
| Surface | `src/cli.rs` | JSON schema, exit codes, command behavior |

## Open choices

| Topic | Current position | Decision gate |
|---|---|---|
| Cross-host leases | Not implemented; leases carry a host name and never conflict across hosts | A real multi-host workflow where two machines write the same shared checkout or service |
| Kernel enforcement | Not implemented; claims are enforced at wrappers and git, otherwise advisory | A measured case where an unwrapped agent repeatedly ignores scan findings, plus a distribution-safe mechanism |
| Git shim default | Opt-in | A week of shim use on this machine with no unexpected git breakage |
| Session root attribution | Nearest harness process, with ccr-worker ancestors grouping their codex child | A harness whose process tree breaks the nearest-root assumption |
| Watch/notification mode | Not implemented; `scan --check` is one-shot | A workflow that needs sub-minute awareness rather than a preflight check |
| Launchd integration tests | Fake launcher plus local HTTP server | A disposable launchd job that can be restarted in tests without disturbing user services |

## Contracts to preserve

1. **One writer, atomic claims.** All lease mutations go through `Store` and a single SQLite transaction. No JSON sidecar state.
2. **Dead-holder reaping is mandatory.** Every transaction reaps leases whose holder PID is gone and waiters whose deadline passed.
3. **Claims never repair.** Detection may recommend removing a stale lock; it must not remove one.
4. **Worktree claims are exact.** `worktree:<path>` covers only that path. Path claims still cover descendants.
5. **Sessions do not hold refs.** Refs are claimed for the duration of a ref-mutating command, avoiding shared-to-exclusive upgrades.
6. **Commit verifies branch.** A branch change after claim is a conflict, not a warning.
7. **Exit codes are stable.** 3 means conflict or guard denial; scripts depend on it.
8. **JSON is a public surface.** Field renames require a schema version bump.
9. **No network.** The tool does not listen on a socket.

## Work log

### 2026-10-23: research and v1

**Objective.** Determine whether local agent collisions are a real, recurring failure mode and build a machine-wide coordination primitive.

**Evidence gathered.**

- Scanned `~/.codex/sessions` and `~/.claude/projects` with a schema-aware extractor that skips instruction attachments. Signature counts: 146 git `index.lock` mentions across 85 files, 120 port-bind failures across 54 files, 115 SQLite-busy errors across 17 files, 180 cargo build-lock waits across 80 files, 106 stale/orphan lock mentions across 64 files, 16 file-busy errors.
- The lock counts are an upper bound: many `index.lock` matches are sandbox `Operation not permitted` failures, not peer contention. Port and SQLite matches are dominated by real repeated failures.
- Memory and receipts add specific incidents: a wrong-branch commit after a concurrent branch flip (2026-09-25), a task worktree commandeered and swept (2026-10-18), a CCR restart that dropped three in-flight worker requests (2026-10-18), launchd transient bind races, a shared Codex skills cache rewrite race, and browser profile locks.
- Live probe: 101 listening sockets, seven CCR listeners, ten ZCode sessions, multiple worktrees across `lapis` and `AlphaHENG`.

**Design selected.** Daemonless SQLite lease store; resources for path, worktree, refs, port, service, cache, device, name, and pool; FIFO waiters; PID+start identity; `arbiter run` supervisor; `arbiter git` guard; `arbiter scan`; launchd restart protocol; optional git shim.

**Implementation and verification.** See `evidence/2026-10-23-build.md`. The suite covers conflict algebra, shared/exclusive modes, pools, cross-process conflict, queue acquisition after release, dead-holder reaping, environment injection, exit-code propagation, git guard denial and branch verification, clean guards, shared-worktree detection, service dry-run, config generation, and shim lifecycle.

**Non-claims.** The suite does not prove live launchd restarts, does not prove enforcement against agents that bypass all Arbiter entry points, and does not validate cross-host behavior.

## Next gates

- Install the release binary and host config; run `arbiter scan` against the real worktrees and services for one week.
- Decide on the git shim after a week of opt-in use.
- Add a watch mode only if preflight checks miss a real collision.
