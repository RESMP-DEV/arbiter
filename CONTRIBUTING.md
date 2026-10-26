# Contributing

Arbiter is a small, opinionated tool with a short list of non-negotiables in
[AGENTS.md](AGENTS.md); read that first. This file is procedure.

## Setup

Requires a Rust toolchain. Verification is local by policy; there is no CI.

```bash
just quick    # cargo fmt --check + clippy -D warnings
just check    # quick + cargo test + script tests, writes build/reports/<mode>/receipt.json
```

## Working in the repository

- One owner per area; the ownership table is in AGENTS.md. State the interface before a change that crosses areas.
- Keep worktrees under `.worktrees/`. On a machine with arbiter installed, follow the arbiter discipline while developing arbiter: wrap the session in `arbiter run --auto` or preflight with `arbiter check worktree:$PWD`, and use `arbiter git` for commit, checkout, clean, and worktree operations.
- Atomic, conventional commits (`fix:`, `docs:`, `chore:`); keep the repository functional at every step.

## Required checks

| Change | Required evidence |
|---|---|
| Docs, examples, README | `just quick` |
| `scripts/` | script tests via `just check` |
| Resource or store semantics | unit tests for overlap/mode/capacity, integration tests for cross-process conflict and release, a test that a dead holder is reaped, plus `just check` |
| Git guard or service protocol | operation-classification and branch-verification tests; fake-launcher coverage for service changes |

## Work records

Every non-trivial change lands with a dated entry in
[docs/architecture.md](docs/architecture.md) and a receipt in
`build/reports/` or tracked `evidence/`. A receipt states the objective,
source revision, commands with exit codes, observed results, and non-claims.
Conversation summaries are not receipts.

## Reporting issues

Open a GitHub issue for bugs and gaps. Security matters go through
[SECURITY.md](SECURITY.md) instead. Pull requests are reviewed before merge
to `main`.
