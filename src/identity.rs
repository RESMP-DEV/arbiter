use std::collections::BTreeMap;
use std::env;

use crate::process::{self, ProcInfo};
use crate::util;

#[derive(Clone, Debug)]
pub struct Agent {
    pub id: String,
    pub pid: u32,
    pub pid_start: Option<String>,
}

const HARNESSES: &[&str] = &[
    "codex",
    "claude",
    "zcode-cli",
    "qoder",
    "cursor-agent",
    "gemini",
    "aider",
    "opencode",
    "kimi",
    "grok",
    "crush",
];

pub fn resolve(explicit: Option<&str>) -> Agent {
    let tty = util::tty_name();
    if let Some(id) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return Agent {
            id: id.to_string(),
            pid: std::process::id(),
            pid_start: process::current_pid_start(),
        };
    }
    if let Ok(id) = env::var("ARBITER_AGENT") {
        let id = id.trim().to_string();
        if !id.is_empty() {
            return Agent {
                id,
                pid: std::process::id(),
                pid_start: process::current_pid_start(),
            };
        }
    }

    let table = process::ps_table();
    let ancestry = process::ancestors(std::process::id(), &table);
    let nearest_harness = ancestry
        .iter()
        .find_map(|pid| harness_for(*pid, &table))
        .map(|(pid, kind)| (pid, kind.to_string()));
    let worker_ancestor = ancestry.iter().any(|pid| {
        table
            .get(pid)
            .map(|info| is_worker(&info.command))
            .unwrap_or(false)
    });

    let env_session = [
        ("CODEX_THREAD_ID", "codex"),
        ("CODEX_SESSION_ID", "codex"),
        ("CLAUDE_SESSION_ID", "claude"),
        ("CLAUDE_CODE_SESSION_ID", "claude"),
        ("ZCODE_SESSION_ID", "zcode"),
        ("ZCODE_THREAD_ID", "zcode"),
        ("QODER_SESSION_ID", "qoder"),
        ("QODER_THREAD_ID", "qoder"),
    ]
    .iter()
    .find_map(|(key, kind)| {
        env::var(key)
            .ok()
            .map(|value| (value.trim().to_string(), kind.to_string()))
            .filter(|(value, _)| !value.is_empty())
    });

    if let Some((session, kind)) = env_session {
        let kind = if worker_ancestor {
            "ccr-worker"
        } else {
            kind.as_str()
        };
        let pid = nearest_harness
            .as_ref()
            .map(|(pid, _)| *pid)
            .unwrap_or_else(std::process::id);
        return Agent {
            id: format!("{kind}:{session}"),
            pid,
            pid_start: process::proc_start(pid),
        };
    }

    if let Some((pid, kind)) = nearest_harness {
        return Agent {
            id: format!("{kind}:pid{pid}"),
            pid,
            pid_start: process::proc_start(pid),
        };
    }

    let user = util::current_user();
    let host = util::hostname();
    let suffix = tty
        .as_deref()
        .map(|value| value.trim_start_matches("/dev/").replace('/', "-"))
        .unwrap_or_else(|| format!("pid{}", std::process::id()));
    Agent {
        id: format!("human:{user}@{host}:{suffix}"),
        pid: std::process::id(),
        pid_start: process::current_pid_start(),
    }
}

fn harness_for(pid: u32, table: &BTreeMap<u32, ProcInfo>) -> Option<(u32, &'static str)> {
    let info = table.get(&pid)?;
    let kind = harness_kind(&info.command)?;
    Some((pid, kind))
}

fn noisy_command(command: &str) -> bool {
    // Desktop app helpers, extension hosts, and app-server daemons share a
    // parent with real sessions but are not independent workspaces.
    command.contains("Helper")
        || command.contains("app-server")
        || command.contains("chrome-extension")
        || command.contains("qoder-search.bundle")
        || command.contains("qoder-worker-runtime")
        || command.contains("qoder-agent-sdk")
        || command.contains(" --eval ")
        || command.contains("/.qoder/plugins/")
        || command.contains("/.codex/plugins/")
}

fn harness_static(harness: &str) -> &'static str {
    match harness {
        "codex" => "codex",
        "claude" => "claude",
        "zcode-cli" => "zcode",
        "qoder" => "qoder",
        "cursor-agent" => "cursor-agent",
        "gemini" => "gemini",
        "aider" => "aider",
        "opencode" => "opencode",
        "kimi" => "kimi",
        "grok" => "grok",
        "crush" => "crush",
        _ => "agent",
    }
}

pub fn is_worker(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    lower.contains("ccr-worker") || lower.contains("ccr_worker")
}

pub fn harness_kind(command: &str) -> Option<&'static str> {
    if noisy_command(command) {
        return None;
    }
    let base = process::command_basename(command);
    for harness in HARNESSES {
        if base == *harness {
            return Some(harness_static(harness));
        }
    }
    if is_worker(command) {
        return Some("ccr-worker");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_agent_wins() {
        let agent = resolve(Some("test:unit"));
        assert_eq!(agent.id, "test:unit");
    }

    #[test]
    fn detects_worker_commands() {
        assert!(is_worker("/Users/kearm/.local/bin/ccr-worker --consumer x"));
        assert!(is_worker("python ccr_worker.py"));
        assert!(!is_worker("codex exec"));
    }

    #[test]
    fn harness_detection_uses_basename() {
        assert_eq!(harness_kind("/opt/homebrew/bin/codex --x"), Some("codex"));
        assert_eq!(harness_kind("zcode-cli"), Some("zcode"));
        assert_eq!(harness_kind("python receiver.py"), None);
    }
}
