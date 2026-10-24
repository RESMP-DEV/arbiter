# Collision research, 2026-10-23

## Question

Do concurrent coding agents on this machine actually collide, and if so, at which resources?

## Method

A schema-aware extractor scanned Codex rollouts (`~/.codex/sessions`) and Claude Code transcripts (`~/.claude/projects`) for tool output and message text, skipping instruction attachments, developer prompts, and the embedded `AGENTS.md` bodies that otherwise dominate keyword matches. Signature counts are occurrences, not unique incidents; a single failure repeated in one session counts multiple times.

## Signature counts

| Signature | Occurrences | Files |
|---|---:|---:|
| cargo build/package lock waits | 180 | 80 |
| git `index.lock` failures/mentions | 146 | 85 |
| port bind failures (`EADDRINUSE`, `Address already in use`) | 120 | 54 |
| SQLite `database is locked` / `SQLITE_BUSY` | 115 | 17 |
| stale/orphan lock mentions | 106 | 64 |
| file-busy errors | 16 | 2 |

Caveats:

- Most `index.lock` matches are sandbox `Operation not permitted` failures on linked worktrees, not peer contention. The count is an upper bound.
- Cargo lock waits are normal build serialization, but they confirm that multiple build processes routinely share target directories.
- Port and SQLite signatures are dominated by repeated real failures in a small number of sessions, not one-off text.

## Specific incidents

- **Wrong-branch commit, ZCode, 2026-09-25.** Another session switched `~/ZCode` from the current branch to `feat/session-sweeper` about eight seconds before a commit. The commit landed on the wrong branch and was repaired with cherry-pick plus `git branch -f`. Reflog evidence exists.
- **Worktree commandeering, 2026-10-18.** A consolidation session checked out `main` inside another task's `.worktrees/automation-system-scheduling`, committed there, pushed, and swept untracked files, including a build receipt. The committed work survived on its branch; the untracked receipt did not.
- **CCR listener fight.** A manually started listener competed with a launchd `KeepAlive` job for the same port, producing recurring `Address already in use`. A restart performed without an idle gate dropped three in-flight worker requests (2026-10-18).
- **Launchd drain race.** After `launchctl kickstart -k`, the old process can hold the port while draining; a transient bind error is not necessarily a failed restart.
- **SQLite contention.** ocr-poller and worker state produced `database is locked`, degrading retry state and blocking worker slots.
- **Shared cache rewrite.** `~/.codex/skills/.system` is rewritten in place and was hit concurrently by multiple Codex processes.
- **Browser profile locks.** Concurrent browser automation serialized on profile locks and left orphaned processes and temp profiles.

## Live machine probe

At 2026-10-23T03:13Z:

- 101 listening sockets (deduplicated by port/proto/pid: 88 rows in the scanner).
- Seven CCR listeners: 3456, 3457, 3458, 3461, 3462, 3473, 34590.
- Ten `zcode-cli` sessions and multiple Codex sessions.
- Worktrees across `lapis` (6), `AlphaHENG` (5), `peeklytic` (6), and other repos (34 total worktrees scanned).
- `3462` was a manually started `ccr-rust` process, not a launchd job: a concrete recurrence of the manual-vs-managed listener pattern.

The first full scan with the host config found one error-level collision already present:

```
shared_worktree: 2 agent sessions share worktree /Users/kearm/peeklytic
  codex pid 32159: codex resume 01a164ad-a3b1-7790-876d-efc71e4c1692
  codex pid 80330: codex resume 01a169fb-7024-7852-bd7a-9373c6548f27
```

Both sessions had cwd `/Users/kearm/peeklytic` on `main`. The portal is served from that primary checkout, so template and CSS edits there are live immediately. This is the same class of hazard recorded in the 2026-10-22 portal incident.

## Conclusions that shaped the build

1. Worktree ownership is the highest-value primitive: it prevents wrong-branch commits and commandeering.
2. Port and service coordination must include an idle gate and a new-PID check, not just a lock.
3. Lease state must be crash-safe and holder-aware; PID-only or TTL-only designs fail on both crashes and long builds.
4. Detection has to work without agent cooperation, because unwrapped sessions already exist.
5. Claims must be exact to a worktree; a repo-wide exclusive claim would serialize agents that are correctly isolated in separate worktrees.
6. Advisory claims must be labeled advisory. Only wrapper and git-guard boundaries can refuse an operation.
