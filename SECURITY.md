# Security policy

## Scope

Arbiter is a single-user, machine-local CLI. It listens on no socket, holds
no credentials, and writes one SQLite database under `~/.local/state/arbiter/`.

The threat model is accidental collisions between cooperating agents, not
adversarial processes. Enforcement exists only at the `arbiter run` and
`arbiter git` wrappers plus the opt-in git shim, and detection is read-only.
Arbiter does not sandbox hostile code and must not be relied on for
isolation against a malicious process.

## Reporting

Report vulnerabilities through a private GitHub security advisory on
RESMP-DEV/arbiter rather than a public issue.

## Supported versions

The latest release tag and `main`.
