//! SQLite storage for audit records, object versions and alerts.

use crate::audit::{Record, Source, Via};
use crate::diff::{diff, normalize, same_content, Change};
use crate::rules::{judge, Alert, Lookup};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::Serialize;
use crate::integrity::{link, Fields};
use crate::relations::{extract, pod_template, selector_matches};
use crate::watch::ObjMsg;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct Store {
    pub(crate) conn: Arc<Mutex<Connection>>,
    /// Settings that decide alerts (rules, change windows, working hours).
    pub(crate) cfg: Arc<crate::config::Config>,
    /// The re-judging of stored records after a settings change, when one has run.
    pub(crate) job: Arc<Mutex<Option<crate::rulestate::Job>>>,
}

/// Every chained record after `?1`, in id order, all clusters in one pass. Reading by id (the
/// table's own order) needs no sort. Asking per cluster did: SQLite sorted a whole cluster in a
/// temporary table first, in memory on the main connection (about 200 MB at 500,000 records,
/// which ended in the pod being killed at its 512 MiB limit) and in a temporary file on the
/// reader, which fails where the root filesystem is read-only. An index on the cluster would
/// avoid the sort too, but SQLite then picked it for the object detail as well (4 ms to 167 ms).
const CHAIN_SQL: &str = "SELECT id, audit_id, source, ts, actor, principal, user_id, human, via, caller, verb, api_group, resource, subresource,
        namespace, name, code, source_ip, user_agent, request_uri, request_object, hash, cluster
 FROM events NOT INDEXED WHERE id > ?1 AND hash IS NOT NULL ORDER BY id";

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
  source TEXT NOT NULL DEFAULT 'k8s', user_id TEXT NOT NULL DEFAULT '', link_id INTEGER, story_id INTEGER, hash TEXT, received_ms INTEGER,
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
CREATE TABLE IF NOT EXISTS chain (cluster TEXT PRIMARY KEY, head_id INTEGER NOT NULL, head_hash TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS chain_base (cluster TEXT PRIMARY KEY, base_id INTEGER NOT NULL, base_hash TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS anchors (
  id INTEGER PRIMARY KEY, cluster TEXT NOT NULL, event_id INTEGER NOT NULL, hash TEXT NOT NULL, at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS forward_cursor (
  target TEXT PRIMARY KEY, event_id INTEGER NOT NULL DEFAULT 0, anchor_id INTEGER NOT NULL DEFAULT 0,
  last_ok_ms INTEGER, last_error TEXT
);
CREATE TABLE IF NOT EXISTS heartbeats (
  cluster TEXT NOT NULL, collector TEXT NOT NULL, instance TEXT NOT NULL, last_ms INTEGER NOT NULL, details TEXT NOT NULL,
  first_ms INTEGER,
  PRIMARY KEY (cluster, collector, instance)
);
CREATE TABLE IF NOT EXISTS gaps (
  id INTEGER PRIMARY KEY, cluster TEXT NOT NULL, collector TEXT NOT NULL, instance TEXT NOT NULL, from_ms INTEGER NOT NULL, to_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS gaps_time ON gaps(to_ms);
CREATE TABLE IF NOT EXISTS security_events (
  id INTEGER PRIMARY KEY, cluster TEXT NOT NULL, nv_id TEXT NOT NULL, ts_ms INTEGER NOT NULL, name TEXT NOT NULL, level TEXT NOT NULL,
  namespace TEXT NOT NULL, workload TEXT NOT NULL, proc_name TEXT NOT NULL, proc_cmd TEXT NOT NULL, proc_parent TEXT NOT NULL,
  proc_user TEXT NOT NULL, action TEXT NOT NULL, message TEXT NOT NULL,
  UNIQUE (cluster, nv_id)
);
CREATE INDEX IF NOT EXISTS security_pod ON security_events(cluster, namespace, ts_ms);
CREATE TABLE IF NOT EXISTS rejected_lines (
  id INTEGER PRIMARY KEY, cluster TEXT NOT NULL, source TEXT NOT NULL, received_ms INTEGER NOT NULL,
  len INTEGER NOT NULL, sha256 TEXT NOT NULL, error TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS rejected_time ON rejected_lines(received_ms);
CREATE TABLE IF NOT EXISTS listing (
  cluster TEXT NOT NULL, api_group TEXT NOT NULL, resource TEXT NOT NULL, first INTEGER NOT NULL,
  PRIMARY KEY (cluster, api_group, resource)
);
CREATE TABLE IF NOT EXISTS listed (
  cluster TEXT NOT NULL, api_group TEXT NOT NULL, resource TEXT NOT NULL,
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
    ("hash", "TEXT"),
    ("received_ms", "INTEGER"),
];

/// A Rancher record that was matched to the downstream record of the same action is shown
/// through that downstream record; this hides the Rancher half so nothing is counted twice.
/// What groups records into one person: the Rancher user ID, else the name.
const ACTOR_KEY: &str = "CASE WHEN user_id<>'' THEN user_id ELSE actor END";

const SHOWN: &str = "NOT (e.source='rancher' AND e.link_id IS NOT NULL)";
pub const SHOWN_PUB: &str = SHOWN;

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
    pub audit_id: String,
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
    /// Severity of each alert as judged (by the settings in force then, or at the last re-judging).
    pub alert_severity: BTreeMap<String, String>,
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

/// Three missed heartbeats (one a minute) count as a gap.
pub const GAP_MS: i64 = 180_000;

/// A file collector (one per node) silent this long is taken as a node that was removed or
/// replaced, not a node that is down: the change map stops marking the time after it as
/// unseen, and the coverage page lists it as retired. If it checks in again, the whole
/// silence is recorded as a gap as usual. Its row is dropped once it is older than retention.
pub const RETIRED_MS: i64 = 86_400_000;

#[derive(Debug, Serialize)]
pub struct CoverageRow {
    pub cluster: String,
    pub collectors: Vec<Value>,
    /// Periods not collected: silences of a collector (toMs null: still silent) and periods a
    /// collector reported itself, each with a reason.
    pub gaps: Vec<Value>,
    /// Lines a collector sent that could not be read, set aside so the rest of the batch was stored.
    pub rejected: Vec<Value>,
    /// Lines set aside per source and node in the range (the lines: /api/rejected).
    #[serde(rename = "rejectedCounts")]
    pub rejected_counts: Vec<Value>,
    /// Where a collector's collection starts ("collection-start" reports): nothing before it
    /// was collected by that instance, or, with detail "readExisting", what it read at install.
    pub starts: Vec<Value>,
}

/// Reason shown for a gap found from missing heartbeats (stored as '').
const SILENT: &str = "collector-silent";

/// Reason of a report that marks where a collector's collection starts (not a gap).
pub const START: &str = "collection-start";

pub struct RestoreSource {
    pub cluster: String,
    pub kind: String,
    pub namespace: String,
    pub name: String,
    pub change: String,
    pub observed: String,
    pub by: Option<String>,
    pub before: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct GitopsRow {
    pub event_id: i64,
    pub cluster: String,
    pub ts: String,
    pub actor: String,
    pub verb: String,
    pub resource: String,
    pub namespace: String,
    pub name: String,
    /// "<kind>/<namespace>/<name>" of the Fleet source when the labels say, else the objectset ID.
    pub source: Option<String>,
    /// When Fleet wrote the object again afterwards (drift correction). None: still differs
    /// from Git, as far as the audit log shows (correctDrift is off by default).
    pub reverted_at: Option<String>,
}

/// The Fleet source of a deployed object, from its markers.
pub fn fleet_marker(body: &Value) -> Option<String> {
    let label = |k: &str| body.pointer(&format!("/metadata/labels/{}", k.replace('/', "~1"))).and_then(Value::as_str);
    if let (Some(kind), Some(name)) = (label("fleet.cattle.io/managed-by-kind"), label("fleet.cattle.io/managed-by-name")) {
        return Some(format!("{kind}/{}/{name}", label("fleet.cattle.io/managed-by-namespace").unwrap_or("")));
    }
    body.pointer("/metadata/annotations/objectset.rio.cattle.io~1id").and_then(Value::as_str).map(|id| format!("objectset/{id}"))
}

#[derive(Debug, Serialize)]
pub struct ChainReport {
    pub cluster: String,
    /// Records before this were removed by retention; the chain is checked from here.
    pub base_id: i64,
    pub verified: i64,
    /// The first record whose link does not match: something was changed, inserted or removed.
    pub first_bad_id: Option<i64>,
    pub head_id: i64,
    pub head_hash: String,
}

/// A record as sent to a SIEM.
#[derive(Debug, Serialize, Clone)]
pub struct ForwardRecord {
    pub id: i64,
    pub cluster: String,
    pub ts: String,
    pub source: String,
    pub audit_id: String,
    pub actor: String,
    pub user_id: String,
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
    pub rules: Vec<String>,
    pub risky: bool,
    pub story_id: Option<i64>,
    /// This record's link in its cluster's chain.
    pub hash: Option<String>,
    /// From the matched Rancher record, when there is one.
    pub rancher_user_agent: Option<String>,
    pub rancher_source_ip: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct Anchor {
    pub id: i64,
    pub cluster: String,
    pub event_id: i64,
    pub hash: String,
    pub at_ms: i64,
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
    pub alert_severity: BTreeMap<String, String>,
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
pub(crate) fn counts(r: &Record, alerts: &[Alert]) -> bool {
    crate::audit::is_write(&r.verb)
        || matches!(r.subresource.as_str(), "exec" | "attach" | "portforward")
        || r.source == Source::Rancher
        || !alerts.is_empty()
}

/// "rule:severity,rule:severity" as aggregated from the alerts table.
fn rules_of(agg: &Option<String>) -> Vec<String> {
    agg.as_deref().map(|s| s.split(',').map(|x| x.split(':').next().unwrap_or(x).to_string()).collect()).unwrap_or_default()
}

fn severities_of(agg: &Option<String>) -> BTreeMap<String, String> {
    agg.as_deref()
        .map(|s| s.split(',').filter_map(|x| x.split_once(':')).map(|(r, v)| (r.to_string(), v.to_string())).collect())
        .unwrap_or_default()
}

/// Where alert-derived data goes: the live tables, or the shadow tables a re-judging fills
/// before it replaces the live ones in one step.
#[derive(Clone, Copy)]
pub(crate) struct Tables {
    pub stories: &'static str,
    pub hourly: &'static str,
    pub set_story: &'static str,
}

impl Tables {
    pub const LIVE: Tables = Tables { stories: "stories", hourly: "hourly", set_story: "UPDATE events SET story_id=?1 WHERE id=?2" };
    pub const SHADOW: Tables = Tables { stories: "rc_stories", hourly: "rc_hourly", set_story: "INSERT INTO rc_story_of (story_id, event_id) VALUES (?1, ?2)" };
}

/// The latest stored version of an object at a time; if none was stored by then (the object
/// was first seen later), the earliest one.
pub(crate) fn version_as_of(c: &Connection, cluster: &str, group: &str, resource: &str, ns: &str, name: &str, at_ms: i64) -> anyhow::Result<Option<Value>> {
    let mut st = c.prepare_cached(
        "SELECT body FROM versions WHERE cluster=?1 AND api_group=?2 AND resource=?3 AND namespace=?4 AND name=?5
         ORDER BY observed_ms <= ?6 DESC, CASE WHEN observed_ms <= ?6 THEN -observed_ms ELSE observed_ms END, id DESC LIMIT 1",
    )?;
    let body: Option<String> = st.query_row(params![cluster, group, resource, ns, name, at_ms], |r| r.get(0)).optional()?;
    Ok(body.and_then(|b| serde_json::from_str::<Value>(&b).ok()))
}

impl Lookup for Connection {
    fn namespace_labels(&self, cluster: &str, ns: &str, at_ms: i64) -> anyhow::Result<Option<Map<String, Value>>> {
        Ok(version_as_of(self, cluster, "", "namespaces", "", ns, at_ms)?
            .and_then(|v| v.pointer("/metadata/labels").and_then(Value::as_object).cloned()))
    }
    fn fleet_source(&self, cluster: &str, group: &str, resource: &str, ns: &str, name: &str, at_ms: i64) -> anyhow::Result<Option<String>> {
        Ok(version_as_of(self, cluster, group, resource, ns, name, at_ms)?.as_ref().and_then(fleet_marker))
    }
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

/// Small tables the change map reads instead of scanning all history on every page view:
/// the newest event and version per cluster, and the kind of each resource. Triggers keep them
/// current on every insert path; a database from before them is filled once.
fn prepare_stats(conn: &Connection) -> anyhow::Result<()> {
    let existed: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='cluster_stats')", [], |r| r.get(0))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS cluster_stats (
           cluster TEXT PRIMARY KEY, last_event_ms INTEGER, last_version_ms INTEGER
         ) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS resource_kinds (
           cluster TEXT NOT NULL, api_group TEXT NOT NULL, resource TEXT NOT NULL, kind TEXT NOT NULL,
           PRIMARY KEY (cluster, api_group, resource, kind)
         ) WITHOUT ROWID;
         CREATE TRIGGER IF NOT EXISTS events_stats AFTER INSERT ON events BEGIN
           INSERT INTO cluster_stats (cluster, last_event_ms) VALUES (NEW.cluster, NEW.ts_ms)
           ON CONFLICT (cluster) DO UPDATE SET last_event_ms = max(coalesce(last_event_ms, excluded.last_event_ms), excluded.last_event_ms);
         END;
         CREATE TRIGGER IF NOT EXISTS versions_stats AFTER INSERT ON versions BEGIN
           INSERT INTO cluster_stats (cluster, last_version_ms) VALUES (NEW.cluster, NEW.observed_ms)
           ON CONFLICT (cluster) DO UPDATE SET last_version_ms = max(coalesce(last_version_ms, excluded.last_version_ms), excluded.last_version_ms);
           INSERT OR IGNORE INTO resource_kinds (cluster, api_group, resource, kind) VALUES (NEW.cluster, NEW.api_group, NEW.resource, NEW.kind);
         END;",
    )?;
    if !existed {
        conn.execute_batch(
            "INSERT INTO cluster_stats (cluster, last_event_ms) SELECT cluster, max(ts_ms) FROM events WHERE true GROUP BY cluster
               ON CONFLICT (cluster) DO UPDATE SET last_event_ms = excluded.last_event_ms;
             INSERT INTO cluster_stats (cluster, last_version_ms) SELECT cluster, max(observed_ms) FROM versions WHERE true GROUP BY cluster
               ON CONFLICT (cluster) DO UPDATE SET last_version_ms = excluded.last_version_ms;
             INSERT OR IGNORE INTO resource_kinds (cluster, api_group, resource, kind) SELECT DISTINCT cluster, api_group, resource, kind FROM versions;",
        )?;
    }
    Ok(())
}

impl Store {
    /// Open with the built-in rule settings.
    #[cfg(test)]
    pub fn open(path: &str) -> anyhow::Result<Self> {
        Self::open_with(path, Arc::new(crate::config::Config::defaults()))
    }

    /// A second, read-only connection to the same database, for long reads (the change map's
    /// chain check) that should not hold the main connection. WAL lets it read while the main
    /// connection writes. An in-memory database has no second connection; the store itself is used.
    pub fn reader(&self) -> Store {
        let path = self.conn.lock().unwrap().path().map(str::to_string).filter(|p| !p.is_empty());
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
        match path.and_then(|p| Connection::open_with_flags(p, flags).ok()) {
            Some(c) => Store { conn: Arc::new(Mutex::new(c)), cfg: self.cfg.clone(), job: self.job.clone() },
            None => self.clone(),
        }
    }

    pub fn open_with(path: &str, cfg: Arc<crate::config::Config>) -> anyhow::Result<Self> {
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
        // 0.2.1: when an instance first checked in, so a replaced pod's gap ends where its successor began.
        let beat_cols: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('heartbeats')")?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        if !beat_cols.iter().any(|h| h == "first_ms") {
            conn.execute_batch("ALTER TABLE heartbeats ADD COLUMN first_ms INTEGER")?;
        }
        // Gaps reported by a collector say why; '' is a heartbeat silence.
        let gap_cols: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('gaps')")?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for col in ["reason", "detail"] {
            if !gap_cols.iter().any(|h| h == col) {
                conn.execute_batch(&format!("ALTER TABLE gaps ADD COLUMN {col} TEXT NOT NULL DEFAULT ''"))?;
            }
        }
        // A collector resends a report it got no answer for: one row per report. Heartbeat
        // silences (reason '') are found by the server once and are not covered.
        conn.execute_batch(
            "DELETE FROM gaps WHERE reason <> '' AND id NOT IN
               (SELECT MIN(id) FROM gaps WHERE reason <> '' GROUP BY cluster, collector, instance, from_ms, reason);
             CREATE UNIQUE INDEX IF NOT EXISTS gaps_reported ON gaps(cluster, collector, instance, from_ms, reason) WHERE reason <> '';",
        )?;
        // Where a set-aside line was read (node, file, offset), so a resent batch records it once.
        let rej_cols: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('rejected_lines')")?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for (col, ddl) in [("instance", "TEXT NOT NULL DEFAULT ''"), ("file", "TEXT NOT NULL DEFAULT ''"), ("offset", "INTEGER")] {
            if !rej_cols.iter().any(|h| h == col) {
                conn.execute_batch(&format!("ALTER TABLE rejected_lines ADD COLUMN {col} {ddl}"))?;
            }
        }
        conn.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS rejected_at ON rejected_lines(cluster, instance, file, offset, sha256) WHERE offset IS NOT NULL;",
        )?;
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS events_pair ON events(cluster, user_id, source, ts_ms);
             CREATE INDEX IF NOT EXISTS events_link ON events(link_id);",
        )?;
        crate::rulestate::prepare(&conn)?;
        prepare_stats(&conn)?;
        crate::identity::prepare(&conn, &cfg.identities)?;
        // Ingest waits briefly instead of failing while a background build holds the write lock.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        crate::shown::prepare(&conn, conn.path().map(str::to_string).filter(|p| !p.is_empty()))?;
        let store = Store { conn: Arc::new(Mutex::new(conn)), cfg, job: Arc::new(Mutex::new(None)) };
        store.register_settings(Utc::now().timestamp_millis())?;
        Ok(store)
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

    /// Whether the object's latest stored version is deployed by Fleet, and from which source.
    /// Fleet marks what it deploys with its objectset ID annotation (all versions) and, in
    /// newer versions, labels naming the GitRepo. Helm's own labels are not enough: any Helm
    /// release has them.
    fn fleet_source(c: &Connection, cluster: &str, group: &str, resource: &str, ns: &str, name: &str) -> anyhow::Result<Option<String>> {
        let body: Option<String> = c
            .query_row(
                "SELECT body FROM versions WHERE cluster=?1 AND api_group=?2 AND resource=?3 AND namespace=?4 AND name=?5 ORDER BY id DESC LIMIT 1",
                params![cluster, group, resource, ns, name],
                |r| r.get(0),
            )
            .optional()?;
        let Some(body) = body.and_then(|b| serde_json::from_str::<Value>(&b).ok()) else { return Ok(None) };
        Ok(fleet_marker(&body))
    }

    /// Fleet-managed objects people changed in the range, and whether Fleet put them back.
    pub fn gitops_bypasses(&self, from_ms: i64, to_ms: i64) -> anyhow::Result<Vec<GitopsRow>> {
        let c = self.conn.lock().unwrap();
        let mut st = c.prepare(&format!(
            "SELECT e.id, e.cluster, e.ts, e.ts_ms, e.actor, e.verb, e.api_group, e.resource, e.namespace, e.name FROM events e
             JOIN alerts a ON a.event_id=e.id AND a.rule='gitops_bypassed'
             WHERE e.ts_ms BETWEEN ?1 AND ?2 AND {SHOWN} ORDER BY e.ts_ms"
        ))?;
        let rows: Vec<(i64, String, String, i64, String, String, String, String, String, String)> = st
            .query_map(params![from_ms, to_ms], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?)))?
            .collect::<Result<_, _>>()?;
        drop(st);
        let mut out = Vec::new();
        for (id, cluster, ts, ts_ms, actor, verb, group, resource, ns, name) in rows {
            let source = Self::fleet_source(&c, &cluster, &group, &resource, &ns, &name)?;
            // Fleet's agent writing the same object afterwards: drift correction put it back.
            let reverted: Option<String> = c
                .query_row(
                    "SELECT ts FROM events WHERE cluster=?1 AND api_group=?2 AND resource=?3 AND namespace=?4 AND name=?5 AND ts_ms > ?6
                       AND caller LIKE 'system:serviceaccount:cattle-fleet%:fleet-agent' AND verb IN ('update','patch','create')
                       AND code BETWEEN 200 AND 299 ORDER BY ts_ms LIMIT 1",
                    params![cluster, group, resource, ns, name, ts_ms],
                    |r| r.get(0),
                )
                .optional()?;
            out.push(GitopsRow { event_id: id, cluster, ts, actor, verb, resource, namespace: ns, name, source, reverted_at: reverted });
        }
        Ok(out)
    }

    fn bump(tx: &Connection, cluster: &str, t: i64, key: &str, human: bool, writes: i64, alerts: i64) -> anyhow::Result<()> {
        Self::bump_in(tx, Tables::LIVE, cluster, t, key, human, writes, alerts)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn bump_in(tx: &Connection, tb: Tables, cluster: &str, t: i64, key: &str, human: bool, writes: i64, alerts: i64) -> anyhow::Result<()> {
        let hour = t - t.rem_euclid(3_600_000);
        tx.prepare_cached(&format!(
            "INSERT INTO {h} (cluster, hour_ms, actor_key, human, writes, alerts) VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(cluster, hour_ms, actor_key) DO UPDATE SET writes=writes+excluded.writes, alerts=alerts+excluded.alerts,
               human=max(human, excluded.human)",
            h = tb.hourly
        ))?
        .execute(params![cluster, hour, key, human as i64, writes, alerts])?;
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
    fn add_to_story(tx: &Connection, id: i64, cluster: &str, key: &str, via: &str, t: i64, alerts: &[Alert]) -> anyhow::Result<()> {
        Self::add_to_story_in(tx, Tables::LIVE, id, cluster, key, via, t, alerts)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn add_to_story_in(tx: &Connection, tb: Tables, id: i64, cluster: &str, key: &str, via: &str, t: i64, alerts: &[Alert]) -> anyhow::Result<()> {
        const GAP: i64 = 10 * 60_000;
        let sev = if alerts.iter().any(|a| a.severity == "high") { "high" } else { "medium" };
        let hit: Option<i64> = tx
            .prepare_cached(&format!(
                "SELECT id FROM {st} WHERE cluster=?1 AND actor_key=?2 AND via=?3 AND last_ms >= ?4 AND first_ms <= ?5
                 ORDER BY abs(last_ms - ?6) LIMIT 1",
                st = tb.stories
            ))?
            .query_row(params![cluster, key, via, t - GAP, t + GAP, t], |r| r.get(0))
            .optional()?;
        let sid = match hit {
            Some(sid) => {
                tx.prepare_cached(&format!(
                    "UPDATE {st} SET first_ms=min(first_ms, ?2), last_ms=max(last_ms, ?2), count=count+1,
                       severity=CASE WHEN severity='high' OR ?3='high' THEN 'high' ELSE 'medium' END WHERE id=?1",
                    st = tb.stories
                ))?
                .execute(params![sid, t, sev])?;
                sid
            }
            None => {
                tx.prepare_cached(&format!(
                    "INSERT INTO {st} (cluster, actor_key, via, severity, first_ms, last_ms, count) VALUES (?1,?2,?3,?4,?5,?5,1)",
                    st = tb.stories
                ))?
                .execute(params![cluster, key, via, sev, t])?;
                tx.last_insert_rowid()
            }
        };
        tx.prepare_cached(tb.set_story)?.execute(params![sid, id])?;
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
                    (SELECT group_concat(rule || ':' || severity) FROM alerts a WHERE a.event_id=events.id)
             FROM events WHERE story_id=?1 ORDER BY ts_ms",
        )?;
        for s in rows.iter_mut() {
            s.events = ev
                .query_map(params![s.id], |r| {
                    let al: Option<String> = r.get(8)?;
                    Ok(StoryEvent {
                        id: r.get(0)?, ts: r.get(1)?, verb: r.get(2)?, resource: r.get(3)?, subresource: r.get(4)?,
                        namespace: r.get(5)?, name: r.get(6)?, alerts: rules_of(&al), alert_severity: severities_of(&al),
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
    pub fn insert_events(&self, cluster: &str, batch: &[Record]) -> anyhow::Result<usize> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        let mut stored = 0;
        {
            let mut ins = tx.prepare_cached(
                "INSERT OR IGNORE INTO events (cluster,audit_id,ts_ms,ts,actor,principal,human,via,caller,verb,api_group,resource,subresource,namespace,name,code,source_ip,user_agent,request_uri,request_object,source,user_id)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)",
            )?;
            let mut al = tx.prepare_cached("INSERT INTO alerts (event_id, rule, severity) VALUES (?1,?2,?3)")?;
            for r in batch {
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
                let (via, ts, source) = (r.via.as_str(), r.ts.to_rfc3339(), r.source.as_str());
                let fields = Fields {
                    cluster: cl, audit_id: &r.audit_id, source, ts: &ts, actor: &r.actor, principal: &r.principal, user_id: &r.user_id,
                    human: r.human, via, caller: &r.caller, verb: &r.verb, api_group: &r.api_group, resource: &r.resource,
                    subresource: &r.subresource, namespace: &r.namespace, name: &r.name, code: r.code, source_ip: &r.source_ip,
                    user_agent: &r.user_agent, request_uri: &r.request_uri, request_object: body.as_deref(),
                };
                let prev: String = tx.query_row("SELECT head_hash FROM chain WHERE cluster=?1", params![cl], |x| x.get(0)).optional()?.unwrap_or_default();
                let h = link(&prev, &fields);
                tx.execute("UPDATE events SET hash=?1, received_ms=?3 WHERE id=?2", params![h, id, Utc::now().timestamp_millis()])?;
                tx.execute(
                    "INSERT INTO chain (cluster, head_id, head_hash) VALUES (?1,?2,?3) ON CONFLICT(cluster) DO UPDATE SET head_id=excluded.head_id, head_hash=excluded.head_hash",
                    params![cl, id, h],
                )?;
                let class = crate::identity::resolve(&tx, &self.cfg.identities, &r.actor, &r.user_id, r.human, &r.user_agent, ms(&r.ts))?;
                let alerts = &judge(r, crate::identity::is_person(class), cl, &self.cfg, &*tx)?;
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

    pub fn insert_event(&self, cluster: &str, r: &Record) -> anyhow::Result<bool> {
        Ok(self.insert_events(cluster, std::slice::from_ref(r))? == 1)
    }

    /// True if this resource type was listed to the end before, or any version of it was ever
    /// stored, i.e. a list is a re-list. A type with no objects stores no versions, so the
    /// finished list is remembered in `listed`; otherwise an object created while the
    /// collector was down would be taken as part of a first look and never shown.
    pub fn has_any(&self, cluster: &str, group: &str, resource: &str) -> anyhow::Result<bool> {
        let c = self.conn.lock().unwrap();
        Ok(c.query_row(
            "SELECT 1 FROM versions WHERE cluster=?1 AND api_group=?2 AND resource=?3
             UNION ALL SELECT 1 FROM listed WHERE cluster=?1 AND api_group=?2 AND resource=?3 LIMIT 1",
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
                // Latest by time (then ID): delete versions can carry a different clock than
                // the change before them, so ID order alone can be wrong for a past instant.
                "SELECT namespace, name, uid, body FROM (
                   SELECT namespace, name, uid, body, change,
                          row_number() OVER (PARTITION BY namespace, name ORDER BY observed_ms DESC, id DESC) AS rn
                   FROM versions WHERE cluster=?1 AND api_group='management.cattle.io' AND resource=?2 AND observed_ms <= ?3)
                 WHERE rn = 1 AND change <> 'deleted'",
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
        // Existed before tracking began, or its creation fell out of retention: the first
        // version left is a later change, whose author is not the creator.
        if !matches!(change.as_str(), "added" | "added_unseen") {
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
                "SELECT name, body, observed_ms FROM (
                   SELECT name, body, observed_ms, change,
                          row_number() OVER (PARTITION BY name ORDER BY observed_ms DESC, id DESC) AS rn
                   FROM versions WHERE cluster=?1 AND api_group='management.cattle.io' AND resource=?2 AND observed_ms <= ?3)
                 WHERE rn = 1 AND change <> 'deleted'",
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

    /// Walk each cluster's chain from its base and report the first record that does not fit.
    pub fn verify_chain(&self, cluster: Option<&str>) -> anyhow::Result<Vec<ChainReport>> {
        let c = self.conn.lock().unwrap();
        let clusters: Vec<String> = c
            .prepare("SELECT cluster FROM chain WHERE (?1 IS NULL OR cluster=?1) ORDER BY cluster")?
            .query_map(params![cluster], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        // Per cluster: base id, previous hash, verified, first bad id, head (id, hash).
        struct Walk {
            base_id: i64,
            prev: String,
            verified: i64,
            first_bad: Option<i64>,
            head: (i64, String),
        }
        let mut walks: std::collections::BTreeMap<String, Walk> = std::collections::BTreeMap::new();
        for cl in clusters {
            let (base_id, prev): (i64, String) =
                c.query_row("SELECT base_id, base_hash FROM chain_base WHERE cluster=?1", params![cl], |r| Ok((r.get(0)?, r.get(1)?))).optional()?.unwrap_or((0, String::new()));
            walks.insert(cl, Walk { base_id, head: (base_id, prev.clone()), prev, verified: 0, first_bad: None });
        }
        let from = walks.values().map(|w| w.base_id).min().unwrap_or(0);
        let mut st = c.prepare_cached(CHAIN_SQL)?;
        let mut rows = st.query(params![from])?;
        let mut open = walks.len();
        while open > 0 {
            let Some(r) = rows.next()? else { break };
            let cl: String = r.get(22)?;
            let id: i64 = r.get(0)?;
            let Some(w) = walks.get_mut(&cl) else { continue };
            if w.first_bad.is_some() || id <= w.base_id {
                continue;
            }
            let g = |i: usize| -> rusqlite::Result<String> { r.get(i) };
            let (ro, stored): (Option<String>, String) = (r.get(20)?, r.get(21)?);
            let (aid, src, ts, actor, pr, uid, via, caller, verb, grp, res, sub, ns, name, sip, ua, uri) =
                (g(1)?, g(2)?, g(3)?, g(4)?, g(5)?, g(6)?, g(8)?, g(9)?, g(10)?, g(11)?, g(12)?, g(13)?, g(14)?, g(15)?, g(17)?, g(18)?, g(19)?);
            let fields = Fields {
                cluster: &cl, audit_id: &aid, source: &src, ts: &ts, actor: &actor, principal: &pr, user_id: &uid,
                human: r.get::<_, i64>(7)? == 1, via: &via, caller: &caller, verb: &verb, api_group: &grp, resource: &res,
                subresource: &sub, namespace: &ns, name: &name, code: r.get(16)?, source_ip: &sip, user_agent: &ua,
                request_uri: &uri, request_object: ro.as_deref(),
            };
            if link(&w.prev, &fields) != stored {
                w.first_bad = Some(id);
                open -= 1;
                continue;
            }
            w.prev = stored;
            w.head = (id, w.prev.clone());
            w.verified += 1;
        }
        Ok(walks
            .into_iter()
            .map(|(cl, w)| ChainReport { cluster: cl, base_id: w.base_id, verified: w.verified, first_bad_id: w.first_bad, head_id: w.head.0, head_hash: w.head.1 })
            .collect())
    }

    pub fn forward_cursor(&self, target: &str) -> anyhow::Result<(i64, i64)> {
        let c = self.conn.lock().unwrap();
        c.execute("INSERT OR IGNORE INTO forward_cursor (target) VALUES (?1)", params![target])?;
        Ok(c.query_row("SELECT event_id, anchor_id FROM forward_cursor WHERE target=?1", params![target], |r| Ok((r.get(0)?, r.get(1)?)))?)
    }

    /// Move a target's cursor after a successful send, or record why it failed.
    pub fn forward_done(&self, target: &str, event_id: Option<i64>, anchor_id: Option<i64>, error: Option<String>) -> anyhow::Result<()> {
        let c = self.conn.lock().unwrap();
        match error {
            Some(e) => c.execute("UPDATE forward_cursor SET last_error=?2 WHERE target=?1", params![target, e])?,
            None => c.execute(
                "UPDATE forward_cursor SET event_id=coalesce(?2, event_id), anchor_id=coalesce(?3, anchor_id), last_ok_ms=?4, last_error=NULL WHERE target=?1",
                params![target, event_id, anchor_id, Utc::now().timestamp_millis()],
            )?,
        };
        Ok(())
    }

    /// Per target: last success, last error, records still to send.
    pub fn forward_status(&self) -> anyhow::Result<Vec<(String, Option<i64>, Option<String>, i64)>> {
        let c = self.conn.lock().unwrap();
        let rows = c
            .prepare(&format!(
                "SELECT target, last_ok_ms, last_error,
                   (SELECT count(*) FROM events e WHERE e.id > f.event_id AND NOT (e.source='rancher' AND e.link_id > f.event_id)) FROM forward_cursor f ORDER BY target"
            ))?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Records to forward: after the cursor, received at least the hold-back ago. The Rancher
    /// half of a paired action is left out while its downstream record is still to be sent
    /// (that one carries the Rancher fields); if the downstream record went out before the
    /// pair was made, the Rancher half goes out on its own.
    pub fn forward_records(&self, after_id: i64, received_before_ms: i64, limit: i64) -> anyhow::Result<Vec<ForwardRecord>> {
        let c = self.conn.lock().unwrap();
        // Stop at the first record still inside the hold-back so the cursor never skips one.
        let upto: i64 = c.query_row(
            "SELECT coalesce((SELECT min(id) - 1 FROM events WHERE id > ?1 AND received_ms > ?2), (SELECT coalesce(max(id), 0) FROM events))",
            params![after_id, received_before_ms],
            |r| r.get(0),
        )?;
        let mut st = c.prepare(&format!(
            "SELECT e.id, e.cluster, e.ts, e.source, e.audit_id, e.actor, e.user_id, e.principal, e.human, e.via, e.caller, e.verb,
                    e.api_group, e.resource, e.subresource, e.namespace, e.name, e.code, e.source_ip, e.user_agent, e.hash, e.story_id,
                    (SELECT group_concat(rule) FROM alerts a WHERE a.event_id=e.id),
                    (SELECT count(*) FROM alerts a WHERE a.event_id=e.id AND a.severity IN ('high','medium')),
                    r.user_agent, r.source_ip
             FROM events e LEFT JOIN events r ON r.id=e.link_id AND e.source='k8s'
             WHERE e.id > ?1 AND e.id <= ?2 AND NOT (e.source='rancher' AND e.link_id > ?1) ORDER BY e.id LIMIT ?3"
        ))?;
        let rows = st
            .query_map(params![after_id, upto, limit], |r| {
                let rules: Option<String> = r.get(22)?;
                let rua: Option<String> = r.get(24)?;
                Ok(ForwardRecord {
                    id: r.get(0)?, cluster: r.get(1)?, ts: r.get(2)?, source: r.get(3)?, audit_id: r.get(4)?, actor: r.get(5)?,
                    user_id: r.get(6)?, principal: r.get(7)?, human: r.get::<_, i64>(8)? == 1, via: r.get(9)?, caller: r.get(10)?,
                    verb: r.get(11)?, api_group: r.get(12)?, resource: r.get(13)?, subresource: r.get(14)?, namespace: r.get(15)?,
                    name: r.get(16)?, code: r.get(17)?, source_ip: r.get(18)?, user_agent: r.get(19)?, hash: r.get(20)?, story_id: r.get(21)?,
                    rules: rules.map(|s| s.split(',').map(String::from).collect()).unwrap_or_default(),
                    risky: r.get::<_, i64>(23)? > 0,
                    rancher_user_agent: rua,
                    rancher_source_ip: r.get(25)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Record the current head of every chain that moved since its last anchor.
    pub fn take_anchors(&self, now: DateTime<Utc>) -> anyhow::Result<Vec<Anchor>> {
        let c = self.conn.lock().unwrap();
        let heads: Vec<(String, i64, String)> = c
            .prepare(
                "SELECT ch.cluster, ch.head_id, ch.head_hash FROM chain ch
                 WHERE ch.head_id > coalesce((SELECT max(event_id) FROM anchors a WHERE a.cluster = ch.cluster), 0)",
            )?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        let mut out = Vec::new();
        for (cluster, event_id, hash) in heads {
            c.execute("INSERT INTO anchors (cluster, event_id, hash, at_ms) VALUES (?1,?2,?3,?4)", params![cluster, event_id, hash, ms(&now)])?;
            out.push(Anchor { id: c.last_insert_rowid(), cluster, event_id, hash, at_ms: ms(&now) });
        }
        Ok(out)
    }

    pub fn anchors(&self, after_id: i64, limit: i64) -> anyhow::Result<Vec<Anchor>> {
        let c = self.conn.lock().unwrap();
        let rows = c
            .prepare("SELECT id, cluster, event_id, hash, at_ms FROM anchors WHERE id > ?1 ORDER BY id LIMIT ?2")?
            .query_map(params![after_id, limit], |r| Ok(Anchor { id: r.get(0)?, cluster: r.get(1)?, event_id: r.get(2)?, hash: r.get(3)?, at_ms: r.get(4)? }))?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Check an anchor kept elsewhere: the chain must be intact up to that record and the
    /// record must still carry that hash.
    pub fn check_anchor(&self, cluster: &str, event_id: i64, hash: &str) -> anyhow::Result<&'static str> {
        let report = self.verify_chain(Some(cluster))?;
        let Some(r) = report.first() else { return Ok("unknown_cluster") };
        if r.first_bad_id.is_some_and(|b| b <= event_id) {
            return Ok("chain_broken");
        }
        if event_id <= r.base_id {
            return Ok("pruned");
        }
        let stored: Option<String> = self.conn.lock().unwrap().query_row("SELECT hash FROM events WHERE id=?1 AND cluster=?2", params![event_id, cluster], |x| x.get(0)).optional()?.flatten();
        Ok(match stored {
            Some(h) if h == hash => "match",
            Some(_) => "mismatch",
            None => "missing",
        })
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
                    let c = self.conn.lock().unwrap();
                    c.execute("DELETE FROM listing WHERE cluster=?1 AND api_group=?2 AND resource=?3", params![cluster, group, resource])?;
                    c.execute("INSERT OR IGNORE INTO listed (cluster, api_group, resource) VALUES (?1,?2,?3)", params![cluster, group, resource])?;
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
                } else if same_content(&prev, &body) {
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
            // What the user directory says about this user can change who counts as a person.
            crate::identity::reclassify(&c, &self.cfg.identities, Some(&key.name))?;
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
            // An object with finalizers goes in two steps: the delete request sets the deletion
            // timestamp ("deleting", linked to that request), and the object is gone once the
            // finalizers are done ("deleted"). Both come from the same request, so the second
            // takes the first one's event instead of looking for another delete.
            if change == "deleted" {
                let prev: Option<(String, Option<i64>)> = c
                    .query_row(
                        "SELECT change, event_id FROM versions WHERE cluster=?1 AND api_group=?2 AND resource=?3 AND namespace=?4 AND name=?5 AND id < ?6 ORDER BY id DESC LIMIT 1",
                        params![cluster, group, resource, ns, name, vid],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                if let Some(("deleting", Some(eid))) = prev.as_ref().map(|(ch, e)| (ch.as_str(), *e)) {
                    c.execute("UPDATE versions SET event_id=?1 WHERE id=?2", params![eid, vid])?;
                    linked += 1;
                    continue;
                }
            }
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
            // keep their order. A version found after a gap may fold several people's edits
            // into one diff: it is linked only when exactly one event could have made it
            // since the previous version; otherwise it stays unattributed.
            let unseen = change.ends_with("_unseen");
            let mut from = obs - before_ms;
            if unseen {
                let prev: Option<i64> = c
                    .query_row(
                        "SELECT observed_ms FROM versions WHERE cluster=?1 AND api_group=?2 AND resource=?3 AND namespace=?4 AND name=?5 AND id < ?6 ORDER BY id DESC LIMIT 1",
                        params![cluster, group, resource, ns, name, vid],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(p) = prev {
                    from = from.max(p + 1);
                }
            }
            let sql = format!(
                "SELECT e.id FROM events e WHERE e.cluster=?1 AND e.api_group=?2 AND e.resource=?3 AND e.namespace=?4
                 AND (e.name=?5 OR (e.name='' AND e.verb IN ('create','deletecollection')))
                 AND e.verb IN {verbs} AND e.subresource IN ('','scale') AND e.code BETWEEN 200 AND 299
                 AND e.ts_ms BETWEEN ?6 AND ?7 AND {SHOWN}
                 AND (e.verb = 'deletecollection' OR NOT EXISTS (SELECT 1 FROM versions v WHERE v.event_id = e.id))
                 ORDER BY e.ts_ms {order}, e.id {order} LIMIT 2"
            );
            let hits: Vec<i64> = c
                .prepare(&sql)?
                .query_map(params![cluster, group, resource, ns, name, from, obs + 5_000], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            let hit = if unseen && hits.len() > 1 { None } else { hits.first().copied() };
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

    /// For a stored version: the object's previous version (what to restore), and who made
    /// the change. None when the version does not exist.
    pub fn restore_source(&self, vid: i64) -> anyhow::Result<Option<RestoreSource>> {
        let c = self.conn.lock().unwrap();
        let row: Option<(String, String, String, String, String, String, String, String, Option<i64>)> = c
            .query_row(
                "SELECT cluster, api_group, resource, kind, namespace, name, change, observed, event_id FROM versions WHERE id=?1",
                params![vid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?)),
            )
            .optional()?;
        let Some((cluster, group, resource, kind, ns, name, change, observed, eid)) = row else { return Ok(None) };
        let before: Option<String> = c
            .query_row(
                "SELECT body FROM versions WHERE cluster=?1 AND api_group=?2 AND resource=?3 AND namespace=?4 AND name=?5 AND id < ?6 ORDER BY id DESC LIMIT 1",
                params![cluster, group, resource, ns, name, vid],
                |r| r.get(0),
            )
            .optional()?;
        // A deletion stores the last state seen; restoring "before the delete" means that.
        let before = match (change.as_str(), before) {
            ("deleted", None) => c.query_row("SELECT body FROM versions WHERE id=?1", params![vid], |r| r.get(0)).optional()?,
            (_, b) => b,
        };
        let by: Option<String> = match eid {
            Some(e) => c.query_row("SELECT actor FROM events WHERE id=?1", params![e], |r| r.get(0)).optional()?,
            None => None,
        };
        Ok(Some(RestoreSource {
            cluster, kind, namespace: ns, name, change, observed, by,
            before: before.and_then(|b| serde_json::from_str(&b).ok()),
        }))
    }

    /// A collector checked in. A silence longer than three heartbeats is recorded as a gap.
    pub fn heartbeat(&self, cluster: &str, collector: &str, instance: &str, details: &Value, now_ms: i64) -> anyhow::Result<()> {
        let c = self.conn.lock().unwrap();
        let prev: Option<i64> = c
            .query_row("SELECT last_ms FROM heartbeats WHERE cluster=?1 AND collector=?2 AND instance=?3", params![cluster, collector, instance], |r| r.get(0))
            .optional()?;
        if let Some(p) = prev.filter(|p| now_ms - p > GAP_MS) {
            c.execute("INSERT INTO gaps (cluster, collector, instance, from_ms, to_ms) VALUES (?1,?2,?3,?4,?5)", params![cluster, collector, instance, p, now_ms])?;
        }
        c.execute(
            "INSERT INTO heartbeats (cluster, collector, instance, last_ms, details, first_ms) VALUES (?1,?2,?3,?4,?5,?4)
             ON CONFLICT(cluster, collector, instance) DO UPDATE SET last_ms=excluded.last_ms, details=excluded.details",
            params![cluster, collector, instance, now_ms, details.to_string()],
        )?;
        // watch, rancher and neuvector run as one pod per cluster: a new pod name replaces the
        // old one, so a silent old instance ends its gap here instead of staying "still silent"
        // forever. The gap ends where the earliest instance still reporting after it began (rows
        // from before 0.2.1 have no first_ms: now). A rollout hands over within GAP_MS: no gap.
        // collect-file runs one per node, where another node checking in proves nothing.
        if collector != "file" {
            let stale: Vec<(String, i64)> = c
                .prepare("SELECT instance, last_ms FROM heartbeats WHERE cluster=?1 AND collector=?2 AND instance<>?3 AND ?4 - last_ms > ?5")?
                .query_map(params![cluster, collector, instance, now_ms, GAP_MS], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?;
            for (old, last) in stale {
                let next: i64 = c.query_row(
                    "SELECT MIN(COALESCE(first_ms, ?4)) FROM heartbeats WHERE cluster=?1 AND collector=?2 AND instance<>?3 AND last_ms > ?5",
                    params![cluster, collector, old, now_ms, last],
                    |r| r.get(0),
                )?;
                if next - last > GAP_MS {
                    c.execute("INSERT INTO gaps (cluster, collector, instance, from_ms, to_ms) VALUES (?1,?2,?3,?4,?5)", params![cluster, collector, old, last, next])?;
                }
                c.execute("DELETE FROM heartbeats WHERE cluster=?1 AND collector=?2 AND instance=?3", params![cluster, collector, old])?;
            }
        }
        Ok(())
    }

    /// A period a collector says it did not collect (it restarted, or a file went away unread),
    /// or where its collection starts ("collection-start"). A report sent again is ignored.
    pub fn record_gap(&self, cluster: &str, g: &crate::collect::Gap) -> anyhow::Result<bool> {
        let c = self.conn.lock().unwrap();
        let n = c.execute(
            "INSERT OR IGNORE INTO gaps (cluster, collector, instance, from_ms, to_ms, reason, detail) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![cluster, g.collector, g.instance, g.from_ms, g.to_ms, g.reason, g.detail],
        )?;
        Ok(n == 1)
    }

    /// Whether the server has heard from this collector instance before (a check-in, or a
    /// period recorded for it), and when it last checked in. A collector without its state
    /// file asks this to tell a first install from a lost state directory.
    pub fn known_instance(&self, cluster: &str, collector: &str, instance: &str) -> anyhow::Result<Option<i64>> {
        let c = self.conn.lock().unwrap();
        let last: Option<i64> = c.query_row(
            "SELECT MAX(t) FROM (SELECT last_ms AS t FROM heartbeats WHERE cluster=?1 AND collector=?2 AND instance=?3
                                 UNION ALL SELECT to_ms FROM gaps WHERE cluster=?1 AND collector=?2 AND instance=?3)",
            params![cluster, collector, instance],
            |r| r.get(0),
        )?;
        Ok(last)
    }

    /// Lines set aside from a batch: only their length, hash, the parse error and where they
    /// were read are kept, never the text (it may hold a request body). `at` is (node, file,
    /// offset of the batch); a line already recorded at the same place is not recorded again.
    pub fn record_rejected(&self, cluster: &str, source: &str, now_ms: i64, at: Option<(&str, &str, i64)>, lines: &[crate::audit::SetAside]) -> anyhow::Result<()> {
        use sha2::{Digest, Sha256};
        let c = self.conn.lock().unwrap();
        for (line, error, pos) in lines {
            let hash: String = Sha256::digest(line).iter().map(|b| format!("{b:02x}")).collect();
            let error: String = error.chars().take(200).collect();
            let (instance, file, offset) = match at {
                Some((i, f, o)) => (i, f, Some(o + *pos as i64)),
                None => ("", "", None),
            };
            c.execute(
                "INSERT OR IGNORE INTO rejected_lines (cluster, source, received_ms, len, sha256, error, instance, file, offset) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![cluster, source, now_ms, line.len() as i64, hash, error, instance, file, offset],
            )?;
        }
        Ok(())
    }

    /// The lines set aside in a range, newest first (no line text).
    pub fn rejected(&self, cluster: Option<&str>, from_ms: i64, to_ms: i64, limit: i64) -> anyhow::Result<Vec<Value>> {
        let c = self.conn.lock().unwrap();
        let rows = c
            .prepare(
                "SELECT cluster, source, instance, file, offset, received_ms, len, sha256, error FROM rejected_lines
                 WHERE received_ms BETWEEN ?1 AND ?2 AND (?3 IS NULL OR cluster = ?3) ORDER BY received_ms DESC, id DESC LIMIT ?4",
            )?
            .query_map(params![from_ms, to_ms, cluster, limit], |r| {
                Ok(json!({"cluster": r.get::<_, String>(0)?, "source": r.get::<_, String>(1)?, "instance": r.get::<_, String>(2)?,
                          "file": r.get::<_, String>(3)?, "offset": r.get::<_, Option<i64>>(4)?, "receivedMs": r.get::<_, i64>(5)?,
                          "len": r.get::<_, i64>(6)?, "sha256": r.get::<_, String>(7)?, "reason": r.get::<_, String>(8)?}))
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Per cluster: each collector's last check-in and report, and gaps in the range
    /// (including a silence still going on).
    pub fn coverage(&self, from_ms: i64, to_ms: i64, now_ms: i64) -> anyhow::Result<Vec<CoverageRow>> {
        let c = self.conn.lock().unwrap();
        let beats: Vec<(String, String, String, i64, String)> = c
            .prepare("SELECT cluster, collector, instance, last_ms, details FROM heartbeats ORDER BY cluster, collector, instance")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
            .collect::<Result<_, _>>()?;
        type GapRow = (String, String, String, i64, i64, String, String);
        let gaps: Vec<GapRow> = c
            .prepare("SELECT cluster, collector, instance, from_ms, to_ms, reason, detail FROM gaps WHERE to_ms >= ?1 AND from_ms <= ?2 ORDER BY from_ms")?
            .query_map(params![from_ms, to_ms], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))?
            .collect::<Result<_, _>>()?;
        let rejected: Vec<(String, String, i64, i64, String, String)> = c
            .prepare("SELECT cluster, source, received_ms, len, sha256, error FROM rejected_lines WHERE received_ms BETWEEN ?1 AND ?2 ORDER BY received_ms")?
            .query_map(params![from_ms, to_ms], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
            .collect::<Result<_, _>>()?;
        let mut out: Vec<CoverageRow> = Vec::new();
        let row = |out: &mut Vec<CoverageRow>, cl: &str| -> usize {
            match out.iter().position(|r| r.cluster == cl) {
                Some(i) => i,
                None => {
                    out.push(CoverageRow { cluster: cl.to_string(), collectors: vec![], gaps: vec![], rejected: vec![], rejected_counts: vec![], starts: vec![] });
                    out.len() - 1
                }
            }
        };
        for (cl, col, inst, last, details) in beats {
            let i = row(&mut out, &cl);
            let stale = now_ms - last > GAP_MS;
            let retired = col == "file" && now_ms - last > RETIRED_MS;
            out[i].collectors.push(json!({"collector": col, "instance": inst, "lastMs": last, "stale": stale, "retired": retired,
                                          "details": serde_json::from_str::<Value>(&details).unwrap_or(Value::Null)}));
            if stale && last <= to_ms {
                out[i].gaps.push(json!({"kind": "heartbeat", "collector": col, "instance": inst, "fromMs": last, "toMs": null, "reason": SILENT, "detail": ""}));
            }
        }
        for (cl, col, inst, f, t, reason, detail) in gaps {
            let i = row(&mut out, &cl);
            // Both kinds are kept side by side: a silence the server noticed and what the
            // collector reported about the same time are separate facts.
            let v = |kind: &str, reason: &str| json!({"kind": kind, "collector": col, "instance": inst, "fromMs": f, "toMs": t, "reason": reason, "detail": detail});
            if reason == START {
                out[i].starts.push(v("start", &reason));
            } else if reason.is_empty() {
                out[i].gaps.push(v("heartbeat", SILENT));
            } else {
                out[i].gaps.push(v("reported", &reason));
            }
        }
        for (cl, source, at, len, sha, error) in rejected {
            let i = row(&mut out, &cl);
            out[i].rejected.push(json!({"source": source, "receivedMs": at, "len": len, "sha256": sha, "error": error}));
        }
        let counts: Vec<(String, String, String, i64)> = c
            .prepare("SELECT cluster, source, instance, COUNT(*) FROM rejected_lines WHERE received_ms BETWEEN ?1 AND ?2 GROUP BY cluster, source, instance")?
            .query_map(params![from_ms, to_ms], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<Result<_, _>>()?;
        for (cl, source, inst, n) in counts {
            let i = row(&mut out, &cl);
            out[i].rejected_counts.push(json!({"source": source, "instance": inst, "count": n}));
        }
        Ok(out)
    }

    pub fn insert_security(&self, cluster: &str, items: &[crate::neuvector::Incident]) -> anyhow::Result<usize> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        let mut n = 0;
        for i in items {
            n += tx.execute(
                "INSERT OR IGNORE INTO security_events (cluster, nv_id, ts_ms, name, level, namespace, workload, proc_name, proc_cmd, proc_parent, proc_user, action, message)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                params![cluster, i.id, i.reported_timestamp * 1000, i.name, i.level, i.workload_domain, i.workload_name, i.proc_name, i.proc_cmd,
                        i.proc_parent_name, i.proc_effective_user, i.action, i.msg],
            )?;
        }
        tx.commit()?;
        Ok(n)
    }

    /// Processes SUSE Security flagged in a pod's containers in a time window (around an exec).
    /// Its workload name is the container's name as the runtime reports it, which carries
    /// the pod name; matched by prefix.
    pub fn security_for_pod(&self, cluster: &str, ns: &str, pod: &str, from_ms: i64, to_ms: i64) -> anyhow::Result<Vec<Value>> {
        let c = self.conn.lock().unwrap();
        let pat = format!("%{}%", pod.replace('%', "").replace('_', "\\_"));
        let rows = c
            .prepare(
                "SELECT ts_ms, name, level, workload, proc_name, proc_cmd, proc_parent, proc_user, action, message FROM security_events
                 WHERE cluster=?1 AND namespace=?2 AND workload LIKE ?3 ESCAPE '\\' AND ts_ms BETWEEN ?4 AND ?5 ORDER BY ts_ms LIMIT 200",
            )?
            .query_map(params![cluster, ns, pat, from_ms, to_ms], |r| {
                let parent: String = r.get(6)?;
                Ok(json!({"tsMs": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?, "level": r.get::<_, String>(2)?, "workload": r.get::<_, String>(3)?,
                          "process": r.get::<_, String>(4)?, "command": r.get::<_, String>(5)?, "parent": parent.clone(),
                          "startedByRuntime": crate::neuvector::started_by_runtime(&parent),
                          "user": r.get::<_, String>(7)?, "action": r.get::<_, String>(8)?, "message": r.get::<_, String>(9)?}))
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Labels of a namespace as last seen.
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
                    (SELECT group_concat(rule || ':' || severity) FROM alerts a WHERE a.event_id=e.id),
                    (SELECT v.id FROM versions v WHERE v.event_id=e.id ORDER BY v.id DESC LIMIT 1),
                    e.source,e.user_id,r.ts,r.user_agent,r.source_ip,r.request_uri,e.audit_id
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
                    id: r.get(0)?, cluster: r.get(1)?, audit_id: r.get(27)?, source: r.get(21)?, user_id: r.get(22)?, rancher,
                    ts: r.get(2)?, actor: r.get(3)?, principal: r.get(4)?,
                    human: r.get::<_, i64>(5)? == 1, via: r.get(6)?, caller: r.get(7)?, verb: r.get(8)?,
                    api_group: r.get(9)?, resource: r.get(10)?, subresource: r.get(11)?, namespace: r.get(12)?,
                    name: r.get(13)?, code: r.get(14)?, source_ip: r.get(15)?, user_agent: r.get(16)?, request_uri: r.get(17)?,
                    request_object: ro.and_then(|s| serde_json::from_str(&s).ok()),
                    alerts: rules_of(&al),
                    alert_severity: severities_of(&al),
                    version_id: r.get(20)?,
                    changes: None,
                    change: None,
                    body: None,
                })
            })?
            .collect::<Result<_, _>>()?;
        drop(stmt);
        for row in rows.iter_mut() {
            if let Some(vid) = row.version_id
                && let Some((changes, change, body)) = Self::version_detail(&c, vid)? {
                    row.changes = Some(changes);
                    row.change = Some(change);
                    row.body = body;
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
        // Events go oldest-first by ID, up to the first one still inside the retention, so each
        // cluster's chain stays one unbroken run; the last removed link becomes its new base.
        // Records that arrived late (old time, new ID) wait until everything before them goes.
        // The newest record always stays: without AUTOINCREMENT, SQLite gives the next record
        // max(id) + 1, so deleting the newest would let IDs repeat (cursors and the chain rely
        // on IDs only growing).
        let last: Option<i64> = c.query_row(
            "SELECT min(CASE WHEN EXISTS (SELECT 1 FROM events WHERE ts_ms >= ?1) THEN (SELECT min(id) - 1 FROM events WHERE ts_ms >= ?1) ELSE (SELECT max(id) FROM events) END,
                       (SELECT max(id) - 1 FROM events))",
            params![cutoff],
            |r| r.get(0),
        )?;
        let mut n = 0;
        if let Some(last) = last.filter(|l| *l > 0) {
            c.execute(
                "INSERT INTO chain_base (cluster, base_id, base_hash)
                   SELECT e.cluster, e.id, e.hash FROM events e
                   WHERE e.hash IS NOT NULL AND e.id = (SELECT max(id) FROM events x WHERE x.cluster = e.cluster AND x.id <= ?1 AND x.hash IS NOT NULL)
                 ON CONFLICT(cluster) DO UPDATE SET base_id=excluded.base_id, base_hash=excluded.base_hash
                   WHERE excluded.base_id > chain_base.base_id",
                params![last],
            )?;
            // Keep events that kept versions still point at, so their author is not lost.
            n = c.execute(
                "DELETE FROM events WHERE id <= ?1 AND id NOT IN (SELECT event_id FROM versions WHERE event_id IS NOT NULL)",
                params![last],
            )?;
        }
        c.execute("DELETE FROM anchors WHERE at_ms < ?1 AND id < (SELECT max(id) FROM anchors)", params![cutoff])?;
        c.execute("DELETE FROM gaps WHERE to_ms < ?1", params![cutoff])?;
        c.execute("DELETE FROM security_events WHERE ts_ms < ?1", params![cutoff])?;
        c.execute("DELETE FROM hourly WHERE hour_ms < ?1", params![cutoff])?;
        c.execute("DELETE FROM stories WHERE last_ms < ?1", params![cutoff])?;
        c.execute("DELETE FROM rejected_lines WHERE received_ms < ?1", params![cutoff])?;
        // A node's collector silent since before retention: the node is long gone (RETIRED_MS).
        c.execute("DELETE FROM heartbeats WHERE collector='file' AND last_ms < ?1", params![cutoff])?;
        Ok(n + m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::{parse_body, to_record};
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
    fn the_chain_check_reads_in_order_without_sorting() {
        // A temporary sort here held a whole cluster in memory (or needed a writable /tmp).
        let s = Store::open(":memory:").unwrap();
        let c = s.conn.lock().unwrap();
        let plan: Vec<String> = c
            .prepare(&format!("EXPLAIN QUERY PLAN {CHAIN_SQL}"))
            .unwrap()
            .query_map(params![0], |r| r.get(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(plan.iter().all(|l| !l.contains("TEMP B-TREE")), "{plan:?}");
        assert!(plan.iter().all(|l| !l.contains("INDEX")), "read by id, not through an index: {plan:?}");
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
        assert!(s.insert_event("c1", &r).unwrap());
        assert!(!s.insert_event("c1", &r).unwrap(), "duplicates are ignored");
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
        s.insert_event("c1", &patch("a", "alice", "2026-10-14T02:00:01Z")).unwrap();
        s.insert_event("c1", &patch("b", "bob", "2026-10-14T02:00:02Z")).unwrap();
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
        s.insert_event("c1", &patch("g", "alice", "2026-10-14T02:05:00Z")).unwrap();
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

    #[test]
    fn gap_with_two_editors_stays_unattributed() {
        // Lab 2026-10-17: one edit by a node admin and one by a Rancher user while the watch
        // was down came back as one diff and was credited to the last one.
        let s = Store::open(":memory:").unwrap();
        let t = Utc.with_ymd_and_hms(2026, 10, 14, 2, 0, 0).unwrap();
        s.insert_version(&key(), "Deployment", "u1", &obj(1), "baseline", t).unwrap();
        s.insert_event("c1", &patch("g1", "bob", "2026-10-14T02:03:00Z")).unwrap();
        s.insert_event("c1", &patch("g2", "alice", "2026-10-14T02:05:00Z")).unwrap();
        s.insert_version(&key(), "Deployment", "u1", &obj(2), "resync", t + chrono::Duration::minutes(10)).unwrap();
        assert_eq!(s.link_since(0).unwrap(), 0);
        let h = s.history(&key()).unwrap();
        assert_eq!((h[0].change.as_str(), h[0].actor.as_deref()), ("modified_unseen", None));
    }

    #[test]
    fn list_items_without_types_are_not_a_change() {
        let s = Store::open(":memory:").unwrap();
        let t = Utc.with_ymd_and_hms(2026, 10, 14, 2, 0, 0).unwrap();
        s.insert_version(&key(), "Deployment", "u1", &obj(1), "baseline", t).unwrap();
        let mut typed = obj(1);
        typed["apiVersion"] = json!("apps/v1");
        typed["kind"] = json!("Deployment");
        assert_eq!(s.insert_version(&key(), "Deployment", "u1", &typed, "modified", t).unwrap(), None);
    }

    #[test]
    fn replaced_watch_pod_ends_the_gap() {
        let s = Store::open(":memory:").unwrap();
        let t0 = 1_000_000_000_000;
        s.heartbeat("c1", "watch", "pod-a", &json!({}), t0).unwrap();
        s.heartbeat("c1", "file", "node-1", &json!({}), t0).unwrap();
        s.heartbeat("c1", "file", "node-2", &json!({}), t0 + 600_000).unwrap();
        s.heartbeat("c1", "watch", "pod-b", &json!({}), t0 + 600_000).unwrap();
        let cov = s.coverage(0, i64::MAX, t0 + 620_000).unwrap();
        let gaps: Vec<(String, String, Option<i64>)> = cov[0].gaps.iter()
            .map(|g| (g["collector"].as_str().unwrap().into(), g["instance"].as_str().unwrap().into(), g["toMs"].as_i64())).collect();
        assert!(gaps.contains(&("watch".into(), "pod-a".into(), Some(t0 + 600_000))), "{gaps:?}");
        // One file collector per node: node-1 is still silent.
        assert!(gaps.contains(&("file".into(), "node-1".into(), None)), "{gaps:?}");
        assert_eq!(cov[0].collectors.iter().filter(|c| c["collector"] == "watch").count(), 1);
    }

    #[test]
    fn rollout_handover_is_not_a_gap() {
        let s = Store::open(":memory:").unwrap();
        let t0 = 1_000_000_000_000;
        // rolling update: new pod checks in 70 s after the old one's last report, both every 60 s
        s.heartbeat("c1", "watch", "pod-a", &json!({}), t0).unwrap();
        s.heartbeat("c1", "rancher", "r-a", &json!({}), t0).unwrap();
        s.heartbeat("c1", "rancher", "r-b", &json!({}), t0 - 30_000).unwrap(); // overlap: started before r-a stopped
        for i in 0..6 {
            s.heartbeat("c1", "watch", "pod-b", &json!({}), t0 + 70_000 + i * 60_000).unwrap();
            s.heartbeat("c1", "rancher", "r-b", &json!({}), t0 + 30_000 + i * 60_000).unwrap();
        }
        let cov = s.coverage(0, i64::MAX, t0 + 400_000).unwrap();
        assert!(cov[0].gaps.is_empty(), "{:?}", cov[0].gaps);
        assert_eq!(cov[0].collectors.len(), 2);

        // pod-b stops; pod-c comes 10 minutes later: the gap ends when pod-c began, not when noticed
        let t1 = t0 + 70_000 + 5 * 60_000;
        for i in 0..5 {
            s.heartbeat("c1", "watch", "pod-c", &json!({}), t1 + 600_000 + i * 60_000).unwrap();
        }
        let cov = s.coverage(0, i64::MAX, t1 + 900_000).unwrap();
        let gaps: Vec<(i64, Option<i64>)> = cov[0].gaps.iter().filter(|g| g["collector"] == "watch").map(|g| (g["fromMs"].as_i64().unwrap(), g["toMs"].as_i64())).collect();
        assert_eq!(gaps, vec![(t1, Some(t1 + 600_000))]);
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
        s.insert_event("c1", &patch("k1", "alice", "2026-10-14T02:00:00.700Z")).unwrap();
        s.insert_event("local", &rancher_put("r1", "2026-10-14T10:00:01+08:00")).unwrap();
        let rows = all(&s);
        assert_eq!(rows.len(), 1, "the Rancher half is hidden");
        assert_eq!((rows[0].source.as_str(), rows[0].user_id.as_str()), ("k8s", "u-1"));
        assert_eq!(rows[0].rancher.as_ref().unwrap().user_agent, "kubectl/v1.35.0");

        // Rancher first; the downstream record arrives later and still pairs.
        let s = Store::open(":memory:").unwrap();
        s.insert_event("local", &rancher_put("r1", "2026-10-14T02:00:00Z")).unwrap();
        assert_eq!(all(&s)[0].source, "rancher", "until then, the Rancher record stands alone");
        s.insert_event("c1", &patch("k1", "alice", "2026-10-14T02:00:00.400Z")).unwrap();
        assert_eq!(all(&s).len(), 1);
    }

    #[test]
    fn pairing_needs_the_same_action_and_time() {
        let s = Store::open(":memory:").unwrap();
        s.insert_event("c1", &patch("k1", "alice", "2026-10-14T02:00:00Z")).unwrap();
        // Too late: 10 s after.
        s.insert_event("local", &rancher_put("r1", "2026-10-14T02:00:10Z")).unwrap();
        // Other cluster.
        let mut other = rancher_put("r2", "2026-10-14T02:00:01Z");
        other.cluster = Some("c2".into());
        s.insert_event("local", &other).unwrap();
        assert_eq!(all(&s).len(), 3);
        // Two edits in a row pair one to one, nearest first.
        s.insert_event("c1", &patch("k2", "alice", "2026-10-14T02:00:09.800Z")).unwrap();
        assert_eq!(all(&s).len(), 3);
    }

    #[test]
    fn version_moves_to_the_downstream_half() {
        let s = Store::open(":memory:").unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 10, 14, 2, 0, 0).unwrap();
        s.insert_version(&key(), "Deployment", "u1", &obj(1), "baseline", t0).unwrap();
        let vid = s.insert_version(&key(), "Deployment", "u1", &obj(2), "modified", t0 + chrono::Duration::seconds(1)).unwrap().unwrap();
        // Only Rancher's record so far: the version is credited to it.
        s.insert_event("local", &rancher_put("r1", "2026-10-14T02:00:01Z")).unwrap();
        assert_eq!(s.link_since(0).unwrap(), 1);
        s.insert_event("c1", &patch("k1", "alice", "2026-10-14T02:00:00.900Z")).unwrap();
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
        s.insert_event("c1", &patch("k1", "alice", "2026-10-14T02:00:00Z")).unwrap();
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

    /// A type with no objects at its first list stores no versions; the next list after a
    /// restart is still a re-list, so an object created meanwhile is a change seen late, not
    /// part of a first look (which the change map leaves out).
    #[test]
    fn an_empty_type_listed_once_is_not_a_first_look_again() {
        let s = Store::open(":memory:").unwrap();
        let start = ObjMsg::ListStart { group: "apps".into(), resource: "deployments".into() };
        let done = |names: &[&str]| ObjMsg::ListDone { group: "apps".into(), resource: "deployments".into(), listed: names.iter().map(|n| ("shop".to_string(), n.to_string())).collect() };
        s.apply_objects("c1", &[start.clone(), done(&[])]).unwrap();
        s.apply_objects("c1", &[start, ver("list", "web", 1), done(&["web"])]).unwrap();
        assert_eq!(s.history(&key()).unwrap()[0].change, "added_unseen");
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

    /// A night-time patch through Rancher by user `uid` (23:00 in Taipei, a Wednesday).
    fn night_patch(id: &str, uid: &str) -> Record {
        let ev = format!(r#"{{"auditID":"{id}","stage":"ResponseComplete","verb":"patch",
          "user":{{"username":"system:serviceaccount:cattle-impersonation-system:cattle-impersonation-{uid}"}},
          "impersonatedUser":{{"username":"{uid}","extra":{{"username":["{uid}-name"]}}}},
          "objectRef":{{"resource":"deployments","namespace":"shop","name":"web","apiGroup":"apps"}},
          "responseStatus":{{"code":200}},"stageTimestamp":"2026-10-14T15:00:00Z"}}"#);
        to_record(&parse_body(ev.as_bytes()).unwrap()[0]).unwrap()
    }

    fn alerts_of(s: &Store, audit_id: &str) -> Vec<String> {
        let c = s.conn.lock().unwrap();
        c.prepare("SELECT a.rule FROM alerts a JOIN events e ON a.event_id = e.id WHERE e.audit_id = ?1 ORDER BY 1")
            .unwrap()
            .query_map(params![audit_id], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// Rules look at people only, and who is a person comes from the identity table: Rancher's
    /// own provisioning account and whitelisted names no longer trip "off hours". The record
    /// itself (and its hash) is not changed.
    #[test]
    fn only_people_trip_rules_and_the_identity_table_decides_who_is_one() {
        let s = Store::open(":memory:").unwrap();
        let mut provisioning = user_msg("baseline", "uid-1", "2026-10-01T00:00:00Z", 0);
        if let ObjMsg::Version { object, .. } = &mut provisioning {
            object["principalIds"] = json!(["system://provisioning/fleet-default/rke2"]);
        }
        s.apply_objects("local", &[provisioning]).unwrap();
        s.insert_event("c1", &night_patch("n1", "u-1")).unwrap();
        s.insert_event("c1", &night_patch("n2", "u-2")).unwrap();
        assert_eq!(alerts_of(&s, "n1"), Vec::<String>::new(), "Rancher's provisioning account is not a person");
        assert_eq!(alerts_of(&s, "n2"), vec!["off_hours".to_string()]);
        let human: i64 = s.conn.lock().unwrap().query_row("SELECT human FROM events WHERE audit_id='n1'", [], |r| r.get(0)).unwrap();
        assert_eq!(human, 1, "the stored record keeps its own guess");

        let mut cfg = crate::config::Config::defaults();
        cfg.identities = crate::identity::Whitelist::parse(r#"[{"names": ["u-2"], "class": "component", "reason": "排程工具"}]"#).unwrap();
        let w = Store::open_with(":memory:", Arc::new(cfg)).unwrap();
        w.insert_event("c1", &night_patch("n3", "u-2")).unwrap();
        assert_eq!(alerts_of(&w, "n3"), Vec::<String>::new(), "a whitelisted name is what the whitelist says");
        let rows = crate::identity::list(&w.conn.lock().unwrap()).unwrap();
        assert_eq!((rows[0]["class"].as_str(), rows[0]["source"].as_str()), (Some("component"), Some("whitelist")));
    }

    /// Changing the whitelist changes the settings fingerprint, so stored judgements are offered
    /// to be judged again.
    #[test]
    fn the_whitelist_is_part_of_the_settings() {
        let a = crate::config::Config::defaults();
        let mut b = crate::config::Config::defaults();
        b.identities = crate::identity::Whitelist::parse(r#"[{"names": ["x"], "class": "component", "reason": "r"}]"#).unwrap();
        assert_ne!(crate::rules::fingerprint(&a).0, crate::rules::fingerprint(&b).0);
    }

    #[test]
    fn deleted_and_readded_user_is_one_person_with_two_lifetimes() {
        let s = Store::open(":memory:").unwrap();
        s.apply_objects("local", &[user_msg("list", "A", "2026-10-13T00:00:00Z", 0)]).unwrap();
        s.insert_event("c1", &patch("k1", "alice", "2026-10-14T02:00:00Z")).unwrap();
        s.apply_objects("local", &[user_msg("deleted", "A", "2026-10-13T00:00:00Z", 180)]).unwrap();
        // Same principal logs in again: same u-1, new object.
        s.apply_objects("local", &[user_msg("modified", "B", "2026-10-14T05:00:00Z", 300)]).unwrap();
        s.insert_event("c1", &patch("k2", "alice.chen", "2026-10-14T06:00:00Z")).unwrap();

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
        for (id, ts) in [("a", "2026-10-14T15:00:00Z"), ("b", "2026-10-14T15:06:00Z"), ("c", "2026-10-14T15:30:00Z")] {
            let r = direct(id, "create", "clusterrolebindings", id, ts);
            s.insert_event("c1", &r).unwrap();
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
        s.insert_event("local", &rancher_put("r1", "2026-10-14T02:00:01Z")).unwrap();
        assert_eq!(s.hourly(None, 0, i64::MAX).unwrap()[0].writes, 1);
        s.insert_event("c1", &patch("k1", "alice", "2026-10-14T02:00:00.900Z")).unwrap();
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
        s.insert_event("c1", &patch("k1", "alice", "2026-10-14T02:00:00Z")).unwrap();
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
        s.insert_event("local", &r).unwrap();
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
    fn chain_detects_tampering_and_survives_retention() {
        let s = Store::open(":memory:").unwrap();
        for (i, ts) in ["2026-09-01T00:00:00Z", "2026-09-02T00:00:00Z", "2026-10-14T00:00:00Z", "2026-10-14T01:00:00Z"].iter().enumerate() {
            s.insert_event("c1", &patch(&format!("p{i}"), "alice", ts)).unwrap();
        }
        let r = &s.verify_chain(None).unwrap()[0];
        assert_eq!((r.verified, r.first_bad_id), (4, None));
        let anchor = s.take_anchors(Utc::now()).unwrap().remove(0);
        assert!(s.take_anchors(Utc::now()).unwrap().is_empty(), "nothing new to anchor");
        assert_eq!(s.check_anchor("c1", anchor.event_id, &anchor.hash).unwrap(), "match");

        // Retention removes the two September records; what is left still verifies.
        let now = Utc.with_ymd_and_hms(2026, 10, 15, 0, 0, 0).unwrap();
        s.prune(30, now).unwrap();
        let r = &s.verify_chain(None).unwrap()[0];
        assert_eq!((r.base_id, r.verified, r.first_bad_id), (2, 2, None));

        // Set-aside lines and long-silent node collectors go with the records of their time.
        {
            let c = s.conn.lock().unwrap();
            c.execute("INSERT INTO rejected_lines (cluster, source, received_ms, len, sha256, error) VALUES ('c1', 'k8s', ?1, 1, 'x', 'bad')", params![ms(&now) - 40 * 86_400_000]).unwrap();
            c.execute("INSERT INTO heartbeats (cluster, collector, instance, last_ms, details) VALUES ('c1', 'file', 'gone', ?1, '{}'), ('c1', 'file', 'down', ?2, '{}'), ('c1', 'watch', 'w', ?1, '{}')",
                      params![ms(&now) - 40 * 86_400_000, ms(&now) - 2 * 86_400_000]).unwrap();
        }
        s.prune(30, now).unwrap();
        {
            let c = s.conn.lock().unwrap();
            let rejected: i64 = c.query_row("SELECT count(*) FROM rejected_lines", [], |r| r.get(0)).unwrap();
            assert_eq!(rejected, 0);
            let left: Vec<String> = c.prepare("SELECT instance FROM heartbeats ORDER BY instance").unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
            assert_eq!(left, ["down", "w"], "a node silent two days stays listed; only one silent past retention goes");
        }

        // Someone edits a stored record directly.
        s.conn.lock().unwrap().execute("UPDATE events SET actor='mallory' WHERE audit_id='p2'", []).unwrap();
        let r = &s.verify_chain(None).unwrap()[0];
        assert_eq!(r.first_bad_id, Some(3));
        assert_eq!(s.check_anchor("c1", anchor.event_id, &anchor.hash).unwrap(), "chain_broken");
    }

    #[test]
    fn edits_to_fleet_objects_are_flagged_and_reverts_seen() {
        let s = Store::open(":memory:").unwrap();
        let t = Utc.with_ymd_and_hms(2026, 10, 14, 1, 0, 0).unwrap();
        let fleet = json!({"metadata":{"name":"web","namespace":"shop","labels":{"fleet.cattle.io/managed-by-kind":"gitrepo","fleet.cattle.io/managed-by-namespace":"fleet-default","fleet.cattle.io/managed-by-name":"shop-apps"}},"spec":{"replicas":3}});
        s.insert_version(&key(), "Deployment", "u1", &fleet, "baseline", t).unwrap();
        let r = patch("h1", "alice", "2026-10-14T02:00:00Z");
        s.insert_event("c1", &r).unwrap();
        let fleet_fix = r#"{"auditID":"f1","stage":"ResponseComplete","verb":"patch","user":{"username":"system:serviceaccount:cattle-fleet-system:fleet-agent"},
          "objectRef":{"resource":"deployments","namespace":"shop","name":"web","apiGroup":"apps"},"responseStatus":{"code":200},"stageTimestamp":"2026-10-14T02:00:06Z"}"#;
        s.insert_event("c1", &to_record(&parse_body(fleet_fix.replace('\n', " ").as_bytes()).unwrap()[0]).unwrap()).unwrap();
        let g = s.gitops_bypasses(0, i64::MAX).unwrap();
        assert_eq!(g.len(), 1, "only the person's edit");
        assert_eq!(g[0].source.as_deref(), Some("gitrepo/fleet-default/shop-apps"));
        assert!(g[0].reverted_at.is_some());
        // An object not deployed by Fleet: no flag.
        let other = ObjKey { name: "api".into(), ..key() };
        s.insert_version(&other, "Deployment", "u2", &obj(1), "baseline", t).unwrap();
        let mut r = patch("h2", "alice", "2026-10-14T03:00:00Z");
        r.name = "api".into();
        s.insert_event("c1", &r).unwrap();
        assert_eq!(s.gitops_bypasses(0, i64::MAX).unwrap().len(), 1);
        assert_eq!(fleet_marker(&json!({"metadata":{"annotations":{"objectset.rio.cattle.io/id":"default-shop"}}})).as_deref(), Some("objectset/default-shop"));
    }

    #[test]
    fn silent_collectors_become_gaps() {
        let s = Store::open(":memory:").unwrap();
        let t0 = 1_000_000_000_000;
        s.heartbeat("c1", "file", "node-1", &json!({"auditLog":"present"}), t0).unwrap();
        s.heartbeat("c1", "file", "node-1", &json!({}), t0 + 60_000).unwrap();
        // Ten minutes of silence, then it comes back.
        s.heartbeat("c1", "file", "node-1", &json!({}), t0 + 660_000).unwrap();
        s.heartbeat("c1", "watch", "pod-a", &json!({}), t0).unwrap();
        let cov = s.coverage(0, i64::MAX, t0 + 700_000).unwrap();
        let c1 = &cov[0];
        let gaps: Vec<(String, Option<i64>)> = c1.gaps.iter().map(|g| (g["collector"].as_str().unwrap().to_string(), g["toMs"].as_i64())).collect();
        assert!(gaps.contains(&("file".into(), Some(t0 + 660_000))), "closed gap: {gaps:?}");
        assert!(gaps.contains(&("watch".into(), None)), "watch is still silent: {gaps:?}");
    }

    #[test]
    fn flagged_processes_are_found_by_pod_and_time() {
        let s = Store::open(":memory:").unwrap();
        let inc = |id: &str, wl: &str, ts: i64| crate::neuvector::Incident {
            id: id.into(), name: "Process.Profile.Violation".into(), level: "Warning".into(), reported_timestamp: ts,
            workload_name: wl.into(), workload_domain: "shop".into(), proc_name: "curl".into(), proc_cmd: "curl http://x/".into(),
            proc_parent_name: "bash".into(), ..Default::default()
        };
        let n = s.insert_security("c1", &[inc("a", "web-6c9f7d-2kq8v", 1_000), inc("b", "api-1", 1_010), inc("a", "web-6c9f7d-2kq8v", 1_000)]).unwrap();
        assert_eq!(n, 2, "duplicates ignored");
        let found = s.security_for_pod("c1", "shop", "web-6c9f7d-2kq8v", 999_000, 1_100_000).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0]["command"], "curl http://x/");
        assert!(s.security_for_pod("c1", "shop", "web-6c9f7d-2kq8v", 1_001_000, 1_100_000).unwrap().is_empty());
    }

    #[test]
    fn review_fixes_chain_base_and_id_reuse() {
        let s = Store::open(":memory:").unwrap();
        let t = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();
        // id 1 is kept by prune (a version points at it), id 2 goes, id 3 stays.
        s.insert_version(&key(), "Deployment", "u1", &obj(1), "baseline", t).unwrap();
        s.insert_event("c1", &patch("a", "alice", "2026-09-01T00:00:00Z")).unwrap();
        s.insert_version(&key(), "Deployment", "u1", &obj(2), "modified", t + chrono::Duration::seconds(1)).unwrap();
        assert_eq!(s.link_since(0).unwrap(), 1);
        s.insert_event("c1", &patch("b", "alice", "2026-09-02T00:00:00Z")).unwrap();
        s.insert_event("c1", &patch("c", "alice", "2026-10-14T00:00:00Z")).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 10, 15, 0, 0, 0).unwrap();
        s.prune(30, now).unwrap();
        s.prune(30, now + chrono::Duration::hours(6)).unwrap();
        let r = &s.verify_chain(None).unwrap()[0];
        assert_eq!((r.base_id, r.first_bad_id), (2, None), "the base never moves back to a kept row");

        // Everything old: the newest record stays, so IDs keep growing.
        let s = Store::open(":memory:").unwrap();
        for (i, ts) in ["2026-09-01T00:00:00Z", "2026-09-02T00:00:00Z"].iter().enumerate() {
            s.insert_event("c1", &patch(&format!("p{i}"), "alice", ts)).unwrap();
        }
        s.prune(30, now).unwrap();
        s.insert_event("c1", &patch("new", "alice", "2026-10-14T00:00:00Z")).unwrap();
        let ids: Vec<i64> = s.conn.lock().unwrap().prepare("SELECT id FROM events ORDER BY id").unwrap().query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
        assert_eq!(ids, vec![2, 3]);
        assert_eq!(s.verify_chain(None).unwrap()[0].first_bad_id, None);
    }

    #[test]
    fn review_fixes_late_rancher_half_is_forwarded() {
        let s = Store::open(":memory:").unwrap();
        s.insert_event("c1", &patch("k1", "alice", "2026-10-14T02:00:00.700Z")).unwrap();
        // The downstream record goes out first.
        let first = s.forward_records(0, i64::MAX, 100).unwrap();
        assert_eq!(first.len(), 1);
        s.forward_done("t", Some(first[0].id), None, None).unwrap();
        // The Rancher half arrives late and pairs with it: it must still be sent.
        s.insert_event("local", &rancher_put("r1", "2026-10-14T10:00:01+08:00")).unwrap();
        let late = s.forward_records(first[0].id, i64::MAX, 100).unwrap();
        assert_eq!((late.len(), late[0].source.as_str()), (1, "rancher"));
        // But a Rancher half paired before its partner was sent stays hidden.
        let s = Store::open(":memory:").unwrap();
        s.insert_event("local", &rancher_put("r1", "2026-10-14T10:00:01+08:00")).unwrap();
        s.insert_event("c1", &patch("k1", "alice", "2026-10-14T02:00:00.700Z")).unwrap();
        let both = s.forward_records(0, i64::MAX, 100).unwrap();
        assert_eq!((both.len(), both[0].source.as_str(), both[0].rancher_user_agent.as_deref()), (1, "k8s", Some("kubectl/v1.35.0")));
    }

    #[test]
    fn review_fixes_creator_unknown_after_retention() {
        let s = Store::open(":memory:").unwrap();
        let mk = |change: &str, at: DateTime<Utc>| {
            let key = ObjKey { cluster: "local".into(), api_group: "management.cattle.io".into(), resource: "clusterroletemplatebindings".into(), namespace: "c-1".into(), name: "crtb-x".into() };
            s.insert_version(&key, "ClusterRoleTemplateBinding", "b1", &json!({"clusterName":"c-1","roleTemplateName":"r","userName":"u-1","n":at.timestamp()}), change, at).unwrap()
        };
        let t = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();
        mk("added", t);
        mk("modified", t + chrono::Duration::days(4));
        // Retention removes the creation; the first version left is a modification.
        s.conn.lock().unwrap().execute("DELETE FROM versions WHERE change='added'", []).unwrap();
        let o = s.access_objects("local", i64::MAX).unwrap();
        assert!(o[0].before_tracking && o[0].created_by.is_none());
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
        s.insert_event("c1", &r).unwrap();
        assert_eq!(s.link_since(0).unwrap(), 1);
        let h = s.history(&ns).unwrap();
        assert_eq!((h[0].change.as_str(), h[0].actor.as_deref()), ("deleting", Some("system:admin")));
        // The finalizers finish later and the object goes: same request, same person. Left
        // unlinked it showed on the change map as a change nobody made ("unclear").
        s.insert_version(&ns, "Namespace", "n1", &json!({"metadata":{"name":"old"}}), "deleted", t + chrono::Duration::seconds(40)).unwrap();
        assert_eq!(s.link_since(0).unwrap(), 1);
        let h = s.history(&ns).unwrap();
        assert_eq!((h[0].change.as_str(), h[0].actor.as_deref()), ("deleted", Some("system:admin")));
        let unlinked: i64 = s.conn.lock().unwrap().query_row("SELECT count(*) FROM versions WHERE event_id IS NULL AND change <> 'baseline'", [], |r| r.get(0)).unwrap();
        assert_eq!(unlinked, 0);
    }

    #[test]
    fn a_resent_gap_report_is_kept_once_and_both_kinds_are_returned() {
        let s = Store::open(":memory:").unwrap();
        let t0 = 1_800_000_000_000i64;
        let g = crate::collect::Gap { collector: "file".into(), instance: "node-1".into(), from_ms: t0, to_ms: t0 + 600_000, reason: "collector-restart".into(), detail: "audit.log".into() };
        assert!(s.record_gap("c1", &g).unwrap());
        assert!(!s.record_gap("c1", &g).unwrap(), "the same report again is ignored");
        // The server also saw the node fall silent over the same time.
        s.heartbeat("c1", "file", "node-1", &json!({}), t0).unwrap();
        s.heartbeat("c1", "file", "node-1", &json!({}), t0 + 600_000).unwrap();
        let start = crate::collect::Gap { reason: START.into(), detail: "fromNow".into(), to_ms: t0, ..g.clone() };
        s.record_gap("c1", &start).unwrap();
        let cov = s.coverage(t0 - 1, t0 + 700_000, t0 + 600_001).unwrap();
        let kinds: Vec<(String, String)> = cov[0].gaps.iter().map(|g| (g["kind"].as_str().unwrap().into(), g["reason"].as_str().unwrap().into())).collect();
        assert_eq!(kinds.len(), 2, "{kinds:?}");
        assert!(kinds.contains(&("reported".into(), "collector-restart".into())), "{kinds:?}");
        assert!(kinds.contains(&("heartbeat".into(), SILENT.into())), "{kinds:?}");
        assert_eq!(cov[0].starts.len(), 1, "the start mark is not a gap");
        assert_eq!(s.known_instance("c1", "file", "node-1").unwrap(), Some(t0 + 600_000));
        assert_eq!(s.known_instance("c1", "file", "node-2").unwrap(), None);
    }

    #[test]
    fn set_aside_lines_are_listed_without_text_and_once() {
        let s = Store::open(":memory:").unwrap();
        let lines = vec![(br#"{"secret":"body"#.to_vec(), "EOF".to_string(), 120usize)];
        s.record_rejected("c1", "k8s-audit", 5, Some(("node-1", "audit.log", 1000)), &lines).unwrap();
        s.record_rejected("c1", "k8s-audit", 6, Some(("node-1", "audit.log", 1000)), &lines).unwrap(); // resent batch
        let rows = s.rejected(None, 0, 10, 100).unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["offset"], 1120);
        assert_eq!(rows[0]["file"], "audit.log");
        assert_eq!(rows[0]["instance"], "node-1");
        assert!(!serde_json::to_string(&rows).unwrap().contains("secret"), "no line text");
        let cov = s.coverage(0, 10, 10).unwrap();
        assert_eq!(cov[0].rejected_counts[0]["count"], 1);
    }

    /// Upgrade a copy of a real database (never the original): AL_TEST_DB=<copy> cargo test -- --ignored migrates
    #[test]
    #[ignore]
    fn migrates_a_copy_of_a_real_database() {
        let path = std::env::var("AL_TEST_DB").expect("AL_TEST_DB: path to a copy");
        let before: i64 = rusqlite::Connection::open(&path).unwrap().query_row("SELECT COUNT(*) FROM gaps", [], |r| r.get(0)).unwrap();
        let s = Store::open(&path).unwrap();
        let after: i64 = s.conn.lock().unwrap().query_row("SELECT COUNT(*) FROM gaps", [], |r| r.get(0)).unwrap();
        assert_eq!(before, after, "old gaps (heartbeat silences) are all kept");
        let silent: i64 = s.conn.lock().unwrap().query_row("SELECT COUNT(*) FROM gaps WHERE reason = ''", [], |r| r.get(0)).unwrap();
        assert_eq!(silent, after, "old rows are heartbeat silences");
        let g = crate::collect::Gap { collector: "file".into(), instance: "mig-node".into(), from_ms: 1, to_ms: 2, reason: "collector-restart".into(), detail: String::new() };
        assert!(s.record_gap("mig", &g).unwrap());
        assert!(!s.record_gap("mig", &g).unwrap(), "the unique key works on the upgraded database");
        s.record_rejected("mig", "k8s-audit", 1, Some(("mig-node", "audit.log", 10)), &[(b"x".to_vec(), "e".into(), 0)]).unwrap();
        s.record_rejected("mig", "k8s-audit", 1, Some(("mig-node", "audit.log", 10)), &[(b"x".to_vec(), "e".into(), 0)]).unwrap();
        assert_eq!(s.rejected(Some("mig"), 0, 10, 10).unwrap().len(), 1);
        let cov = s.coverage(0, i64::MAX / 2, chrono::Utc::now().timestamp_millis()).unwrap();
        let kinds: usize = cov.iter().map(|r| r.gaps.iter().filter(|g| g["kind"] == "heartbeat").count()).sum();
        println!("gaps before={before} after={after} heartbeat-kind in coverage={kinds}");
        // Opening again changes nothing.
        drop(s);
        let s = Store::open(&path).unwrap();
        let again: i64 = s.conn.lock().unwrap().query_row("SELECT COUNT(*) FROM gaps", [], |r| r.get(0)).unwrap();
        assert_eq!(again, after + 1);
    }

    /// cluster_stats and resource_kinds always equal what scanning the history would give:
    /// filled once for a database from before them, and kept by triggers after that.
    #[test]
    fn stats_tables_match_the_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.db");
        let p = path.to_str().unwrap();
        let ev = |c: &Connection, cl: &str, id: &str, t: i64| {
            c.execute(
                "INSERT INTO events (cluster, audit_id, ts_ms, ts, actor, principal, human, via, caller, verb, api_group, resource, subresource,
                                     namespace, name, code, source_ip, user_agent, request_uri, source)
                 VALUES (?1, ?2, ?3, '', 'a', '', 1, 'direct', 'a', 'patch', '', 'configmaps', '', 'app', ?2, 200, '', '', '', 'k8s')",
                params![cl, id, t],
            )
            .unwrap();
        };
        let ver = |c: &Connection, cl: &str, res: &str, kind: &str, t: i64| {
            c.execute(
                "INSERT INTO versions (cluster, api_group, resource, kind, namespace, name, uid, observed_ms, observed, change, body)
                 VALUES (?1, 'apps', ?2, ?3, 'app', 'x', 'u', ?4, '', 'modified', '{}')",
                params![cl, res, kind, t],
            )
            .unwrap();
        };
        let check = |c: &Connection| {
            let stats: Vec<(String, Option<i64>, Option<i64>)> =
                c.prepare("SELECT cluster, last_event_ms, last_version_ms FROM cluster_stats ORDER BY cluster").unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(Result::unwrap).collect();
            let direct: Vec<(String, Option<i64>, Option<i64>)> = c
                .prepare(
                    "SELECT cl, (SELECT max(ts_ms) FROM events WHERE cluster = cl), (SELECT max(observed_ms) FROM versions WHERE cluster = cl)
                     FROM (SELECT cluster AS cl FROM events UNION SELECT cluster FROM versions) ORDER BY cl",
                )
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            assert_eq!(stats, direct);
            let kinds: Vec<(String, String, String, String)> =
                c.prepare("SELECT cluster, api_group, resource, kind FROM resource_kinds ORDER BY 1, 2, 3, 4").unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap().map(Result::unwrap).collect();
            let direct: Vec<(String, String, String, String)> = c
                .prepare("SELECT DISTINCT cluster, api_group, resource, kind FROM versions ORDER BY 1, 2, 3, 4")
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            assert_eq!(kinds, direct);
        };
        {
            // A database from before the tables: data, then the tables and triggers removed.
            let s = Store::open(p).unwrap();
            let c = s.conn.lock().unwrap();
            ev(&c, "c1", "a", 10);
            ev(&c, "c1", "b", 30);
            ev(&c, "c2", "c", 20);
            ver(&c, "c1", "deployments", "Deployment", 25);
            ver(&c, "c3", "statefulsets", "StatefulSet", 5);
            c.execute_batch("DROP TRIGGER events_stats; DROP TRIGGER versions_stats; DROP TABLE cluster_stats; DROP TABLE resource_kinds;").unwrap();
        }
        let s = Store::open(p).unwrap();
        let c = s.conn.lock().unwrap();
        check(&c);
        // After that the triggers keep them current, including an older record arriving late.
        ev(&c, "c1", "d", 5);
        ev(&c, "c2", "e", 40);
        ver(&c, "c2", "daemonsets", "DaemonSet", 50);
        ver(&c, "c1", "deployments", "Deployment", 1);
        check(&c);
        let last: i64 = c.query_row("SELECT last_event_ms FROM cluster_stats WHERE cluster = 'c1'", [], |r| r.get(0)).unwrap();
        assert_eq!(last, 30);
    }
}
