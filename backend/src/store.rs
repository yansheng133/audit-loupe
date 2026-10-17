//! SQLite storage for audit records, object versions and alerts.

use crate::audit::{Record, Source, Via};
use crate::diff::{diff, normalize, Change};
use crate::rules::Alert;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::Serialize;
use crate::relations::{extract, pod_template, selector_matches};
use crate::watch::ObjMsg;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA temp_store = MEMORY;
CREATE TABLE IF NOT EXISTS events (
  id INTEGER PRIMARY KEY,
  cluster TEXT NOT NULL, audit_id TEXT NOT NULL,
  ts_ms INTEGER NOT NULL, ts TEXT NOT NULL,
  actor TEXT NOT NULL, principal TEXT NOT NULL, human INTEGER NOT NULL, via TEXT NOT NULL, caller TEXT NOT NULL,
  verb TEXT NOT NULL, api_group TEXT NOT NULL, resource TEXT NOT NULL, subresource TEXT NOT NULL,
  namespace TEXT NOT NULL, name TEXT NOT NULL, code INTEGER NOT NULL,
  source_ip TEXT NOT NULL, user_agent TEXT NOT NULL, request_uri TEXT NOT NULL, request_object TEXT,
  source TEXT NOT NULL DEFAULT 'k8s', user_id TEXT NOT NULL DEFAULT '', link_id INTEGER, story_id INTEGER,
  UNIQUE(cluster, audit_id)
);
CREATE INDEX IF NOT EXISTS events_ts ON events(ts_ms);
CREATE INDEX IF NOT EXISTS events_actor ON events(actor, ts_ms);
CREATE INDEX IF NOT EXISTS events_obj ON events(cluster, resource, namespace, name, ts_ms);
CREATE TABLE IF NOT EXISTS alerts (
  id INTEGER PRIMARY KEY, event_id INTEGER NOT NULL REFERENCES events(id) ON DELETE CASCADE,
  rule TEXT NOT NULL, severity TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS alerts_event ON alerts(event_id);
CREATE TABLE IF NOT EXISTS versions (
  id INTEGER PRIMARY KEY,
  cluster TEXT NOT NULL, api_group TEXT NOT NULL, resource TEXT NOT NULL, kind TEXT NOT NULL,
  namespace TEXT NOT NULL, name TEXT NOT NULL, uid TEXT NOT NULL,
  observed_ms INTEGER NOT NULL, observed TEXT NOT NULL,
  change TEXT NOT NULL, body TEXT NOT NULL, event_id INTEGER REFERENCES events(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS versions_obj ON versions(cluster, api_group, resource, namespace, name, id);
CREATE INDEX IF NOT EXISTS versions_unlinked ON versions(event_id, observed_ms);
CREATE INDEX IF NOT EXISTS versions_event ON versions(event_id);
CREATE INDEX IF NOT EXISTS versions_kind ON versions(cluster, kind, namespace, name, id);
CREATE TABLE IF NOT EXISTS relations (
  cluster TEXT NOT NULL, from_kind TEXT NOT NULL, from_ns TEXT NOT NULL, from_name TEXT NOT NULL,
  rel TEXT NOT NULL, to_kind TEXT NOT NULL, to_ns TEXT NOT NULL, to_name TEXT NOT NULL, selector TEXT
);
CREATE INDEX IF NOT EXISTS relations_from ON relations(cluster, from_kind, from_ns, from_name);
CREATE INDEX IF NOT EXISTS relations_to ON relations(cluster, to_kind, to_ns, to_name);
CREATE INDEX IF NOT EXISTS relations_sel ON relations(cluster, from_ns) WHERE selector IS NOT NULL;
CREATE TABLE IF NOT EXISTS hourly (
  cluster TEXT NOT NULL, hour_ms INTEGER NOT NULL, actor_key TEXT NOT NULL, human INTEGER NOT NULL,
  writes INTEGER NOT NULL, alerts INTEGER NOT NULL,
  PRIMARY KEY (cluster, hour_ms, actor_key)
);
CREATE TABLE IF NOT EXISTS stories (
  id INTEGER PRIMARY KEY, cluster TEXT NOT NULL, actor_key TEXT NOT NULL, via TEXT NOT NULL,
  severity TEXT NOT NULL, first_ms INTEGER NOT NULL, last_ms INTEGER NOT NULL, count INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS stories_actor ON stories(cluster, actor_key, via, last_ms);
CREATE INDEX IF NOT EXISTS stories_time ON stories(last_ms);
CREATE TABLE IF NOT EXISTS users (
  user_id TEXT NOT NULL, uid TEXT NOT NULL,
  display_name TEXT NOT NULL, username TEXT NOT NULL, principal_ids TEXT NOT NULL,
  local INTEGER NOT NULL, created_ms INTEGER NOT NULL, deleted_ms INTEGER,
  PRIMARY KEY (user_id, uid)
);
CREATE TABLE IF NOT EXISTS listing (
  cluster TEXT NOT NULL, api_group TEXT NOT NULL, resource TEXT NOT NULL, first INTEGER NOT NULL,
  PRIMARY KEY (cluster, api_group, resource)
);
"#;

/// Version kinds:
/// - baseline: first time this tool saw the object
/// - added / modified / deleting / deleted: seen live by the watch
/// - added_unseen / modified_unseen: found on a re-list after a gap (restart, lost watch)
/// Columns added after the first release, for databases created before them.
const ADDED_COLUMNS: &[(&str, &str)] = &[
    ("source", "TEXT NOT NULL DEFAULT 'k8s'"),
    ("user_id", "TEXT NOT NULL DEFAULT ''"),
    ("link_id", "INTEGER"),
    ("story_id", "INTEGER"),
];

/// A Rancher record that was matched to the downstream record of the same action is shown
/// through that downstream record; this hides the Rancher half so nothing is counted twice.
/// What groups records into one person: the Rancher user ID, else the name.
const ACTOR_KEY: &str = "CASE WHEN user_id<>'' THEN user_id ELSE actor END";

const SHOWN: &str = "NOT (e.source='rancher' AND e.link_id IS NOT NULL)";

/// Window for matching a Rancher record to the downstream record of the same action, as
/// (Rancher time - downstream time). Rancher stamps the response time to the whole second,
/// after the downstream call returned, so the difference is within -1 s .. +2 s; one more
/// second each way allows for clock differences between nodes.
const PAIR_MIN_MS: i64 = -2_000;
const PAIR_MAX_MS: i64 = 3_000;

pub const LINKABLE: &str = "('added','modified','deleting','deleted','added_unseen','modified_unseen')";

/// Identifies one Kubernetes object.
#[derive(Debug, Clone, Serialize)]
pub struct ObjKey {
    pub cluster: String,
    pub api_group: String,
    pub resource: String,
    pub namespace: String,
    pub name: String,
}

/// The Rancher half of an action, when it was matched to the downstream record.
#[derive(Debug, Serialize)]
pub struct RancherSide {
    pub ts: String,
    pub user_agent: String,
    pub source_ip: String,
    pub request_uri: String,
}

#[derive(Debug, Serialize)]
pub struct EventRow {
    pub id: i64,
    pub cluster: String,
    /// "k8s" or "rancher".
    pub source: String,
    pub user_id: String,
    /// For a downstream record: the matching Rancher record (UI or kubectl, client address).
    pub rancher: Option<RancherSide>,
    pub ts: String,
    pub actor: String,
    pub principal: String,
    pub human: bool,
    pub via: String,
    pub caller: String,
    pub verb: String,
    pub api_group: String,
    pub resource: String,
    pub subresource: String,
    pub namespace: String,
    pub name: String,
    pub code: i64,
    pub source_ip: String,
    pub user_agent: String,
    pub request_uri: String,
    pub request_object: Option<Value>,
    pub alerts: Vec<String>,
    /// Set when the change was matched to a stored object version.
    pub version_id: Option<i64>,
    pub changes: Option<Vec<Change>>,
    /// What the matched version did to the object: added, modified, deleting, deleted, ...
    pub change: Option<String>,
    /// The object as created or as last seen before deletion, when there is no diff to show.
    pub body: Option<Value>,
}

/// A version that no audit record could be matched to: a change made while collection was
/// down, or on a cluster without an audit source.
#[derive(Debug, Serialize)]
pub struct UnattributedRow {
    pub id: i64,
    pub cluster: String,
    pub kind: String,
    pub api_group: String,
    pub resource: String,
    pub namespace: String,
    pub name: String,
    pub change: String,
    pub observed: String,
    pub changes: Vec<Change>,
}

/// What each cluster has sent in a range, for the coverage label.
#[derive(Debug, Serialize)]
pub struct ClusterRow {
    pub cluster: String,
    pub k8s_events: i64,
    pub rancher_events: i64,
    pub versions: i64,
    pub last_event: Option<String>,
    pub last_version: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct VersionRow {
    pub id: i64,
    pub observed: String,
    pub change: String,
    pub kind: String,
    pub uid: String,
    pub actor: Option<String>,
    pub via: Option<String>,
    pub verb: Option<String>,
    pub event_id: Option<i64>,
    pub changes: Vec<Change>,
    pub body: Value,
}

#[derive(Debug, Serialize)]
pub struct ObjectRow {
    pub cluster: String,
    pub api_group: String,
    pub resource: String,
    pub kind: String,
    pub namespace: String,
    pub name: String,
    pub versions: i64,
    pub last_change: String,
}

/// One lifetime of a Rancher user ID (between creation and deletion of one User object).
#[derive(Debug, Serialize, Clone)]
pub struct UserLife {
    pub uid: String,
    pub display_name: String,
    pub principal_ids: Vec<String>,
    pub local: bool,
    pub created_ms: i64,
    pub deleted_ms: Option<i64>,
    /// Changes made in this lifetime within the requested range (filled by `actors`).
    pub writes: i64,
}

pub type AccessObj = crate::access::Obj;

/// Rancher's permission objects, all in management.cattle.io on the Rancher cluster.
const ACCESS_TYPES: [&str; 5] = ["globalroles", "globalrolebindings", "roletemplates", "clusterroletemplatebindings", "projectroletemplatebindings"];

#[derive(Debug, Serialize)]
pub struct RoleChange {
    pub kind: String,
    pub name: String,
    pub change: String,
    pub at_ms: i64,
    pub by: Option<String>,
    /// "rancher_sync": a built-in role Rancher changed itself (typically on upgrade).
    pub reason: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct HourRow {
    pub hour_ms: i64,
    pub actor_key: String,
    pub human: bool,
    pub writes: i64,
    pub alerts: i64,
}

#[derive(Debug, Serialize)]
pub struct StoryEvent {
    pub id: i64,
    pub ts: String,
    pub verb: String,
    pub resource: String,
    pub subresource: String,
    pub namespace: String,
    pub name: String,
    pub alerts: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct StoryRow {
    pub id: i64,
    pub cluster: String,
    pub actor_key: String,
    pub actor: String,
    pub via: String,
    pub severity: String,
    pub first_ms: i64,
    pub last_ms: i64,
    pub count: i64,
    pub events: Vec<StoryEvent>,
}

#[derive(Debug, Serialize)]
pub struct RelatedRow {
    /// "out": this object points at the other; "in": the other points at this one.
    pub direction: &'static str,
    pub rel: String,
    pub kind: String,
    pub namespace: String,
    pub name: String,
    /// Last change seen (not counting the first sighting), when the object is tracked.
    pub last_change: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ActorRow {
    /// The Rancher user ID when there is one, otherwise the name; what to filter by.
    pub key: String,
    /// Name to show: the directory's latest name, else the name in the newest record.
    pub actor: String,
    pub user_id: String,
    pub human: bool,
    pub writes: i64,
    pub alerts: i64,
    pub last_seen: String,
    /// The latest lifetime ended (the user is deleted now).
    pub deleted: bool,
    /// Known lifetimes; more than one means deleted and re-added, shown merged with a note.
    pub lifetimes: Vec<UserLife>,
}

pub struct Query {
    pub cluster: Option<String>,
    pub actor: Option<String>,
    pub from_ms: i64,
    pub to_ms: i64,
    pub humans_only: bool,
    pub writes_only: bool,
    pub alerts_only: bool,
    pub limit: i64,
}

const WORKLOAD_KINDS: &str = "('Deployment','StatefulSet','DaemonSet','Job','CronJob')";

const WRITE_VERBS: &str = "('create','update','patch','delete','deletecollection')";

/// Whether a record counts as a change on the time axis: the same rule as the default timeline.
fn counts(r: &Record, alerts: &[Alert]) -> bool {
    crate::audit::is_write(&r.verb)
        || matches!(r.subresource.as_str(), "exec" | "attach" | "portforward")
        || r.source == Source::Rancher
        || !alerts.is_empty()
}

fn ms(t: &DateTime<Utc>) -> i64 {
    t.timestamp_millis()
}

fn has_deletion(v: &Value) -> bool {
    v.pointer("/metadata/deletionTimestamp").is_some()
}

/// Changes worth showing for a version, given the previous stored body.
fn changes_for(change: &str, prev: Option<&Value>, body: &Value) -> Vec<Change> {
    match (change, prev) {
        ("baseline" | "added" | "added_unseen", _) | (_, None) => vec![],
        (_, Some(p)) => diff(p, body),
    }
}

impl Store {
    pub fn open(path: &str) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        conn.execute_batch(SCHEMA)?;
        let have: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('events')")?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for (col, ddl) in ADDED_COLUMNS {
            if !have.iter().any(|h| h == col) {
                conn.execute_batch(&format!("ALTER TABLE events ADD COLUMN {col} {ddl}"))?;
            }
        }
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS events_pair ON events(cluster, user_id, source, ts_ms);
             CREATE INDEX IF NOT EXISTS events_link ON events(link_id);",
        )?;
        Ok(Store { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Match a newly stored record to the other half of the same action: the Rancher record
    /// of a request Rancher proxied, and the downstream record of it (impersonated). Either
    /// can arrive first. Returns whether a match was made.
    fn pair(tx: &Transaction, id: i64, cluster: &str, r: &Record) -> anyhow::Result<Option<i64>> {
        if r.user_id.is_empty() || r.via != Via::Rancher {
            return Ok(None);
        }
        let t = ms(&r.ts);
        let (other, lo, hi) = match r.source {
            Source::K8s => ("rancher", t + PAIR_MIN_MS, t + PAIR_MAX_MS),
            Source::Rancher => ("k8s", t - PAIR_MAX_MS, t - PAIR_MIN_MS),
        };
        // Steve and Norman name some types differently from the Kubernetes API (singular,
        // case), so the resource only has to agree up to a plural "s".
        let hit: Option<i64> = tx
            .query_row(
                "SELECT id FROM events WHERE cluster=?1 AND source=?2 AND user_id=?3 AND verb=?4
                   AND namespace=?5 AND name=?6 AND subresource=?7 AND link_id IS NULL
                   AND (resource=?8 OR resource=?8 || 's' OR ?8=resource || 's')
                   AND ts_ms BETWEEN ?9 AND ?10
                 ORDER BY abs(ts_ms - ?11), id LIMIT 1",
                params![cluster, other, r.user_id, r.verb, r.namespace, r.name, r.subresource, r.resource, lo, hi, t],
                |row| row.get(0),
            )
            .optional()?;
        let Some(other_id) = hit else { return Ok(None) };
        tx.execute("UPDATE events SET link_id=?1 WHERE id=?2", params![other_id, id])?;
        tx.execute("UPDATE events SET link_id=?1 WHERE id=?2", params![id, other_id])?;
        // A version may already have been credited to the Rancher half; the downstream
        // record is the one shown, so it takes the version over.
        let (k8s, rancher) = if r.source == Source::K8s { (id, other_id) } else { (other_id, id) };
        tx.execute("UPDATE versions SET event_id=?1 WHERE event_id=?2", params![k8s, rancher])?;
        Ok(Some(other_id))
    }

    fn bump(tx: &Transaction, cluster: &str, t: i64, key: &str, human: bool, writes: i64, alerts: i64) -> anyhow::Result<()> {
        let hour = t - t.rem_euclid(3_600_000);
        tx.execute(
            "INSERT INTO hourly (cluster, hour_ms, actor_key, human, writes, alerts) VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(cluster, hour_ms, actor_key) DO UPDATE SET writes=writes+excluded.writes, alerts=alerts+excluded.alerts,
               human=max(human, excluded.human)",
            params![cluster, hour, key, human as i64, writes, alerts],
        )?;
        Ok(())
    }

    /// Take a Rancher record back out of the hourly counts and its story, once it turned out
    /// to be the other half of a downstream record.
    fn uncount(tx: &Transaction, id: i64) -> anyhow::Result<()> {
        let (cluster, t, key, human, story, alerts): (String, i64, String, i64, Option<i64>, i64) = tx.query_row(
            &format!("SELECT cluster, ts_ms, {ACTOR_KEY}, human, story_id, (SELECT count(*) FROM alerts WHERE event_id=events.id) FROM events WHERE id=?1"),
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )?;
        Self::bump(tx, &cluster, t, &key, human == 1, -1, -((alerts > 0) as i64))?;
        if let Some(sid) = story {
            tx.execute("UPDATE events SET story_id=NULL WHERE id=?1", params![id])?;
            tx.execute("UPDATE stories SET count=count-1 WHERE id=?1", params![sid])?;
            tx.execute("DELETE FROM stories WHERE id=?1 AND count<=0", params![sid])?;
        }
        Ok(())
    }

    /// Medium and high risk actions by the same person, the same way in, on the same cluster
    /// within 10 minutes of each other read as one story ("needs attention" list).
    fn add_to_story(tx: &Transaction, id: i64, cluster: &str, key: &str, via: &str, t: i64, alerts: &[Alert]) -> anyhow::Result<()> {
        const GAP: i64 = 10 * 60_000;
        let sev = if alerts.iter().any(|a| a.severity == "high") { "high" } else { "medium" };
        let hit: Option<i64> = tx
            .query_row(
                "SELECT id FROM stories WHERE cluster=?1 AND actor_key=?2 AND via=?3 AND last_ms >= ?4 AND first_ms <= ?5
                 ORDER BY abs(last_ms - ?6) LIMIT 1",
                params![cluster, key, via, t - GAP, t + GAP, t],
                |r| r.get(0),
            )
            .optional()?;
        let sid = match hit {
            Some(sid) => {
                tx.execute(
                    "UPDATE stories SET first_ms=min(first_ms, ?2), last_ms=max(last_ms, ?2), count=count+1,
                       severity=CASE WHEN severity='high' OR ?3='high' THEN 'high' ELSE 'medium' END WHERE id=?1",
                    params![sid, t, sev],
                )?;
                sid
            }
            None => {
                tx.execute(
                    "INSERT INTO stories (cluster, actor_key, via, severity, first_ms, last_ms, count) VALUES (?1,?2,?3,?4,?5,?5,1)",
                    params![cluster, key, via, sev, t],
                )?;
                tx.last_insert_rowid()
            }
        };
        tx.execute("UPDATE events SET story_id=?1 WHERE id=?2", params![sid, id])?;
        Ok(())
    }

    /// Hourly change counts per person, for the time axis.
    pub fn hourly(&self, cluster: Option<&str>, from_ms: i64, to_ms: i64) -> anyhow::Result<Vec<HourRow>> {
        let c = self.conn.lock().unwrap();
        let mut st = c.prepare(
            "SELECT hour_ms, actor_key, max(human), sum(writes), sum(alerts) FROM hourly
             WHERE hour_ms BETWEEN ?1 AND ?2 AND (?3 IS NULL OR cluster=?3) GROUP BY hour_ms, actor_key HAVING sum(writes) > 0 ORDER BY hour_ms",
        )?;
        let rows = st
            .query_map(params![from_ms - 3_600_000, to_ms, cluster], |r| {
                Ok(HourRow { hour_ms: r.get(0)?, actor_key: r.get(1)?, human: r.get::<_, i64>(2)? == 1, writes: r.get(3)?, alerts: r.get(4)? })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Stories in the range, most serious first, each with its events.
    pub fn stories(&self, cluster: Option<&str>, from_ms: i64, to_ms: i64) -> anyhow::Result<Vec<StoryRow>> {
        let c = self.conn.lock().unwrap();
        let mut st = c.prepare(
            "SELECT id, cluster, actor_key, via, severity, first_ms, last_ms, count FROM stories
             WHERE last_ms >= ?1 AND first_ms <= ?2 AND (?3 IS NULL OR cluster=?3)
             ORDER BY CASE severity WHEN 'high' THEN 0 ELSE 1 END, first_ms",
        )?;
        let mut rows: Vec<StoryRow> = st
            .query_map(params![from_ms, to_ms, cluster], |r| {
                Ok(StoryRow {
                    id: r.get(0)?, cluster: r.get(1)?, actor_key: r.get(2)?, via: r.get(3)?, severity: r.get(4)?,
                    first_ms: r.get(5)?, last_ms: r.get(6)?, count: r.get(7)?, actor: String::new(), events: vec![],
                })
            })?
            .collect::<Result<_, _>>()?;
        drop(st);
        let mut ev = c.prepare_cached(
            "SELECT id, ts, verb, resource, subresource, namespace, name, actor,
                    (SELECT group_concat(rule) FROM alerts a WHERE a.event_id=events.id)
             FROM events WHERE story_id=?1 ORDER BY ts_ms",
        )?;
        for s in rows.iter_mut() {
            s.events = ev
                .query_map(params![s.id], |r| {
                    let al: Option<String> = r.get(8)?;
                    Ok(StoryEvent {
                        id: r.get(0)?, ts: r.get(1)?, verb: r.get(2)?, resource: r.get(3)?, subresource: r.get(4)?,
                        namespace: r.get(5)?, name: r.get(6)?, alerts: al.map(|a| a.split(',').map(String::from).collect()).unwrap_or_default(),
                    })
                })?
                .collect::<Result<_, _>>()?;
            let snapshot: Option<String> = c.query_row("SELECT actor FROM events WHERE story_id=?1 ORDER BY ts_ms DESC LIMIT 1", params![s.id], |r| r.get(0)).optional()?;
            let directory: Option<String> = c
                .query_row("SELECT display_name FROM users WHERE user_id=?1 ORDER BY created_ms DESC LIMIT 1", params![s.actor_key], |r| r.get(0))
                .optional()?;
            s.actor = directory.or(snapshot).unwrap_or_else(|| s.actor_key.clone());
        }
        Ok(rows)
    }

    /// Insert a batch in one transaction. Returns how many were new (collectors resend a batch
    /// that was not acknowledged, and exec sessions arrive twice). `cluster` is the sender's
    /// cluster; a record that names its own target cluster (Rancher proxy) goes there.
    pub fn insert_events(&self, cluster: &str, batch: &[(Record, Vec<Alert>)]) -> anyhow::Result<usize> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        let mut stored = 0;
        {
            let mut ins = tx.prepare_cached(
                "INSERT OR IGNORE INTO events (cluster,audit_id,ts_ms,ts,actor,principal,human,via,caller,verb,api_group,resource,subresource,namespace,name,code,source_ip,user_agent,request_uri,request_object,source,user_id)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)",
            )?;
            let mut al = tx.prepare_cached("INSERT INTO alerts (event_id, rule, severity) VALUES (?1,?2,?3)")?;
            for (r, alerts) in batch {
                let cl = r.cluster.as_deref().unwrap_or(cluster);
                // Request bodies are kept only for people; controller status writes are noise.
                let body = if r.human { r.request_object.as_ref().map(|v| v.to_string()) } else { None };
                let n = ins.execute(params![
                    cl, r.audit_id, ms(&r.ts), r.ts.to_rfc3339(), r.actor, r.principal, r.human as i64,
                    r.via.as_str(), r.caller, r.verb, r.api_group, r.resource, r.subresource, r.namespace, r.name,
                    r.code, r.source_ip, r.user_agent, r.request_uri, body, r.source.as_str(), r.user_id
                ])?;
                if n == 0 {
                    continue;
                }
                stored += 1;
                let id = tx.last_insert_rowid();
                for a in alerts {
                    al.execute(params![id, a.rule, a.severity])?;
                }
                let paired = Self::pair(&tx, id, cl, r)?;
                let key = if r.user_id.is_empty() { r.actor.as_str() } else { r.user_id.as_str() };
                let t = ms(&r.ts);
                match (r.source, paired) {
                    // The downstream record is the one shown; this half was never counted.
                    (Source::Rancher, Some(_)) => continue,
                    // This record takes over from a Rancher half that was counted on its own.
                    (Source::K8s, Some(other)) => Self::uncount(&tx, other)?,
                    _ => {}
                }
                if counts(r, alerts) {
                    Self::bump(&tx, cl, t, key, r.human, 1, !alerts.is_empty() as i64)?;
                }
                if alerts.iter().any(|a| matches!(a.severity, "high" | "medium")) {
                    Self::add_to_story(&tx, id, cl, key, r.via.as_str(), t, alerts)?;
                }
            }
        }
        tx.commit()?;
        Ok(stored)
    }

    pub fn insert_event(&self, cluster: &str, r: &Record, alerts: &[Alert]) -> anyhow::Result<bool> {
        Ok(self.insert_events(cluster, &[(r.clone(), alerts.to_vec())])? == 1)
    }

    /// True if any version of this resource type was ever stored, i.e. a list is a re-list.
    pub fn has_any(&self, cluster: &str, group: &str, resource: &str) -> anyhow::Result<bool> {
        let c = self.conn.lock().unwrap();
        Ok(c.query_row(
            "SELECT 1 FROM versions WHERE cluster=?1 AND api_group=?2 AND resource=?3 LIMIT 1",
            params![cluster, group, resource],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
    }

    /// Objects related to one object, both ways. Selector relations (Service, NetworkPolicy)
    /// are matched against the current pod labels of workloads in the namespace.
    pub fn related(&self, cluster: &str, kind: &str, ns: &str, name: &str) -> anyhow::Result<Vec<RelatedRow>> {
        let c = self.conn.lock().unwrap();
        let mut out: Vec<RelatedRow> = Vec::new();
        let workloads = |c: &Connection| -> anyhow::Result<Vec<(String, String, Map<String, Value>)>> {
            let mut st = c.prepare_cached(&format!(
                "SELECT kind, name, body FROM versions WHERE id IN (
                   SELECT max(id) FROM versions WHERE cluster=?1 AND namespace=?2 AND kind IN {WORKLOAD_KINDS} GROUP BY kind, name)
                 AND change <> 'deleted'"
            ))?;
            let rows: Vec<(String, String, String)> = st.query_map(params![cluster, ns], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
            Ok(rows
                .into_iter()
                .filter_map(|(k, n, b)| {
                    let body: Value = serde_json::from_str(&b).ok()?;
                    let labels = pod_template(&k, &body)?.pointer("/metadata/labels")?.as_object()?.clone();
                    Some((k, n, labels))
                })
                .collect())
        };
        // Outgoing.
        let mut st = c.prepare_cached("SELECT rel, to_kind, to_ns, to_name, selector FROM relations WHERE cluster=?1 AND from_kind=?2 AND from_ns=?3 AND from_name=?4")?;
        let rows: Vec<(String, String, String, String, Option<String>)> =
            st.query_map(params![cluster, kind, ns, name], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<Result<_, _>>()?;
        drop(st);
        for (rel, tk, tns, tn, sel) in rows {
            match sel.and_then(|s| serde_json::from_str::<Value>(&s).ok()) {
                Some(sel) => {
                    for (wk, wn, labels) in workloads(&c)? {
                        if selector_matches(&sel, &labels) {
                            out.push(RelatedRow { direction: "out", rel: rel.clone(), kind: wk, namespace: ns.into(), name: wn, last_change: None });
                        }
                    }
                }
                None => out.push(RelatedRow { direction: "out", rel, kind: tk, namespace: tns, name: tn, last_change: None }),
            }
        }
        // Incoming by name.
        let mut st = c.prepare_cached("SELECT rel, from_kind, from_ns, from_name FROM relations WHERE cluster=?1 AND to_kind=?2 AND to_ns=?3 AND to_name=?4")?;
        let rows: Vec<(String, String, String, String)> =
            st.query_map(params![cluster, kind, ns, name], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<_, _>>()?;
        drop(st);
        out.extend(rows.into_iter().map(|(rel, k, n, nm)| RelatedRow { direction: "in", rel, kind: k, namespace: n, name: nm, last_change: None }));
        // Incoming by selector, when this object is a workload.
        let mine = workloads(&c)?.into_iter().find(|(k, n, _)| k == kind && n == name).map(|(_, _, l)| l);
        if let Some(labels) = mine {
            let mut st = c.prepare_cached("SELECT rel, from_kind, from_name, selector FROM relations WHERE cluster=?1 AND from_ns=?2 AND selector IS NOT NULL")?;
            let rows: Vec<(String, String, String, String)> =
                st.query_map(params![cluster, ns], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<_, _>>()?;
            drop(st);
            for (rel, k, n, sel) in rows {
                if serde_json::from_str::<Value>(&sel).is_ok_and(|s| selector_matches(&s, &labels)) {
                    out.push(RelatedRow { direction: "in", rel, kind: k, namespace: ns.into(), name: n, last_change: None });
                }
            }
        }
        // When each related object last changed, if it is tracked at all.
        let mut st = c.prepare_cached(
            "SELECT observed FROM versions WHERE cluster=?1 AND kind=?2 AND namespace=?3 AND name=?4 AND change <> 'baseline' ORDER BY id DESC LIMIT 1",
        )?;
        for r in out.iter_mut() {
            r.last_change = st.query_row(params![cluster, r.kind, r.namespace, r.name], |row| row.get(0)).optional()?;
        }
        Ok(out)
    }

    /// Rancher permission objects as they were at `at_ms`, with who created each one.
    pub fn access_objects(&self, cluster: &str, at_ms: i64) -> anyhow::Result<Vec<AccessObj>> {
        let c = self.conn.lock().unwrap();
        let mut out = Vec::new();
        for resource in ACCESS_TYPES {
            let mut st = c.prepare_cached(
                "SELECT namespace, name, uid, body FROM versions WHERE id IN (
                   SELECT max(id) FROM versions WHERE cluster=?1 AND api_group='management.cattle.io' AND resource=?2 AND observed_ms <= ?3
                   GROUP BY namespace, name)
                 AND change <> 'deleted'",
            )?;
            let rows: Vec<(String, String, String, String)> =
                st.query_map(params![cluster, resource, at_ms], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<_, _>>()?;
            drop(st);
            for (namespace, name, uid, body) in rows {
                let (first_seen_ms, before_tracking, created_by, inferred) = Self::creation(&c, cluster, resource, &namespace, &name, &uid)?;
                out.push(AccessObj {
                    resource: resource.to_string(), namespace, name, body: serde_json::from_str(&body).unwrap_or(Value::Null),
                    first_seen_ms, before_tracking, created_by, created_by_inferred: inferred,
                });
            }
        }
        Ok(out)
    }

    /// When an object (this UID) was first seen and who created it. Exact when the first
    /// version was matched to an audit record; otherwise the nearest create of that type by
    /// a person shortly before (Norman creates carry no namespace or name to match on).
    fn creation(c: &Connection, cluster: &str, resource: &str, ns: &str, name: &str, uid: &str) -> anyhow::Result<(i64, bool, Option<String>, bool)> {
        let (vid, obs, change, eid): (i64, i64, String, Option<i64>) = c.query_row(
            "SELECT id, observed_ms, change, event_id FROM versions WHERE cluster=?1 AND api_group='management.cattle.io' AND resource=?2
               AND namespace=?3 AND name=?4 AND uid=?5 ORDER BY id LIMIT 1",
            params![cluster, resource, ns, name, uid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        let _ = vid;
        if change == "baseline" {
            return Ok((obs, true, None, false));
        }
        if let Some(eid) = eid {
            let who: Option<String> = c.query_row("SELECT actor FROM events WHERE id=?1", params![eid], |r| r.get(0)).optional()?;
            return Ok((obs, false, who, false));
        }
        let who: Option<String> = c
            .query_row(
                "SELECT actor FROM events WHERE cluster=?1 AND resource=?2 AND verb='create' AND human=1 AND code BETWEEN 200 AND 299
                   AND (name=?3 OR name='') AND ts_ms BETWEEN ?4 AND ?5 ORDER BY abs(ts_ms - ?6) LIMIT 1",
                params![cluster, resource, name, obs - 20_000, obs + 5_000, obs],
                |r| r.get(0),
            )
            .optional()?;
        let inferred = who.is_some();
        Ok((obs, false, who, inferred))
    }

    /// Users as they were at `at_ms`: (user ID, name, principal IDs, groups, when the group
    /// list was last seen to change).
    pub fn access_users(&self, cluster: &str, at_ms: i64) -> anyhow::Result<Vec<(String, String, Vec<String>, Vec<String>, Option<i64>)>> {
        let c = self.conn.lock().unwrap();
        let latest = |resource: &str| -> anyhow::Result<Vec<(String, String, i64)>> {
            let mut st = c.prepare_cached(
                "SELECT name, body, observed_ms FROM versions WHERE id IN (
                   SELECT max(id) FROM versions WHERE cluster=?1 AND api_group='management.cattle.io' AND resource=?2 AND observed_ms <= ?3 GROUP BY name)
                 AND change <> 'deleted'",
            )?;
            let rows = st.query_map(params![cluster, resource, at_ms], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
            Ok(rows)
        };
        let attrs: HashMap<String, (Value, i64)> = latest("userattributes")?
            .into_iter()
            .map(|(n, b, t)| (n, (serde_json::from_str(&b).unwrap_or(Value::Null), t)))
            .collect();
        let mut out = Vec::new();
        for (name, body, _) in latest("users")? {
            let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let display = [v.get("displayName"), v.get("username")]
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .find(|s| !s.is_empty())
                .unwrap_or(&name)
                .to_string();
            let principals: Vec<String> = v.get("principalIds").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            let (groups, seen) = attrs.get(&name).map(|(ua, t)| (crate::access::groups_of(ua), Some(*t))).unwrap_or_default();
            out.push((name, display, principals, groups, seen));
        }
        Ok(out)
    }

    /// Who deleted a binding between two times, and whether Rancher did it on its own right
    /// after deleting the user it named.
    pub fn access_removal(&self, cluster: &str, resource: &str, ns: &str, name: &str, subject: &str, from_ms: i64, to_ms: i64) -> anyhow::Result<(Option<i64>, Option<String>, Option<&'static str>)> {
        let c = self.conn.lock().unwrap();
        let row: Option<(i64, Option<i64>)> = c
            .query_row(
                "SELECT observed_ms, event_id FROM versions WHERE cluster=?1 AND api_group='management.cattle.io' AND resource=?2 AND namespace=?3 AND name=?4
                   AND change IN ('deleting','deleted') AND observed_ms > ?5 AND observed_ms <= ?6 ORDER BY id LIMIT 1",
                params![cluster, resource, ns, name, from_ms, to_ms],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((at, eid)) = row else { return Ok((None, None, None)) };
        let who: Option<(String, i64)> = match eid {
            Some(e) => c.query_row("SELECT actor, human FROM events WHERE id=?1", params![e], |r| Ok((r.get(0)?, r.get(1)?))).optional()?,
            None => None,
        };
        if let Some((actor, 1)) = &who {
            return Ok((Some(at), Some(actor.clone()), None));
        }
        let user_gone: Option<i64> = c
            .query_row("SELECT max(deleted_ms) FROM users WHERE user_id=?1 AND deleted_ms BETWEEN ?2 AND ?3", params![subject, at - 600_000, at + 5_000], |r| r.get(0))
            .optional()?
            .flatten();
        Ok((Some(at), who.map(|w| w.0), user_gone.map(|_| "rancher_cleanup")))
    }

    /// Role definitions (RoleTemplate, GlobalRole) changed between two times, with who changed them.
    pub fn role_changes(&self, cluster: &str, from_ms: i64, to_ms: i64) -> anyhow::Result<Vec<RoleChange>> {
        let c = self.conn.lock().unwrap();
        let mut st = c.prepare(
            "SELECT v.resource, v.name, v.change, v.observed_ms, v.body, e.actor, e.human FROM versions v LEFT JOIN events e ON v.event_id=e.id
             WHERE v.cluster=?1 AND v.api_group='management.cattle.io' AND v.resource IN ('roletemplates','globalroles')
               AND v.change NOT IN ('baseline') AND v.observed_ms > ?2 AND v.observed_ms <= ?3 ORDER BY v.observed_ms",
        )?;
        let rows = st
            .query_map(params![cluster, from_ms, to_ms], |r| {
                let body: String = r.get(4)?;
                let builtin = serde_json::from_str::<Value>(&body).ok().and_then(|b| b.get("builtin").and_then(Value::as_bool)).unwrap_or(false);
                let actor: Option<String> = r.get(5)?;
                let human: Option<i64> = r.get(6)?;
                Ok(RoleChange {
                    kind: r.get(0)?, name: r.get(1)?, change: r.get(2)?, at_ms: r.get(3)?,
                    by: actor.filter(|_| human == Some(1)),
                    reason: if builtin && human != Some(1) { Some("rancher_sync") } else { None },
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Keep the user directory current. One row per lifetime of a user ID: an external
    /// user deleted and later re-added gets the same `u-xxxxx` (Rancher derives it from the
    /// principal) but a new object UID. Rows are never removed, so names stay known after
    /// the user is gone.
    fn record_user(c: &Connection, user_id: &str, uid: &str, obj: &Value, change: &str, at: DateTime<Utc>) -> anyhow::Result<()> {
        if change == "deleted" {
            c.execute("UPDATE users SET deleted_ms=?3 WHERE user_id=?1 AND uid=?2 AND deleted_ms IS NULL", params![user_id, uid, ms(&at)])?;
            return Ok(());
        }
        let s = |p: &str| obj.pointer(p).and_then(Value::as_str).unwrap_or("").to_string();
        let username = s("/username");
        let display = [s("/displayName"), username.clone(), user_id.to_string()].into_iter().find(|v| !v.is_empty()).unwrap_or_default();
        let principals: Vec<String> = obj
            .get("principalIds")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let local = principals.iter().any(|p| p.starts_with("local://"));
        let created = obj
            .pointer("/metadata/creationTimestamp")
            .and_then(Value::as_str)
            .and_then(|t| t.parse::<DateTime<Utc>>().ok())
            .unwrap_or(at);
        c.execute(
            "INSERT INTO users (user_id, uid, display_name, username, principal_ids, local, created_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(user_id, uid) DO UPDATE SET display_name=excluded.display_name, username=excluded.username,
               principal_ids=excluded.principal_ids, local=excluded.local",
            params![user_id, uid, display, username, serde_json::to_string(&principals)?, local as i64, ms(&created)],
        )?;
        Ok(())
    }

    /// Lifetimes of the given user IDs, oldest first.
    fn lifetimes(c: &Connection, user_id: &str) -> anyhow::Result<Vec<UserLife>> {
        let mut stmt = c.prepare_cached(
            "SELECT uid, display_name, principal_ids, local, created_ms, deleted_ms FROM users WHERE user_id=?1 ORDER BY created_ms",
        )?;
        let rows = stmt
            .query_map(params![user_id], |r| {
                let p: String = r.get(2)?;
                Ok(UserLife {
                    uid: r.get(0)?,
                    display_name: r.get(1)?,
                    principal_ids: serde_json::from_str(&p).unwrap_or_default(),
                    local: r.get::<_, i64>(3)? == 1,
                    created_ms: r.get(4)?,
                    deleted_ms: r.get(5)?,
                    writes: 0,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn users(&self) -> anyhow::Result<Vec<(String, Vec<UserLife>)>> {
        let c = self.conn.lock().unwrap();
        let ids: Vec<String> = c.prepare("SELECT DISTINCT user_id FROM users ORDER BY user_id")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        ids.into_iter().map(|id| Ok((id.clone(), Self::lifetimes(&c, &id)?))).collect()
    }

    /// Apply a batch from `collect-watch`. Returns how many versions were stored.
    pub fn apply_objects(&self, cluster: &str, msgs: &[ObjMsg]) -> anyhow::Result<usize> {
        let mut stored = 0;
        for m in msgs {
            match m {
                ObjMsg::ListStart { group, resource } => {
                    // The very first list of a type is the baseline; later lists are re-lists
                    // after a gap, so anything new or different was changed while unseen.
                    let first = !self.has_any(cluster, group, resource)?;
                    self.conn.lock().unwrap().execute(
                        "INSERT OR REPLACE INTO listing (cluster, api_group, resource, first) VALUES (?1,?2,?3,?4)",
                        params![cluster, group, resource, first as i64],
                    )?;
                }
                ObjMsg::Version { group, resource, kind, namespace, name, uid, seen, at, object } => {
                    let seen = match seen.as_str() {
                        "list" => {
                            let first: Option<i64> = self.conn.lock().unwrap().query_row(
                                "SELECT first FROM listing WHERE cluster=?1 AND api_group=?2 AND resource=?3",
                                params![cluster, group, resource],
                                |r| r.get(0),
                            ).optional()?;
                            if first == Some(1) { "baseline" } else { "resync" }
                        }
                        "deleted" => "deleted",
                        _ => "modified",
                    };
                    let key = ObjKey { cluster: cluster.into(), api_group: group.clone(), resource: resource.clone(), namespace: namespace.clone(), name: name.clone() };
                    if self.insert_version(&key, kind, uid, object, seen, *at)?.is_some() {
                        stored += 1;
                    }
                }
                ObjMsg::ListDone { group, resource, listed } => {
                    let listed: HashSet<(String, String)> = listed.iter().cloned().collect();
                    for (namespace, name, uid, kind, body) in self.alive(cluster, group, resource)? {
                        if !listed.contains(&(namespace.clone(), name.clone())) {
                            let key = ObjKey { cluster: cluster.into(), api_group: group.clone(), resource: resource.clone(), namespace, name };
                            if self.insert_version(&key, &kind, &uid, &body, "deleted", Utc::now())?.is_some() {
                                stored += 1;
                            }
                        }
                    }
                    self.conn.lock().unwrap().execute(
                        "DELETE FROM listing WHERE cluster=?1 AND api_group=?2 AND resource=?3",
                        params![cluster, group, resource],
                    )?;
                }
            }
        }
        Ok(stored)
    }

    /// Objects whose latest stored version is not "deleted", as (namespace, name, uid, kind, body).
    pub fn alive(&self, cluster: &str, group: &str, resource: &str) -> anyhow::Result<Vec<(String, String, String, String, Value)>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT v.namespace, v.name, v.uid, v.kind, v.body FROM versions v
             WHERE v.id IN (SELECT max(id) FROM versions WHERE cluster=?1 AND api_group=?2 AND resource=?3 GROUP BY namespace, name)
               AND v.change <> 'deleted'",
        )?;
        let rows = stmt
            .query_map(params![cluster, group, resource], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get::<_, String>(4)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().map(|(a, b, c, d, e)| (a, b, c, d, serde_json::from_str(&e).unwrap_or(Value::Null))).collect())
    }

    /// Store a new version if the normalised content differs from the latest one.
    /// `seen` is "baseline" (first ever list), "resync" (re-list), "modified" or "deleted".
    pub fn insert_version(&self, key: &ObjKey, kind: &str, uid: &str, obj: &Value, seen: &str, at: DateTime<Utc>) -> anyhow::Result<Option<i64>> {
        let body = normalize(obj);
        let c = self.conn.lock().unwrap();
        let last: Option<(String, String, String)> = c
            .query_row(
                "SELECT body, uid, change FROM versions WHERE cluster=?1 AND api_group=?2 AND resource=?3 AND namespace=?4 AND name=?5 ORDER BY id DESC LIMIT 1",
                params![key.cluster, key.api_group, key.resource, key.namespace, key.name],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let change = match &last {
            Some((prev, prev_uid, prev_change)) if prev_uid == uid && prev_change != "deleted" => {
                let prev: Value = serde_json::from_str(prev).unwrap_or(Value::Null);
                if seen == "deleted" {
                    "deleted"
                } else if prev == body {
                    return Ok(None);
                } else if !has_deletion(&prev) && has_deletion(&body) {
                    "deleting"
                } else if seen == "baseline" || seen == "resync" {
                    "modified_unseen"
                } else {
                    "modified"
                }
            }
            _ => match seen {
                "deleted" => "deleted",
                "baseline" => "baseline",
                "resync" => "added_unseen",
                _ => "added",
            },
        };
        c.execute(
            "INSERT INTO versions (cluster,api_group,resource,kind,namespace,name,uid,observed_ms,observed,change,body) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![key.cluster, key.api_group, key.resource, kind, key.namespace, key.name, uid, ms(&at), at.to_rfc3339(), change, body.to_string()],
        )?;
        let id = c.last_insert_rowid();
        // Relations follow the latest version; a deleted object points at nothing.
        c.execute(
            "DELETE FROM relations WHERE cluster=?1 AND from_kind=?2 AND from_ns=?3 AND from_name=?4",
            params![key.cluster, kind, key.namespace, key.name],
        )?;
        if change != "deleted" {
            let mut ins = c.prepare_cached(
                "INSERT INTO relations (cluster, from_kind, from_ns, from_name, rel, to_kind, to_ns, to_name, selector) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            )?;
            for r in extract(kind, &key.namespace, obj) {
                ins.execute(params![key.cluster, kind, key.namespace, key.name, r.rel, r.to_kind, r.to_namespace, r.to_name, r.selector.map(|s| s.to_string())])?;
            }
        }
        if key.api_group == "management.cattle.io" && key.resource == "users" {
            Self::record_user(&c, &key.name, uid, obj, change, at)?;
        }
        Ok(Some(id))
    }

    /// Match recent unlinked versions to the audit event that caused them. Audit
    /// webhooks are batched, so this runs periodically rather than on insert.
    pub fn link_pending(&self, now: DateTime<Utc>) -> anyhow::Result<usize> {
        self.link_since(ms(&now) - 45 * 60 * 1000)
    }

    pub fn link_since(&self, since_ms: i64) -> anyhow::Result<usize> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(&format!(
            "SELECT id, cluster, api_group, resource, namespace, name, observed_ms, change FROM versions
             WHERE event_id IS NULL AND change IN {LINKABLE} AND observed_ms > ?1 ORDER BY observed_ms, id"
        ))?;
        let pending: Vec<(i64, String, String, String, String, String, i64, String)> = stmt
            .query_map(params![since_ms], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?))
            })?
            .collect::<Result<_, _>>()?;
        drop(stmt);
        let mut linked = 0;
        for (vid, cluster, group, resource, ns, name, obs, change) in pending {
            let (verbs, before_ms, order) = match change.as_str() {
                "added" => ("('create','update','patch')", 20_000, "ASC"),
                "added_unseen" => ("('create','update','patch')", 30 * 60_000, "DESC"),
                "modified" => ("('update','patch')", 20_000, "ASC"),
                "modified_unseen" => ("('update','patch')", 30 * 60_000, "DESC"),
                // Objects with finalizers: deletionTimestamp appears first, the object goes later.
                "deleting" => ("('delete','deletecollection')", 20_000, "ASC"),
                _ => ("('delete','deletecollection')", 10 * 60_000, "DESC"),
            };
            // Live changes take the earliest unlinked event in the window so two quick edits
            // keep their order; gap changes take the latest one before the version was seen.
            let sql = format!(
                "SELECT e.id FROM events e WHERE e.cluster=?1 AND e.api_group=?2 AND e.resource=?3 AND e.namespace=?4
                 AND (e.name=?5 OR (e.name='' AND e.verb IN ('create','deletecollection')))
                 AND e.verb IN {verbs} AND e.subresource IN ('','scale') AND e.code BETWEEN 200 AND 299
                 AND e.ts_ms BETWEEN ?6 AND ?7 AND {SHOWN}
                 AND (e.verb = 'deletecollection' OR NOT EXISTS (SELECT 1 FROM versions v WHERE v.event_id = e.id))
                 ORDER BY e.ts_ms {order}, e.id {order} LIMIT 1"
            );
            let hit: Option<i64> = c
                .query_row(&sql, params![cluster, group, resource, ns, name, obs - before_ms, obs + 5_000], |r| r.get(0))
                .optional()?;
            if let Some(eid) = hit {
                c.execute("UPDATE versions SET event_id=?1 WHERE id=?2", params![eid, vid])?;
                linked += 1;
            }
        }
        Ok(linked)
    }

    fn version_changes(c: &Connection, vid: i64) -> anyhow::Result<Option<Vec<Change>>> {
        Ok(Self::version_detail(c, vid)?.map(|(ch, _, _)| ch))
    }

    /// Diff against the previous version, the change kind, and the body for created or
    /// deleted objects (which have no diff).
    fn version_detail(c: &Connection, vid: i64) -> anyhow::Result<Option<(Vec<Change>, String, Option<Value>)>> {
        let row: Option<(String, String, String, String, String, String, String)> = c
            .query_row(
                "SELECT cluster, api_group, resource, namespace, name, body, change FROM versions WHERE id=?1",
                params![vid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
            )
            .optional()?;
        let Some((cl, g, res, ns, name, body, change)) = row else { return Ok(None) };
        let prev: Option<String> = c
            .query_row(
                "SELECT body FROM versions WHERE cluster=?1 AND api_group=?2 AND resource=?3 AND namespace=?4 AND name=?5 AND id < ?6 ORDER BY id DESC LIMIT 1",
                params![cl, g, res, ns, name, vid],
                |r| r.get(0),
            )
            .optional()?;
        let new: Value = serde_json::from_str(&body)?;
        let old: Option<Value> = prev.map(|p| serde_json::from_str(&p)).transpose()?;
        let changes = changes_for(&change, old.as_ref(), &new);
        let body = matches!(change.as_str(), "added" | "added_unseen" | "deleted").then_some(new);
        Ok(Some((changes, change, body)))
    }

    pub fn unattributed(&self, from_ms: i64, to_ms: i64) -> anyhow::Result<Vec<UnattributedRow>> {
        let c = self.conn.lock().unwrap();
        let mut st = c.prepare(
            "SELECT id, cluster, kind, api_group, resource, namespace, name, change, observed FROM versions
             WHERE event_id IS NULL AND change <> 'baseline' AND observed_ms BETWEEN ?1 AND ?2 ORDER BY observed_ms LIMIT 2000",
        )?;
        let mut rows: Vec<UnattributedRow> = st
            .query_map(params![from_ms, to_ms], |r| {
                Ok(UnattributedRow {
                    id: r.get(0)?, cluster: r.get(1)?, kind: r.get(2)?, api_group: r.get(3)?, resource: r.get(4)?,
                    namespace: r.get(5)?, name: r.get(6)?, change: r.get(7)?, observed: r.get(8)?, changes: vec![],
                })
            })?
            .collect::<Result<_, _>>()?;
        drop(st);
        for row in rows.iter_mut() {
            row.changes = Self::version_changes(&c, row.id)?.unwrap_or_default();
        }
        Ok(rows)
    }

    pub fn clusters(&self, from_ms: i64, to_ms: i64) -> anyhow::Result<Vec<ClusterRow>> {
        let c = self.conn.lock().unwrap();
        let mut st = c.prepare(
            "SELECT k.cluster,
               (SELECT count(*) FROM events WHERE cluster=k.cluster AND source='k8s' AND ts_ms BETWEEN ?1 AND ?2),
               (SELECT count(*) FROM events WHERE cluster=k.cluster AND source='rancher' AND ts_ms BETWEEN ?1 AND ?2),
               (SELECT count(*) FROM versions WHERE cluster=k.cluster AND observed_ms BETWEEN ?1 AND ?2),
               (SELECT max(ts) FROM events WHERE cluster=k.cluster),
               (SELECT max(observed) FROM versions WHERE cluster=k.cluster)
             FROM (SELECT DISTINCT cluster FROM events UNION SELECT DISTINCT cluster FROM versions) k ORDER BY k.cluster",
        )?;
        let rows = st
            .query_map(params![from_ms, to_ms], |r| {
                Ok(ClusterRow { cluster: r.get(0)?, k8s_events: r.get(1)?, rancher_events: r.get(2)?, versions: r.get(3)?, last_event: r.get(4)?, last_version: r.get(5)? })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Namespace -> Rancher project ID, from the `field.cattle.io/projectId` label of each
    /// namespace's latest version.
    pub fn namespace_projects(&self, cluster: Option<&str>) -> anyhow::Result<Vec<(String, String, String)>> {
        let c = self.conn.lock().unwrap();
        let mut st = c.prepare(
            "SELECT cluster, name, body FROM versions WHERE id IN (
               SELECT max(id) FROM versions WHERE api_group='' AND resource='namespaces' AND (?1 IS NULL OR cluster=?1) GROUP BY cluster, name)
             AND change <> 'deleted'",
        )?;
        let rows: Vec<(String, String, String)> = st.query_map(params![cluster], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
        Ok(rows
            .into_iter()
            .filter_map(|(cl, ns, body)| {
                let v: Value = serde_json::from_str(&body).ok()?;
                let p = v.pointer("/metadata/labels/field.cattle.io~1projectId")?.as_str()?.to_string();
                Some((cl, ns, p))
            })
            .collect())
    }

    pub fn timeline(&self, q: &Query) -> anyhow::Result<Vec<EventRow>> {
        let c = self.conn.lock().unwrap();
        let mut sql = String::from(
            "SELECT e.id,e.cluster,e.ts,e.actor,e.principal,e.human,e.via,e.caller,e.verb,e.api_group,e.resource,e.subresource,
                    e.namespace,e.name,e.code,e.source_ip,e.user_agent,e.request_uri,e.request_object,
                    (SELECT group_concat(rule) FROM alerts a WHERE a.event_id=e.id),
                    (SELECT v.id FROM versions v WHERE v.event_id=e.id ORDER BY v.id DESC LIMIT 1),
                    e.source,e.user_id,r.ts,r.user_agent,r.source_ip,r.request_uri
             FROM events e LEFT JOIN events r ON r.id=e.link_id AND e.source='k8s'
             WHERE e.ts_ms BETWEEN ?1 AND ?2",
        );
        sql += &format!(" AND {SHOWN}");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(q.from_ms), Box::new(q.to_ms)];
        if let Some(cl) = &q.cluster {
            args.push(Box::new(cl.clone()));
            sql += &format!(" AND e.cluster=?{}", args.len());
        }
        if let Some(a) = &q.actor {
            // A user ID (all its lifetimes) or, for callers without one, the name.
            args.push(Box::new(a.clone()));
            sql += &format!(" AND (e.user_id=?{n} OR (e.user_id='' AND e.actor=?{n}))", n = args.len());
        }
        if q.humans_only {
            sql += " AND e.human=1";
        }
        if q.alerts_only {
            sql += " AND EXISTS (SELECT 1 FROM alerts a WHERE a.event_id=e.id)";
        } else if q.writes_only {
            // Rancher records are already reduced to changes, actions and logins at parse time.
            sql += &format!(" AND (e.verb IN {WRITE_VERBS} OR e.subresource IN ('exec','attach','portforward') OR e.source='rancher' OR EXISTS (SELECT 1 FROM alerts a WHERE a.event_id=e.id))");
        }
        args.push(Box::new(q.limit));
        sql += &format!(" ORDER BY e.ts_ms DESC, e.id DESC LIMIT ?{}", args.len());
        let mut stmt = c.prepare(&sql)?;
        let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let mut rows: Vec<EventRow> = stmt
            .query_map(refs.as_slice(), |r| {
                let ro: Option<String> = r.get(18)?;
                let al: Option<String> = r.get(19)?;
                let rts: Option<String> = r.get(23)?;
                let rancher = match rts {
                    Some(ts) => Some(RancherSide { ts, user_agent: r.get(24)?, source_ip: r.get(25)?, request_uri: r.get(26)? }),
                    None => None,
                };
                Ok(EventRow {
                    id: r.get(0)?, cluster: r.get(1)?, source: r.get(21)?, user_id: r.get(22)?, rancher,
                    ts: r.get(2)?, actor: r.get(3)?, principal: r.get(4)?,
                    human: r.get::<_, i64>(5)? == 1, via: r.get(6)?, caller: r.get(7)?, verb: r.get(8)?,
                    api_group: r.get(9)?, resource: r.get(10)?, subresource: r.get(11)?, namespace: r.get(12)?,
                    name: r.get(13)?, code: r.get(14)?, source_ip: r.get(15)?, user_agent: r.get(16)?, request_uri: r.get(17)?,
                    request_object: ro.and_then(|s| serde_json::from_str(&s).ok()),
                    alerts: al.map(|s| s.split(',').map(String::from).collect()).unwrap_or_default(),
                    version_id: r.get(20)?,
                    changes: None,
                    change: None,
                    body: None,
                })
            })?
            .collect::<Result<_, _>>()?;
        drop(stmt);
        for row in rows.iter_mut() {
            if let Some(vid) = row.version_id {
                if let Some((changes, change, body)) = Self::version_detail(&c, vid)? {
                    row.changes = Some(changes);
                    row.change = Some(change);
                    row.body = body;
                }
            }
        }
        Ok(rows)
    }

    /// People and components active in the range, one row per Rancher user ID (all its
    /// lifetimes merged) or per name when there is no user ID.
    pub fn actors(&self, from_ms: i64, to_ms: i64) -> anyhow::Result<Vec<ActorRow>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(&format!(
            "SELECT {ACTOR_KEY} AS k, max(user_id), max(human),
                    sum(CASE WHEN verb IN {WRITE_VERBS} OR source='rancher' THEN 1 ELSE 0 END),
                    (SELECT count(*) FROM alerts a JOIN events e2 ON a.event_id=e2.id
                      WHERE (CASE WHEN e2.user_id<>'' THEN e2.user_id ELSE e2.actor END)=(CASE WHEN e.user_id<>'' THEN e.user_id ELSE e.actor END) AND e2.ts_ms BETWEEN ?1 AND ?2
                        AND NOT (e2.source='rancher' AND e2.link_id IS NOT NULL)),
                    max(ts),
                    (SELECT e3.actor FROM events e3 WHERE (CASE WHEN e3.user_id<>'' THEN e3.user_id ELSE e3.actor END)=(CASE WHEN e.user_id<>'' THEN e.user_id ELSE e.actor END) ORDER BY e3.ts_ms DESC LIMIT 1)
             FROM events e WHERE ts_ms BETWEEN ?1 AND ?2 AND {SHOWN} GROUP BY k ORDER BY max(human) DESC, 4 DESC"
        ))?;
        let mut rows: Vec<ActorRow> = stmt
            .query_map(params![from_ms, to_ms], |r| {
                Ok(ActorRow {
                    key: r.get(0)?, user_id: r.get(1)?, human: r.get::<_, i64>(2)? == 1, writes: r.get(3)?, alerts: r.get(4)?,
                    last_seen: r.get(5)?, actor: r.get(6)?, deleted: false, lifetimes: vec![],
                })
            })?
            .collect::<Result<_, _>>()?;
        drop(stmt);
        for row in rows.iter_mut().filter(|r| !r.user_id.is_empty()) {
            let mut lives = Self::lifetimes(&c, &row.user_id)?;
            for l in lives.iter_mut() {
                l.writes = c.query_row(
                    &format!("SELECT count(*) FROM events e WHERE user_id=?1 AND ts_ms BETWEEN max(?2, ?3) AND min(?4, ?5) AND {SHOWN}
                              AND (verb IN {WRITE_VERBS} OR source='rancher')"),
                    params![row.user_id, from_ms, l.created_ms, to_ms, l.deleted_ms.unwrap_or(i64::MAX)],
                    |r| r.get(0),
                )?;
            }
            if let Some(last) = lives.last() {
                row.actor = last.display_name.clone();
                row.deleted = last.deleted_ms.is_some();
            }
            row.lifetimes = lives;
        }
        Ok(rows)
    }

    pub fn objects(&self, cluster: Option<&str>, search: Option<&str>, limit: i64) -> anyhow::Result<Vec<ObjectRow>> {
        let c = self.conn.lock().unwrap();
        let pat = format!("%{}%", search.unwrap_or(""));
        let mut stmt = c.prepare(
            "SELECT cluster, api_group, resource, max(kind), namespace, name, count(*), max(observed)
             FROM versions WHERE (?1 IS NULL OR cluster=?1) AND (namespace || '/' || name || ' ' || kind) LIKE ?2
             GROUP BY cluster, api_group, resource, namespace, name
             HAVING count(*) > 1 OR max(change) <> 'baseline'
             ORDER BY max(observed_ms) DESC LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![cluster, pat, limit], |r| {
                Ok(ObjectRow {
                    cluster: r.get(0)?, api_group: r.get(1)?, resource: r.get(2)?, kind: r.get(3)?,
                    namespace: r.get(4)?, name: r.get(5)?, versions: r.get(6)?, last_change: r.get(7)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn history(&self, key: &ObjKey) -> anyhow::Result<Vec<VersionRow>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT v.id, v.observed, v.change, v.kind, v.uid, v.body, e.actor, e.via, e.verb, e.id
             FROM versions v LEFT JOIN events e ON v.event_id = e.id
             WHERE v.cluster=?1 AND v.api_group=?2 AND v.resource=?3 AND v.namespace=?4 AND v.name=?5 ORDER BY v.id",
        )?;
        let raw: Vec<(i64, String, String, String, String, String, Option<String>, Option<String>, Option<String>, Option<i64>)> = stmt
            .query_map(params![key.cluster, key.api_group, key.resource, key.namespace, key.name], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?))
            })?
            .collect::<Result<_, _>>()?;
        let mut out = Vec::new();
        let mut prev: Option<Value> = None;
        for (id, observed, change, kind, uid, body, actor, via, verb, event_id) in raw {
            let body: Value = serde_json::from_str(&body)?;
            let changes = changes_for(&change, prev.as_ref(), &body);
            prev = Some(body.clone());
            out.push(VersionRow { id, observed, change, kind, uid, actor, via, verb, event_id, changes, body });
        }
        out.reverse();
        Ok(out)
    }

    pub fn alerts(&self, from_ms: i64, to_ms: i64, limit: i64) -> anyhow::Result<Vec<EventRow>> {
        self.timeline(&Query { cluster: None, actor: None, from_ms, to_ms, humans_only: true, writes_only: false, alerts_only: true, limit })
    }

    pub fn prune(&self, retention_days: i64, now: DateTime<Utc>) -> anyhow::Result<usize> {
        let cutoff = ms(&now) - retention_days * 86_400_000;
        let c = self.conn.lock().unwrap();
        // Keep the newest version of every object so later diffs still have a base.
        let m = c.execute(
            "DELETE FROM versions WHERE observed_ms < ?1 AND id NOT IN (SELECT max(id) FROM versions GROUP BY cluster, api_group, resource, namespace, name)",
            params![cutoff],
        )?;
        // Keep events that kept versions still point at, so their author is not lost.
        let n = c.execute(
            "DELETE FROM events WHERE ts_ms < ?1 AND id NOT IN (SELECT event_id FROM versions WHERE event_id IS NOT NULL)",
            params![cutoff],
        )?;
        c.execute("DELETE FROM hourly WHERE hour_ms < ?1", params![cutoff])?;
        c.execute("DELETE FROM stories WHERE last_ms < ?1", params![cutoff])?;
        Ok(n + m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::{parse_body, to_record};
    use crate::config::Config;
    use crate::rules::evaluate;
    use chrono::TimeZone;
    use serde_json::json;

    fn key() -> ObjKey {
        ObjKey { cluster: "c1".into(), api_group: "apps".into(), resource: "deployments".into(), namespace: "shop".into(), name: "web".into() }
    }

    fn patch(id: &str, user: &str, ts: &str) -> Record {
        let ev = format!(r#"{{"auditID":"{id}","stage":"ResponseComplete","verb":"patch",
          "user":{{"username":"system:serviceaccount:cattle-impersonation-system:cattle-impersonation-u-1"}},
          "impersonatedUser":{{"username":"u-1","extra":{{"username":["{user}"]}}}},
          "objectRef":{{"resource":"deployments","namespace":"shop","name":"web","apiGroup":"apps"}},
          "responseStatus":{{"code":200}},"stageTimestamp":"{ts}"}}"#);
        to_record(&parse_body(ev.as_bytes()).unwrap()[0]).unwrap()
    }

    fn obj(n: i64) -> Value {
        json!({"metadata":{"name":"web","namespace":"shop","resourceVersion": n.to_string()},"spec":{"replicas":n}})
    }

    #[test]
    fn version_is_linked_to_the_patch_that_caused_it() {
        let s = Store::open(":memory:").unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 10, 14, 2, 0, 0).unwrap();
        assert!(s.insert_version(&key(), "Deployment", "u1", &obj(3), "baseline", t0).unwrap().is_some());
        let mut same = obj(3);
        same["status"] = json!({"ready": 3});
        assert!(s.insert_version(&key(), "Deployment", "u1", &same, "modified", t0).unwrap().is_none(), "status-only update ignored");
        let t1 = t0 + chrono::Duration::seconds(1);
        let vid = s.insert_version(&key(), "Deployment", "u1", &obj(5), "modified", t1).unwrap().unwrap();
        let r = patch("p1", "alice", "2026-10-14T02:00:00.600Z");
        assert!(s.insert_event("c1", &r, &evaluate(&r, &Config::test())).unwrap());
        assert!(!s.insert_event("c1", &r, &[]).unwrap(), "duplicates are ignored");
        assert_eq!(s.link_since(0).unwrap(), 1);

        let h = s.history(&key()).unwrap();
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].id, vid);
        assert_eq!(h[0].actor.as_deref(), Some("alice"));
        assert_eq!(h[0].changes[0].path, "spec.replicas");

        let q = Query { cluster: None, actor: Some("u-1".into()), from_ms: 0, to_ms: i64::MAX, humans_only: true, writes_only: true, alerts_only: false, limit: 50 };
        let tl = s.timeline(&q).unwrap();
        assert_eq!(tl.len(), 1);
        assert_eq!(tl[0].version_id, Some(vid));
        assert_eq!(tl[0].changes.as_ref().unwrap()[0].new, Some(json!(5)));
    }

    #[test]
    fn two_quick_edits_keep_their_authors_in_order() {
        let s = Store::open(":memory:").unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 10, 14, 2, 0, 0).unwrap();
        s.insert_version(&key(), "Deployment", "u1", &obj(1), "baseline", t0).unwrap();
        // Watch lags: both versions are observed after both requests completed.
        let v2 = s.insert_version(&key(), "Deployment", "u1", &obj(2), "modified", t0 + chrono::Duration::seconds(4)).unwrap().unwrap();
        let v3 = s.insert_version(&key(), "Deployment", "u1", &obj(3), "modified", t0 + chrono::Duration::seconds(5)).unwrap().unwrap();
        s.insert_event("c1", &patch("a", "alice", "2026-10-14T02:00:01Z"), &[]).unwrap();
        s.insert_event("c1", &patch("b", "bob", "2026-10-14T02:00:02Z"), &[]).unwrap();
        assert_eq!(s.link_since(0).unwrap(), 2);
        let h = s.history(&key()).unwrap();
        let by_id = |id| h.iter().find(|v| v.id == id).unwrap().actor.clone();
        assert_eq!(by_id(v2).as_deref(), Some("alice"));
        assert_eq!(by_id(v3).as_deref(), Some("bob"));
    }

    #[test]
    fn resync_marks_gaps_and_can_still_link() {
        let s = Store::open(":memory:").unwrap();
        let t = Utc.with_ymd_and_hms(2026, 10, 14, 2, 0, 0).unwrap();
        s.insert_version(&key(), "Deployment", "u1", &obj(1), "baseline", t).unwrap();
        s.insert_event("c1", &patch("g", "alice", "2026-10-14T02:05:00Z"), &[]).unwrap();
        s.insert_version(&key(), "Deployment", "u1", &obj(2), "resync", t + chrono::Duration::minutes(10)).unwrap();
        assert_eq!(s.link_since(0).unwrap(), 1);
        let h = s.history(&key()).unwrap();
        assert_eq!(h[0].change, "modified_unseen");
        assert_eq!(h[0].actor.as_deref(), Some("alice"));
        // A new object found on re-list is "added_unseen", not a baseline.
        let other = ObjKey { name: "api".into(), ..key() };
        s.insert_version(&other, "Deployment", "u2", &obj(1), "resync", t).unwrap();
        assert_eq!(s.history(&other).unwrap()[0].change, "added_unseen");
    }

    fn rancher_put(id: &str, ts: &str) -> Record {
        let line = format!(r#"{{"auditID":"{id}","requestURI":"/k8s/clusters/c1/v1/apps.deployments/shop/web","method":"PATCH",
          "user":{{"name":"u-1","extra":{{"username":["alice"]}}}},"responseCode":200,"responseTimestamp":"{ts}",
          "requestHeader":{{"User-Agent":["kubectl/v1.35.0"]}}}}"#);
        crate::rancher::to_record(&crate::rancher::parse_fixture(&line)[0]).unwrap()
    }

    fn all(s: &Store) -> Vec<EventRow> {
        s.timeline(&Query { cluster: None, actor: None, from_ms: 0, to_ms: i64::MAX, humans_only: false, writes_only: false, alerts_only: false, limit: 50 }).unwrap()
    }

    #[test]
    fn rancher_and_downstream_halves_become_one_row() {
        // Downstream first, Rancher (whole seconds, a little later) second.
        let s = Store::open(":memory:").unwrap();
        s.insert_event("c1", &patch("k1", "alice", "2026-10-14T02:00:00.700Z"), &[]).unwrap();
        s.insert_event("local", &rancher_put("r1", "2026-10-14T10:00:01+08:00"), &[]).unwrap();
        let rows = all(&s);
        assert_eq!(rows.len(), 1, "the Rancher half is hidden");
        assert_eq!((rows[0].source.as_str(), rows[0].user_id.as_str()), ("k8s", "u-1"));
        assert_eq!(rows[0].rancher.as_ref().unwrap().user_agent, "kubectl/v1.35.0");

        // Rancher first; the downstream record arrives later and still pairs.
        let s = Store::open(":memory:").unwrap();
        s.insert_event("local", &rancher_put("r1", "2026-10-14T02:00:00Z"), &[]).unwrap();
        assert_eq!(all(&s)[0].source, "rancher", "until then, the Rancher record stands alone");
        s.insert_event("c1", &patch("k1", "alice", "2026-10-14T02:00:00.400Z"), &[]).unwrap();
        assert_eq!(all(&s).len(), 1);
    }

    #[test]
    fn pairing_needs_the_same_action_and_time() {
        let s = Store::open(":memory:").unwrap();
        s.insert_event("c1", &patch("k1", "alice", "2026-10-14T02:00:00Z"), &[]).unwrap();
        // Too late: 10 s after.
        s.insert_event("local", &rancher_put("r1", "2026-10-14T02:00:10Z"), &[]).unwrap();
        // Other cluster.
        let mut other = rancher_put("r2", "2026-10-14T02:00:01Z");
        other.cluster = Some("c2".into());
        s.insert_event("local", &other, &[]).unwrap();
        assert_eq!(all(&s).len(), 3);
        // Two edits in a row pair one to one, nearest first.
        s.insert_event("c1", &patch("k2", "alice", "2026-10-14T02:00:09.800Z"), &[]).unwrap();
        assert_eq!(all(&s).len(), 3);
    }

    #[test]
    fn version_moves_to_the_downstream_half() {
        let s = Store::open(":memory:").unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 10, 14, 2, 0, 0).unwrap();
        s.insert_version(&key(), "Deployment", "u1", &obj(1), "baseline", t0).unwrap();
        let vid = s.insert_version(&key(), "Deployment", "u1", &obj(2), "modified", t0 + chrono::Duration::seconds(1)).unwrap().unwrap();
        // Only Rancher's record so far: the version is credited to it.
        s.insert_event("local", &rancher_put("r1", "2026-10-14T02:00:01Z"), &[]).unwrap();
        assert_eq!(s.link_since(0).unwrap(), 1);
        s.insert_event("c1", &patch("k1", "alice", "2026-10-14T02:00:00.900Z"), &[]).unwrap();
        let rows = all(&s);
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].source.as_str(), rows[0].version_id), ("k8s", Some(vid)));
    }

    #[test]
    fn old_databases_get_the_new_columns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.db");
        let p = path.to_str().unwrap();
        Connection::open(p).unwrap().execute_batch(
            "CREATE TABLE events (id INTEGER PRIMARY KEY, cluster TEXT NOT NULL, audit_id TEXT NOT NULL, ts_ms INTEGER NOT NULL, ts TEXT NOT NULL,
               actor TEXT NOT NULL, principal TEXT NOT NULL, human INTEGER NOT NULL, via TEXT NOT NULL, caller TEXT NOT NULL,
               verb TEXT NOT NULL, api_group TEXT NOT NULL, resource TEXT NOT NULL, subresource TEXT NOT NULL,
               namespace TEXT NOT NULL, name TEXT NOT NULL, code INTEGER NOT NULL,
               source_ip TEXT NOT NULL, user_agent TEXT NOT NULL, request_uri TEXT NOT NULL, request_object TEXT, UNIQUE(cluster, audit_id));
             INSERT INTO events VALUES (1,'c1','old',0,'1970-01-01T00:00:00Z','bob','',1,'direct','bob','delete','','namespaces','','','x',200,'','','',NULL);").unwrap();
        let s = Store::open(p).unwrap();
        s.insert_event("c1", &patch("k1", "alice", "2026-10-14T02:00:00Z"), &[]).unwrap();
        let rows = all(&s);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].source, "k8s", "existing rows default to the Kubernetes source");
    }

    fn ver(seen: &str, name: &str, replicas: i64) -> ObjMsg {
        ObjMsg::Version {
            group: "apps".into(), resource: "deployments".into(), kind: "Deployment".into(),
            namespace: "shop".into(), name: name.into(), uid: format!("uid-{name}"), seen: seen.into(),
            at: Utc.with_ymd_and_hms(2026, 10, 14, 2, 0, 0).unwrap(), object: obj(replicas),
        }
    }

    #[test]
    fn pushed_lists_are_baselines_then_resyncs() {
        let s = Store::open(":memory:").unwrap();
        let start = ObjMsg::ListStart { group: "apps".into(), resource: "deployments".into() };
        let done = |names: &[&str]| ObjMsg::ListDone { group: "apps".into(), resource: "deployments".into(), listed: names.iter().map(|n| ("shop".to_string(), n.to_string())).collect() };
        s.apply_objects("c1", &[start.clone(), ver("list", "web", 1), ver("list", "api", 1), done(&["web", "api"])]).unwrap();
        assert_eq!(s.history(&key()).unwrap()[0].change, "baseline");
        // Collector restarts: web changed meanwhile, api was deleted meanwhile.
        s.apply_objects("c1", &[start, ver("list", "web", 2), done(&["web"])]).unwrap();
        assert_eq!(s.history(&key()).unwrap()[0].change, "modified_unseen");
        let api = ObjKey { name: "api".into(), ..key() };
        assert_eq!(s.history(&api).unwrap()[0].change, "deleted");
        // Live changes after that.
        assert_eq!(s.apply_objects("c1", &[ver("modified", "web", 3)]).unwrap(), 1);
        assert_eq!(s.history(&key()).unwrap()[0].change, "modified");
    }

    fn user_msg(seen: &str, uid: &str, created: &str, at_min: i64) -> ObjMsg {
        ObjMsg::Version {
            group: "management.cattle.io".into(), resource: "users".into(), kind: "User".into(),
            namespace: "".into(), name: "u-1".into(), uid: uid.into(), seen: seen.into(),
            at: Utc.with_ymd_and_hms(2026, 10, 14, 0, 0, 0).unwrap() + chrono::Duration::minutes(at_min),
            object: json!({"metadata":{"name":"u-1","creationTimestamp":created},"displayName":"Alice Chen","username":"",
                           "principalIds":["openldap_user://uid=alice","local://u-1"]}),
        }
    }

    #[test]
    fn deleted_and_readded_user_is_one_person_with_two_lifetimes() {
        let s = Store::open(":memory:").unwrap();
        s.apply_objects("local", &[user_msg("list", "A", "2026-10-13T00:00:00Z", 0)]).unwrap();
        s.insert_event("c1", &patch("k1", "alice", "2026-10-14T02:00:00Z"), &[]).unwrap();
        s.apply_objects("local", &[user_msg("deleted", "A", "2026-10-13T00:00:00Z", 180)]).unwrap();
        // Same principal logs in again: same u-1, new object.
        s.apply_objects("local", &[user_msg("modified", "B", "2026-10-14T05:00:00Z", 300)]).unwrap();
        s.insert_event("c1", &patch("k2", "alice.chen", "2026-10-14T06:00:00Z"), &[]).unwrap();

        let actors = s.actors(0, i64::MAX).unwrap();
        assert_eq!(actors.len(), 1, "merged by user ID");
        let a = &actors[0];
        assert_eq!((a.key.as_str(), a.actor.as_str(), a.writes, a.deleted), ("u-1", "Alice Chen", 2, false));
        assert_eq!(a.lifetimes.iter().map(|l| (l.uid.as_str(), l.writes)).collect::<Vec<_>>(), vec![("A", 1), ("B", 1)]);
        assert!(a.lifetimes[0].deleted_ms.is_some() && a.lifetimes[1].deleted_ms.is_none());
        // Filtering by the user ID finds both, whatever name each record carries.
        let q = Query { cluster: None, actor: Some("u-1".into()), from_ms: 0, to_ms: i64::MAX, humans_only: true, writes_only: true, alerts_only: false, limit: 50 };
        assert_eq!(s.timeline(&q).unwrap().len(), 2);
        assert_eq!(s.users().unwrap()[0].1.len(), 2);
    }

    #[test]
    fn relations_both_ways_and_through_selectors() {
        let s = Store::open(":memory:").unwrap();
        let t = Utc.with_ymd_and_hms(2026, 10, 14, 2, 0, 0).unwrap();
        let dep = json!({"metadata":{"name":"web","namespace":"shop"},"spec":{"template":{"metadata":{"labels":{"app":"web"}},
            "spec":{"volumes":[{"name":"c","configMap":{"name":"web-config"}}],"containers":[{"name":"web"}]}}}});
        s.insert_version(&key(), "Deployment", "d1", &dep, "baseline", t).unwrap();
        let svc = ObjKey { api_group: "".into(), resource: "services".into(), ..key() };
        s.insert_version(&svc, "Service", "s1", &json!({"metadata":{"name":"web"},"spec":{"selector":{"app":"web"}}}), "baseline", t).unwrap();
        let cm = ObjKey { api_group: "".into(), resource: "configmaps".into(), name: "web-config".into(), ..key() };
        s.insert_version(&cm, "ConfigMap", "c1", &json!({"metadata":{"name":"web-config"},"data":{"a":"1"}}), "baseline", t).unwrap();
        s.insert_version(&cm, "ConfigMap", "c1", &json!({"metadata":{"name":"web-config"},"data":{"a":"2"}}), "modified", t + chrono::Duration::minutes(5)).unwrap();

        let r = s.related("c1", "Deployment", "shop", "web").unwrap();
        let has = |d: &str, rel: &str, k: &str| r.iter().any(|x| x.direction == d && x.rel == rel && x.kind == k);
        assert!(has("out", "mounts", "ConfigMap") && has("in", "selects", "Service"), "{r:?}");
        assert!(r.iter().find(|x| x.kind == "ConfigMap").unwrap().last_change.is_some(), "the ConfigMap changed");
        let r = s.related("c1", "Service", "shop", "web").unwrap();
        assert_eq!((r[0].direction, r[0].kind.as_str(), r[0].name.as_str()), ("out", "Deployment", "web"));
        // Labels change: the Service no longer selects it.
        let mut moved = dep.clone();
        moved["spec"]["template"]["metadata"]["labels"]["app"] = json!("web-v2");
        s.insert_version(&key(), "Deployment", "d1", &moved, "modified", t + chrono::Duration::minutes(9)).unwrap();
        assert!(s.related("c1", "Service", "shop", "web").unwrap().is_empty());
    }

    fn direct(id: &str, verb: &str, resource: &str, name: &str, ts: &str) -> Record {
        let ev = format!(r#"{{"auditID":"{id}","stage":"ResponseComplete","verb":"{verb}","user":{{"username":"system:admin"}},
          "objectRef":{{"resource":"{resource}","name":"{name}","apiGroup":"rbac.authorization.k8s.io"}},"responseStatus":{{"code":200}},"stageTimestamp":"{ts}"}}"#);
        to_record(&parse_body(ev.as_bytes()).unwrap()[0]).unwrap()
    }

    #[test]
    fn risky_bursts_become_one_story_and_hours_are_counted() {
        let s = Store::open(":memory:").unwrap();
        let cfg = Config::test();
        for (id, ts) in [("a", "2026-10-14T15:00:00Z"), ("b", "2026-10-14T15:06:00Z"), ("c", "2026-10-14T15:30:00Z")] {
            let r = direct(id, "create", "clusterrolebindings", id, ts);
            s.insert_event("c1", &r, &evaluate(&r, &cfg)).unwrap();
        }
        let st = s.stories(None, 0, i64::MAX).unwrap();
        assert_eq!(st.iter().map(|x| x.count).collect::<Vec<_>>(), vec![2, 1], "15:00 and 15:06 together, 15:30 alone");
        assert_eq!(st[0].actor, "system:admin");
        let h = s.hourly(None, 0, i64::MAX).unwrap();
        assert_eq!((h.len(), h[0].writes, h[0].alerts), (1, 3, 3));
    }

    #[test]
    fn a_paired_rancher_half_is_not_counted_twice() {
        let s = Store::open(":memory:").unwrap();
        s.insert_event("local", &rancher_put("r1", "2026-10-14T02:00:01Z"), &[]).unwrap();
        assert_eq!(s.hourly(None, 0, i64::MAX).unwrap()[0].writes, 1);
        s.insert_event("c1", &patch("k1", "alice", "2026-10-14T02:00:00.900Z"), &[]).unwrap();
        let h = s.hourly(None, 0, i64::MAX).unwrap();
        assert_eq!(h.iter().map(|x| x.writes).sum::<i64>(), 1, "{h:?}");
    }

    #[test]
    fn unattributed_versions_projects_and_clusters() {
        let s = Store::open(":memory:").unwrap();
        let t = Utc.with_ymd_and_hms(2026, 10, 14, 2, 0, 0).unwrap();
        s.insert_version(&key(), "Deployment", "u1", &obj(1), "baseline", t).unwrap();
        s.insert_version(&key(), "Deployment", "u1", &obj(2), "resync", t + chrono::Duration::minutes(30)).unwrap();
        let u = s.unattributed(0, i64::MAX).unwrap();
        assert_eq!((u.len(), u[0].change.as_str(), u[0].changes[0].path.as_str()), (1, "modified_unseen", "spec.replicas"));
        let ns = ObjKey { api_group: "".into(), resource: "namespaces".into(), namespace: "".into(), name: "shop".into(), ..key() };
        s.insert_version(&ns, "Namespace", "n1", &json!({"metadata":{"name":"shop","labels":{"field.cattle.io/projectId":"p-abc"}}}), "baseline", t).unwrap();
        assert_eq!(s.namespace_projects(None).unwrap(), vec![("c1".into(), "shop".into(), "p-abc".into())]);
        s.insert_event("c1", &patch("k1", "alice", "2026-10-14T02:00:00Z"), &[]).unwrap();
        let c = s.clusters(0, i64::MAX).unwrap();
        assert_eq!((c[0].cluster.as_str(), c[0].k8s_events, c[0].rancher_events), ("c1", 1, 0));
    }

    #[test]
    fn access_review_from_stored_versions() {
        let s = Store::open(":memory:").unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
        let mk = |res: &str, ns: &str, name: &str, uid: &str, body: Value, seen: &str, at: DateTime<Utc>| {
            let key = ObjKey { cluster: "local".into(), api_group: "management.cattle.io".into(), resource: res.into(), namespace: ns.into(), name: name.into() };
            let kind = match res { "users" => "User", "roletemplates" => "RoleTemplate", _ => "ClusterRoleTemplateBinding" };
            s.insert_version(&key, kind, uid, &body, seen, at).unwrap()
        };
        mk("roletemplates", "", "cluster-owner", "rt1", json!({"rules":[{"apiGroups":["*"],"resources":["*"],"verbs":["*"]}]}), "baseline", t0);
        mk("users", "", "u-dave", "ud", json!({"displayName":"Dave","principalIds":["local://u-dave"]}), "baseline", t0);
        // carol grants dave cluster-owner through the Rancher API (Norman: no namespace/name to match).
        let t1 = t0 + chrono::Duration::days(3);
        let line = format!(r#"{{"auditID":"g1","requestURI":"/v3/clusterroletemplatebindings","method":"POST","user":{{"name":"u-carol","extra":{{"username":["carol"]}}}},"responseCode":201,"responseTimestamp":"{}"}}"#, t1.to_rfc3339());
        let r = crate::rancher::to_record(&crate::rancher::parse_fixture(&line)[0]).unwrap();
        s.insert_event("local", &r, &[]).unwrap();
        mk("clusterroletemplatebindings", "c-m-1", "crtb-x", "b1", json!({"clusterName":"c-m-1","roleTemplateName":"cluster-owner","userName":"u-dave"}), "modified", t1 + chrono::Duration::seconds(1));
        // Later Rancher deletes the user, then cleans up the binding itself.
        let t2 = t0 + chrono::Duration::days(10);
        mk("users", "", "u-dave", "ud", json!({"displayName":"Dave"}), "deleted", t2);
        mk("clusterroletemplatebindings", "c-m-1", "crtb-x", "b1", json!({"clusterName":"c-m-1","roleTemplateName":"cluster-owner","userName":"u-dave"}), "deleted", t2 + chrono::Duration::seconds(3));

        let mid = (t0 + chrono::Duration::days(5)).timestamp_millis();
        let snap = crate::access::snapshot(mid, &s.access_objects("local", mid).unwrap(), s.access_users("local", mid).unwrap());
        let g = &snap.users[0].grants[0];
        assert_eq!((snap.users[0].display_name.as_str(), g.role.as_str(), g.admin_like), ("Dave", "cluster-owner", true));
        assert_eq!((g.granted_by.as_deref(), g.granted_by_inferred), (Some("carol"), true));
        let later = (t0 + chrono::Duration::days(20)).timestamp_millis();
        assert!(s.access_objects("local", later).unwrap().iter().all(|o| o.resource != "clusterroletemplatebindings"));
        let (at, by, reason) = s.access_removal("local", "clusterroletemplatebindings", "c-m-1", "crtb-x", "u-dave", mid, later).unwrap();
        assert!(at.is_some() && by.is_none());
        assert_eq!(reason, Some("rancher_cleanup"));
    }

    #[test]
    fn deletion_timestamp_is_attributed_to_the_delete() {
        let s = Store::open(":memory:").unwrap();
        let t = Utc.with_ymd_and_hms(2026, 10, 14, 2, 0, 0).unwrap();
        let ns = ObjKey { cluster: "c1".into(), api_group: "".into(), resource: "namespaces".into(), namespace: "".into(), name: "old".into() };
        s.insert_version(&ns, "Namespace", "n1", &json!({"metadata":{"name":"old"}}), "baseline", t).unwrap();
        s.insert_version(&ns, "Namespace", "n1", &json!({"metadata":{"name":"old","deletionTimestamp":"2026-10-14T02:00:01Z"}}), "modified", t + chrono::Duration::seconds(1)).unwrap();
        let ev = r#"{"auditID":"d","stage":"ResponseComplete","verb":"delete","user":{"username":"system:admin"},
          "objectRef":{"resource":"namespaces","namespace":"old","name":"old","apiVersion":"v1"},
          "responseStatus":{"code":200},"stageTimestamp":"2026-10-14T02:00:00.500Z"}"#;
        let r = to_record(&parse_body(ev.as_bytes()).unwrap()[0]).unwrap();
        s.insert_event("c1", &r, &[]).unwrap();
        assert_eq!(s.link_since(0).unwrap(), 1);
        let h = s.history(&ns).unwrap();
        assert_eq!((h[0].change.as_str(), h[0].actor.as_deref()), ("deleting", Some("system:admin")));
    }
}
