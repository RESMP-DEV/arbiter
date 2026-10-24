use std::env;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};

static STATE_DIR_OVERRIDE: OnceLock<Option<PathBuf>> = OnceLock::new();

pub fn set_state_dir_override(path: Option<PathBuf>) {
    let _ = STATE_DIR_OVERRIDE.set(path);
}

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| Error::Env("HOME is not set".into()))
}

pub fn state_dir() -> Result<PathBuf> {
    if let Some(Some(path)) = STATE_DIR_OVERRIDE.get() {
        return Ok(path.clone());
    }
    if let Some(raw) = env::var_os("ARBITER_STATE_DIR") {
        return Ok(expand_tilde(Path::new(&raw)));
    }
    if let Some(raw) = env::var_os("XDG_STATE_HOME") {
        return Ok(PathBuf::from(raw).join("arbiter"));
    }
    Ok(home_dir()?.join(".local/state/arbiter"))
}

pub fn config_path() -> Result<PathBuf> {
    if let Some(raw) = env::var_os("ARBITER_CONFIG") {
        return Ok(expand_tilde(Path::new(&raw)));
    }
    if let Some(raw) = env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(raw).join("arbiter/config.toml"));
    }
    Ok(home_dir()?.join(".config/arbiter/config.toml"))
}

pub fn expand_tilde(path: &Path) -> PathBuf {
    let Some(first) = path.components().next() else {
        return path.to_path_buf();
    };
    if first != Component::Normal("~".as_ref()) {
        return path.to_path_buf();
    }
    let Ok(home) = home_dir() else {
        return path.to_path_buf();
    };
    home.join(path.strip_prefix("~").unwrap_or(path))
}

/// Canonicalize an existing path, or resolve the nearest existing ancestor and
/// append the missing remainder. Claims often name paths that do not exist yet.
pub fn canonicalize_lenient(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()?.join(path)
    };
    let normalized = normalize_lexically(&absolute);
    if let Ok(canon) = normalized.canonicalize() {
        return Ok(canon);
    }
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut cursor = normalized.as_path();
    loop {
        if let Some(name) = cursor.file_name() {
            missing.push(name.to_os_string());
        }
        match cursor.parent() {
            Some(parent) => {
                if let Ok(mut base) = parent.canonicalize() {
                    for part in missing.iter().rev() {
                        base.push(part);
                    }
                    return Ok(normalize_lexically(&base));
                }
                cursor = parent;
            }
            None => {
                return Err(Error::Usage(format!(
                    "cannot resolve path {}",
                    path.display()
                )));
            }
        }
    }
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from("/")
    } else {
        out
    }
}

pub fn parse_duration(raw: &str) -> Result<Duration> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(Error::Usage("empty duration".into()));
    }
    if let Ok(seconds) = raw.parse::<u64>() {
        return Ok(Duration::from_secs(seconds));
    }
    humantime::parse_duration(raw)
        .map_err(|e| Error::Usage(format!("invalid duration {raw:?}: {e}")))
}

pub fn format_duration(d: Duration) -> String {
    humantime::format_duration(d).to_string()
}

pub fn format_timestamp(secs: f64) -> String {
    let Ok(ts) = time::OffsetDateTime::from_unix_timestamp(secs.round() as i64) else {
        return "?".into();
    };
    let format =
        time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]Z");
    ts.to_offset(time::UtcOffset::UTC)
        .format(&format)
        .unwrap_or_else(|_| "?".into())
}

pub fn format_age(secs: f64) -> String {
    if secs < 0.0 {
        return "0s".into();
    }
    let d = Duration::from_secs(secs.round().max(0.0) as u64);
    if d.as_secs() < 60 {
        format!("{}s", d.as_secs())
    } else if d.as_secs() < 3600 {
        format!("{}m", d.as_secs() / 60)
    } else if d.as_secs() < 86_400 {
        format!("{}h", d.as_secs() / 3600)
    } else {
        format!("{}d", d.as_secs() / 86_400)
    }
}

pub fn hostname() -> String {
    let mut buf = [0u8; 256];
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc == 0 {
        let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
        if let Ok(name) = std::str::from_utf8(&buf[..end]) {
            return name.to_string();
        }
    }
    "unknown".into()
}

pub fn current_user() -> String {
    env::var("USER")
        .or_else(|_| env::var("LOGNAME"))
        .unwrap_or_else(|_| "unknown".into())
}

pub fn tty_name() -> Option<String> {
    unsafe {
        if libc::isatty(libc::STDIN_FILENO) != 1 {
            return None;
        }
        let ptr = libc::ttyname(libc::STDIN_FILENO);
        if ptr.is_null() {
            return None;
        }
        let cstr = std::ffi::CStr::from_ptr(ptr);
        cstr.to_str().ok().map(|s| s.to_string())
    }
}

pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
