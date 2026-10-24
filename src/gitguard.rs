use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Serialize;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::identity::Agent;
use crate::process;
use crate::resource::{Kind, Mode, Resource};
use crate::store::{ClaimRequest, Store};

#[derive(Clone, Debug, Serialize)]
pub struct GitInfo {
    pub worktree: PathBuf,
    pub common_dir: PathBuf,
    pub branch: String,
    pub head: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpClass {
    ReadOnly,
    Index,
    Refs,
    IndexAndRefs,
    Commit,
    Clean { dry_run: bool },
    WorktreeRemove,
}

pub fn run_git(store: &Store, agent: &Agent, args: Vec<OsString>) -> Result<i32> {
    let args: Vec<OsString> = if args.first().map(|a| a == "--").unwrap_or(false) {
        args.into_iter().skip(1).collect()
    } else {
        args
    };
    if args.is_empty() {
        return Err(Error::Usage("arbiter git requires a git subcommand".into()));
    }
    let git = real_git(store.config())?;
    let cwd = working_dir(&args)?;
    let Some(info) = detect(&cwd, &git) else {
        // Not in a repository: init/clone and diagnostics still work.
        return passthrough(&git, &args, &cwd);
    };
    let subcommand = subcommand(&args);
    let class = classify(&args);

    if !store.config().git.guard {
        return passthrough(&git, &args, &cwd);
    }

    let mut needed = Vec::new();
    match class {
        OpClass::Index | OpClass::Commit => {
            needed.push(path_claim(&info.worktree, Mode::Exclusive, store.config())?);
            if class == OpClass::Commit {
                needed.push(refs_claim(&info.common_dir, store.config())?);
            }
        }
        OpClass::Refs => needed.push(refs_claim(&info.common_dir, store.config())?),
        OpClass::IndexAndRefs => {
            needed.push(path_claim(&info.worktree, Mode::Exclusive, store.config())?);
            needed.push(refs_claim(&info.common_dir, store.config())?);
        }
        OpClass::WorktreeRemove => {
            let target = worktree_target(&args)
                .ok_or_else(|| Error::Usage("git worktree remove requires a target path".into()))?;
            guard_worktree_remove(store, agent, &target)?;
            needed.push(refs_claim(&info.common_dir, store.config())?);
        }
        OpClass::Clean { dry_run } => {
            if !dry_run {
                guard_clean(store, agent, &info)?;
            }
        }
        OpClass::ReadOnly => {}
    }

    if class == OpClass::Commit {
        let own = own_path_lease(store, agent, &info.worktree)?;
        if let Some(lease) = &own {
            if let Some(expected) = lease.meta.get("branch").and_then(|v| v.as_str()) {
                if !expected.is_empty() && expected != info.branch {
                    let message = format!(
                        "refusing commit: HEAD is on {:?} but this session claimed {:?} for {}.\n\
                         Another process may have switched branches in this worktree. Verify with `git reflog` and \
                         `git branch --show-current`; recover with a new worktree or cherry-pick instead of resetting.\n\
                         Lease: {} ({})",
                        info.branch,
                        expected,
                        info.worktree.display(),
                        lease.id,
                        lease.agent
                    );
                    let _ = store.log_event(
                        "git.guard.denied",
                        Some(&agent.id),
                        Some(&lease.group_id),
                        Some(&needed),
                        Some(serde_json::json!({ "reason": "branch_changed", "branch": info.branch, "expected": expected })),
                    );
                    return Err(Error::Conflict(message));
                }
            }
        }
    }

    let mut acquired = None;
    if !needed.is_empty() {
        let wait = std::env::var("ARBITER_GIT_WAIT")
            .ok()
            .and_then(|raw| crate::util::parse_duration(&raw).ok())
            .unwrap_or(Duration::ZERO);
        let own = store
            .active()?
            .into_iter()
            .filter(|l| l.agent == agent.id)
            .collect::<Vec<_>>();
        let case_insensitive = store.config().paths.is_case_insensitive();
        // A session may hold a shared claim (for example refs:ro). A mutating
        // git command needs exclusivity; drop only this agent's conflicting
        // shared leases and let the atomic claim arbitrate the upgrade.
        for lease in own.iter() {
            if lease.mode != Mode::Shared {
                continue;
            }
            if needed
                .iter()
                .any(|resource| resource.conflicts_with(&lease.resource(), case_insensitive))
            {
                let _ = store.release_one(&lease.id, &agent.id);
            }
        }
        let own = store
            .active()?
            .into_iter()
            .filter(|l| l.agent == agent.id)
            .collect::<Vec<_>>();
        let missing: Vec<Resource> = needed
            .iter()
            .filter(|resource| {
                !own.iter().any(|lease| {
                    lease.resource().conflicts_with(resource, case_insensitive)
                        && lease.mode == Mode::Exclusive
                })
            })
            .cloned()
            .collect();
        if !missing.is_empty() {
            let mut with_meta = Vec::new();
            for mut resource in missing {
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
                }
                with_meta.push(resource);
            }
            let result = store.claim(ClaimRequest {
                agent: agent.id.clone(),
                resources: Resource::dedupe(with_meta),
                ttl: store.config().defaults.git_lease_ttl,
                wait,
                reason: format!("git {subcommand}"),
                pid: Some((
                    std::process::id(),
                    process::current_pid_start().unwrap_or_default(),
                )),
                agent_pid: Some((agent.pid, agent.pid_start.clone().unwrap_or_default())),
                command: Some(render(&args)),
            })?;
            acquired = Some(result.group_id);
        }
    }

    let code = run_child(&git, &args, &cwd);
    if code.as_ref().map(|code| *code == 0).unwrap_or(false)
        && matches!(subcommand.as_str(), "checkout" | "switch")
    {
        if let Some(lease) = own_path_lease(store, agent, &info.worktree)? {
            if let Some(updated) = detect(&info.worktree, &git) {
                let _ = store.update_lease_meta(
                    &lease.id,
                    serde_json::json!({ "branch": updated.branch, "head": updated.head }),
                );
            }
        }
    }
    if let Some(group) = acquired {
        if let Err(err) = store.release(&group, &agent.id, false) {
            eprintln!("arbiter: warning: could not release git lease: {err}");
        }
    }
    let code = code?;
    let _ = store.log_event(
        "git.run",
        Some(&agent.id),
        None,
        None,
        Some(serde_json::json!({
            "subcommand": subcommand,
            "branch": info.branch,
            "worktree": info.worktree.display().to_string(),
            "exit_code": code,
        })),
    );
    Ok(code)
}

fn guard_clean(store: &Store, agent: &Agent, info: &GitInfo) -> Result<()> {
    let live = live_agents_in(&info.worktree)?;
    if let Some((pid, kind, _)) = live.iter().find(|(pid, _, _)| *pid != agent.pid) {
        let _ = store.log_event(
            "git.guard.denied",
            Some(&agent.id),
            None,
            None,
            Some(
                serde_json::json!({ "reason": "clean_with_live_agent", "pid": pid, "agent": kind }),
            ),
        );
        return Err(Error::Conflict(format!(
            "refusing `git clean` in {}: live {kind} process {pid} has cwd in this worktree.\n\
             Another session may own untracked files. Move the work to a separate worktree or stop that session first.",
            info.worktree.display()
        )));
    }
    let own = own_path_lease(store, agent, &info.worktree)?;
    if own.is_none() {
        let _ = store.log_event(
            "git.guard.denied",
            Some(&agent.id),
            None,
            None,
            Some(serde_json::json!({ "reason": "clean_without_claim", "worktree": info.worktree.display().to_string() })),
        );
        return Err(Error::Conflict(format!(
            "refusing `git clean` in {}: this agent holds no worktree claim, and untracked files may belong to another session.\n\
             Claim it first with `arbiter claim path:{}` or run agents through `arbiter run --auto -- ...`. Use `git clean -n` to preview.",
            info.worktree.display(),
            info.worktree.display()
        )));
    }
    Ok(())
}

fn guard_worktree_remove(store: &Store, agent: &Agent, target: &Path) -> Result<()> {
    let case_insensitive = store.config().paths.is_case_insensitive();
    let target_resource = path_claim(target, Mode::Exclusive, store.config())?;
    for lease in store.active()? {
        if lease.agent != agent.id
            && target_resource.conflicts_with(&lease.resource(), case_insensitive)
        {
            let _ = store.log_event(
                "git.guard.denied",
                Some(&agent.id),
                Some(&lease.group_id),
                Some(std::slice::from_ref(&target_resource)),
                Some(serde_json::json!({ "reason": "worktree_remove_claimed", "holder": lease.agent })),
            );
            return Err(Error::Conflict(format!(
                "refusing to remove worktree {}: {} holds {}",
                target.display(),
                lease.agent,
                lease.resource()
            )));
        }
    }
    if let Some((pid, kind, cwd)) = live_agents_in(target)?.into_iter().next() {
        let _ = store.log_event(
            "git.guard.denied",
            Some(&agent.id),
            None,
            None,
            Some(serde_json::json!({ "reason": "worktree_remove_live_agent", "pid": pid, "agent": kind })),
        );
        return Err(Error::Conflict(format!(
            "refusing to remove worktree {}: live {kind} process {pid} has cwd {}",
            target.display(),
            cwd.display()
        )));
    }
    Ok(())
}

fn live_agents_in(path: &Path) -> Result<Vec<(u32, String, PathBuf)>> {
    let table = process::ps_table();
    let target = crate::util::canonicalize_lenient(path)?;
    let mut out = Vec::new();
    for (pid, info) in &table {
        let Some(kind) = crate::identity::harness_kind(&info.command) else {
            continue;
        };
        if *pid == std::process::id() {
            continue;
        }
        let Some(cwd) = process::cwd_of(*pid) else {
            continue;
        };
        if cwd.starts_with(&target) {
            out.push((*pid, kind.to_string(), cwd));
        }
    }
    Ok(out)
}

fn own_path_lease(
    store: &Store,
    agent: &Agent,
    worktree: &Path,
) -> Result<Option<crate::store::Lease>> {
    let resource = path_claim(worktree, Mode::Exclusive, store.config())?;
    Ok(store
        .active()?
        .into_iter()
        .filter(|lease| {
            lease.agent == agent.id
                && lease.mode == Mode::Exclusive
                && resource.conflicts_with(
                    &lease.resource(),
                    store.config().paths.is_case_insensitive(),
                )
        })
        .max_by(|a, b| a.created_at.total_cmp(&b.created_at)))
}

fn path_claim(path: &Path, mode: Mode, config: &Config) -> Result<Resource> {
    let canonical = crate::util::canonicalize_lenient(path)?;
    let _ = config;
    let key = canonical.to_string_lossy().to_string();
    Ok(Resource {
        kind: Kind::Path,
        key,
        display: canonical.display().to_string(),
        mode,
        exact: true,
        host: None,
        proto: None,
        meta: serde_json::json!({
            "path": canonical.display().to_string(),
            "worktree": true,
        }),
    })
}

fn refs_claim(common_dir: &Path, config: &Config) -> Result<Resource> {
    let canonical = crate::util::canonicalize_lenient(common_dir)?;
    let _ = config;
    let key = canonical.to_string_lossy().to_string();
    Ok(Resource {
        kind: Kind::Refs,
        key,
        display: format!("refs:{}", canonical.display()),
        mode: Mode::Exclusive,
        exact: true,
        host: None,
        proto: None,
        meta: serde_json::json!({ "repo": canonical.display().to_string() }),
    })
}

fn working_dir(args: &[OsString]) -> Result<PathBuf> {
    let mut dir = std::env::current_dir()?;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "-C" {
            if let Some(value) = iter.next() {
                dir = PathBuf::from(value);
            }
        } else if let Some(value) = arg.to_str().and_then(|s| s.strip_prefix("-C")) {
            if !value.is_empty() {
                dir = PathBuf::from(value);
            }
        }
    }
    Ok(dir)
}

fn subcommand(args: &[OsString]) -> String {
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        let Some(raw) = arg.to_str() else {
            return "?".into();
        };
        if raw == "-C" || raw == "-c" || raw == "--git-dir" || raw == "--work-tree" {
            iter.next();
            continue;
        }
        if raw.starts_with('-') {
            continue;
        }
        return raw.to_string();
    }
    "?".into()
}

fn classify(args: &[OsString]) -> OpClass {
    let sub = subcommand(args);
    let rest: Vec<String> = args
        .iter()
        .filter_map(|a| a.to_str().map(|s| s.to_string()))
        .collect();
    let flags = |needle: &str| rest.iter().any(|a| a == needle);
    match sub.as_str() {
        "status" | "log" | "diff" | "show" | "rev-parse" | "ls-files" | "ls-tree" | "blame"
        | "grep" | "describe" | "shortlog" | "reflog" | "for-each-ref" | "cat-file"
        | "check-ignore" | "check-attr" | "var" | "help" | "version" | "init" | "clone"
        | "symbolic-ref" => OpClass::ReadOnly,
        "branch" => {
            if flags("-d")
                || flags("-D")
                || flags("--delete")
                || flags("-m")
                || flags("-M")
                || flags("--move")
                || flags("-f")
                || flags("--force")
                || flags("-u")
                || flags("--set-upstream-to")
                || flags("--unset-upstream")
                || flags("-c")
                || flags("-C")
                || flags("--copy")
            {
                OpClass::Refs
            } else if positional_after_subcommand(args, "branch").is_empty() {
                OpClass::ReadOnly
            } else {
                OpClass::Refs
            }
        }
        "tag" => {
            if flags("-l") || flags("--list") || flags("-v") || rest.len() == 1 {
                OpClass::ReadOnly
            } else {
                OpClass::Refs
            }
        }
        "remote" => {
            if rest.iter().any(|a| {
                matches!(
                    a.as_str(),
                    "add" | "remove" | "rm" | "set-url" | "prune" | "set-head" | "set-branches"
                )
            }) {
                OpClass::Refs
            } else {
                OpClass::ReadOnly
            }
        }
        "config" => {
            if flags("--get")
                || flags("--get-all")
                || flags("--list")
                || flags("-l")
                || flags("--show-origin")
            {
                OpClass::ReadOnly
            } else {
                OpClass::Refs
            }
        }
        "stash" => {
            if rest.iter().any(|a| matches!(a.as_str(), "list" | "show")) {
                OpClass::ReadOnly
            } else {
                OpClass::Index
            }
        }
        "add" | "rm" | "mv" | "restore" | "apply" | "am" => OpClass::Index,
        "checkout" | "switch" => {
            if flags("-b")
                || flags("-B")
                || flags("--orphan")
                || flags("-c")
                || flags("-C")
                || flags("--create")
            {
                OpClass::IndexAndRefs
            } else {
                OpClass::Index
            }
        }
        "reset" => OpClass::IndexAndRefs,
        "commit" => OpClass::Commit,
        "merge" | "rebase" | "cherry-pick" | "revert" | "pull" => OpClass::IndexAndRefs,
        "fetch" | "push" | "gc" | "update-ref" | "filter-branch" => OpClass::Refs,
        "clean" => OpClass::Clean {
            dry_run: flags("-n") || flags("--dry-run"),
        },
        "worktree" => {
            if rest.iter().any(|a| matches!(a.as_str(), "remove" | "rm")) {
                OpClass::WorktreeRemove
            } else if rest
                .iter()
                .any(|a| matches!(a.as_str(), "add" | "prune" | "move"))
            {
                OpClass::Refs
            } else {
                OpClass::ReadOnly
            }
        }
        _ => OpClass::ReadOnly,
    }
}

fn positional_after_subcommand(args: &[OsString], sub: &str) -> Vec<String> {
    let mut seen = false;
    let mut skip_next = false;
    let mut out = Vec::new();
    for arg in args {
        let Some(raw) = arg.to_str() else {
            continue;
        };
        if skip_next {
            skip_next = false;
            continue;
        }
        if matches!(raw, "-C" | "-c" | "--git-dir" | "--work-tree") {
            skip_next = true;
            continue;
        }
        if !seen {
            if raw == sub {
                seen = true;
            }
            continue;
        }
        if !raw.starts_with('-') {
            out.push(raw.to_string());
        }
    }
    out
}

fn worktree_target(args: &[OsString]) -> Option<PathBuf> {
    let mut saw_remove = false;
    for arg in args {
        let raw = arg.to_str()?;
        if matches!(raw, "remove" | "rm") {
            saw_remove = true;
            continue;
        }
        if saw_remove && !raw.starts_with('-') {
            return Some(PathBuf::from(raw));
        }
    }
    None
}

pub fn detect(dir: &Path, git: &Path) -> Option<GitInfo> {
    let worktree = git_output(git, dir, &["rev-parse", "--show-toplevel"])?;
    let worktree = PathBuf::from(worktree);
    let common = git_output(
        git,
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .or_else(|| git_output(git, dir, &["rev-parse", "--git-common-dir"]))?;
    let common_dir = crate::util::canonicalize_lenient(&worktree.join(common)).ok()?;
    let branch = git_output(git, dir, &["symbolic-ref", "--short", "-q", "HEAD"])
        .unwrap_or_else(|| "HEAD".into());
    let head = git_output(git, dir, &["rev-parse", "--verify", "HEAD"]);
    Some(GitInfo {
        worktree: crate::util::canonicalize_lenient(&worktree).unwrap_or(worktree),
        common_dir,
        branch,
        head,
    })
}

fn git_output(git: &Path, dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new(git)
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

fn passthrough(git: &Path, args: &[OsString], cwd: &Path) -> Result<i32> {
    run_child(git, args, cwd)
}

fn run_child(git: &Path, args: &[OsString], cwd: &Path) -> Result<i32> {
    use std::os::unix::process::ExitStatusExt;
    let status = Command::new(git)
        .args(args)
        .current_dir(cwd)
        .status()
        .map_err(|e| Error::Failed(format!("cannot run {}: {e}", git.display())))?;
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
}

pub fn real_git(config: &Config) -> Result<PathBuf> {
    if let Ok(raw) = std::env::var("ARBITER_GIT_BIN") {
        let path = PathBuf::from(raw);
        if path.is_file() {
            return Ok(path);
        }
    }
    if let Some(path) = &config.git.binary {
        if path.is_file() {
            return Ok(path.clone());
        }
    }
    let self_path = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok());
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("git");
            if !candidate.is_file() {
                continue;
            }
            let canonical = candidate.canonicalize().unwrap_or(candidate.clone());
            if Some(&canonical) == self_path.as_ref() {
                continue;
            }
            return Ok(canonical);
        }
    }
    for fallback in [
        "/usr/bin/git",
        "/opt/homebrew/bin/git",
        "/usr/local/bin/git",
    ] {
        let path = PathBuf::from(fallback);
        if path.is_file() {
            if let Ok(canonical) = path.canonicalize() {
                if Some(&canonical) != self_path.as_ref() {
                    return Ok(canonical);
                }
            }
        }
    }
    Err(Error::Env(
        "no real git binary found; set ARBITER_GIT_BIN".into(),
    ))
}

pub fn install_shim(dir: &Path, dry_run: bool) -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    fs::create_dir_all(dir)?;
    let link = dir.join("git");
    if link.exists() {
        let existing = link.canonicalize().unwrap_or_else(|_| link.clone());
        if existing == exe.canonicalize().unwrap_or(exe.clone()) {
            return Ok(link);
        }
        return Err(Error::Failed(format!(
            "{} already exists and is not an Arbiter shim; refusing to overwrite",
            link.display()
        )));
    }
    if !dry_run {
        std::os::unix::fs::symlink(&exe, &link)?;
    }
    Ok(link)
}

pub fn uninstall_shim(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let exe = std::env::current_exe()?.canonicalize()?;
    let target = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if target != exe {
        return Err(Error::Failed(format!(
            "{} does not point at this arbiter binary; not removing",
            path.display()
        )));
    }
    fs::remove_file(path)?;
    Ok(true)
}

fn render(args: &[OsString]) -> String {
    args.iter()
        .map(|a| a.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(raw: &[&str]) -> Vec<OsString> {
        raw.iter().map(OsString::from).collect()
    }

    #[test]
    fn classifies_mutations() {
        assert_eq!(classify(&args(&["status"])), OpClass::ReadOnly);
        assert_eq!(classify(&args(&["commit", "-m", "x"])), OpClass::Commit);
        assert_eq!(classify(&args(&["push"])), OpClass::Refs);
        assert_eq!(classify(&args(&["fetch"])), OpClass::Refs);
        assert_eq!(classify(&args(&["checkout", "main"])), OpClass::Index);
        assert_eq!(classify(&args(&["pull"])), OpClass::IndexAndRefs);
        assert_eq!(
            classify(&args(&["clean", "-n"])),
            OpClass::Clean { dry_run: true }
        );
        assert_eq!(
            classify(&args(&["clean", "-fd"])),
            OpClass::Clean { dry_run: false }
        );
        assert_eq!(
            classify(&args(&["worktree", "remove", "/tmp/x"])),
            OpClass::WorktreeRemove
        );
        assert_eq!(classify(&args(&["worktree", "list"])), OpClass::ReadOnly);
    }

    #[test]
    fn finds_remove_target() {
        let target = worktree_target(&args(&["worktree", "remove", "--force", "/tmp/wt"]));
        assert_eq!(target.as_deref(), Some(Path::new("/tmp/wt")));
    }

    #[test]
    fn reads_dash_c() {
        let dir = working_dir(&args(&["-C", "/tmp", "status"])).unwrap();
        assert_eq!(dir, PathBuf::from("/tmp"));
    }
}
