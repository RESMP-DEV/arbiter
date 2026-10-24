use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use crate::config::{Config, RepoConfig};
use crate::error::Result;
use crate::gitguard::{self};
use crate::identity;
use crate::process::{self, ProcInfo};
use crate::resource::{Kind, Mode};
use crate::store::{Lease, Store};
use crate::util;

#[derive(Clone, Debug, Default)]
pub struct ScanOptions {
    pub roots: Vec<PathBuf>,
    pub include_dirty: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warn,
    Info,
}

#[derive(Clone, Debug, Serialize)]
pub struct Finding {
    pub severity: Severity,
    pub code: String,
    pub message: String,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AgentProc {
    pub pid: u32,
    pub ppid: u32,
    pub root_pid: u32,
    pub kind: String,
    pub command: String,
    pub cwd: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ClaimRef {
    pub agent: String,
    pub resource: String,
    pub kind: Kind,
    pub mode: Mode,
    pub holder_pid: Option<u32>,
    pub session_pid: Option<u32>,
}

impl ClaimRef {
    fn from_lease(lease: &Lease) -> Self {
        Self {
            agent: lease.agent.clone(),
            resource: lease.resource().to_string(),
            kind: lease.kind,
            mode: lease.mode,
            holder_pid: lease.pid,
            session_pid: lease
                .meta
                .get("agent_pid")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32),
        }
    }

    fn matches(&self, agent: &AgentProc) -> bool {
        [self.holder_pid, self.session_pid]
            .into_iter()
            .flatten()
            .any(|pid| pid == agent.pid || pid == agent.root_pid)
    }

    fn exclusive_worktree(&self) -> bool {
        self.kind == Kind::Path && self.mode == Mode::Exclusive
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct WorktreeReport {
    pub path: PathBuf,
    pub branch: String,
    pub head: Option<String>,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
    pub dirty: Option<usize>,
    pub claims: Vec<ClaimRef>,
    pub agents: Vec<AgentProc>,
    pub prunable: Option<String>,
}

impl WorktreeReport {
    fn claimed_by(&self) -> String {
        if self.claims.is_empty() {
            "-".into()
        } else {
            self.claims
                .iter()
                .map(|claim| claim.agent.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
                .join(",")
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PortReport {
    pub port: u16,
    pub proto: String,
    pub host: String,
    pub pid: u32,
    pub command: String,
    pub agent: Option<String>,
    pub claimed_by: Option<String>,
    pub known: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ServiceReport {
    pub label: String,
    pub launchd_label: String,
    pub pid: Option<u32>,
    pub port: Option<u16>,
    pub claimed_by: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LockReport {
    pub path: PathBuf,
    pub age_secs: f64,
    pub stale: bool,
    pub git_process: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ScanReport {
    pub generated_at: f64,
    pub host: String,
    pub leases: Vec<Lease>,
    pub waiters: Vec<crate::store::Waiter>,
    pub worktrees: Vec<WorktreeReport>,
    pub ports: Vec<PortReport>,
    pub services: Vec<ServiceReport>,
    pub locks: Vec<LockReport>,
    pub agents: Vec<AgentProc>,
    pub findings: Vec<Finding>,
}

impl ScanReport {
    pub fn has_errors(&self) -> bool {
        self.findings
            .iter()
            .any(|finding| finding.severity == Severity::Error)
    }

    pub fn has_warnings(&self) -> bool {
        self.findings
            .iter()
            .any(|finding| finding.severity == Severity::Warn)
    }
}

pub fn scan(store: &Store, options: &ScanOptions) -> Result<ScanReport> {
    let config = store.config();
    let leases = store.active()?;
    let waiters = store.waiters()?;
    let agents = collect_agents();
    let table = process::ps_table();
    let mut findings = Vec::new();

    let roots = if options.roots.is_empty() {
        if config.scan.roots.is_empty() {
            vec![std::env::current_dir()?]
        } else {
            config.scan.roots.clone()
        }
    } else {
        options.roots.clone()
    };

    let git = gitguard::real_git(config).ok();
    let mut worktrees = Vec::new();
    let mut repo_ports: BTreeSet<u16> = BTreeSet::new();
    let mut repo_services: BTreeSet<String> = BTreeSet::new();
    let mut seen = BTreeSet::new();

    if let Some(git) = &git {
        for root in &roots {
            let root = util::canonicalize_lenient(root).unwrap_or_else(|_| root.clone());
            let Some(info) = gitguard::detect(&root, git) else {
                continue;
            };
            if let Ok(Some(repo_config)) = RepoConfig::load(&info.worktree) {
                repo_ports.extend(repo_config.resources.ports.iter().copied());
                repo_services.extend(repo_config.resources.services.iter().cloned());
            }
            for raw in worktree_list(git, &info.worktree) {
                if !seen.insert(raw.path.clone()) {
                    continue;
                }
                let include_dirty = options.include_dirty.unwrap_or(config.scan.include_dirty);
                let dirty = if include_dirty {
                    dirty_count(git, &raw.path)
                } else {
                    None
                };
                let wt_agents: Vec<AgentProc> = agents
                    .iter()
                    .filter(|agent| {
                        agent
                            .cwd
                            .as_ref()
                            .map(|cwd| cwd.starts_with(&raw.path))
                            .unwrap_or(false)
                    })
                    .cloned()
                    .collect();
                let claims: Vec<ClaimRef> = leases
                    .iter()
                    .filter(|lease| {
                        claim_covers_worktree(lease, &raw.path, &raw.common_dir, config)
                    })
                    .map(ClaimRef::from_lease)
                    .collect();
                worktrees.push(WorktreeReport {
                    path: raw.path.clone(),
                    branch: raw.branch.clone(),
                    head: raw.head.clone(),
                    git_dir: raw.git_dir.clone(),
                    common_dir: raw.common_dir.clone(),
                    dirty,
                    claims,
                    agents: wt_agents,
                    prunable: raw.prunable.clone(),
                });
            }
        }
    }

    add_worktree_findings(&worktrees, &mut findings);

    let known_ports: BTreeSet<u16> = config
        .scan
        .known_ports
        .iter()
        .copied()
        .chain(repo_ports.iter().copied())
        .chain(config.services.values().filter_map(|s| s.port))
        .collect();

    let ports = collect_ports(&leases, &table, &known_ports, &repo_ports, &mut findings);

    let mut services = Vec::new();
    let mut service_labels: BTreeSet<String> = config.services.keys().cloned().collect();
    service_labels.extend(repo_services);
    for label in service_labels {
        let launchd_label = config
            .services
            .get(&label)
            .map(|service| service.label(&label).to_string())
            .unwrap_or_else(|| label.clone());
        let pid = process::launchctl_pid(&launchd_label);
        let claimed_by = leases
            .iter()
            .find(|lease| lease.kind == Kind::Service && lease.key == label)
            .map(|lease| lease.agent.clone());
        if claimed_by.is_some() && pid.is_none() {
            findings.push(Finding {
                severity: Severity::Warn,
                code: "service_claimed_not_running".into(),
                message: format!(
                    "service {label} is claimed by {} but launchd reports no running PID",
                    claimed_by.as_deref().unwrap_or("?")
                ),
                detail: Some(launchd_label.clone()),
            });
        }
        services.push(ServiceReport {
            label: label.clone(),
            launchd_label,
            pid,
            port: config.services.get(&label).and_then(|s| s.port),
            claimed_by,
        });
    }

    let mut locks = Vec::new();
    for wt in &worktrees {
        for lock in lock_files(&wt.git_dir, &wt.common_dir) {
            let age_secs = lock
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|m| m.elapsed().ok())
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            let stale = age_secs >= config.scan.stale_lock_after.as_secs_f64();
            let git_process = git_process_in(&wt.path, &wt.common_dir, &table);
            if stale && !git_process {
                findings.push(Finding {
                    severity: Severity::Warn,
                    code: "stale_git_lock".into(),
                    message: format!(
                        "{} has a stale git lock; no git process is active in this worktree",
                        lock.display()
                    ),
                    detail: Some(format!("age {}", util::format_age(age_secs))),
                });
            }
            locks.push(LockReport {
                path: lock,
                age_secs,
                stale,
                git_process,
            });
        }
    }

    for lease in &leases {
        if lease.kind == Kind::Port && lease.created_at + 60.0 < util::now() {
            let port_number = lease
                .key
                .split('/')
                .next()
                .and_then(|v| v.parse::<u16>().ok());
            let live = port_number
                .map(|number| ports.iter().any(|port| port.port == number))
                .unwrap_or(false);
            if !live {
                findings.push(Finding {
                    severity: Severity::Warn,
                    code: "claimed_port_not_listening".into(),
                    message: format!(
                        "{} claims port {} but no listener is active",
                        lease.agent, lease.display
                    ),
                    detail: Some(format!("lease {}", lease.id)),
                });
            }
        }
    }

    if !waiters.is_empty() {
        findings.push(Finding {
            severity: Severity::Info,
            code: "waiters_pending".into(),
            message: format!("{} claim waiter(s) are queued", waiters.len()),
            detail: None,
        });
    }

    Ok(ScanReport {
        generated_at: util::now(),
        host: util::hostname(),
        leases,
        waiters,
        worktrees,
        ports,
        services,
        locks,
        agents,
        findings,
    })
}

#[derive(Clone, Debug)]
struct RawWorktree {
    path: PathBuf,
    branch: String,
    head: Option<String>,
    git_dir: PathBuf,
    common_dir: PathBuf,
    prunable: Option<String>,
}

fn worktree_list(git: &Path, repo: &Path) -> Vec<RawWorktree> {
    let output = Command::new(git)
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo)
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut out = Vec::new();
    let mut current: Option<RawWorktree> = None;
    for line in text.lines() {
        if line.trim().is_empty() {
            finish_worktree(&mut current, &mut out);
            continue;
        }
        if let Some(path) = line.strip_prefix("worktree ") {
            finish_worktree(&mut current, &mut out);
            let path =
                util::canonicalize_lenient(Path::new(path)).unwrap_or_else(|_| PathBuf::from(path));
            let git_dir = git_dir(git, &path).unwrap_or_else(|| PathBuf::from(".git"));
            let common_dir = common_dir_from_gitdir(&git_dir);
            current = Some(RawWorktree {
                path,
                branch: "HEAD".into(),
                head: None,
                git_dir,
                common_dir,
                prunable: None,
            });
        } else if let Some(head) = line.strip_prefix("HEAD ") {
            if let Some(wt) = current.as_mut() {
                wt.head = Some(head.to_string());
            }
        } else if let Some(branch) = line.strip_prefix("branch ") {
            if let Some(wt) = current.as_mut() {
                wt.branch = branch
                    .strip_prefix("refs/heads/")
                    .unwrap_or(branch)
                    .to_string();
            }
        } else if let Some(reason) = line.strip_prefix("prunable ") {
            if let Some(wt) = current.as_mut() {
                wt.prunable = Some(reason.to_string());
            }
        }
    }
    finish_worktree(&mut current, &mut out);
    out
}

fn finish_worktree(current: &mut Option<RawWorktree>, out: &mut Vec<RawWorktree>) {
    if let Some(wt) = current.take() {
        out.push(wt);
    }
}

fn git_dir(git: &Path, worktree: &Path) -> Option<PathBuf> {
    let output = Command::new(git)
        .args(["rev-parse", "--path-format=absolute", "--git-dir"])
        .current_dir(worktree)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(PathBuf::from(text))
    }
}

fn common_dir_from_gitdir(gitdir: &Path) -> PathBuf {
    if let Some(parent) = gitdir.parent() {
        if parent
            .file_name()
            .map(|n| n == "worktrees")
            .unwrap_or(false)
        {
            if let Some(common) = parent.parent() {
                return common.to_path_buf();
            }
        }
    }
    gitdir.to_path_buf()
}

fn dirty_count(git: &Path, worktree: &Path) -> Option<usize> {
    let output = Command::new(git)
        .args(["status", "--porcelain", "--untracked-files=normal"])
        .current_dir(worktree)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).lines().count())
}

fn lock_files(gitdir: &Path, common_dir: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for base in [gitdir, common_dir] {
        for name in ["index.lock", "HEAD.lock", "packed-refs.lock", "config.lock"] {
            candidates.push(base.join(name));
        }
    }
    candidates.sort();
    candidates.dedup();
    candidates
        .into_iter()
        .filter(|path| path.exists())
        .collect()
}

fn git_process_in(worktree: &Path, common: &Path, table: &BTreeMap<u32, ProcInfo>) -> bool {
    for (pid, info) in table {
        if process::command_basename(&info.command) != "git" {
            continue;
        }
        if let Some(cwd) = process::cwd_of(*pid) {
            if cwd.starts_with(worktree) || cwd.starts_with(common) {
                return true;
            }
        }
    }
    false
}

fn claim_covers_worktree(lease: &Lease, worktree: &Path, common: &Path, config: &Config) -> bool {
    match lease.kind {
        Kind::Path => {
            let key = worktree.to_string_lossy().to_string();
            let resource = lease.resource();
            crate::resource::paths_overlap(
                &resource.key,
                &key,
                config.paths.is_case_insensitive(),
                resource.exact,
                true,
            )
        }
        Kind::Refs => {
            let key = common.to_string_lossy().to_string();
            lease.key == key
        }
        _ => false,
    }
}

fn collect_agents() -> Vec<AgentProc> {
    let table = process::ps_table();
    let mut roots = BTreeMap::new();
    for (pid, info) in &table {
        if identity::harness_kind(&info.command).is_none() {
            continue;
        }
        // A worker launcher and the codex process it spawns are one session.
        // Otherwise each harness process is its own session root; rolling up
        // to a desktop app ancestor would merge unrelated sessions.
        let ancestry = process::ancestors(*pid, &table);
        let worker_root = ancestry
            .iter()
            .rev()
            .find(|ancestor| {
                table
                    .get(ancestor)
                    .map(|info| identity::is_worker(&info.command))
                    .unwrap_or(false)
            })
            .copied();
        let root = if identity::is_worker(
            &table
                .get(pid)
                .map(|info| info.command.clone())
                .unwrap_or_default(),
        ) {
            *pid
        } else {
            worker_root.unwrap_or(*pid)
        };
        roots.insert(*pid, root);
    }
    let mut out = Vec::new();
    for (pid, info) in &table {
        let Some(root) = roots.get(pid) else {
            continue;
        };
        let Some(cwd) = process::cwd_of(*pid) else {
            continue;
        };
        let kind = identity::harness_kind(&info.command).unwrap_or("agent");
        out.push(AgentProc {
            pid: *pid,
            ppid: info.ppid,
            root_pid: *root,
            kind: kind.to_string(),
            command: util::truncate(&util::one_line(&info.command), 160),
            cwd: Some(cwd),
        });
    }
    out.sort_by_key(|agent| agent.pid);
    out
}

fn collect_ports(
    leases: &[Lease],
    table: &BTreeMap<u32, ProcInfo>,
    known_ports: &BTreeSet<u16>,
    repo_ports: &BTreeSet<u16>,
    findings: &mut Vec<Finding>,
) -> Vec<PortReport> {
    let listeners = process::listeners();
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for listener in listeners {
        if !seen.insert((listener.port, listener.proto.clone(), listener.pid)) {
            continue;
        }
        let port_key = format!("{}/{}", listener.port, listener.proto);
        let claim = leases.iter().find(|lease| {
            lease.kind == Kind::Port
                && lease.key == port_key
                && hosts_overlap(lease.host.as_deref().unwrap_or("*"), &listener.host)
        });
        let agent = agent_for_pid(listener.pid, table);
        let known = known_ports.contains(&listener.port);
        if claim.is_none() && known {
            // A project-declared port that nobody claimed is actionable; a
            // long-running launchd or system listener is expected and stays
            // informational.
            let severity = if repo_ports.contains(&listener.port) {
                Severity::Warn
            } else {
                Severity::Info
            };
            findings.push(Finding {
                severity,
                code: "unclaimed_listener".into(),
                message: format!(
                    "known port {} is listening ({} pid {}) but no agent holds a claim",
                    listener.port, listener.command, listener.pid
                ),
                detail: agent.as_ref().map(|(name, _)| name.clone()),
            });
        }
        if let (Some(claim), Some((agent_name, agent_pid))) = (claim, agent.as_ref()) {
            let claim_ref = ClaimRef::from_lease(claim);
            let has_numeric = claim_ref.holder_pid.is_some() || claim_ref.session_pid.is_some();
            let numeric_match = [claim_ref.holder_pid, claim_ref.session_pid]
                .into_iter()
                .flatten()
                .any(|pid| pid == *agent_pid);
            if has_numeric && !numeric_match {
                findings.push(Finding {
                    severity: Severity::Error,
                    code: "port_owner_mismatch".into(),
                    message: format!(
                        "port {} is claimed by {} but the listener belongs to {}",
                        listener.port, claim.agent, agent_name
                    ),
                    detail: Some(format!("pid {}", listener.pid)),
                });
            }
        }
        out.push(PortReport {
            port: listener.port,
            proto: listener.proto,
            host: listener.host,
            pid: listener.pid,
            command: listener.command,
            agent: agent.map(|(name, _)| name),
            claimed_by: claim.map(|lease| lease.agent.clone()),
            known: known || claim.is_some(),
        });
    }
    out.sort_by_key(|port| (port.port, port.proto.clone()));
    out
}

fn agent_for_pid(pid: u32, table: &BTreeMap<u32, ProcInfo>) -> Option<(String, u32)> {
    if let Some(info) = table.get(&pid) {
        if let Some(kind) = identity::harness_kind(&info.command) {
            return Some((format!("{kind}:pid{pid}"), pid));
        }
    }
    for ancestor in process::ancestors(pid, table) {
        if let Some(info) = table.get(&ancestor) {
            if let Some(kind) = identity::harness_kind(&info.command) {
                return Some((format!("{kind}:pid{ancestor}"), ancestor));
            }
        }
    }
    None
}

fn hosts_overlap(a: &str, b: &str) -> bool {
    a == "*" || b == "*" || a == b
}

fn add_worktree_findings(worktrees: &[WorktreeReport], findings: &mut Vec<Finding>) {
    for wt in worktrees {
        let distinct_roots: BTreeSet<u32> = wt.agents.iter().map(|agent| agent.root_pid).collect();
        if distinct_roots.len() > 1 {
            findings.push(Finding {
                severity: Severity::Error,
                code: "shared_worktree".into(),
                message: format!(
                    "{} agent sessions share worktree {}",
                    distinct_roots.len(),
                    wt.path.display()
                ),
                detail: Some(
                    wt.agents
                        .iter()
                        .map(|agent| {
                            format!("{} pid {} ({})", agent.kind, agent.pid, agent.command)
                        })
                        .collect::<Vec<_>>()
                        .join("; "),
                ),
            });
        }
        for claim in wt.claims.iter().filter(|claim| claim.exclusive_worktree()) {
            for agent in &wt.agents {
                if !claim.matches(agent) {
                    findings.push(Finding {
                        severity: Severity::Error,
                        code: "claim_conflict_live_agent".into(),
                        message: format!(
                            "worktree {} is claimed by {} but live {} pid {} is working in it",
                            wt.path.display(),
                            claim.agent,
                            agent.kind,
                            agent.pid
                        ),
                        detail: Some(claim.resource.clone()),
                    });
                }
            }
        }
        if let Some(reason) = &wt.prunable {
            findings.push(Finding {
                severity: Severity::Warn,
                code: "prunable_worktree".into(),
                message: format!("worktree {} is prunable: {reason}", wt.path.display()),
                detail: None,
            });
        }
        if wt.dirty.unwrap_or(0) > 0 && wt.claims.is_empty() && wt.agents.is_empty() {
            findings.push(Finding {
                severity: Severity::Info,
                code: "orphan_dirty_worktree".into(),
                message: format!(
                    "worktree {} has {} uncommitted path(s), no claim, and no live agent",
                    wt.path.display(),
                    wt.dirty.unwrap_or(0)
                ),
                detail: None,
            });
        }
    }
}

pub fn print_report(report: &ScanReport, strict: bool, all_ports: bool) {
    println!(
        "Arbiter scan at {} on {}",
        util::format_timestamp(report.generated_at),
        report.host
    );

    println!("\nLEASES ({} active)", report.leases.len());
    if report.leases.is_empty() {
        println!("  none");
    }
    for lease in &report.leases {
        println!(
            "  {:<30} {:<9} {:<40} pid={} expires in {}  {}",
            lease.agent,
            lease.mode.as_str(),
            util::truncate(&lease.display, 40),
            lease
                .pid
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "-".into()),
            util::format_age(lease.remaining(util::now())),
            lease
                .meta
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("")
        );
    }

    println!("\nWORKTREES ({})", report.worktrees.len());
    for wt in &report.worktrees {
        let agents = wt
            .agents
            .iter()
            .map(|agent| format!("{}:{}", agent.kind, agent.pid))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "  {} [{}] dirty={} claims={} agents={}{}",
            wt.path.display(),
            wt.branch,
            wt.dirty
                .map(|d| d.to_string())
                .unwrap_or_else(|| "?".into()),
            wt.claimed_by(),
            if agents.is_empty() {
                "-".into()
            } else {
                agents
            },
            wt.prunable
                .as_ref()
                .map(|reason| format!(" prunable={reason}"))
                .unwrap_or_default()
        );
    }

    let shown: Vec<&PortReport> = report
        .ports
        .iter()
        .filter(|port| all_ports || port.known || port.claimed_by.is_some() || port.agent.is_some())
        .collect();
    println!("\nPORTS ({})", shown.len());
    for port in &shown {
        println!(
            "  {:>5}/{} {:<14} pid={:<7} agent={:<22} claim={}",
            port.port,
            port.proto,
            util::truncate(&port.command, 14),
            port.pid,
            port.agent.as_deref().unwrap_or("-"),
            port.claimed_by.as_deref().unwrap_or("-")
        );
    }
    if shown.len() < report.ports.len() {
        println!(
            "  ... {} unclaimed system listener(s) hidden; use --all-ports",
            report.ports.len() - shown.len()
        );
    }

    println!("\nSERVICES ({})", report.services.len());
    for service in &report.services {
        println!(
            "  {:<34} pid={:<7} port={:<6} claim={}",
            service.label,
            service
                .pid
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "-".into()),
            service
                .port
                .map(|port| port.to_string())
                .unwrap_or_else(|| "-".into()),
            service.claimed_by.as_deref().unwrap_or("-")
        );
    }

    if !report.locks.is_empty() {
        println!("\nGIT LOCKS ({})", report.locks.len());
        for lock in &report.locks {
            println!(
                "  {} age={} stale={} git_process={}",
                lock.path.display(),
                util::format_age(lock.age_secs),
                lock.stale,
                lock.git_process
            );
        }
    }

    println!("\nFINDINGS ({})", report.findings.len());
    if report.findings.is_empty() {
        println!("  none");
    }
    for finding in &report.findings {
        println!(
            "  [{}] {:<26} {}",
            match finding.severity {
                Severity::Error => "error",
                Severity::Warn => "warn ",
                Severity::Info => "info ",
            },
            finding.code,
            finding.message
        );
        if let Some(detail) = &finding.detail {
            println!("        {detail}");
        }
    }

    let errors = report
        .findings
        .iter()
        .filter(|finding| finding.severity == Severity::Error)
        .count();
    let warnings = report
        .findings
        .iter()
        .filter(|finding| finding.severity == Severity::Warn)
        .count();
    println!("\nsummary: {errors} error(s), {warnings} warning(s)");
    if strict && warnings > 0 {
        println!("strict mode: warnings are treated as failures");
    }
}
