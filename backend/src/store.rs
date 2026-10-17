//! SQLite storage for audit records, object versions and alerts.

use crate::audit::Record;
use crate::diff::{diff, normalize, Change};
use crate::rules::Alert;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;
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
"#;

/// Version kinds:
/// - baseline: first time this tool saw the object
/// - added / modified / deleting / deleted: seen live by the watch
/// - added_unseen / modified_unseen: found on a re-list after a gap (restart, lost watch)
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

#[derive(Debug, Serialize)]
pub struct EventRow {
    pub id: i64,
    pub cluster: String,
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

#[derive(Debug, Serialize)]
pub struct ActorRow {
    pub actor: String,
    pub human: bool,
    pub writes: i64,
    pub alerts: i64,
    pub last_seen: String,
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

const WRITE_VERBS: &str = "('create','update','patch','delete','deletecollection')";

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
        Ok(Store { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Insert a batch in one transaction. Returns how many were new
    /// (the webhook can resend batches, and exec sessions arrive twice).
    pub fn insert_events(&self, cluster: &str, batch: &[(Record, Vec<Alert>)]) -> anyhow::Result<usize> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        let mut stored = 0;
        {
            let mut ins = tx.prepare_cached(
                "INSERT OR IGNORE INTO events (cluster,audit_id,ts_ms,ts,actor,principal,human,via,caller,verb,api_group,resource,subresource,namespace,name,code,source_ip,user_agent,request_uri,request_object)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)",
            )?;
            let mut al = tx.prepare_cached("INSERT INTO alerts (event_id, rule, severity) VALUES (?1,?2,?3)")?;
            for (r, alerts) in batch {
                // Request bodies are kept only for people; controller status writes are noise.
                let body = if r.human { r.request_object.as_ref().map(|v| v.to_string()) } else { None };
                let n = ins.execute(params![
                    cluster, r.audit_id, ms(&r.ts), r.ts.to_rfc3339(), r.actor, r.principal, r.human as i64,
                    r.via.as_str(), r.caller, r.verb, r.api_group, r.resource, r.subresource, r.namespace, r.name,
                    r.code, r.source_ip, r.user_agent, r.request_uri, body
                ])?;
                if n == 0 {
                    continue;
                }
                stored += 1;
                let id = tx.last_insert_rowid();
                for a in alerts {
                    al.execute(params![id, a.rule, a.severity])?;
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
        Ok(Some(c.last_insert_rowid()))
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
                 AND e.ts_ms BETWEEN ?6 AND ?7
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
        Ok(Some(changes_for(&change, old.as_ref(), &new)))
    }

    pub fn timeline(&self, q: &Query) -> anyhow::Result<Vec<EventRow>> {
        let c = self.conn.lock().unwrap();
        let mut sql = String::from(
            "SELECT e.id,e.cluster,e.ts,e.actor,e.principal,e.human,e.via,e.caller,e.verb,e.api_group,e.resource,e.subresource,
                    e.namespace,e.name,e.code,e.source_ip,e.user_agent,e.request_uri,e.request_object,
                    (SELECT group_concat(rule) FROM alerts a WHERE a.event_id=e.id),
                    (SELECT v.id FROM versions v WHERE v.event_id=e.id ORDER BY v.id DESC LIMIT 1)
             FROM events e WHERE e.ts_ms BETWEEN ?1 AND ?2",
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(q.from_ms), Box::new(q.to_ms)];
        if let Some(cl) = &q.cluster {
            args.push(Box::new(cl.clone()));
            sql += &format!(" AND e.cluster=?{}", args.len());
        }
        if let Some(a) = &q.actor {
            args.push(Box::new(a.clone()));
            sql += &format!(" AND e.actor=?{}", args.len());
        }
        if q.humans_only {
            sql += " AND e.human=1";
        }
        if q.alerts_only {
            sql += " AND EXISTS (SELECT 1 FROM alerts a WHERE a.event_id=e.id)";
        } else if q.writes_only {
            sql += &format!(" AND (e.verb IN {WRITE_VERBS} OR e.subresource IN ('exec','attach','portforward') OR EXISTS (SELECT 1 FROM alerts a WHERE a.event_id=e.id))");
        }
        args.push(Box::new(q.limit));
        sql += &format!(" ORDER BY e.ts_ms DESC, e.id DESC LIMIT ?{}", args.len());
        let mut stmt = c.prepare(&sql)?;
        let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let mut rows: Vec<EventRow> = stmt
            .query_map(refs.as_slice(), |r| {
                let ro: Option<String> = r.get(18)?;
                let al: Option<String> = r.get(19)?;
                Ok(EventRow {
                    id: r.get(0)?, cluster: r.get(1)?, ts: r.get(2)?, actor: r.get(3)?, principal: r.get(4)?,
                    human: r.get::<_, i64>(5)? == 1, via: r.get(6)?, caller: r.get(7)?, verb: r.get(8)?,
                    api_group: r.get(9)?, resource: r.get(10)?, subresource: r.get(11)?, namespace: r.get(12)?,
                    name: r.get(13)?, code: r.get(14)?, source_ip: r.get(15)?, user_agent: r.get(16)?, request_uri: r.get(17)?,
                    request_object: ro.and_then(|s| serde_json::from_str(&s).ok()),
                    alerts: al.map(|s| s.split(',').map(String::from).collect()).unwrap_or_default(),
                    version_id: r.get(20)?,
                    changes: None,
                })
            })?
            .collect::<Result<_, _>>()?;
        drop(stmt);
        for row in rows.iter_mut() {
            if let Some(vid) = row.version_id {
                row.changes = Self::version_changes(&c, vid)?;
            }
        }
        Ok(rows)
    }

    pub fn actors(&self, from_ms: i64, to_ms: i64) -> anyhow::Result<Vec<ActorRow>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(&format!(
            "SELECT actor, max(human), sum(CASE WHEN verb IN {WRITE_VERBS} THEN 1 ELSE 0 END),
                    (SELECT count(*) FROM alerts a JOIN events e2 ON a.event_id=e2.id WHERE e2.actor=e.actor AND e2.ts_ms BETWEEN ?1 AND ?2),
                    max(ts)
             FROM events e WHERE ts_ms BETWEEN ?1 AND ?2 GROUP BY actor ORDER BY max(human) DESC, 3 DESC"
        ))?;
        let rows = stmt
            .query_map(params![from_ms, to_ms], |r| {
                Ok(ActorRow { actor: r.get(0)?, human: r.get::<_, i64>(1)? == 1, writes: r.get(2)?, alerts: r.get(3)?, last_seen: r.get(4)? })
            })?
            .collect::<Result<_, _>>()?;
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

        let q = Query { cluster: None, actor: Some("alice".into()), from_ms: 0, to_ms: i64::MAX, humans_only: true, writes_only: true, alerts_only: false, limit: 50 };
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
