use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::config::ServiceConfig;
use crate::error::{Error, Result};
use crate::identity::Agent;
use crate::process;
use crate::resource::{Kind, Mode, Resource};
use crate::store::{ClaimRequest, Store};
use crate::util;

#[derive(Clone, Debug, Serialize)]
pub struct ServiceStatus {
    pub label: String,
    pub launchd_label: String,
    pub pid: Option<u32>,
    pub port: Option<u16>,
    pub claimed_by: Option<String>,
    pub health_url: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RestartReport {
    pub label: String,
    pub before_pid: Option<u32>,
    pub after_pid: Option<u32>,
    pub idle_waited_secs: f64,
    pub health_ok: bool,
    pub dry_run: bool,
}

pub fn list(store: &Store) -> Result<Vec<ServiceStatus>> {
    let leases = store.active()?;
    Ok(store
        .config()
        .services
        .iter()
        .map(|(label, config)| {
            let launchd_label = config.label(label).to_string();
            let claimed_by = leases
                .iter()
                .find(|lease| lease.kind == Kind::Service && lease.key == *label)
                .map(|lease| lease.agent.clone());
            ServiceStatus {
                label: label.clone(),
                launchd_label: launchd_label.clone(),
                pid: process::launchctl_pid(&launchd_label),
                port: config.port,
                claimed_by,
                health_url: config.health_url.clone(),
            }
        })
        .collect())
}

pub fn restart(
    store: &Store,
    agent: &Agent,
    label: &str,
    wait: Duration,
    health_timeout: Option<Duration>,
    dry_run: bool,
) -> Result<RestartReport> {
    let config = store.config().service(label).cloned().ok_or_else(|| {
        Error::Usage(format!(
            "service {label:?} is not configured under [services.{label:?}]"
        ))
    })?;
    let launchd_label = config.label(label).to_string();
    if config.idle_url.is_some() && config.idle_key.is_none() {
        return Err(Error::Usage(format!(
            "service {label:?} sets idle_url without idle_key"
        )));
    }
    let before_pid = process::launchctl_pid(&launchd_label);

    if dry_run {
        return Ok(RestartReport {
            label: label.to_string(),
            before_pid,
            after_pid: before_pid,
            idle_waited_secs: 0.0,
            health_ok: true,
            dry_run: true,
        });
    }

    let claims = service_claims(label, &config)?;
    let result = store.claim(ClaimRequest {
        agent: agent.id.clone(),
        resources: Resource::dedupe(claims.clone()),
        ttl: Duration::from_secs(10 * 60),
        wait,
        reason: format!("restart service {label}"),
        pid: None,
        agent_pid: Some((agent.pid, agent.pid_start.clone().unwrap_or_default())),
        command: Some(format!("service restart {label}")),
    })?;
    let group_id = result.group_id.clone();

    let outcome = restart_locked(&config, &launchd_label, before_pid, health_timeout);
    let release = store.release(&group_id, &agent.id, false);
    match outcome {
        Ok((after_pid, idle_waited, health_ok)) => {
            let _ = store.log_event(
                "service.restart",
                Some(&agent.id),
                Some(&group_id),
                Some(&claims),
                Some(serde_json::json!({
                    "label": label,
                    "before_pid": before_pid,
                    "after_pid": after_pid,
                    "idle_waited_secs": idle_waited,
                    "health_ok": health_ok,
                })),
            );
            if let Err(err) = release {
                eprintln!("arbiter: warning: could not release service lease: {err}");
            }
            Ok(RestartReport {
                label: label.to_string(),
                before_pid,
                after_pid,
                idle_waited_secs: idle_waited,
                health_ok,
                dry_run: false,
            })
        }
        Err(err) => {
            let _ = store.log_event(
                "service.restart.failed",
                Some(&agent.id),
                Some(&group_id),
                Some(&claims),
                Some(serde_json::json!({
                    "label": label,
                    "before_pid": before_pid,
                    "error": err.to_string(),
                })),
            );
            if let Err(release_err) = release {
                eprintln!("arbiter: warning: could not release service lease: {release_err}");
            }
            Err(err)
        }
    }
}

fn service_claims(label: &str, config: &ServiceConfig) -> Result<Vec<Resource>> {
    let mut claims = vec![Resource {
        kind: Kind::Service,
        key: label.to_string(),
        display: format!("service:{label}"),
        mode: Mode::Exclusive,
        exact: true,
        host: None,
        proto: None,
        meta: serde_json::json!({ "service": label }),
    }];
    if let Some(port) = config.port {
        claims.push(Resource {
            kind: Kind::Port,
            key: format!("{port}/tcp"),
            display: format!("port:{port}"),
            mode: Mode::Exclusive,
            exact: false,
            host: Some("*".into()),
            proto: Some("tcp".into()),
            meta: serde_json::json!({ "port": port }),
        });
    }
    Ok(claims)
}

fn restart_locked(
    config: &ServiceConfig,
    launchd_label: &str,
    before_pid: Option<u32>,
    health_timeout: Option<Duration>,
) -> Result<(Option<u32>, f64, bool)> {
    let idle_started = Instant::now();
    if let (Some(url), Some(key)) = (&config.idle_url, &config.idle_key) {
        poll_until(config.idle_timeout, "service to become idle", || {
            let (status, body) = http_get(url, Duration::from_secs(3))?;
            if !(200..300).contains(&status) {
                return Ok(false);
            }
            let json: serde_json::Value = serde_json::from_str(&body)
                .map_err(|e| Error::Failed(format!("idle endpoint returned non-JSON: {e}")))?;
            let value = json_path(&json, key)
                .ok_or_else(|| Error::Failed(format!("idle key {key:?} missing from {url}")))?;
            Ok(*value == config.idle_value)
        })?;
    }
    let idle_waited = idle_started.elapsed().as_secs_f64();

    let status = process::launchctl_kickstart(launchd_label)
        .map_err(|e| Error::Failed(format!("cannot run launchctl: {e}")))?;
    if !status.success() {
        return Err(Error::Failed(format!(
            "launchctl kickstart {launchd_label} failed with {status}"
        )));
    }

    let timeout = health_timeout.unwrap_or(config.health_timeout);
    poll_until(timeout, "restarted service to become healthy", || {
        let current = process::launchctl_pid(launchd_label);
        let pid_replaced = match before_pid {
            Some(before) => current.map(|pid| pid != before).unwrap_or(false),
            None => current.is_some(),
        };
        if !pid_replaced {
            return Ok(false);
        }
        if let Some(url) = &config.health_url {
            let (status, _) = http_get(url, Duration::from_secs(3))?;
            return Ok((200..300).contains(&status));
        }
        Ok(true)
    })?;

    let after_pid = process::launchctl_pid(launchd_label);
    Ok((after_pid, idle_waited, true))
}

fn poll_until<F>(timeout: Duration, what: &str, mut probe: F) -> Result<()>
where
    F: FnMut() -> Result<bool>,
{
    let started = Instant::now();
    let mut last_error: Option<String> = None;
    loop {
        match probe() {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(err) => last_error = Some(err.to_string()),
        }
        if started.elapsed() >= timeout {
            return Err(Error::Timeout(match last_error {
                Some(error) => format!(
                    "timed out after {} waiting for {what}; last error: {error}",
                    util::format_duration(timeout)
                ),
                None => format!(
                    "timed out after {} waiting for {what}",
                    util::format_duration(timeout)
                ),
            }));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn json_path<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut cursor = value;
    for part in path.split('.') {
        cursor = cursor.get(part)?;
    }
    Some(cursor)
}

fn http_get(url: &str, timeout: Duration) -> Result<(u16, String)> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| Error::Usage(format!("only http:// URLs are supported: {url}")))?;
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    let (host, port) = split_authority(authority)?;
    let addr = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| Error::Failed(format!("cannot resolve {host}:{port}: {e}")))?
        .next()
        .ok_or_else(|| Error::Failed(format!("no address for {host}:{port}")))?;
    let mut stream = TcpStream::connect_timeout(&addr, timeout)
        .map_err(|e| Error::Failed(format!("cannot connect to {host}:{port}: {e}")))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nUser-Agent: arbiter\r\n\r\n"
    );
    stream.write_all(request.as_bytes())?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let status = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| Error::Failed(format!("malformed HTTP response from {url}")))?;
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    Ok((status, body))
}

fn split_authority(authority: &str) -> Result<(String, u16)> {
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, tail) = rest
            .split_once(']')
            .ok_or_else(|| Error::Usage(format!("invalid IPv6 authority {authority:?}")))?;
        let port = tail
            .strip_prefix(':')
            .and_then(|p| p.parse().ok())
            .unwrap_or(80);
        return Ok((host.to_string(), port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => Ok((
            host.to_string(),
            port.parse()
                .map_err(|_| Error::Usage(format!("invalid port in {authority:?}")))?,
        )),
        None => Ok((authority.to_string(), 80)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_paths_navigate() {
        let value = serde_json::json!({"a": {"b": 0}, "c": 2});
        assert_eq!(json_path(&value, "a.b"), Some(&serde_json::json!(0)));
        assert_eq!(json_path(&value, "c"), Some(&serde_json::json!(2)));
        assert_eq!(json_path(&value, "a.x"), None);
    }

    #[test]
    fn parses_authorities() {
        assert_eq!(
            split_authority("127.0.0.1:3456").unwrap(),
            ("127.0.0.1".into(), 3456)
        );
        assert_eq!(split_authority("[::1]:8000").unwrap(), ("::1".into(), 8000));
        assert_eq!(
            split_authority("localhost").unwrap(),
            ("localhost".into(), 80)
        );
    }
}
