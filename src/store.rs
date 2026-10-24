use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::process;
use crate::resource::{Kind, Mode, Resource};
use crate::util;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Lease {
    pub id: String,
    pub group_id: String,
    pub agent: String,
    pub kind: Kind,
    pub key: String,
    pub display: String,
    pub mode: Mode,
    pub exact: bool,
    pub host: Option<String>,
    pub proto: Option<String>,
    pub meta: serde_json::Value,
    pub pid: Option<u32>,
    pub pid_start: Option<String>,
    pub host_name: String,
    pub created_at: f64,
    pub heartbeat_at: f64,
    pub expires_at: f64,
}

impl Lease {
    pub fn resource(&self) -> Resource {
        Resource {
            kind: self.kind,
            key: self.key.clone(),
            display: self.display.clone(),
            mode: self.mode,
            exact: self.exact,
            host: self.host.clone(),
            proto: self.proto.clone(),
            meta: self.meta.clone(),
        }
    }

    pub fn remaining(&self, now: f64) -> f64 {
        (self.expires_at - now).max(0.0)
    }

    pub fn holder(&self) -> String {
        match (self.pid, self.pid_start.as_deref()) {
            (Some(pid), Some(_)) => format!("{} (pid {pid})", self.agent),
            _ => self.agent.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Waiter {
    pub id: String,
    pub agent: String,
    pub resources: Vec<Resource>,
    pub requested_at: f64,
    pub expires_at: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Conflict {
    pub requested: Resource,
    pub holder: Option<Lease>,
    pub reason: String,
}

impl Conflict {
    pub fn describe(&self) -> String {
        match &self.holder {
            Some(lease) => format!(
                "{} is held by {} for {} (expires in {}, reason: {})",
                self.requested,
                lease.holder(),
                util::format_duration(Duration::from_secs(
                    (now() - lease.created_at).max(0.0).round() as u64
                )),
                util::format_duration(Duration::from_secs(
                    (lease.expires_at - now()).max(0.0).round() as u64
                )),
                lease_reason(lease),
            ),
            None => format!("{}: {}", self.requested, self.reason),
        }
    }
}

fn lease_reason(lease: &Lease) -> String {
    lease
        .meta
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("-")
        .to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub id: i64,
    pub ts: f64,
    pub kind: String,
    pub agent: Option<String>,
    pub lease_id: Option<String>,
    pub resources: Option<String>,
    pub detail: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ClaimRequest {
    pub agent: String,
    pub resources: Vec<Resource>,
    pub ttl: Duration,
    pub wait: Duration,
    pub reason: String,
    pub pid: Option<(u32, String)>,
    pub agent_pid: Option<(u32, String)>,
    pub command: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ClaimResult {
    pub group_id: String,
    pub leases: Vec<Lease>,
    pub waited: Duration,
}

pub struct Store {
    conn: Connection,
    config: Config,
    host: String,
    path: PathBuf,
}

impl Store {
    pub fn open(config: Config) -> Result<Self> {
        let dir = util::state_dir()?;
        fs::create_dir_all(&dir)?;
        set_private_dir(&dir);
        let path = dir.join("state.sqlite3");
        let mut last_error = None;
        for attempt in 0..10 {
            match Connection::open(&path) {
                Ok(conn) => {
                    let setup = (|| -> Result<()> {
                        conn.busy_timeout(Duration::from_secs(5))?;
                        conn.query_row("PRAGMA journal_mode=WAL", [], |row| {
                            row.get::<_, String>(0)
                        })?;
                        conn.pragma_update(None, "synchronous", "NORMAL")?;
                        conn.pragma_update(None, "foreign_keys", "ON")?;
                        Ok(())
                    })();
                    match setup {
                        Ok(()) => {
                            let store = Self {
                                conn,
                                config,
                                host: util::hostname(),
                                path,
                            };
                            store.migrate()?;
                            set_private_file(&store.path);
                            return Ok(store);
                        }
                        Err(err) => last_error = Some(err),
                    }
                }
                Err(err) => last_error = Some(Error::Db(err)),
            }
            // Two processes initializing a fresh WAL database can race the
            // journal-mode switch; retry briefly instead of failing the claim.
            if attempt < 9 {
                thread::sleep(Duration::from_millis(50 + attempt * 25));
            }
        }
        Err(last_error.unwrap_or_else(|| Error::Failed("could not open lease database".into())))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS leases (
                id TEXT PRIMARY KEY,
                group_id TEXT NOT NULL,
                agent TEXT NOT NULL,
                kind TEXT NOT NULL,
                key TEXT NOT NULL,
                display TEXT NOT NULL,
                mode TEXT NOT NULL,
                exact INTEGER NOT NULL DEFAULT 0,
                host TEXT,
                proto TEXT,
                meta TEXT NOT NULL,
                pid INTEGER,
                pid_start TEXT,
                host_name TEXT NOT NULL,
                created_at REAL NOT NULL,
                heartbeat_at REAL NOT NULL,
                expires_at REAL NOT NULL
            );
            CREATE INDEX IF NOT EXISTS leases_group ON leases(group_id);
            CREATE INDEX IF NOT EXISTS leases_kind_key ON leases(kind, key);
            CREATE INDEX IF NOT EXISTS leases_host ON leases(host_name);

            CREATE TABLE IF NOT EXISTS waiters (
                id TEXT PRIMARY KEY,
                agent TEXT NOT NULL,
                resources TEXT NOT NULL,
                host_name TEXT NOT NULL,
                pid INTEGER,
                pid_start TEXT,
                requested_at REAL NOT NULL,
                expires_at REAL NOT NULL
            );
            CREATE INDEX IF NOT EXISTS waiters_host ON waiters(host_name, requested_at);

            CREATE TABLE IF NOT EXISTS events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts REAL NOT NULL,
                kind TEXT NOT NULL,
                agent TEXT,
                lease_id TEXT,
                resources TEXT,
                detail TEXT
            );
            CREATE INDEX IF NOT EXISTS events_ts ON events(ts);

            PRAGMA user_version = 1;
            "#,
        )?;
        Ok(())
    }

    pub fn claim(&self, request: ClaimRequest) -> Result<ClaimResult> {
        if request.resources.is_empty() {
            return Err(Error::Usage("no resources to claim".into()));
        }
        let started = Instant::now();
        let deadline = started + request.wait;
        let waiter_id = if request.wait > Duration::ZERO {
            Some(self.insert_waiter(&request)?)
        } else {
            None
        };

        loop {
            let outcome = self.try_claim(&request, waiter_id.as_deref())?;
            match outcome {
                TryClaim::Acquired(mut result) => {
                    if waiter_id.is_some() {
                        result.waited = started.elapsed();
                    }
                    return Ok(result);
                }
                TryClaim::Blocked {
                    conflicts,
                    earlier_waiter,
                } => {
                    if request.wait == Duration::ZERO {
                        if let Some(id) = &waiter_id {
                            let _ = self.delete_waiter(id);
                        }
                        return Err(Error::Conflict(format_conflicts(&conflicts)));
                    }
                    if earlier_waiter {
                        // Another waiter is ahead of us; do not barge.
                    }
                    if Instant::now() >= deadline {
                        if let Some(id) = &waiter_id {
                            let _ = self.delete_waiter(id);
                        }
                        return Err(Error::Timeout(format!(
                            "timed out after {} waiting for {}: {}",
                            util::format_duration(request.wait),
                            request
                                .resources
                                .iter()
                                .map(|r| r.to_string())
                                .collect::<Vec<_>>()
                                .join(", "),
                            format_conflicts(&conflicts)
                        )));
                    }
                    let jitter = (now() * 1000.0) as u64 % 40;
                    thread::sleep(Duration::from_millis(210 + jitter));
                }
            }
        }
    }

    fn insert_waiter(&self, request: &ClaimRequest) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        self.reap(&tx)?;
        let requested_at = now();
        let expires_at = requested_at + request.wait.as_secs_f64() + 5.0;
        let resources = serde_json::to_string(&request.resources)?;
        let (pid, pid_start) = match &request.pid {
            Some((pid, start)) => (Some(*pid), Some(start.clone())),
            None => (None, None),
        };
        tx.execute(
            "INSERT INTO waiters (id, agent, resources, host_name, pid, pid_start, requested_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![id, request.agent, resources, self.host, pid, pid_start, requested_at, expires_at],
        )?;
        self.log_event_tx(
            &tx,
            "claim.queued",
            Some(&request.agent),
            None,
            Some(&request.resources),
            Some(&serde_json::json!({ "waiter": id, "wait": request.wait.as_secs_f64() })),
        )?;
        tx.commit()?;
        Ok(id)
    }

    fn delete_waiter(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM waiters WHERE id = ?1", params![id])?;
        Ok(())
    }

    fn try_claim(&self, request: &ClaimRequest, waiter_id: Option<&str>) -> Result<TryClaim> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        self.reap(&tx)?;
        let leases = self.active_tx(&tx)?;
        let now = now();

        let mut earlier_waiter = false;
        if let Some(id) = waiter_id {
            earlier_waiter = self.earlier_waiter_blocks(&tx, id, &request.resources, now)?;
        }

        let mut conflicts = Vec::new();
        if !earlier_waiter {
            for resource in &request.resources {
                if resource.kind != Kind::Pool {
                    for lease in &leases {
                        if resource.conflicts_with(&lease.resource(), self.case_insensitive()) {
                            conflicts.push(Conflict {
                                requested: resource.clone(),
                                holder: Some(lease.clone()),
                                reason: "conflict".into(),
                            });
                        }
                    }
                }
                if resource.kind == Kind::Pool {
                    let held = leases
                        .iter()
                        .filter(|l| l.kind == Kind::Pool && l.key == resource.key)
                        .count();
                    let capacity = self.config.pool_capacity(&resource.key);
                    if held >= capacity {
                        conflicts.push(Conflict {
                            requested: resource.clone(),
                            holder: None,
                            reason: format!(
                                "pool is full: {held}/{capacity} slots held; active holders: {}",
                                leases
                                    .iter()
                                    .filter(|l| l.kind == Kind::Pool && l.key == resource.key)
                                    .map(|l| l.holder())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                        });
                    }
                }
            }
        }

        if !conflicts.is_empty() || earlier_waiter {
            return Ok(TryClaim::Blocked {
                conflicts,
                earlier_waiter,
            });
        }

        let group_id = Uuid::new_v4().to_string();
        let mut inserted = Vec::new();
        for resource in &request.resources {
            let id = Uuid::new_v4().to_string();
            let mut meta = resource.meta.clone();
            if let Some(object) = meta.as_object_mut() {
                object.insert(
                    "reason".into(),
                    serde_json::Value::String(request.reason.clone()),
                );
                if let Some((agent_pid, agent_start)) = &request.agent_pid {
                    object.insert("agent_pid".into(), serde_json::json!(agent_pid));
                    object.insert(
                        "agent_start".into(),
                        serde_json::Value::String(agent_start.clone()),
                    );
                }
                if let Some(command) = &request.command {
                    object.insert("command".into(), serde_json::Value::String(command.clone()));
                }
            }
            let (pid, pid_start) = match &request.pid {
                Some((pid, start)) => (Some(*pid), Some(start.clone())),
                None => (None, None),
            };
            tx.execute(
                "INSERT INTO leases (id, group_id, agent, kind, key, display, mode, exact, host, proto, meta, pid, pid_start, host_name, created_at, heartbeat_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?15, ?16)",
                params![
                    id,
                    group_id,
                    request.agent,
                    resource.kind.as_str(),
                    resource.key,
                    resource.display,
                    resource.mode.as_str(),
                    resource.exact as i64,
                    resource.host,
                    resource.proto,
                    serde_json::to_string(&meta)?,
                    pid,
                    pid_start,
                    self.host,
                    now,
                    now + request.ttl.as_secs_f64(),
                ],
            )?;
            inserted.push(Lease {
                id,
                group_id: group_id.clone(),
                agent: request.agent.clone(),
                kind: resource.kind,
                key: resource.key.clone(),
                display: resource.display.clone(),
                mode: resource.mode,
                exact: resource.exact,
                host: resource.host.clone(),
                proto: resource.proto.clone(),
                meta,
                pid,
                pid_start,
                host_name: self.host.clone(),
                created_at: now,
                heartbeat_at: now,
                expires_at: now + request.ttl.as_secs_f64(),
            });
        }
        if let Some(id) = waiter_id {
            tx.execute("DELETE FROM waiters WHERE id = ?1", params![id])?;
        }
        self.log_event_tx(
            &tx,
            "claim.acquired",
            Some(&request.agent),
            Some(&group_id),
            Some(&request.resources),
            Some(&serde_json::json!({
                "reason": request.reason,
                "ttl": request.ttl.as_secs_f64(),
                "pid": request.pid.as_ref().map(|(pid, _)| *pid),
            })),
        )?;
        tx.commit()?;
        Ok(TryClaim::Acquired(ClaimResult {
            group_id,
            leases: inserted,
            waited: Duration::ZERO,
        }))
    }

    fn earlier_waiter_blocks(
        &self,
        tx: &Transaction,
        waiter_id: &str,
        resources: &[Resource],
        now: f64,
    ) -> Result<bool> {
        let mut stmt = tx.prepare(
            "SELECT id, agent, resources, requested_at, expires_at, pid, pid_start
             FROM waiters
             WHERE host_name = ?1 AND expires_at > ?2
             ORDER BY requested_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![self.host, now], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, f64>(3)?,
                row.get::<_, f64>(4)?,
                row.get::<_, Option<u32>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?;
        let mut earlier = Vec::new();
        for row in rows {
            let (id, agent, resources_raw, requested_at, expires_at, pid, pid_start) = row?;
            if id == waiter_id {
                break;
            }
            let alive = match pid {
                Some(pid) => process::is_alive(pid, pid_start.as_deref()),
                None => true,
            };
            if !alive || expires_at <= now {
                continue;
            }
            earlier.push((id, agent, resources_raw, requested_at));
        }
        for (_, _, resources_raw, _) in earlier {
            let Ok(other) = serde_json::from_str::<Vec<Resource>>(&resources_raw) else {
                continue;
            };
            if resources.iter().any(|a| {
                other
                    .iter()
                    .any(|b| a.conflicts_with(b, self.case_insensitive()))
            }) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn case_insensitive(&self) -> bool {
        self.config.paths.is_case_insensitive()
    }

    fn reap(&self, tx: &Transaction) -> Result<()> {
        let now = now();
        let mut stmt = tx.prepare(
            "SELECT id, group_id, agent, display, pid, pid_start, expires_at FROM leases",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<u32>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, f64>(6)?,
            ))
        })?;
        let candidates: Vec<_> = rows.filter_map(std::result::Result::ok).collect();
        drop(stmt);
        let mut expired = Vec::new();
        for (id, group_id, agent, display, pid, pid_start, expires_at) in candidates {
            let dead = match pid {
                Some(pid) => !process::is_alive(pid, pid_start.as_deref()),
                None => false,
            };
            if dead || expires_at <= now {
                expired.push((id, group_id, agent, display, dead));
            }
        }
        for (id, group_id, agent, display, dead) in expired {
            tx.execute("DELETE FROM leases WHERE id = ?1", params![id])?;
            self.log_event_tx(
                tx,
                if dead {
                    "lease.reaped_dead"
                } else {
                    "lease.expired"
                },
                Some(&agent),
                Some(&group_id),
                None,
                Some(&serde_json::json!({ "lease": id, "resource": display })),
            )?;
        }

        type WaiterState = (String, String, Option<u32>, Option<String>, f64);
        let waiters: Vec<WaiterState> = {
            let mut stmt = tx.prepare(
                "SELECT id, agent, pid, pid_start, expires_at FROM waiters WHERE host_name = ?1",
            )?;
            let rows = stmt.query_map(params![self.host], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<u32>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, f64>(4)?,
                ))
            })?;
            rows.filter_map(std::result::Result::ok).collect()
        };
        for (id, agent, pid, pid_start, expires_at) in waiters {
            let dead = match pid {
                Some(pid) => !process::is_alive(pid, pid_start.as_deref()),
                None => false,
            };
            if dead || expires_at <= now {
                tx.execute("DELETE FROM waiters WHERE id = ?1", params![id])?;
                self.log_event_tx(
                    tx,
                    "waiter.expired",
                    Some(&agent),
                    None,
                    None,
                    Some(&serde_json::json!({ "waiter": id, "dead": dead })),
                )?;
            }
        }
        Ok(())
    }

    fn active_tx(&self, tx: &Transaction) -> Result<Vec<Lease>> {
        let mut stmt = tx.prepare(
            "SELECT id, group_id, agent, kind, key, display, mode, exact, host, proto, meta, pid, pid_start, host_name, created_at, heartbeat_at, expires_at
             FROM leases WHERE host_name = ?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![self.host], row_to_lease)?;
        Ok(rows.filter_map(std::result::Result::ok).collect())
    }

    pub fn active(&self) -> Result<Vec<Lease>> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        self.reap(&tx)?;
        let leases = self.active_tx(&tx)?;
        tx.commit()?;
        Ok(leases)
    }

    pub fn check(&self, resources: &[Resource]) -> Result<Vec<Conflict>> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        self.reap(&tx)?;
        let leases = self.active_tx(&tx)?;
        tx.commit()?;
        let mut conflicts = Vec::new();
        for resource in resources {
            for lease in &leases {
                if resource.conflicts_with(&lease.resource(), self.case_insensitive()) {
                    conflicts.push(Conflict {
                        requested: resource.clone(),
                        holder: Some(lease.clone()),
                        reason: "conflict".into(),
                    });
                }
            }
        }
        Ok(conflicts)
    }

    pub fn release(&self, group_or_id: &str, agent: &str, force: bool) -> Result<Vec<Lease>> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        self.reap(&tx)?;
        let mut stmt = tx.prepare(
            "SELECT id, group_id, agent, kind, key, display, mode, exact, host, proto, meta, pid, pid_start, host_name, created_at, heartbeat_at, expires_at
             FROM leases WHERE group_id = ?1 OR id = ?1",
        )?;
        let rows = stmt.query_map(params![group_or_id], row_to_lease)?;
        let leases: Vec<Lease> = rows.filter_map(std::result::Result::ok).collect();
        drop(stmt);
        if leases.is_empty() {
            return Err(Error::Failed(format!(
                "no lease or group matches {group_or_id}"
            )));
        }
        if !force {
            let foreign: Vec<&Lease> = leases.iter().filter(|l| l.agent != agent).collect();
            if !foreign.is_empty() {
                return Err(Error::NotOwner(format!(
                    "{} is held by {}; use --force only as the operator",
                    group_or_id,
                    foreign
                        .iter()
                        .map(|l| l.agent.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
        }
        for lease in &leases {
            tx.execute("DELETE FROM leases WHERE id = ?1", params![lease.id])?;
        }
        self.log_event_tx(
            &tx,
            "lease.released",
            Some(agent),
            Some(&leases[0].group_id),
            Some(&leases.iter().map(|l| l.resource()).collect::<Vec<_>>()),
            Some(&serde_json::json!({ "count": leases.len(), "force": force })),
        )?;
        tx.commit()?;
        Ok(leases)
    }

    pub fn release_one(&self, lease_id: &str, agent: &str) -> Result<Option<Lease>> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let mut stmt = tx.prepare(
            "SELECT id, group_id, agent, kind, key, display, mode, exact, host, proto, meta, pid, pid_start, host_name, created_at, heartbeat_at, expires_at
             FROM leases WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![lease_id], row_to_lease)?;
        let lease = rows.next().transpose()?;
        drop(rows);
        drop(stmt);
        let Some(lease) = lease else {
            return Ok(None);
        };
        if lease.agent != agent {
            return Err(Error::NotOwner(format!(
                "lease {lease_id} is held by {}",
                lease.agent
            )));
        }
        tx.execute("DELETE FROM leases WHERE id = ?1", params![lease_id])?;
        let _ = self.log_event_tx(
            &tx,
            "lease.released",
            Some(agent),
            Some(&lease.group_id),
            Some(&[lease.resource()]),
            Some(&serde_json::json!({ "count": 1, "partial": true })),
        );
        tx.commit()?;
        Ok(Some(lease))
    }

    pub fn release_mine(&self, agent: &str) -> Result<Vec<Lease>> {
        let active = self.active()?;
        let mine: Vec<Lease> = active.into_iter().filter(|l| l.agent == agent).collect();
        if mine.is_empty() {
            return Ok(Vec::new());
        }
        let mut released = Vec::new();
        let mut groups = BTreeMap::new();
        for lease in mine {
            groups.insert(lease.group_id.clone(), ());
        }
        for group in groups.keys() {
            if let Ok(mut leases) = self.release(group, agent, false) {
                released.append(&mut leases);
            }
        }
        Ok(released)
    }

    pub fn renew(&self, group_or_id: &str, agent: &str, ttl: Duration) -> Result<usize> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let now = now();
        let changed = tx.execute(
            "UPDATE leases SET expires_at = ?1, heartbeat_at = ?2 WHERE (group_id = ?3 OR id = ?3) AND agent = ?4",
            params![now + ttl.as_secs_f64(), now, group_or_id, agent],
        )?;
        if changed == 0 {
            return Err(Error::NotOwner(format!(
                "no lease or group {group_or_id} is held by {agent}"
            )));
        }
        tx.commit()?;
        Ok(changed)
    }

    pub fn update_lease_meta(&self, lease_id: &str, patch: serde_json::Value) -> Result<()> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let raw: Option<String> = tx
            .query_row(
                "SELECT meta FROM leases WHERE id = ?1",
                params![lease_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(raw) = raw else {
            return Ok(());
        };
        let mut meta: serde_json::Value =
            serde_json::from_str(&raw).unwrap_or_else(|_| serde_json::json!({}));
        if let (Some(object), Some(patch)) = (meta.as_object_mut(), patch.as_object()) {
            for (key, value) in patch {
                object.insert(key.clone(), value.clone());
            }
        }
        tx.execute(
            "UPDATE leases SET meta = ?1 WHERE id = ?2",
            params![meta.to_string(), lease_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn attach_pid(&self, group_id: &str, pid: u32, pid_start: &str) -> Result<()> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE leases SET pid = ?1, pid_start = ?2 WHERE group_id = ?3",
            params![pid, pid_start, group_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn log_event(
        &self,
        kind: &str,
        agent: Option<&str>,
        lease_id: Option<&str>,
        resources: Option<&[Resource]>,
        detail: Option<serde_json::Value>,
    ) -> Result<()> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        self.log_event_tx(&tx, kind, agent, lease_id, resources, detail.as_ref())?;
        tx.commit()?;
        Ok(())
    }

    fn log_event_tx(
        &self,
        tx: &Transaction,
        kind: &str,
        agent: Option<&str>,
        lease_id: Option<&str>,
        resources: Option<&[Resource]>,
        detail: Option<&serde_json::Value>,
    ) -> Result<()> {
        let resources = match resources {
            Some(resources) => Some(serde_json::to_string(
                &resources.iter().map(|r| r.to_string()).collect::<Vec<_>>(),
            )?),
            None => None,
        };
        tx.execute(
            "INSERT INTO events (ts, kind, agent, lease_id, resources, detail) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                now(),
                kind,
                agent,
                lease_id,
                resources,
                detail.map(|v| v.to_string())
            ],
        )?;
        Ok(())
    }

    pub fn events(&self, since: Option<f64>, limit: usize) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, ts, kind, agent, lease_id, resources, detail FROM events
             WHERE (?1 IS NULL OR ts >= ?1) ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![since, limit as i64], |row| {
            Ok(Event {
                id: row.get(0)?,
                ts: row.get(1)?,
                kind: row.get(2)?,
                agent: row.get(3)?,
                lease_id: row.get(4)?,
                resources: row.get(5)?,
                detail: row.get(6)?,
            })
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect())
    }

    pub fn waiters(&self) -> Result<Vec<Waiter>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, agent, resources, requested_at, expires_at FROM waiters WHERE host_name = ?1 ORDER BY requested_at ASC",
        )?;
        let rows = stmt.query_map(params![self.host], |row| {
            let raw: String = row.get(2)?;
            Ok(Waiter {
                id: row.get(0)?,
                agent: row.get(1)?,
                resources: serde_json::from_str(&raw).unwrap_or_default(),
                requested_at: row.get(3)?,
                expires_at: row.get(4)?,
            })
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect())
    }

    pub fn quick_check(&self) -> Result<String> {
        let value: String = self
            .conn
            .query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        Ok(value)
    }
}

enum TryClaim {
    Acquired(ClaimResult),
    Blocked {
        conflicts: Vec<Conflict>,
        earlier_waiter: bool,
    },
}

fn row_to_lease(row: &rusqlite::Row<'_>) -> rusqlite::Result<Lease> {
    let kind_raw: String = row.get(3)?;
    let mode_raw: String = row.get(6)?;
    let meta_raw: String = row.get(10)?;
    Ok(Lease {
        id: row.get(0)?,
        group_id: row.get(1)?,
        agent: row.get(2)?,
        kind: Kind::parse(&kind_raw).unwrap_or(Kind::Name),
        key: row.get(4)?,
        display: row.get(5)?,
        mode: Mode::parse(&mode_raw).unwrap_or(Mode::Exclusive),
        exact: row.get::<_, i64>(7)? != 0,
        host: row.get(8)?,
        proto: row.get(9)?,
        meta: serde_json::from_str(&meta_raw).unwrap_or(serde_json::Value::Null),
        pid: row.get(11)?,
        pid_start: row.get(12)?,
        host_name: row.get(13)?,
        created_at: row.get(14)?,
        heartbeat_at: row.get(15)?,
        expires_at: row.get(16)?,
    })
}

fn format_conflicts(conflicts: &[Conflict]) -> String {
    if conflicts.is_empty() {
        return "resources are held by an earlier waiter".into();
    }
    conflicts
        .iter()
        .map(Conflict::describe)
        .collect::<Vec<_>>()
        .join("; ")
}

fn now() -> f64 {
    util::now()
}

#[cfg(unix)]
fn set_private_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
}

#[cfg(unix)]
fn set_private_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn set_private_dir(_path: &Path) {}
#[cfg(not(unix))]
fn set_private_file(_path: &Path) {}
