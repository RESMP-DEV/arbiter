# Fleet playbook

Running several concurrent coding agents on one macOS or Linux machine with
arbiter. This page is operational; the design contract is in
[architecture.md](architecture.md) and agent wiring in
[integrations.md](integrations.md). Drafted from those two sources and the
README; claims cite the section they came from.

## Scope

This playbook is for running several concurrent coding agents on one macOS or
Linux machine. Arbiter is machine-local; cross-host leases are not
implemented (source: README "Limits"; docs/architecture.md "Open choices").
The service-restart portion uses launchd and is therefore macOS-specific
(source: README "Limits"). Concrete labels, paths, ports, and counts copied
from the maintainer's docs are marked as examples and are not defaults.

## The session-wrapping ladder

The ladder is ordered by enforcement: the first two rungs are enforced at
their boundary, while the preflight rung is advisory plus detection (source:
README "What is enforced"). Use the strongest rung the harness allows.

**Rung 1: wrap the whole session with `arbiter run`.**

```bash
arbiter run --auto -- codex
arbiter run --auto -- claude
arbiter run --auto -- zcode
arbiter run --auto -- <any other harness>
```

The supervisor claims before spawning, heartbeats for the lease lifetime,
and releases on exit; the harness and every process it spawns inherit the
lease environment (source: docs/integrations.md "Wrapping a session"; README
"What is enforced"). For a review-only agent:

```bash
arbiter run --auto --read-only -- claude -p "review this change"
```

Read-only mode downgrades the worktree claim to shared and allows other
readers to coexist; it does not make the agent read-only (source:
docs/integrations.md "Wrapping a session").

**Rung 2: route git operations through `arbiter git`.**

```bash
arbiter git status
arbiter git commit -m "message"
arbiter git -C /path/to/worktree checkout -b feature
```

The guard classifies each operation, claims the worktree and/or refs for the
command's duration, and verifies the branch recorded at claim time before a
commit. An intentional wrapped checkout updates the lease metadata; an
external branch flip leaves it stale and the next commit is refused. The
guard also refuses `git clean` and worktree removal in the guarded cases
described in the integrations page (source: docs/integrations.md "Git guard";
README "What is enforced").

The optional shim extends this rung to every `git` invocation on `PATH`:

```bash
arbiter shim install --dir ~/.local/bin
arbiter shim status
```

It is a symlink named `git` to the arbiter binary, which dispatches to
`arbiter git` and finds the real git by skipping its own path. It requires
`~/.local/bin` to precede `/usr/bin`, requires that no `git` already occupy
that path, and passes unknown git subcommands through as read-only.
Installation is deliberately opt-in (source: docs/integrations.md "Optional
shim"; README "What is enforced").

**Rung 3: use scan preflight when the harness cannot be wrapped.**

Put this in repository or global agent instructions:

```markdown
Before editing a shared checkout, restarting a service, or binding a known port:
1. Run `arbiter check worktree:$PWD` (or `port:8000`, `service:<label>`).
2. If it reports a holder, do not proceed. Work in a separate worktree or wait.
3. If you create a worktree for a task, claim it with
   `arbiter claim worktree:<path> --reason "<task>"` and release it at handoff.
4. Before `git clean`, `git worktree remove`, or a service restart, run
   `arbiter scan --check`.
```

This is advisory unless the harness actually calls `arbiter git` or runs
under `arbiter run` (source: docs/integrations.md "Preflight instead of
wrapping").

## Worktree placement

For several exclusive writers, place each one in its own checkout. A
`worktree:` claim covers exactly that checkout, so a separate worktree of the
same repository remains usable by another agent; the documented incidents
include a shared-checkout branch flip and a commandeered task worktree
(source: README "Quick start", "Why this exists"; docs/architecture.md
"Contracts to preserve" item 4).

For an unwrapped task worktree, claim it on creation and release it at
handoff:

```bash
arbiter claim worktree:<path> --reason "<task>"
arbiter release --mine
```

Under `arbiter run --auto`, the wrapper claims the current checkout
automatically (source: docs/integrations.md "Preflight instead of wrapping";
README "Quick start").

Two unwrapped harness processes with cwd in the same worktree can be
reported by `arbiter scan` as `shared_worktree`. Attribution depends on `ps`
ancestry, `lsof` cwd lookups, environment markers, and configured scan roots;
a harness with no recognizable process name or cwd will not be attributed
(source: docs/integrations.md "Unwrapped sessions").

## A bounded worker fleet

Choose the fleet cap before launch. The cited docs do not define a fleet-size
limit; the architecture says this machine's practical ceiling came from
launcher policy (twelve CCR workers) and machine resources, not lease
coordination, and that no dedicated large-N soak test exists. Example only:
twelve is not a documented validated limit (source: docs/architecture.md
"Deadlock resistance").

For CCR workers, launch each through the toolbelt wrapper:

```bash
ccr-worker-lease --cwd "$PWD" -- <ccr-worker args>
```

It resolves `--cwd`, claims that worktree exclusively, and execs
`arbiter run --auto`. It fails closed when Arbiter is unavailable;
`--unguarded` is the explicit bypass. Without the toolbelt, the documented
alternative is to wrap the launcher with
`arbiter run --auto -- codex-ccr-worker ...` (source: docs/integrations.md
"Toolbelt wrappers", "CCR workers"; README "Toolbelt integration").

Each exclusive writer needs its own claimed checkout; read-only agents can
share one. A wrapped worker inherits the lease environment and can claim
additional resources it owns, for example:

```bash
arbiter claim port:8000 --reason "preview server"
arbiter release --mine
```

Inspect the fleet with `arbiter scan` and `arbiter status --here`;
`arbiter scan --check` exits `3` on collisions (source:
docs/integrations.md "Wrapping a session"; README "Quick start").

## Service restart protocol

On macOS, configure the service once. The following is the maintainer's
example target, not a default: label `com.kearm.ccr-rust`, port `3456`, and
the CCR request metric key (source: docs/integrations.md "Service
restarts").

```toml
[services."com.kearm.ccr-rust"]
port = 3456
idle_url = "http://127.0.0.1:3456/metrics"
idle_key = "ccr_active_requests"
health_url = "http://127.0.0.1:3456/health"
idle_timeout = "2m"
health_timeout = "1m"
```

Then run the guarded restart:

```bash
arbiter service restart com.kearm.ccr-rust --wait 5m
```

The protocol is:

1. Claim `service:<label>` and its configured port.
2. Poll the idle endpoint until the service reports zero in-flight work.
3. Run `launchctl kickstart -k gui/<uid>/<label>`.
4. Wait for a new PID different from the pre-restart PID and a healthy
   endpoint, so a draining old instance is not mistaken for success.
5. Release the claim and write an audit event.

Use this rather than a bare `launchctl kickstart -k` (source: README "Service
restarts"; docs/integrations.md "Service restarts"). Because the restart path
is launchd/`launchctl`, it is macOS-specific, and the cited docs describe no
Linux service-restart path (source: README "Limits"). Do not use it to
restart a desktop app that hosts conversations; relaunching ZCode Preview
requires the user's confirmation (source: docs/integrations.md "Service
restarts").

## The evidence-first loop

Follow the same loop the repository uses for its own mechanisms (source:
README "Why this exists"):

1. **Start from observed failure.** Run `arbiter scan --check` or start from
   a trace or incident; exit `3` flags collision findings (source: README
   "Quick start").
2. **Make the smallest repair.** The README says nothing was designed up
   front as an anti-collision system; each mechanism answers one observed
   failure with the smallest customized tooling that removes that failure
   class (source: README "Why this exists").
3. **Verify locally.** `just check` runs formatting, clippy with
   `-D warnings`, cargo tests, and script tests, and writes a receipt.
   `just quick` runs formatting and clippy only. There is no GitHub Actions
   workflow; verification runs locally by policy (source: README
   "Verification").
4. **Record the receipt and its non-claims.** Receipts land in
   `build/reports/<mode>/receipt.json`. A receipt states what the run proves
   and what it does not. The architecture work log follows the same pattern:
   objective, evidence gathered, design selected, implementation and
   verification, then non-claims (source: README "Verification", "Why this
   exists"; docs/architecture.md "Work log").
5. **State the evidence level.** The design properties are exercised by suite
   coverage for cross-process conflict, queue acquisition after release, and
   dead-holder reaping, and are consistent with one live probe of ten ZCode
   sessions and 101 listening sockets. No dedicated large-N soak test exists
   (source: docs/architecture.md "Deadlock resistance").
