use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

/// A row from `ps -axo pid=,ppid=,command=`.
#[derive(Clone, Debug)]
pub struct ProcInfo {
    pub ppid: u32,
    pub command: String,
}

pub fn ps_table() -> BTreeMap<u32, ProcInfo> {
    let output = Command::new("/bin/ps")
        .args(["-axo", "pid=,ppid=,command="])
        .output();
    let Ok(output) = output else {
        return BTreeMap::new();
    };
    let mut table = BTreeMap::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut rest = line.trim_start();
        let Some(pid) = take_token(&mut rest).and_then(|v| v.parse::<u32>().ok()) else {
            continue;
        };
        let Some(ppid) = take_token(&mut rest).and_then(|v| v.parse::<u32>().ok()) else {
            continue;
        };
        table.insert(
            pid,
            ProcInfo {
                ppid,
                command: rest.trim().to_string(),
            },
        );
    }
    table
}

fn take_token<'a>(rest: &mut &'a str) -> Option<&'a str> {
    let trimmed = rest.trim_start();
    if trimmed.is_empty() {
        *rest = "";
        return None;
    }
    let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
    let token = &trimmed[..end];
    *rest = &trimmed[end..];
    Some(token)
}

pub fn proc_start(pid: u32) -> Option<String> {
    let output = Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

pub fn is_alive(pid: u32, expected_start: Option<&str>) -> bool {
    if pid == 0 {
        return false;
    }
    let Some(start) = proc_start(pid) else {
        return false;
    };
    match expected_start {
        Some(expected) => start == expected,
        None => true,
    }
}

pub fn current_pid_start() -> Option<String> {
    proc_start(std::process::id())
}

pub fn uid() -> u32 {
    unsafe { libc::getuid() }
}

pub fn cwd_of(pid: u32) -> Option<PathBuf> {
    let output = Command::new("/usr/sbin/lsof")
        .args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(path) = line.strip_prefix('n') {
            return Some(PathBuf::from(path));
        }
    }
    None
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Listener {
    pub pid: u32,
    pub command: String,
    pub host: String,
    pub port: u16,
    pub proto: String,
}

pub fn listeners() -> Vec<Listener> {
    let mut out = Vec::new();
    out.extend(listeners_for("TCP"));
    out.extend(listeners_for("UDP"));
    out
}

fn listeners_for(proto: &str) -> Vec<Listener> {
    let filter = format!("-i{proto}");
    let mut args = vec!["-nP", filter.as_str(), "-Fpcn"];
    if proto == "TCP" {
        args.push("-sTCP:LISTEN");
    }
    let output = Command::new("/usr/sbin/lsof").args(args).output();
    let Ok(output) = output else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut pid = 0u32;
    let mut command = String::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some((field, value)) = line.split_at_checked(1) else {
            continue;
        };
        match field {
            "p" => pid = value.parse().unwrap_or(0),
            "c" => command = value.to_string(),
            "n" => {
                if let Some((host, port)) = split_endpoint(value) {
                    out.push(Listener {
                        pid,
                        command: command.clone(),
                        host,
                        port,
                        proto: proto.to_ascii_lowercase(),
                    });
                }
            }
            _ => {}
        }
    }
    out
}

fn split_endpoint(value: &str) -> Option<(String, u16)> {
    let value = value.trim();
    if let Some((host, port)) = value.rsplit_once(':') {
        let port = port.parse::<u16>().ok()?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let host = if host.is_empty() || host == "*" {
            "*".to_string()
        } else {
            host.to_string()
        };
        return Some((host, port));
    }
    None
}

/// Returns the PID launchd reports for a job, if it is running.
pub fn launchctl_pid(label: &str) -> Option<u32> {
    let binary = std::env::var("ARBITER_LAUNCHCTL_BIN").unwrap_or_else(|_| "launchctl".into());
    let output = Command::new(binary).args(["list", label]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let marker = "\"PID\" =";
    let start = text.find(marker)? + marker.len();
    let rest = &text[start..];
    let digits: String = rest
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

pub fn launchctl_kickstart(label: &str) -> std::io::Result<std::process::ExitStatus> {
    let binary = std::env::var("ARBITER_LAUNCHCTL_BIN").unwrap_or_else(|_| "launchctl".into());
    Command::new(binary)
        .args(["kickstart", "-k", &format!("gui/{}", uid()), label])
        .status()
}

/// Walk the parent chain, returning ancestors from child to init.
pub fn ancestors(pid: u32, table: &BTreeMap<u32, ProcInfo>) -> Vec<u32> {
    let mut out = Vec::new();
    let mut cursor = pid;
    let mut guard = 0;
    while let Some(info) = table.get(&cursor) {
        if info.ppid == 0 || info.ppid == cursor {
            break;
        }
        out.push(info.ppid);
        cursor = info.ppid;
        guard += 1;
        if guard > 64 {
            break;
        }
    }
    out
}

pub fn command_basename(command: &str) -> String {
    let first = command.split_whitespace().next().unwrap_or("");
    let base = first.rsplit('/').next().unwrap_or(first);
    base.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_endpoints() {
        assert_eq!(
            split_endpoint("127.0.0.1:8000"),
            Some(("127.0.0.1".into(), 8000))
        );
        assert_eq!(split_endpoint("*:5000"), Some(("*".into(), 5000)));
        assert_eq!(split_endpoint("[::1]:3456"), Some(("::1".into(), 3456)));
        assert_eq!(split_endpoint("nonsense"), None);
    }

    #[test]
    fn basename_handles_flags() {
        assert_eq!(
            command_basename("/Users/kearm/.cargo/bin/ccr-rust --x"),
            "ccr-rust"
        );
        assert_eq!(command_basename("zcode-cli"), "zcode-cli");
    }
}
