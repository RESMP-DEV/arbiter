use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::util;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Path,
    Refs,
    Port,
    Service,
    Device,
    Name,
    Pool,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Path => "path",
            Kind::Refs => "refs",
            Kind::Port => "port",
            Kind::Service => "service",
            Kind::Device => "device",
            Kind::Name => "name",
            Kind::Pool => "pool",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "path" | "worktree" | "cache" => Kind::Path,
            "refs" => Kind::Refs,
            "port" => Kind::Port,
            "service" => Kind::Service,
            "device" => Kind::Device,
            "name" => Kind::Name,
            "pool" => Kind::Pool,
            _ => return None,
        })
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Shared,
    Exclusive,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Shared => "shared",
            Mode::Exclusive => "exclusive",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "shared" | "ro" => Some(Mode::Shared),
            "exclusive" | "rw" => Some(Mode::Exclusive),
            _ => None,
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resource {
    pub kind: Kind,
    /// Canonical comparison key. Path keys are case-folded when the filesystem
    /// is treated as case-insensitive; port keys are "<port>/<proto>".
    pub key: String,
    pub display: String,
    pub mode: Mode,
    /// Path claims normally cover descendants; exact restricts to equality.
    pub exact: bool,
    /// Port bind host. "*" overlaps every host.
    pub host: Option<String>,
    pub proto: Option<String>,
    pub meta: serde_json::Value,
}

impl Resource {
    pub fn parse(spec: &str, config: &Config, cwd: &Path) -> Result<Self> {
        let (body, mode, exact) = split_modifiers(spec)?;
        let (kind_raw, rest) = body.split_once(':').ok_or_else(|| {
            Error::Usage(format!(
                "invalid resource {spec:?}; expected e.g. path:/repo, port:8000, service:label"
            ))
        })?;
        if rest.is_empty() {
            return Err(Error::Usage(format!("resource {spec:?} has no value")));
        }
        match kind_raw {
            "path" | "worktree" => {
                let path = resolve_path(rest, cwd)?;
                let display = if kind_raw == "worktree" {
                    format!("worktree:{}", path.display())
                } else {
                    path.display().to_string()
                };
                Ok(Resource {
                    kind: Kind::Path,
                    key: comparison_path(&path),
                    display,
                    mode: mode.unwrap_or(Mode::Exclusive),
                    exact: exact || kind_raw == "worktree",
                    host: None,
                    proto: None,
                    meta: serde_json::json!({
                        "path": path.display().to_string(),
                        "worktree": kind_raw == "worktree",
                    }),
                })
            }
            "cache" => {
                let path = config.cache_path(rest).ok_or_else(|| {
                    Error::Usage(format!(
                        "cache {rest:?} is not configured under [caches] in the arbiter config"
                    ))
                })?;
                let path = util::canonicalize_lenient(path)?;
                Ok(Resource {
                    kind: Kind::Path,
                    key: comparison_path(&path),
                    display: format!("cache:{rest}"),
                    mode: mode.unwrap_or(Mode::Exclusive),
                    exact,
                    host: None,
                    proto: None,
                    meta: serde_json::json!({
                        "path": path.display().to_string(),
                        "cache": rest,
                    }),
                })
            }
            "refs" => {
                let path = resolve_path(rest, cwd)?;
                Ok(Resource {
                    kind: Kind::Refs,
                    key: comparison_path(&path),
                    display: format!("refs:{}", path.display()),
                    mode: mode.unwrap_or(Mode::Shared),
                    exact: true,
                    host: None,
                    proto: None,
                    meta: serde_json::json!({ "repo": path.display().to_string() }),
                })
            }
            "port" => {
                reject_shared(kind_raw, mode)?;
                let (host, port, proto) = parse_port(rest)?;
                Ok(Resource {
                    kind: Kind::Port,
                    key: format!("{port}/{proto}"),
                    display: format!("port:{rest}"),
                    mode: Mode::Exclusive,
                    exact: false,
                    host: Some(host),
                    proto: Some(proto),
                    meta: serde_json::json!({ "port": port }),
                })
            }
            "service" => {
                reject_shared(kind_raw, mode)?;
                Ok(Resource {
                    kind: Kind::Service,
                    key: rest.to_string(),
                    display: format!("service:{rest}"),
                    mode: Mode::Exclusive,
                    exact: true,
                    host: None,
                    proto: None,
                    meta: serde_json::json!({ "service": rest }),
                })
            }
            "device" => {
                reject_shared(kind_raw, mode)?;
                Ok(Resource {
                    kind: Kind::Device,
                    key: rest.to_string(),
                    display: format!("device:{rest}"),
                    mode: Mode::Exclusive,
                    exact: true,
                    host: None,
                    proto: None,
                    meta: serde_json::json!({ "device": rest }),
                })
            }
            "name" => {
                reject_shared(kind_raw, mode)?;
                Ok(Resource {
                    kind: Kind::Name,
                    key: rest.to_string(),
                    display: format!("name:{rest}"),
                    mode: Mode::Exclusive,
                    exact: true,
                    host: None,
                    proto: None,
                    meta: serde_json::json!({ "name": rest }),
                })
            }
            "pool" => {
                reject_shared(kind_raw, mode)?;
                Ok(Resource {
                    kind: Kind::Pool,
                    key: rest.to_string(),
                    display: format!("pool:{rest}"),
                    mode: Mode::Exclusive,
                    exact: true,
                    host: None,
                    proto: None,
                    meta: serde_json::json!({ "pool": rest }),
                })
            }
            other => Err(Error::Usage(format!(
                "unknown resource kind {other:?}; expected path, worktree, refs, cache, port, service, device, name, or pool"
            ))),
        }
    }

    pub fn conflicts_with(&self, other: &Self, case_insensitive: bool) -> bool {
        if self.kind != other.kind {
            return false;
        }
        if self.mode == Mode::Shared && other.mode == Mode::Shared {
            return false;
        }
        match self.kind {
            Kind::Path => paths_overlap(
                &self.key,
                &other.key,
                case_insensitive,
                self.exact,
                other.exact,
            ),
            Kind::Refs | Kind::Service | Kind::Device | Kind::Name | Kind::Pool => {
                self.key == other.key
            }
            Kind::Port => {
                self.key == other.key
                    && hosts_overlap(
                        self.host.as_deref().unwrap_or("*"),
                        other.host.as_deref().unwrap_or("*"),
                    )
            }
        }
    }

    pub fn path(&self) -> Option<PathBuf> {
        if self.kind != Kind::Path {
            return None;
        }
        self.meta
            .get("path")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
    }

    pub fn is_worktree(&self) -> bool {
        self.meta
            .get("worktree")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    pub fn dedupe(mut resources: Vec<Resource>) -> Vec<Resource> {
        resources.sort_by(|a, b| {
            (a.kind.as_str(), &a.key, a.mode.as_str()).cmp(&(
                b.kind.as_str(),
                &b.key,
                b.mode.as_str(),
            ))
        });
        resources.dedup_by(|a, b| {
            if a.kind != b.kind || a.key != b.key {
                return false;
            }
            // Keep the stronger mode and the narrower scope.
            if a.mode == Mode::Shared && b.mode == Mode::Exclusive {
                a.mode = Mode::Exclusive;
            }
            if a.exact != b.exact {
                a.exact = false;
            }
            true
        });
        resources
    }
}

impl fmt::Display for Resource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display)?;
        if self.mode == Mode::Shared {
            f.write_str(":ro")?;
        }
        if self.exact && self.kind == Kind::Path {
            f.write_str(":exact")?;
        }
        Ok(())
    }
}

fn reject_shared(kind: &str, mode: Option<Mode>) -> Result<()> {
    if mode == Some(Mode::Shared) {
        return Err(Error::Usage(format!(
            "{kind} claims cannot be shared; they are exclusive by nature"
        )));
    }
    Ok(())
}

fn split_modifiers(spec: &str) -> Result<(String, Option<Mode>, bool)> {
    let mut body = spec.trim().to_string();
    let mut mode = None;
    let mut exact = false;
    while let Some((new_mode, is_exact)) = strip_modifier(&mut body) {
        if new_mode.is_some() {
            mode = new_mode;
        }
        exact |= is_exact;
    }
    if body.is_empty() {
        return Err(Error::Usage(format!("invalid resource {spec:?}")));
    }
    Ok((body, mode, exact))
}

fn strip_modifier(body: &mut String) -> Option<(Option<Mode>, bool)> {
    let (head, tail) = body.rsplit_once(':')?;
    // Only treat the suffix as a modifier when a kind:value body remains.
    // This keeps values like `name:shared` or `service:exact` intact.
    if !head.contains(':')
        || head
            .split_once(':')
            .map(|(_, v)| v.is_empty())
            .unwrap_or(true)
    {
        return None;
    }
    let (mode, exact) = match tail {
        "ro" | "shared" => (Some(Mode::Shared), false),
        "rw" | "exclusive" => (Some(Mode::Exclusive), false),
        "exact" => (None, true),
        _ => return None,
    };
    *body = head.to_string();
    Some((mode, exact))
}

fn resolve_path(raw: &str, cwd: &Path) -> Result<PathBuf> {
    let expanded = util::expand_tilde(Path::new(raw));
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    };
    util::canonicalize_lenient(&absolute)
}

fn comparison_path(path: &Path) -> String {
    let text = path.to_string_lossy().to_string();
    if text.len() > 1 {
        text.trim_end_matches('/').to_string()
    } else {
        text
    }
}

pub fn paths_overlap(
    a: &str,
    b: &str,
    case_insensitive: bool,
    a_exact: bool,
    b_exact: bool,
) -> bool {
    let (a, b) = if case_insensitive {
        (a.to_lowercase(), b.to_lowercase())
    } else {
        (a.to_string(), b.to_string())
    };
    if a == b {
        return true;
    }
    let a_root = a == "/";
    let b_root = b == "/";
    if !a_exact && (a_root || b.starts_with(&format!("{a}/"))) {
        return true;
    }
    if !b_exact && (b_root || a.starts_with(&format!("{b}/"))) {
        return true;
    }
    false
}

fn parse_port(raw: &str) -> Result<(String, u16, String)> {
    let mut rest = raw;
    let mut proto = "tcp".to_string();
    if let Some((head, tail)) = rest.rsplit_once('/') {
        match tail {
            "tcp" | "udp" => {
                proto = tail.to_string();
                rest = head;
            }
            _ => return Err(Error::Usage(format!("unknown port protocol {tail:?}"))),
        }
    }
    let (host, port_raw) = if let Some((port, host)) = rest.rsplit_once('@') {
        (normalize_host(host), port)
    } else if let Some((host, port)) = rest.rsplit_once(':') {
        (normalize_host(host), port)
    } else {
        ("*".to_string(), rest)
    };
    let port: u16 = port_raw
        .parse()
        .map_err(|_| Error::Usage(format!("invalid port {port_raw:?}")))?;
    if port == 0 {
        return Err(Error::Usage("port 0 is not a claimable resource".into()));
    }
    Ok((host, port, proto))
}

fn normalize_host(raw: &str) -> String {
    let raw = raw.trim();
    let raw = raw.trim_start_matches('[').trim_end_matches(']');
    match raw {
        "" | "*" | "0.0.0.0" | "::" => "*".to_string(),
        "localhost" => "127.0.0.1".to_string(),
        other => other.to_string(),
    }
}

fn hosts_overlap(a: &str, b: &str) -> bool {
    a == "*" || b == "*" || a == b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        let mut cfg = Config::default();
        cfg.paths.case_insensitive = Some(true);
        cfg.caches
            .insert("uv".into(), PathBuf::from("/tmp/arbiter-test-cache"));
        cfg
    }

    fn parse(spec: &str) -> Resource {
        Resource::parse(spec, &config(), Path::new("/tmp")).unwrap()
    }

    #[test]
    fn path_claims_cover_descendants() {
        let parent = parse("path:/Users/kearm/lapis");
        let child = parse("path:/Users/kearm/lapis/src/main.rs");
        assert!(parent.conflicts_with(&child, true));
        let exact = parse("path:/Users/kearm/lapis:exact");
        assert!(!exact.conflicts_with(&child, true));
        assert!(exact.conflicts_with(&parse("path:/Users/kearm/lapis"), true));
    }

    #[test]
    fn shared_path_claims_coexist() {
        let a = parse("path:/repo:ro");
        let b = parse("path:/repo:ro");
        assert!(!a.conflicts_with(&b, true));
        let c = parse("path:/repo:rw");
        assert!(a.conflicts_with(&c, true));
    }

    #[test]
    fn sibling_paths_do_not_conflict() {
        let a = parse("path:/repo/a");
        let b = parse("path:/repo/b");
        assert!(!a.conflicts_with(&b, true));
    }

    #[test]
    fn case_folding_matches_macos_default() {
        let a = parse("path:/tmp/arbiter-case-probe/Alpha");
        let b = parse("path:/tmp/arbiter-case-probe/alpha");
        assert!(a.conflicts_with(&b, true));
        assert!(!a.conflicts_with(&b, false));
    }

    #[test]
    fn ports_conflict_on_wildcard_and_protocol() {
        let all = parse("port:8000");
        let local = parse("port:8000@127.0.0.1");
        assert!(all.conflicts_with(&local, true));
        let udp = parse("port:8000/udp");
        assert!(!all.conflicts_with(&udp, true));
        let other = parse("port:8001");
        assert!(!all.conflicts_with(&other, true));
    }

    #[test]
    fn refs_default_shared_but_rw_is_exclusive() {
        let ro = parse("refs:/repo/.git");
        let rw = parse("refs:/repo/.git:rw");
        assert_eq!(ro.mode, Mode::Shared);
        assert!(ro.conflicts_with(&rw, true));
        assert!(!ro.conflicts_with(&parse("refs:/repo/.git"), true));
    }

    #[test]
    fn non_shareable_kinds_reject_ro() {
        assert!(Resource::parse("service:foo:ro", &config(), Path::new("/tmp")).is_err());
        assert!(Resource::parse("port:8000:ro", &config(), Path::new("/tmp")).is_err());
    }

    #[test]
    fn cache_resolves_configured_path() {
        let cache = parse("cache:uv");
        let path = parse("path:/tmp/arbiter-test-cache");
        assert!(cache.conflicts_with(&path, true));
    }

    #[test]
    fn dedupe_keeps_strongest_mode() {
        let resources = vec![
            Resource::parse("path:/repo:ro", &config(), Path::new("/tmp")).unwrap(),
            Resource::parse("path:/repo:rw", &config(), Path::new("/tmp")).unwrap(),
        ];
        let deduped = Resource::dedupe(resources);
        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].mode, Mode::Exclusive);
    }

    #[test]
    fn parses_host_before_port() {
        let r = parse("port:127.0.0.1:9000");
        assert_eq!(r.key, "9000/tcp");
        assert_eq!(r.host.as_deref(), Some("127.0.0.1"));
    }
}
