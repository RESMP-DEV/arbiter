# Launchd service-target repair — 2026-10-24

## Objective

Repair the Arbiter service restart protocol when activation of the Qwen3.8-27B
NVFP4 CCR fallback required all four CCR listeners to restart safely.

## Defect and source boundary

- Base revision: `dc0db7c453f7cc31762f115c38955a71e63539cf`.
- Changed files: `src/process.rs`, `tests/cli.rs`, this receipt, and the dated
  architecture entry.
- Before: `Command::args(["kickstart", "-k", &format!("gui/{}", uid()), label])`
  produced four argv entries and launchctl returned usage exit 64.
- After:
  `Command::args(["kickstart", "-k", &format!("gui/{}/{}", uid(), label)])`
  produces the required single `gui/<uid>/<label>` service target.
- The regression test's fake launcher now records its argv. The test asserts
  `kickstart`, `-k`, and one line beginning `gui/` and ending `/<label>`.

## Validation

- `cargo test --manifest-path Cargo.toml service_restart -- --nocapture`: pass.
- `cargo fmt --manifest-path Cargo.toml --check`: pass.
- `cargo build --release --manifest-path Cargo.toml`: pass.
- `just check`: pass, with the machine-readable result in
  `build/reports/local/receipt.json`.
- Release binary SHA-256 installed at `/Users/kearm/.local/bin/arbiter`:
  `2a3787208a1d3b8a692131ac0c685a250b579d5901564dafe80c2ab6f73e00ab`.
- `arbiter service restart com.kearm.ccr-rust --dry-run --json`: pass.

## Live activation evidence

After installing the repaired release binary, `ccr-service-restart` successfully
restarted all four configured CCR listeners through the idle-gated protocol:

| Service | Before PID | After PID | Idle seconds | Health |
| --- | ---: | ---: | ---: | --- |
| `com.kearm.ccr-rust` | 25148 | 29466 | 0.000523 | true |
| `com.kearm.ccr-glm-workers` | 31099 | 29566 | 0.000582 | true |
| `com.kearm.ccr-rust-auth` | 31131 | 29667 | 0.000454 | true |
| `com.kearm.ccr-ocr` | 83373 | 29770 | 0.000778 | true |

The failed pre-repair attempts did not restart a listener. One diagnostic direct
launchctl invocation restarted the main listener while its active-request gauges
were zero; that diagnostic exposed the exact argument boundary and is not used
as acceptance in place of the subsequent guarded run.

## Non-claims

- The automated test still uses a fake launcher and local HTTP server; it does
  not create a disposable launchd job.
- Live acceptance covered four configured macOS user LaunchAgents, not every
  launchd domain or service-target spelling.
- Lease, resource, git-guard, and scan semantics are unchanged.
