# Integrating Arbiter with coding agents

Arbiter works without agent cooperation: `scan` inspects live processes, cwds, listeners, worktrees, and locks. Enforcement at write time requires one of the two wrappers below.

## Wrapping a session

```bash
arbiter run --auto -- codex
arbiter run --auto -- claude
arbiter run --auto -- zcode
arbiter run --auto -- <any other harness>
```

`--auto` claims the current git worktree as an exclusive, exact lease. The harness and every process it spawns inherit:

- `ARBITER_AGENT` — stable session identity
- `ARBITER_LEASE_GROUP` — release handle
- `ARBITER_LEASE_IDS` — comma-separated lease ids
- `ARBITER_WORKTREE` — worktree path, when the claim is a `worktree:` resource

A child that inherits these can claim more resources and release its own leases:

```bash
arbiter claim port:8000 --reason "preview server"
arbiter release --mine
```

For read-only agents:

```bash
arbiter run --auto --read-only -- claude -p "review this change"
```

Read-only mode downgrades the worktree claim to shared. It does not make the agent read-only; it only allows other readers to coexist with it.

## Preflight instead of wrapping

If a harness cannot be wrapped, put this in the repository or global agent instructions:

```markdown
Before editing a shared checkout, restarting a service, or binding a known port:
1. Run `arbiter check worktree:$PWD` (or `port:8000`, `service:<label>`).
2. If it reports a holder, do not proceed. Work in a separate worktree or wait.
3. If you create a worktree for a task, claim it with
   `arbiter claim worktree:<path> --reason "<task>"` and release it at handoff.
4. Before `git clean`, `git worktree remove`, or a service restart, run
   `arbiter scan --check`.
```

This is advisory unless the harness actually calls `arbiter git` or runs under `arbiter run`.

## Git guard

```bash
arbiter git status
arbiter git commit -m "message"
arbiter git -C /path/to/worktree checkout -b feature
```

The guard classifies the operation, claims the worktree and/or refs for the command's duration, and verifies the branch recorded at claim time before a commit. An intentional `arbiter git checkout` updates the lease metadata; a branch flip performed outside the wrapper leaves it stale, and the next commit is refused. The guard also refuses `git clean` without an owned worktree claim or while another live agent has cwd in the worktree, and refuses to remove a worktree that another agent claims or is actively using.

### Optional shim

```bash
arbiter shim install --dir ~/.local/bin
arbiter shim status
```

The shim is a symlink named `git` to the arbiter binary. When invoked as `git`, it dispatches to `arbiter git`, which finds the real git by skipping its own path. Requirements:

- `~/.local/bin` must precede `/usr/bin` on `PATH`.
- Existing `git` in that directory must not already exist.
- Read the operation classifier before relying on it; unknown git subcommands pass through as read-only.

Remove it with `arbiter shim uninstall --path ~/.local/bin/git`.

## Toolbelt wrappers

The local toolbelt ships three entry points:

```bash
install-arbiter                                  # build/install from ~/arbiter
ccr-worker-lease --cwd "$PWD" -- <ccr-worker args>
ccr-service-restart main --wait 5m
```

Use `ccr-worker-lease` for every CCR worker launch. It resolves `--cwd`, claims that worktree exclusively, and execs `arbiter run --auto`. It fails closed when Arbiter is unavailable; `--unguarded` is the explicit bypass. Use `ccr-service-restart` instead of a bare `launchctl kickstart -k`; it routes through `arbiter service restart` with the idle, new-PID, and health checks. The router and worker launcher are not modified.

## CCR workers

CCR workers are `codex exec` processes launched by a Python supervisor. Two options:

1. Wrap the launcher: `arbiter run --auto -- codex-ccr-worker ...`.
2. Add explicit claims to the launcher's workspace setup and release in its cleanup path.

Workers that are sandboxed cannot write `.git/worktrees/*/index.lock` or bind sockets. Arbiter's git guard will therefore fail inside that sandbox for the same reason git does; this is an environment limitation, not a guard bug. Parent-side commits remain the integration step.

## Service restarts

Configure the service once:

```toml
[services."com.kearm.ccr-rust"]
port = 3456
idle_url = "http://127.0.0.1:3456/metrics"
idle_key = "ccr_active_requests"
health_url = "http://127.0.0.1:3456/health"
idle_timeout = "2m"
health_timeout = "1m"
```

Then:

```bash
arbiter service restart com.kearm.ccr-rust --wait 5m
```

The protocol claims the service and port, waits for `ccr_active_requests` to reach `0`, runs `launchctl kickstart -k gui/<uid>/<label>`, waits for a PID different from the pre-restart PID, verifies `/health`, releases, and logs the result.

Do not use this to restart a desktop app that hosts conversations. The rule in `~/.zcode` memories still applies: relaunching ZCode Preview requires the user's confirmation.

## Repository policy

Add `.arbiter.toml` at a repository root:

```toml
[project]
name = "example"

[auto]
# Default is ["worktree:{worktree}"].
# Add project-wide resources only when every session really needs them.
claims = ["worktree:{worktree}"]

[resources]
ports = [8000, 8787]
services = ["com.example.portal"]
protected_paths = ["deploy/"]
```

`arbiter scan` reads this file to know which ports and services are project-relevant, and `arbiter run --auto` uses `auto.claims` when present. `{worktree}`, `{repo}`, `{cwd}`, and `{branch}` are substituted.

## Exit codes and JSON

| Code | Meaning |
|---|---|
| 0 | success |
| 2 | usage error |
| 3 | conflict, guard denial, or `scan --check` findings |
| 4 | wait timeout |
| 5 | failed operation or not owner |
| 6 | environment or config error |

Add `--json` before or after the subcommand (it is global) for machine output. Errors are emitted as `{"error":{"kind":"conflict","message":"..."}}` on stderr.

## Unwrapped sessions

`arbiter scan` detects two harness processes with cwd in the same worktree and reports `shared_worktree`. Detection depends on:

- `ps` ancestry to identify harness processes
- `lsof` cwd lookups
- environment markers (`CODEX_THREAD_ID`, `CLAUDE_SESSION_ID`, `ZCODE_SESSION_ID`, `QODER_SESSION_ID`) when the command runs inside the session
- configured scan roots

A harness with no recognizable process name or cwd will not be attributed. That is a detection limit, not a guarantee of safety.
