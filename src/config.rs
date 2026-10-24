use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::util;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub defaults: Defaults,
    pub paths: PathSettings,
    pub pools: BTreeMap<String, usize>,
    pub caches: BTreeMap<String, PathBuf>,
    pub services: BTreeMap<String, ServiceConfig>,
    pub scan: ScanSettings,
    pub git: GitSettings,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Defaults {
    #[serde(with = "humantime_serde")]
    pub ttl: Duration,
    #[serde(with = "humantime_serde")]
    pub wait: Duration,
    #[serde(with = "humantime_serde")]
    pub heartbeat: Duration,
    #[serde(with = "humantime_serde")]
    pub git_lease_ttl: Duration,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(30 * 60),
            wait: Duration::ZERO,
            heartbeat: Duration::from_secs(10),
            git_lease_ttl: Duration::from_secs(5 * 60),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PathSettings {
    /// None means auto: case-insensitive on macOS, case-sensitive elsewhere.
    pub case_insensitive: Option<bool>,
}

impl PathSettings {
    pub fn is_case_insensitive(&self) -> bool {
        self.case_insensitive.unwrap_or(cfg!(target_os = "macos"))
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServiceConfig {
    pub port: Option<u16>,
    pub launchd_label: Option<String>,
    pub idle_url: Option<String>,
    pub idle_key: Option<String>,
    pub idle_value: serde_json::Value,
    #[serde(with = "humantime_serde")]
    pub idle_timeout: Duration,
    pub health_url: Option<String>,
    #[serde(with = "humantime_serde")]
    pub health_timeout: Duration,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            port: None,
            launchd_label: None,
            idle_url: None,
            idle_key: None,
            idle_value: serde_json::Value::from(0),
            idle_timeout: Duration::from_secs(60),
            health_url: None,
            health_timeout: Duration::from_secs(60),
        }
    }
}

impl ServiceConfig {
    pub fn label<'a>(&'a self, key: &'a str) -> &'a str {
        self.launchd_label.as_deref().unwrap_or(key)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScanSettings {
    pub roots: Vec<PathBuf>,
    pub known_ports: Vec<u16>,
    #[serde(with = "humantime_serde")]
    pub stale_lock_after: Duration,
    pub include_dirty: bool,
}

impl Default for ScanSettings {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            known_ports: Vec::new(),
            stale_lock_after: Duration::from_secs(10 * 60),
            include_dirty: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GitSettings {
    pub binary: Option<PathBuf>,
    pub guard: bool,
}

impl Default for GitSettings {
    fn default() -> Self {
        Self {
            binary: None,
            guard: true,
        }
    }
}

impl Config {
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        let path = match explicit {
            Some(path) => util::expand_tilde(path),
            None => util::config_path()?,
        };
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(&path)?;
        let mut config: Config = toml::from_str(&raw).map_err(|source| Error::Config {
            context: format!("cannot parse {}", path.display()),
            source,
        })?;
        config.normalize(&path)?;
        Ok(config)
    }

    fn normalize(&mut self, path: &Path) -> Result<()> {
        let base = path.parent().unwrap_or(Path::new("/"));
        for value in self.caches.values_mut() {
            let expanded = util::expand_tilde(value);
            *value = if expanded.is_absolute() {
                expanded
            } else {
                base.join(expanded)
            };
        }
        for root in &mut self.scan.roots {
            let expanded = util::expand_tilde(root);
            *root = if expanded.is_absolute() {
                expanded
            } else {
                base.join(expanded)
            };
        }
        if self.defaults.heartbeat.is_zero() {
            return Err(Error::Usage(
                "defaults.heartbeat must be greater than zero".into(),
            ));
        }
        if self.defaults.ttl < self.defaults.heartbeat * 2 {
            return Err(Error::Usage(
                "defaults.ttl must be at least twice defaults.heartbeat".into(),
            ));
        }
        Ok(())
    }

    pub fn service(&self, label: &str) -> Option<&ServiceConfig> {
        self.services.get(label)
    }

    pub fn pool_capacity(&self, name: &str) -> usize {
        self.pools.get(name).copied().unwrap_or(1)
    }

    pub fn cache_path(&self, name: &str) -> Option<&PathBuf> {
        self.caches.get(name)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RepoConfig {
    pub project: ProjectSection,
    pub auto: AutoSection,
    pub resources: ResourceSection,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectSection {
    pub name: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AutoSection {
    pub claims: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResourceSection {
    pub ports: Vec<u16>,
    pub services: Vec<String>,
    pub protected_paths: Vec<PathBuf>,
}

impl RepoConfig {
    pub fn load(repo_root: &Path) -> Result<Option<Self>> {
        let path = repo_root.join(".arbiter.toml");
        if !path.exists() {
            return Ok(None);
        }
        let raw = fs::read_to_string(&path)?;
        toml::from_str(&raw)
            .map(Some)
            .map_err(|source| Error::Config {
                context: format!("cannot parse {}", path.display()),
                source,
            })
    }
}

pub fn default_repo_config(name: &str) -> String {
    format!(
        r#"# Arbiter project policy. Read by `arbiter run --auto` and `arbiter scan`.
[project]
name = "{name}"

[auto]
# Claims taken for a wrapped agent session. {{worktree}}, {{repo}}, {{cwd}}, {{branch}}
# are substituted at runtime. Omit to use the defaults:
# claims = ["worktree:{{worktree}}", "refs:{{repo}}:ro"]

[resources]
# Ports this project commonly binds; scan reports unclaimed listeners.
ports = []
# Launchd service labels (configured globally) this project restarts.
services = []
# Paths where destructive cleanup must be blocked unless claimed.
protected_paths = []
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_global_config() {
        let raw = r#"
[defaults]
ttl = "45m"
heartbeat = "15s"

[pools]
build = 2

[caches]
uv = "~/.cache/uv"

[services."com.example.portal"]
port = 8000
idle_url = "http://127.0.0.1:8000/metrics"
idle_key = "active_requests"
health_url = "http://127.0.0.1:8000/health"
"#;
        let mut cfg: Config = toml::from_str(raw).unwrap();
        cfg.normalize(Path::new("/tmp/arbiter.toml")).unwrap();
        assert_eq!(cfg.defaults.ttl, Duration::from_secs(2700));
        assert_eq!(cfg.pool_capacity("build"), 2);
        assert_eq!(cfg.pool_capacity("absent"), 1);
        assert!(cfg.caches["uv"].ends_with(".cache/uv"));
        assert_eq!(cfg.service("com.example.portal").unwrap().port, Some(8000));
    }

    #[test]
    fn rejects_unknown_keys() {
        let raw = "[defaults]\nttl = \"30m\"\nheartbeat = \"10s\"\nbogus = 1\n";
        assert!(toml::from_str::<Config>(raw).is_err());
    }

    #[test]
    fn parses_repo_config() {
        let raw = r#"
[project]
name = "lapis"
[auto]
claims = ["worktree:{worktree}"]
[resources]
ports = [8000]
services = ["com.kearm.portal"]
"#;
        let repo: RepoConfig = toml::from_str(raw).unwrap();
        assert_eq!(repo.project.name, "lapis");
        assert_eq!(repo.resources.ports, vec![8000]);
        assert_eq!(repo.auto.claims.unwrap().len(), 1);
    }
}
