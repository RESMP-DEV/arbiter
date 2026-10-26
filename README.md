# Arbiter

Local resource leases and collision detection for concurrent coding agents.

Arbiter is a single-user CLI that gives Codex, Claude Code, ZCode, Qoder, CCR workers, and scripts a shared answer to one question: **is another process already using this worktree, repository, port, service, cache, or machine slot?**

It is daemonless. State lives in a SQLite WAL database under `~/.local/state/arbiter/`, and every claim is an atomic transaction. There is no server to keep alive, no socket, no network listener, and no MCP dependency.

## Why this exists

The local traces contain recurring, concrete collisions:

- A ZCode session switched branches in a shared checkout eight seconds before another agent's commit, landing the commit on the wrong branch (reflog evidence, 2026-09-25).
- A consolidation session commandeered another task's worktree, checked out `main`, committed there, pushed, and swept untracked receipt files (2026-10-18).
- A manually started CCR listener fought the launchd-managed one for a port, producing repeated `Address already in use`; an ungated restart dropped three in-flight worker requests (2026-10-18).
- SQLite write contention surfaced as `database is locked`, degraded worker retry state, and blocked worker slots.
- Codex transcript scanning found 146 git `index.lock` failures across 85 session files, 120 port-bind failures across 54 files, and 115 SQLite-busy failures across 17 files. The lock counts include sandbox permission errors, so they are an upper bound rather than a pure collision count.
- A live probe of this Mac found 101 listening sockets, including seven CCR listeners, plus ten concurrent ZCode sessions.

The failure is structural: agents are disciplined in their own context, but nothing gives them a shared, crash-safe view of machine-local resources.

None of this was designed up front as an anti-collision system, and nothing in it manages deadlock in the abstract. Each mechanism answers one observed failure, through the same loop every time: a collision shows up in traces or an incident, the smallest customized tooling that removes that failure class gets built, and a receipt records what the fix does not prove. That loop — build tooling for the failure you actually hit, not the one you can imagine — is the practical best case for running agent fleets today, and this repository is what it produces.

## Install

Requires Rust and macOS or Linux. SQLite is bundled.

```bash
cargo build --release
install -m 0755 target/release/arbiter ~/.local/bin/arbiter
arbiter doctor
```

Optional machine config: copy [`examples/config.toml`](examples/config.toml) to `~/.config/arbiter/config.toml` and adjust roots, ports, services, and caches.

## Quick start

```bash
# Two agents cannot own the same resource.
arbiter claim worktree:/Users/me/repo --reason "editing schema"
arbiter claim worktree:/Users/me/repo          # exit 3, names the holder
arbiter release --mine

# Run a whole agent session under a lease.
arbiter run --auto -- codex
arbiter run --auto -- claude
arbiter run --auto -- zcode

# See what is actually happening on the machine.
arbiter scan
arbiter scan --check          # exit 3 on collisions
arbiter status --here
```

`--auto` claims the current git worktree as exclusive. Separate worktrees of the same repository do not conflict. Claims are exact to the worktree, so sibling worktrees remain usable by other agents. Refs are claimed only for the duration of ref-mutating git commands.

## Resource syntax

| Resource | Meaning | Default mode |
|---|---|---|
| `path:/abs/path` | File tree, covering descendants | exclusive |
| `path:/abs/path:exact` | Only that path | exclusive |
| `path:/abs/path:ro` | Shared read claim | shared |
| `worktree:/abs/checkout` | Git worktree, exact, with branch metadata | exclusive |
| `refs:/abs/repo/.git` | Repository refs | shared |
| `refs:/abs/repo/.git:rw` | Ref mutation (fetch, push, merge, commit) | exclusive |
| `port:8000` | TCP port on every host | exclusive |
| `port:8000@127.0.0.1` | TCP port on one host | exclusive |
| `port:53/udp` | UDP port | exclusive |
| `service:com.example.job` | launchd service restart | exclusive |
| `cache:uv` | Path mapped by `[caches]` in config | exclusive |
| `device:gpu:0` | Hardware device | exclusive |
| `name:deploy` | Arbitrary singleton | exclusive |
| `pool:build` | Slot pool with capacity from `[pools]` | exclusive |

Values named `ro`, `rw`, `shared`, `exclusive`, or `exact` are treated as modifiers only when a `kind:value` body remains, so `name:shared` still names the singleton `shared`.

## Commands

```text
arbiter claim <resource>... [--mode shared|exclusive] [--ttl 30m] [--wait 5m] [--reason TEXT]
arbiter release <group|lease>... | --mine | --all --force
arbiter renew <group>... [--ttl]
arbiter status [--here] [--json]
arbiter check <resource>...              # exit 3 if held
arbiter run [--auto] [--read-only] [--claim RES]... -- CMD...
arbiter scan [--root PATH]... [--check] [--strict] [--all-ports] [--json]
arbiter doctor [--json]
arbiter git -- <git args>
arbiter service list
arbiter service restart <label> [--wait 5m] [--dry-run]
arbiter log [--since 1h] [--limit 50]
arbiter init [--dir PATH]
arbiter shim install --dir ~/.local/bin | status | uninstall --path ...
```

Exit codes: `0` success, `2` usage, `3` conflict or guard denial, `4` wait timeout, `5` operation failed or not owner, `6` environment/config.

## What is enforced

Arbiter has three enforcement levels, and it is honest about which is active:

1. **Wrapped execution (`arbiter run`)** is enforced. The lease is acquired atomically before the child starts, held for its lifetime with a heartbeat, and released on exit. The child receives `ARBITER_AGENT`, `ARBITER_LEASE_GROUP`, and `ARBITER_LEASE_IDS`.
2. **`arbiter git`** is enforced for the operations it classifies: checkout, commit, merge, rebase, reset, fetch, push, clean, worktree removal, and similar. It verifies the branch recorded at claim time before committing, which catches the wrong-branch failure above. An intentional `arbiter git checkout` updates the lease metadata; a branch flip performed outside the wrapper does not.
3. **Everything else is advisory plus detection.** Without a wrapper or the optional git shim, an agent can still write a file or bind a port. `arbiter scan` finds those collisions from live process, cwd, listener, worktree, and lock state and reports who is involved.

The optional git shim makes every `git` invocation on `PATH` pass through `arbiter git`:

```bash
arbiter shim install --dir ~/.local/bin   # symlink ~/.local/bin/git -> arbiter
arbiter shim status
```

Install it only after reading [docs/integrations.md](docs/integrations.md); shadowing git is powerful and deliberately opt-in.

## Toolbelt integration

The local toolbelt wraps the two CCR boundaries where collisions actually occur:

```bash
ccr-worker-lease --cwd /path/to/worktree -- <ccr-worker args>
ccr-service-restart main --wait 5m
install-arbiter
```

`ccr-worker-lease` runs the worker under `arbiter run --auto` and fails closed if Arbiter is missing. `ccr-service-restart` maps `main`, `worker`, `auth`, `ocr`, `portal`, and `ocr-webhook` to their launchd labels and uses the service protocol below. The ccr-rust router, admission slots, and state store are unchanged: routing owns model admission, Arbiter owns machine resources. Vendored copies of both scripts ship in [contrib/toolbelt/](contrib/toolbelt/) with provenance pinned; the toolbelt repository remains canonical.

## Service restarts

Restarting a shared service is the highest-risk routine operation in this environment. Arbiter makes it a protocol:

1. Claim `service:<label>` and its configured port.
2. Poll the idle endpoint until the service reports zero in-flight work.
3. Run `launchctl kickstart -k gui/<uid>/<label>`.
4. Wait for a **new PID** and a healthy endpoint, so a draining old instance is not mistaken for success.
5. Release the claim and write an audit event.

```bash
arbiter service restart com.kearm.ccr-rust --wait 5m
```

## Design notes

- Daemonless SQLite WAL store; `BEGIN IMMEDIATE` for every claim.
- Leases carry holder PID and process start time; dead holders are reaped immediately, and TTL covers crashes that leave no PID.
- FIFO waiters instead of polling barges.
- Path overlap and case folding are filesystem-aware.
- Claims never delete locks, kill processes, or remove worktrees. Detection is read-only.
- JSON output on every inspection command.

See [docs/architecture.md](docs/architecture.md) for the full contract and [docs/integrations.md](docs/integrations.md) for agent setup.

## Limits

- Machine-local only. Cross-host leases are not implemented.
- No kernel enforcement or FUSE interception. An agent that ignores Arbiter is detected, not stopped.
- Process-to-agent attribution uses `ps`, `lsof`, and environment markers; cwd-based detection is heuristic.
- `launchctl` restart logic is macOS-specific.

## Verification

```bash
just check     # fmt, clippy -D warnings, cargo test, script tests, receipt
just quick     # fmt and clippy only
```

Receipts land in `build/reports/<mode>/receipt.json`. There is no GitHub Actions workflow; verification runs locally by policy.

## License

MIT.
