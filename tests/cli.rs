use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.toml"), "").unwrap();
        Self { dir }
    }

    fn state(&self) -> PathBuf {
        self.dir.path().join("state")
    }

    fn config(&self) -> PathBuf {
        self.dir.path().join("config.toml")
    }

    fn arb(&self, agent: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_arbiter"));
        command
            .arg("--state-dir")
            .arg(self.state())
            .arg("--config")
            .arg(self.config())
            .arg("--agent")
            .arg(agent)
            .env("ARBITER_CONFIG", self.config());
        command
    }

    fn run(&self, agent: &str, args: &[&str]) -> Output {
        self.arb(agent).args(args).output().unwrap()
    }

    fn json(&self, agent: &str, args: &[&str]) -> Value {
        let output = self.arb(agent).arg("--json").args(args).output().unwrap();
        assert!(
            output.status.success(),
            "command failed: {:?}\nstdout: {}\nstderr: {}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn status(&self) -> Value {
        self.json("test:observer", &["status"])
    }

    fn lease_count(&self) -> usize {
        self.status()["leases"].as_array().unwrap().len()
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

fn group_of(fixture: &Fixture, agent: &str) -> String {
    let status = fixture.json(agent, &["status"]);
    status["leases"][0]["group_id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn init_repo(root: &Path) {
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "arbiter@example.invalid"]);
    git(&["config", "user.name", "Arbiter Test"]);
    fs::write(root.join("file.txt"), "one\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "init"]);
}

#[test]
fn claim_conflict_and_release() {
    let fixture = Fixture::new();
    let output = fixture.run("codex:A", &["claim", "name:shared", "--reason", "first"]);
    assert!(output.status.success(), "{}", stderr(&output));

    let output = fixture.run("claude:B", &["claim", "name:shared"]);
    assert_eq!(output.status.code(), Some(3));
    assert!(
        stderr(&output).contains("held by codex:A"),
        "{}",
        stderr(&output)
    );
    assert_eq!(fixture.lease_count(), 1);

    let group = group_of(&fixture, "codex:A");
    let output = fixture.run("codex:A", &["release", &group]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(fixture.lease_count(), 0);
}

#[test]
fn shared_claims_coexist_with_exclusive_conflict() {
    let fixture = Fixture::new();
    let path = fixture.dir.path().join("work");
    fs::create_dir(&path).unwrap();
    let spec_ro = format!("path:{}:ro", path.display());
    let spec_rw = format!("path:{}", path.display());

    assert!(fixture.run("a", &["claim", &spec_ro]).status.success());
    assert!(fixture.run("b", &["claim", &spec_ro]).status.success());
    let output = fixture.run("c", &["claim", &spec_rw]);
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(fixture.lease_count(), 2);
}

#[test]
fn run_propagates_exit_and_releases() {
    let fixture = Fixture::new();
    let output = fixture.run(
        "codex:A",
        &["run", "--claim", "name:x", "--", "sh", "-c", "exit 7"],
    );
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(fixture.lease_count(), 0);
}

#[test]
fn run_injects_lease_environment() {
    let fixture = Fixture::new();
    let output = fixture.run(
        "codex:A",
        &[
            "run",
            "--claim",
            "name:env",
            "--",
            "sh",
            "-c",
            "test -n \"$ARBITER_AGENT\" && test -n \"$ARBITER_LEASE_GROUP\"",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
}

#[test]
fn conflict_prevents_child_execution() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .run("codex:A", &["claim", "name:x"])
            .status
            .success()
    );
    let marker = fixture.dir.path().join("marker");
    let output = fixture.run(
        "claude:B",
        &[
            "run",
            "--claim",
            "name:x",
            "--",
            "sh",
            "-c",
            &format!("touch {}", marker.display()),
        ],
    );
    assert_eq!(output.status.code(), Some(3));
    assert!(!marker.exists());
}

#[test]
fn waiter_acquires_after_release() {
    let fixture = Fixture::new();
    assert!(fixture.run("a", &["claim", "name:queued"]).status.success());
    let mut waiter = fixture
        .arb("b")
        .args(["claim", "name:queued", "--wait", "5s"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(400));
    let group = group_of(&fixture, "a");
    assert!(fixture.run("a", &["release", &group]).status.success());
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        if waiter.try_wait().unwrap().is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "waiter did not acquire");
        thread::sleep(Duration::from_millis(100));
    }
    let output = waiter.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(fixture.lease_count(), 1);
}

#[test]
fn dead_holder_is_reaped() {
    let fixture = Fixture::new();
    let mut holder = fixture
        .arb("a")
        .args(["run", "--claim", "name:dead", "--", "sleep", "30"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let lease = loop {
        let status = fixture.status();
        if let Some(lease) = status["leases"].as_array().and_then(|l| l.first()) {
            if lease["pid"].as_u64().is_some() {
                break lease.clone();
            }
        }
        assert!(Instant::now() < deadline, "lease never acquired");
        thread::sleep(Duration::from_millis(50));
    };
    let child_pid = lease["pid"].as_u64().unwrap() as i32;
    unsafe {
        libc::kill(child_pid, libc::SIGKILL);
    }
    let _ = holder.kill();
    let _ = holder.wait();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if fixture.lease_count() == 0 {
            return;
        }
        assert!(Instant::now() < deadline, "dead lease was not reaped");
        thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn pool_capacity_is_enforced() {
    let fixture = Fixture::new();
    fs::write(fixture.config(), "[pools]\nbuild = 2\n").unwrap();
    assert!(fixture.run("a", &["claim", "pool:build"]).status.success());
    assert!(fixture.run("b", &["claim", "pool:build"]).status.success());
    let output = fixture.run("c", &["claim", "pool:build"]);
    assert_eq!(output.status.code(), Some(3));
    assert!(
        stderr(&output).contains("pool is full"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn git_guard_denies_cross_agent_checkout() {
    let fixture = Fixture::new();
    let repo = fixture.dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    init_repo(&repo);
    let spec = format!("worktree:{}", repo.display());
    assert!(fixture.run("codex:A", &["claim", &spec]).status.success());

    let output = fixture.run(
        "claude:B",
        &[
            "git",
            "-C",
            repo.to_str().unwrap(),
            "checkout",
            "-b",
            "stolen",
        ],
    );
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("held by codex:A"),
        "{}",
        stderr(&output)
    );

    let output = fixture.run(
        "claude:B",
        &["git", "-C", repo.to_str().unwrap(), "status", "--short"],
    );
    assert!(output.status.success(), "{}", stderr(&output));
}

#[test]
fn git_guard_verifies_branch_before_commit() {
    let fixture = Fixture::new();
    let repo = fixture.dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    init_repo(&repo);
    let spec = format!("worktree:{}", repo.display());
    assert!(fixture.run("codex:A", &["claim", &spec]).status.success());

    let output = fixture.run(
        "codex:A",
        &[
            "git",
            "-C",
            repo.to_str().unwrap(),
            "commit",
            "--allow-empty",
            "-m",
            "ok",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));

    let status = Command::new("git")
        .args(["checkout", "-q", "-b", "flipped"])
        .current_dir(&repo)
        .status()
        .unwrap();
    assert!(status.success());

    let output = fixture.run(
        "codex:A",
        &[
            "git",
            "-C",
            repo.to_str().unwrap(),
            "commit",
            "--allow-empty",
            "-m",
            "nope",
        ],
    );
    assert_eq!(output.status.code(), Some(3));
    assert!(
        stderr(&output).contains("HEAD is on"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn git_clean_requires_a_claim() {
    let fixture = Fixture::new();
    let repo = fixture.dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    init_repo(&repo);
    fs::write(repo.join("untracked.txt"), "x").unwrap();

    let output = fixture.run("a", &["git", "-C", repo.to_str().unwrap(), "clean", "-n"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let output = fixture.run("a", &["git", "-C", repo.to_str().unwrap(), "clean", "-fd"]);
    assert_eq!(output.status.code(), Some(3));
    assert!(repo.join("untracked.txt").exists());
}

#[test]
fn scan_detects_two_live_sessions_in_one_worktree() {
    let fixture = Fixture::new();
    let repo = fixture.dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    init_repo(&repo);
    let bin = fixture.dir.path().join("bin");
    fs::create_dir(&bin).unwrap();
    std::os::unix::fs::symlink("/bin/sleep", bin.join("codex")).unwrap();
    std::os::unix::fs::symlink("/bin/sleep", bin.join("claude")).unwrap();

    let mut first = Command::new(bin.join("codex"))
        .arg("20")
        .current_dir(&repo)
        .spawn()
        .unwrap();
    let mut second = Command::new(bin.join("claude"))
        .arg("20")
        .current_dir(&repo)
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(400));

    let output = fixture.run(
        "observer",
        &[
            "--json",
            "scan",
            "--root",
            repo.to_str().unwrap(),
            "--no-dirty",
            "--check",
        ],
    );
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let codes: Vec<&str> = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|finding| finding["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"shared_worktree"), "findings: {codes:?}");

    let _ = first.kill();
    let _ = second.kill();
    let _ = first.wait();
    let _ = second.wait();
}

#[test]
fn scan_is_clean_for_a_single_claim() {
    let fixture = Fixture::new();
    let repo = fixture.dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    init_repo(&repo);
    let spec = format!("worktree:{}", repo.display());
    assert!(fixture.run("a", &["claim", &spec]).status.success());
    let output = fixture.run(
        "observer",
        &[
            "--json",
            "scan",
            "--root",
            repo.to_str().unwrap(),
            "--no-dirty",
            "--check",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
}

#[test]
fn service_dry_run_does_not_claim_or_restart() {
    let fixture = Fixture::new();
    fs::write(
        fixture.config(),
        r#"
[services."demo"]
port = 18123
health_url = "http://127.0.0.1:18123/health"
"#,
    )
    .unwrap();
    let output = fixture.run("a", &["service", "restart", "demo", "--dry-run"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(fixture.lease_count(), 0);
}

#[test]
fn init_writes_repo_config() {
    let fixture = Fixture::new();
    let repo = fixture.dir.path().join("project");
    fs::create_dir(&repo).unwrap();
    let output = fixture.run("a", &["init", "--dir", repo.to_str().unwrap()]);
    assert!(output.status.success(), "{}", stderr(&output));
    let config = fs::read_to_string(repo.join(".arbiter.toml")).unwrap();
    assert!(config.contains("[project]"));
    let output = fixture.run("a", &["init", "--dir", repo.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(5));
}

#[test]
fn shim_install_links_arbiter() {
    let fixture = Fixture::new();
    let bin = fixture.dir.path().join("shim");
    let output = fixture.run("a", &["shim", "install", "--dir", bin.to_str().unwrap()]);
    assert!(output.status.success(), "{}", stderr(&output));
    let link = bin.join("git");
    assert!(link.is_symlink());
    let target = fs::canonicalize(&link).unwrap();
    assert_eq!(
        target,
        fs::canonicalize(env!("CARGO_BIN_EXE_arbiter")).unwrap()
    );
    let output = fixture.run(
        "a",
        &["shim", "uninstall", "--path", link.to_str().unwrap()],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!link.exists());
}

#[test]
fn status_here_filters_unrelated_paths() {
    let fixture = Fixture::new();
    let here = fixture.dir.path().join("here");
    let elsewhere = fixture.dir.path().join("elsewhere");
    fs::create_dir(&here).unwrap();
    fs::create_dir(&elsewhere).unwrap();
    assert!(
        fixture
            .run("a", &["claim", &format!("path:{}", here.display())])
            .status
            .success()
    );
    assert!(
        fixture
            .run("b", &["claim", &format!("path:{}", elsewhere.display())])
            .status
            .success()
    );
    let output = fixture
        .arb("observer")
        .args(["--json", "status", "--here"])
        .current_dir(&here)
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["leases"].as_array().unwrap().len(), 1);
}

#[test]
fn worktree_claims_do_not_cover_sibling_worktrees() {
    let fixture = Fixture::new();
    let repo = fixture.dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    init_repo(&repo);
    let linked = repo.join(".worktrees/other");
    let status = Command::new("git")
        .args([
            "worktree",
            "add",
            "-q",
            "-b",
            "other",
            linked.to_str().unwrap(),
        ])
        .current_dir(&repo)
        .status()
        .unwrap();
    assert!(status.success());

    assert!(
        fixture
            .run("a", &["claim", &format!("worktree:{}", repo.display())])
            .status
            .success()
    );
    let output = fixture.run("b", &["claim", &format!("worktree:{}", linked.display())]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(fixture.lease_count(), 2);
}

#[test]
fn git_checkout_updates_the_session_branch() {
    let fixture = Fixture::new();
    let repo = fixture.dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    init_repo(&repo);
    assert!(
        fixture
            .run(
                "codex:A",
                &["claim", &format!("worktree:{}", repo.display())]
            )
            .status
            .success()
    );
    let output = fixture.run(
        "codex:A",
        &[
            "git",
            "-C",
            repo.to_str().unwrap(),
            "checkout",
            "-q",
            "-b",
            "next",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let output = fixture.run(
        "codex:A",
        &[
            "git",
            "-C",
            repo.to_str().unwrap(),
            "commit",
            "--allow-empty",
            "-m",
            "on next",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
}

#[test]
fn git_clean_refuses_a_live_agent_in_the_worktree() {
    let fixture = Fixture::new();
    let repo = fixture.dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    init_repo(&repo);
    fs::write(repo.join("untracked.txt"), "x").unwrap();
    let bin = fixture.dir.path().join("bin");
    fs::create_dir(&bin).unwrap();
    std::os::unix::fs::symlink("/bin/sleep", bin.join("codex")).unwrap();
    let mut live = Command::new(bin.join("codex"))
        .arg("20")
        .current_dir(&repo)
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(300));

    assert!(
        fixture
            .run("a", &["claim", &format!("worktree:{}", repo.display())])
            .status
            .success()
    );
    let output = fixture.run("a", &["git", "-C", repo.to_str().unwrap(), "clean", "-fd"]);
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(repo.join("untracked.txt").exists());

    let _ = live.kill();
    let _ = live.wait();
}

#[test]
fn service_restart_parses_prometheus_idle_metrics() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let fixture = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let server = thread::spawn(move || {
        while !stop_thread.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut buffer = [0u8; 1024];
                    let _ = stream.read(&mut buffer);
                    let request = String::from_utf8_lossy(&buffer);
                    let (status, body) = if request.starts_with("GET /metrics") {
                        ("200 OK", "ccr_active_requests 0\n")
                    } else if request.starts_with("GET /health") {
                        ("200 OK", "ok")
                    } else {
                        ("404 Not Found", "no")
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });

    let pid_file = fixture.dir.path().join("launchd.pid");
    let script = fixture.dir.path().join("launchctl");
    fs::write(
        &script,
        format!(
            r#"#!/bin/bash
state="{}"
if [ "$1" = "list" ]; then
  if [ -f "$state" ]; then echo "\"PID\" = $(cat "$state");"; else echo "\"PID\" = 0;"; fi
  exit 0
fi
if [ "$1" = "kickstart" ]; then
  echo 4242 > "$state"
  exit 0
fi
exit 1
"#,
            pid_file.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        fixture.config(),
        format!(
            r#"[services."demo"]
port = {port}
idle_url = "http://127.0.0.1:{port}/metrics"
idle_key = "ccr_active_requests"
health_url = "http://127.0.0.1:{port}/health"
idle_timeout = "2s"
health_timeout = "2s"
"#
        ),
    )
    .unwrap();

    let output = fixture
        .arb("a")
        .env("ARBITER_LAUNCHCTL_BIN", &script)
        .args(["--json", "service", "restart", "demo"])
        .output()
        .unwrap();
    stop.store(true, Ordering::SeqCst);
    let _ = server.join();
    assert!(output.status.success(), "{}", stderr(&output));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["after_pid"], 4242);
    assert_eq!(report["dry_run"], false);
    assert_eq!(fixture.lease_count(), 0);
}
