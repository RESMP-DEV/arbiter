use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};
use signal_hook::iterator::Signals;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::process;
use crate::resource::Resource;
use crate::store::{ClaimRequest, Store};
use crate::util;

pub struct RunOptions {
    pub command: Vec<std::ffi::OsString>,
    pub claims: Vec<Resource>,
    pub agent: String,
    pub agent_pid: Option<(u32, String)>,
    pub ttl: Duration,
    pub wait: Duration,
    pub reason: String,
}

pub fn run(store: &Store, options: RunOptions) -> Result<i32> {
    if options.command.is_empty() {
        return Err(Error::Usage("no command given; use -- <command>".into()));
    }
    let config = store.config().clone();
    let claims = Resource::dedupe(options.claims);
    let result = store.claim(ClaimRequest {
        agent: options.agent.clone(),
        resources: claims.clone(),
        ttl: options.ttl,
        wait: options.wait,
        reason: options.reason.clone(),
        pid: None,
        agent_pid: options.agent_pid.clone(),
        command: Some(render_command(&options.command)),
    })?;
    if result.waited >= Duration::from_millis(500) {
        eprintln!(
            "arbiter: acquired after waiting {}",
            util::format_duration(result.waited)
        );
    }

    let group_id = result.group_id.clone();
    let lease_ids = result
        .leases
        .iter()
        .map(|lease| lease.id.clone())
        .collect::<Vec<_>>()
        .join(",");

    store.log_event(
        "run.start",
        Some(&options.agent),
        Some(&group_id),
        Some(&claims),
        Some(serde_json::json!({
            "command": render_command(&options.command),
            "cwd": std::env::current_dir().ok().map(|p| p.display().to_string()),
        })),
    )?;

    let mut command = Command::new(&options.command[0]);
    command.args(&options.command[1..]);
    command.env("ARBITER_AGENT", &options.agent);
    command.env("ARBITER_LEASE_GROUP", &group_id);
    command.env("ARBITER_LEASE_IDS", &lease_ids);
    if let Ok(dir) = util::state_dir() {
        command.env("ARBITER_STATE_DIR", dir);
    }
    for claim in &claims {
        if let Some(path) = claim.path() {
            if claim.is_worktree() {
                command.env("ARBITER_WORKTREE", path);
            }
        }
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            let _ = store.release(&group_id, &options.agent, false);
            return Err(Error::Failed(format!(
                "cannot execute {}: {err}",
                options.command[0].to_string_lossy()
            )));
        }
    };
    let child_pid = child.id();
    if let Some(start) = process::proc_start(child_pid) {
        if let Err(err) = store.attach_pid(&group_id, child_pid, &start) {
            eprintln!("arbiter: warning: could not attach pid to lease: {err}");
        }
    }

    let stop = Arc::new(AtomicBool::new(false));
    let heartbeat = spawn_heartbeat(
        config.clone(),
        group_id.clone(),
        options.agent.clone(),
        options.ttl,
        stop.clone(),
    );

    let signal_handle = match Signals::new([SIGINT, SIGTERM, SIGHUP, SIGQUIT]) {
        Ok(mut signals) => {
            let handle = signals.handle();
            thread::spawn(move || {
                for signal in &mut signals {
                    // SIGINT already reaches a same-process-group child through
                    // the terminal; forwarding it again is harmless but noisy.
                    if signal == SIGINT {
                        continue;
                    }
                    unsafe {
                        libc::kill(child_pid as i32, signal);
                    }
                }
            });
            Some(handle)
        }
        Err(err) => {
            eprintln!("arbiter: warning: signal forwarding unavailable: {err}");
            None
        }
    };

    let status = child.wait();
    stop.store(true, Ordering::SeqCst);
    if let Some(handle) = signal_handle {
        handle.close();
    }
    let _ = heartbeat.join();

    let release_result = store.release(&group_id, &options.agent, false);
    let code = match &status {
        Ok(status) => status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)),
        Err(err) => {
            eprintln!("arbiter: waiting for child failed: {err}");
            1
        }
    };
    let _ = store.log_event(
        "run.exit",
        Some(&options.agent),
        Some(&group_id),
        Some(&claims),
        Some(serde_json::json!({ "exit_code": code, "release_ok": release_result.is_ok() })),
    );
    if let Err(err) = release_result {
        let already_gone = matches!(err, Error::Failed(_))
            && err.to_string().contains("no lease or group matches");
        if !already_gone {
            eprintln!("arbiter: warning: lease release failed: {err}");
        }
    }
    Ok(code)
}

fn spawn_heartbeat(
    config: Config,
    group_id: String,
    agent: String,
    ttl: Duration,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let interval = (ttl / 4).clamp(Duration::from_secs(2), Duration::from_secs(30));
        let Ok(store) = Store::open(config) else {
            return;
        };
        while !stop.load(Ordering::SeqCst) {
            let mut slept = Duration::ZERO;
            while slept < interval && !stop.load(Ordering::SeqCst) {
                let slice = Duration::from_millis(200).min(interval - slept);
                thread::sleep(slice);
                slept += slice;
            }
            if stop.load(Ordering::SeqCst) {
                break;
            }
            if store.renew(&group_id, &agent, ttl).is_err() {
                break;
            }
        }
    })
}

fn render_command(command: &[std::ffi::OsString]) -> String {
    command
        .iter()
        .map(|arg| shell_escape(&arg.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_escape(raw: &str) -> String {
    if raw.is_empty() {
        return "''".into();
    }
    if raw
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+".contains(c))
    {
        return raw.to_string();
    }
    format!("'{}'", raw.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_escape_roundtrips_simple() {
        assert_eq!(shell_escape("git"), "git");
        assert_eq!(shell_escape("a b"), "'a b'");
        assert_eq!(shell_escape("a'b"), "'a'\\''b'");
    }
}
