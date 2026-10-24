use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::config::{Config, RepoConfig};
use crate::error::{Error, Result};
use crate::gitguard::{self, GitInfo};
use crate::identity;
use crate::resource::{Kind, Mode, Resource};
use crate::scan::{self, ScanOptions};
use crate::service;
use crate::store::{ClaimRequest, Lease, Store};
use crate::supervisor::{self, RunOptions};
use crate::util;

#[derive(Parser, Debug)]
#[command(
    name = "arbiter",
    version,
    about = "Local resource leases and collision detection for concurrent coding agents",
    disable_help_subcommand = true
)]
pub struct Cli {
    #[arg(long, global = true, help = "Machine-readable JSON output")]
    pub json: bool,
    #[arg(long, global = true, help = "Override the state directory")]
    pub state_dir: Option<PathBuf>,
    #[arg(long, global = true, help = "Override the config file")]
    pub config: Option<PathBuf>,
    #[arg(long, global = true, help = "Agent identity override")]
    pub agent: Option<String>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Acquire one or more resource leases
    Claim {
        resources: Vec<String>,
        #[arg(long, help = "Default mode for path/refs claims: shared or exclusive")]
        mode: Option<String>,
        #[arg(long, help = "Lease lifetime, e.g. 30m")]
        ttl: Option<String>,
        #[arg(long, help = "Queue for up to this long instead of failing")]
        wait: Option<String>,
        #[arg(long, default_value = "manual claim")]
        reason: String,
    },
    /// Release a lease group or individual lease id
    Release {
        targets: Vec<String>,
        #[arg(long, help = "Release every lease on this host (requires --force)")]
        all: bool,
        #[arg(long, help = "Release everything owned by this agent")]
        mine: bool,
        #[arg(long, help = "Operator override; ignores ownership checks")]
        force: bool,
    },
    /// Extend a lease group
    Renew {
        targets: Vec<String>,
        #[arg(long)]
        ttl: Option<String>,
    },
    /// Show active leases and waiters
    Status {
        #[arg(
            long,
            help = "Only leases relevant to the current directory/repository"
        )]
        here: bool,
    },
    /// Exit 3 if any of these resources is currently claimed
    Check { resources: Vec<String> },
    /// Run a command while holding leases for its lifetime
    Run {
        #[arg(long, help = "Claim the current worktree and repository automatically")]
        auto: bool,
        #[arg(long, help = "Downgrade auto path claims to shared (read-only)")]
        read_only: bool,
        #[arg(long = "claim", help = "Additional resource to claim")]
        claims: Vec<String>,
        #[arg(long)]
        ttl: Option<String>,
        #[arg(long)]
        wait: Option<String>,
        #[arg(long, default_value = "wrapped agent session")]
        reason: String,
        #[arg(last = true, required = true, allow_hyphen_values = true)]
        command: Vec<OsString>,
    },
    /// Inspect live worktrees, ports, services, git locks, and agents
    Scan {
        #[arg(long = "root", help = "Repository root to scan (repeatable)")]
        roots: Vec<PathBuf>,
        #[arg(long, help = "Exit 3 when conflicts are found")]
        check: bool,
        #[arg(long, help = "Treat warnings as failures with --check")]
        strict: bool,
        #[arg(long, help = "Skip git status dirty counts")]
        no_dirty: bool,
        #[arg(
            long,
            help = "Show every system listener, not just known/claimed/agent ports"
        )]
        all_ports: bool,
    },
    /// Check the local environment and database
    Doctor,
    /// Run git with claim checks (also used by the optional git shim)
    Git {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Inspect or restart launchd-managed services
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Read the audit event log
    Log {
        #[arg(long, help = "Only events newer than this duration, e.g. 1h")]
        since: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Write a starter .arbiter.toml
    Init {
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        force: bool,
    },
    /// Install or inspect the optional git shim
    Shim {
        #[command(subcommand)]
        command: ShimCommand,
    },
}

#[derive(Subcommand, Debug)]
pub enum ServiceCommand {
    List,
    Restart {
        label: String,
        #[arg(long, help = "Wait this long for a conflicting claim")]
        wait: Option<String>,
        #[arg(long, help = "Override the configured health timeout")]
        health_timeout: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ShimCommand {
    Install {
        #[arg(long)]
        dir: PathBuf,
        #[arg(long)]
        dry_run: bool,
    },
    Status,
    Uninstall {
        #[arg(long)]
        path: PathBuf,
    },
}

pub fn dispatch(cli: Cli) -> Result<i32> {
    let json = cli.json;
    let agent_override = cli.agent.clone();
    let config_path = cli.config.clone();
    let config = Config::load(config_path.as_deref())?;
    let cwd = std::env::current_dir()?;
    let agent = identity::resolve(agent_override.as_deref());

    match cli.command {
        Command::Claim {
            resources,
            mode,
            ttl,
            wait,
            reason,
        } => {
            let store = Store::open(config.clone())?;
            let default_mode = mode
                .as_deref()
                .map(|raw| {
                    Mode::parse(raw).ok_or_else(|| Error::Usage(format!("unknown mode {raw:?}")))
                })
                .transpose()?;
            let mut parsed = parse_resources(&resources, &config, &cwd, default_mode)?;
            enrich_worktree_metadata(&mut parsed, &config);
            if parsed.is_empty() {
                return Err(Error::Usage("claim requires at least one resource".into()));
            }
            parsed = Resource::dedupe(parsed);
            let ttl = parse_ttl(ttl.as_deref(), config.defaults.ttl)?;
            let wait = parse_ttl(wait.as_deref(), config.defaults.wait)?;
            let result = store.claim(ClaimRequest {
                agent: agent.id.clone(),
                resources: parsed.clone(),
                ttl,
                wait,
                reason,
                pid: None,
                agent_pid: Some((agent.pid, agent.pid_start.clone().unwrap_or_default())),
                command: Some("arbiter claim".into()),
            })?;
            if json {
                print_json(&serde_json::json!({
                    "group_id": result.group_id,
                    "agent": agent.id,
                    "waited_secs": result.waited.as_secs_f64(),
                    "leases": result.leases,
                }))?;
            } else {
                println!("claimed group {}", result.group_id);
                for lease in &result.leases {
                    println!("  {} [{}]", lease.resource(), lease.mode);
                }
                if result.waited >= Duration::from_millis(500) {
                    println!("waited {}", util::format_duration(result.waited));
                }
                println!("release with: arbiter release {}", result.group_id);
            }
            Ok(0)
        }
        Command::Release {
            targets,
            all,
            mine,
            force,
        } => {
            let store = Store::open(config.clone())?;
            if all {
                if !force {
                    return Err(Error::Usage(
                        "releasing every lease requires --force; this can disrupt other agents"
                            .into(),
                    ));
                }
                let leases = store.active()?;
                let mut groups = std::collections::BTreeSet::new();
                for lease in &leases {
                    groups.insert(lease.group_id.clone());
                }
                let mut released = Vec::new();
                for group in groups {
                    if let Ok(mut leases) = store.release(&group, &agent.id, true) {
                        released.append(&mut leases);
                    }
                }
                return output_release(&released, json);
            }
            if mine {
                let released = store.release_mine(&agent.id)?;
                return output_release(&released, json);
            }
            if targets.is_empty() {
                return Err(Error::Usage(
                    "release requires lease id(s), --mine, or --all --force".into(),
                ));
            }
            let mut released = Vec::new();
            for target in targets {
                let mut leases = store.release(&target, &agent.id, force)?;
                released.append(&mut leases);
            }
            output_release(&released, json)
        }
        Command::Renew { targets, ttl } => {
            let store = Store::open(config.clone())?;
            if targets.is_empty() {
                return Err(Error::Usage("renew requires at least one lease id".into()));
            }
            let ttl = parse_ttl(ttl.as_deref(), config.defaults.ttl)?;
            let mut renewed = 0;
            for target in &targets {
                renewed += store.renew(target, &agent.id, ttl)?;
            }
            if json {
                print_json(
                    &serde_json::json!({ "renewed": renewed, "ttl_secs": ttl.as_secs_f64() }),
                )?;
            } else {
                println!(
                    "renewed {renewed} lease(s) for {}",
                    util::format_duration(ttl)
                );
            }
            Ok(0)
        }
        Command::Status { here } => {
            let store = Store::open(config.clone())?;
            let leases = store.active()?;
            let repo = gitguard::real_git(&config)
                .ok()
                .and_then(|git| gitguard::detect(&cwd, &git));
            let filtered: Vec<Lease> = if here {
                leases
                    .into_iter()
                    .filter(|lease| lease_relevant(lease, &cwd, repo.as_ref(), &config))
                    .collect()
            } else {
                leases
            };
            let waiters = store.waiters()?;
            if json {
                print_json(&serde_json::json!({
                    "leases": filtered,
                    "waiters": waiters,
                }))?;
            } else {
                print_status(&filtered, &waiters, util::now());
            }
            Ok(0)
        }
        Command::Check { resources } => {
            let store = Store::open(config.clone())?;
            let parsed = parse_resources(&resources, &config, &cwd, None)?;
            if parsed.is_empty() {
                return Err(Error::Usage("check requires at least one resource".into()));
            }
            let conflicts = store.check(&parsed)?;
            if conflicts.is_empty() {
                if json {
                    print_json(&serde_json::json!({ "free": true, "resources": parsed }))?;
                } else {
                    println!("free");
                }
                Ok(0)
            } else {
                if json {
                    print_json(&serde_json::json!({ "free": false, "conflicts": conflicts }))?;
                    return Ok(3);
                }
                Err(Error::Conflict(
                    conflicts
                        .iter()
                        .map(|conflict| conflict.describe())
                        .collect::<Vec<_>>()
                        .join("; "),
                ))
            }
        }
        Command::Run {
            auto,
            read_only,
            claims,
            ttl,
            wait,
            reason,
            command,
        } => {
            let store = Store::open(config.clone())?;
            let mut resources = parse_resources(&claims, &config, &cwd, None)?;
            enrich_worktree_metadata(&mut resources, &config);
            if auto {
                let mut auto_claims = auto_claims(&config, &cwd, read_only)?;
                resources.append(&mut auto_claims);
            }
            if resources.is_empty() {
                return Err(Error::Usage(
                    "run requires --auto or at least one --claim".into(),
                ));
            }
            let ttl = parse_ttl(ttl.as_deref(), config.defaults.ttl)?;
            let wait = parse_ttl(wait.as_deref(), config.defaults.wait)?;
            let code = supervisor::run(
                &store,
                RunOptions {
                    command,
                    claims: Resource::dedupe(resources),
                    agent: agent.id.clone(),
                    agent_pid: Some((agent.pid, agent.pid_start.clone().unwrap_or_default())),
                    ttl,
                    wait,
                    reason,
                },
            )?;
            Ok(code)
        }
        Command::Scan {
            roots,
            check,
            strict,
            no_dirty,
            all_ports,
        } => {
            let store = Store::open(config.clone())?;
            let report = scan::scan(
                &store,
                &ScanOptions {
                    roots,
                    include_dirty: if no_dirty { Some(false) } else { None },
                },
            )?;
            if json {
                print_json(&report)?;
            } else {
                scan::print_report(&report, strict, all_ports);
            }
            if check && (report.has_errors() || (strict && report.has_warnings())) {
                Ok(3)
            } else {
                Ok(0)
            }
        }
        Command::Doctor => doctor(&config, json),
        Command::Git { args } => {
            let store = Store::open(config.clone())?;
            gitguard::run_git(&store, &agent, args)
        }
        Command::Service { command } => {
            let store = Store::open(config.clone())?;
            match command {
                ServiceCommand::List => {
                    let services = service::list(&store)?;
                    if json {
                        print_json(&services)?;
                    } else {
                        for service in services {
                            println!(
                                "{:<34} launchd={:<34} pid={:<7} port={:<6} claim={}",
                                service.label,
                                service.launchd_label,
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
                    }
                    Ok(0)
                }
                ServiceCommand::Restart {
                    label,
                    wait,
                    health_timeout,
                    dry_run,
                } => {
                    let wait = parse_ttl(wait.as_deref(), Duration::ZERO)?;
                    let health_timeout = health_timeout
                        .as_deref()
                        .map(util::parse_duration)
                        .transpose()?;
                    let report =
                        service::restart(&store, &agent, &label, wait, health_timeout, dry_run)?;
                    if json {
                        print_json(&report)?;
                    } else if report.dry_run {
                        println!(
                            "dry run: service {} pid={:?} (no claim, no restart)",
                            report.label, report.before_pid
                        );
                    } else {
                        println!(
                            "restarted {}: pid {} -> {}, idle wait {:.1}s, health {}",
                            report.label,
                            report
                                .before_pid
                                .map(|pid| pid.to_string())
                                .unwrap_or_else(|| "none".into()),
                            report
                                .after_pid
                                .map(|pid| pid.to_string())
                                .unwrap_or_else(|| "none".into()),
                            report.idle_waited_secs,
                            report.health_ok
                        );
                    }
                    Ok(0)
                }
            }
        }
        Command::Log { since, limit } => {
            let store = Store::open(config.clone())?;
            let since = match since {
                Some(raw) => {
                    let duration = util::parse_duration(&raw)?;
                    Some(util::now() - duration.as_secs_f64())
                }
                None => None,
            };
            let events = store.events(since, limit)?;
            if json {
                print_json(&events)?;
            } else {
                for event in events {
                    println!(
                        "{} {:<24} agent={:<28} resources={} {}",
                        util::format_timestamp(event.ts),
                        event.kind,
                        event.agent.as_deref().unwrap_or("-"),
                        event.resources.as_deref().unwrap_or("-"),
                        event.detail.as_deref().unwrap_or("")
                    );
                }
            }
            Ok(0)
        }
        Command::Init { dir, force } => {
            let dir = dir.unwrap_or(cwd);
            if !dir.exists() {
                return Err(Error::Usage(format!(
                    "directory {} does not exist",
                    dir.display()
                )));
            }
            let path = dir.join(".arbiter.toml");
            if path.exists() && !force {
                return Err(Error::Failed(format!(
                    "{} already exists; pass --force to overwrite",
                    path.display()
                )));
            }
            let name = dir
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| "project".into());
            std::fs::write(&path, crate::config::default_repo_config(&name))?;
            if json {
                print_json(&serde_json::json!({ "path": path }))?;
            } else {
                println!("wrote {}", path.display());
            }
            Ok(0)
        }
        Command::Shim { command } => match command {
            ShimCommand::Install { dir, dry_run } => {
                let link = gitguard::install_shim(&dir, dry_run)?;
                if json {
                    print_json(&serde_json::json!({ "link": link, "dry_run": dry_run }))?;
                } else if dry_run {
                    println!("would link {}", link.display());
                } else {
                    println!("linked {} -> arbiter", link.display());
                    println!("ensure {} precedes the system git in PATH", dir.display());
                }
                Ok(0)
            }
            ShimCommand::Status => {
                let current = which_git();
                let exe = std::env::current_exe()
                    .ok()
                    .and_then(|p| p.canonicalize().ok());
                let installed = current
                    .as_ref()
                    .and_then(|p| p.canonicalize().ok())
                    .map(|p| Some(p) == exe)
                    .unwrap_or(false);
                if json {
                    print_json(&serde_json::json!({
                        "installed": installed,
                        "git_on_path": current,
                        "arbiter_exe": exe,
                    }))?;
                } else {
                    println!(
                        "git on PATH: {}",
                        current
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "none".into())
                    );
                    println!("arbiter shim active: {installed}");
                }
                Ok(0)
            }
            ShimCommand::Uninstall { path } => {
                let removed = gitguard::uninstall_shim(&path)?;
                if json {
                    print_json(&serde_json::json!({ "removed": removed, "path": path }))?;
                } else {
                    println!("{}", if removed { "removed" } else { "not present" });
                }
                Ok(0)
            }
        },
    }
}

fn output_release(released: &[Lease], json: bool) -> Result<i32> {
    if json {
        print_json(&serde_json::json!({ "released": released }))?;
    } else if released.is_empty() {
        println!("nothing to release");
    } else {
        let groups: std::collections::BTreeSet<_> = released
            .iter()
            .map(|lease| lease.group_id.clone())
            .collect();
        println!(
            "released {} lease(s) in {} group(s)",
            released.len(),
            groups.len()
        );
        for lease in released {
            println!("  {}", lease.resource());
        }
    }
    Ok(0)
}

fn print_status(leases: &[Lease], waiters: &[crate::store::Waiter], now: f64) {
    if leases.is_empty() {
        println!("no active leases");
    } else {
        println!(
            "{:<9} {:<28} {:<40} {:<8} {:<8} {:<8} REASON",
            "GROUP", "AGENT", "RESOURCE", "MODE", "PID", "EXPIRES"
        );
        for lease in leases {
            println!(
                "{:<9} {:<28} {:<40} {:<8} {:<8} {:<8} {}",
                &lease.group_id[..8.min(lease.group_id.len())],
                util::truncate(&lease.agent, 28),
                util::truncate(&lease.display, 40),
                lease.mode.as_str(),
                lease
                    .pid
                    .map(|pid| pid.to_string())
                    .unwrap_or_else(|| "-".into()),
                util::format_age(lease.remaining(now)),
                lease
                    .meta
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
            );
        }
    }
    if !waiters.is_empty() {
        println!("\nWAITERS ({})", waiters.len());
        for waiter in waiters {
            println!(
                "  {:<28} {} resources, expires in {}",
                waiter.agent,
                waiter.resources.len(),
                util::format_age((waiter.expires_at - now).max(0.0))
            );
        }
    }
}

fn lease_relevant(lease: &Lease, cwd: &Path, repo: Option<&GitInfo>, config: &Config) -> bool {
    match lease.kind {
        Kind::Path => {
            let cwd = util::canonicalize_lenient(cwd).unwrap_or_else(|_| cwd.to_path_buf());
            let cwd_key = cwd.to_string_lossy().to_string();
            crate::resource::paths_overlap(
                &lease.key,
                &cwd_key,
                config.paths.is_case_insensitive(),
                lease.exact,
                true,
            )
        }
        Kind::Refs => repo
            .map(|info| lease.key == info.common_dir.to_string_lossy())
            .unwrap_or(false),
        _ => true,
    }
}

fn enrich_worktree_metadata(resources: &mut [Resource], config: &Config) {
    let Some(git) = gitguard::real_git(config).ok() else {
        return;
    };
    for resource in resources.iter_mut() {
        if resource.kind != Kind::Path || !resource.is_worktree() {
            continue;
        }
        let Some(path) = resource.path() else {
            continue;
        };
        let Some(info) = gitguard::detect(&path, &git) else {
            continue;
        };
        if let Some(object) = resource.meta.as_object_mut() {
            object.insert("branch".into(), serde_json::Value::String(info.branch));
            if let Some(head) = info.head {
                object.insert("head".into(), serde_json::Value::String(head));
            }
        }
    }
}

fn parse_resources(
    specs: &[String],
    config: &Config,
    cwd: &Path,
    default_mode: Option<Mode>,
) -> Result<Vec<Resource>> {
    let mut out = Vec::new();
    for spec in specs {
        let mut resource = Resource::parse(spec, config, cwd)?;
        if let Some(mode) = default_mode {
            if !matches!(resource.kind, Kind::Path | Kind::Refs) && mode == Mode::Shared {
                return Err(Error::Usage(format!(
                    "{} cannot be shared",
                    resource.display
                )));
            }
            resource.mode = mode;
        }
        out.push(resource);
    }
    Ok(out)
}

fn parse_ttl(raw: Option<&str>, default: Duration) -> Result<Duration> {
    match raw {
        Some(value) => util::parse_duration(value),
        None => Ok(default),
    }
}

pub fn auto_claims(config: &Config, cwd: &Path, read_only: bool) -> Result<Vec<Resource>> {
    let Some(git) = gitguard::real_git(config).ok() else {
        return Ok(vec![path_resource(config, cwd, read_only)?]);
    };
    let Some(info) = gitguard::detect(cwd, &git) else {
        return Ok(vec![path_resource(config, cwd, read_only)?]);
    };
    let repo_config = RepoConfig::load(&info.worktree)?;
    let templates = repo_config
        .as_ref()
        .and_then(|repo| repo.auto.claims.clone())
        .unwrap_or_else(|| vec!["worktree:{worktree}".into()]);
    let mut resources = Vec::new();
    for template in templates {
        let substituted = template
            .replace("{worktree}", &info.worktree.to_string_lossy())
            .replace("{repo}", &info.common_dir.to_string_lossy())
            .replace("{cwd}", &cwd.to_string_lossy())
            .replace("{branch}", &info.branch);
        let mut resource = Resource::parse(&substituted, config, cwd)?;
        if resource.kind == Kind::Path {
            if let Some(object) = resource.meta.as_object_mut() {
                object.insert(
                    "branch".into(),
                    serde_json::Value::String(info.branch.clone()),
                );
                if let Some(head) = &info.head {
                    object.insert("head".into(), serde_json::Value::String(head.clone()));
                }
            }
            if read_only {
                resource.mode = Mode::Shared;
            }
        }
        if resource.kind == Kind::Refs && read_only {
            resource.mode = Mode::Shared;
        }
        resources.push(resource);
    }
    Ok(resources)
}

fn path_resource(_config: &Config, path: &Path, read_only: bool) -> Result<Resource> {
    let canonical = util::canonicalize_lenient(path)?;
    let key = canonical.to_string_lossy().to_string();
    Ok(Resource {
        kind: Kind::Path,
        key,
        display: canonical.display().to_string(),
        mode: if read_only {
            Mode::Shared
        } else {
            Mode::Exclusive
        },
        exact: false,
        host: None,
        proto: None,
        meta: serde_json::json!({ "path": canonical.display().to_string() }),
    })
}

#[derive(Serialize)]
struct DoctorReport {
    state_dir: String,
    database: String,
    database_check: String,
    config: String,
    git: Option<String>,
    launchctl: bool,
    shim_active: bool,
    active_leases: usize,
    waiters: usize,
    ok: bool,
    warnings: Vec<String>,
}

fn doctor(config: &Config, json: bool) -> Result<i32> {
    let store = Store::open(config.clone())?;
    let state_dir = store
        .path()
        .parent()
        .unwrap_or(Path::new("/"))
        .to_path_buf();
    let quick = store.quick_check()?;
    let git = gitguard::real_git(config).ok();
    let launchctl = std::process::Command::new("launchctl")
        .arg("list")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false);
    let current_git = which_git();
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok());
    let shim_active = current_git
        .as_ref()
        .and_then(|p| p.canonicalize().ok())
        .map(|p| Some(p) == exe)
        .unwrap_or(false);
    let mut warnings = Vec::new();
    if git.is_none() {
        warnings.push("no real git binary found".into());
    }
    if !launchctl {
        warnings.push("launchctl is unavailable; service restart is disabled".into());
    }
    if !shim_active {
        warnings.push("git shim is not active (optional; wrapper mode still works)".into());
    }
    let active = store.active()?;
    let waiters = store.waiters()?;
    let ok = quick == "ok";
    let report = DoctorReport {
        state_dir: state_dir.display().to_string(),
        database: store.path().display().to_string(),
        database_check: quick,
        config: util::config_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "?".into()),
        git: git.map(|p| p.display().to_string()),
        launchctl,
        shim_active,
        active_leases: active.len(),
        waiters: waiters.len(),
        ok,
        warnings,
    };
    if json {
        print_json(&report)?;
    } else {
        println!("state dir:   {}", report.state_dir);
        println!(
            "database:    {} ({})",
            report.database, report.database_check
        );
        println!("config:      {}", report.config);
        println!(
            "git:         {}",
            report.git.as_deref().unwrap_or("MISSING")
        );
        println!("launchctl:   {}", report.launchctl);
        println!("git shim:    {}", report.shim_active);
        println!(
            "leases:      {} active, {} waiter(s)",
            report.active_leases, report.waiters
        );
        if report.warnings.is_empty() {
            println!("environment: ok");
        } else {
            for warning in &report.warnings {
                println!("warning:     {warning}");
            }
        }
    }
    Ok(if report.ok { 0 } else { 6 })
}

fn which_git() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("git");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
