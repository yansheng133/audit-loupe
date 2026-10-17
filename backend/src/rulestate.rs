//! Which rule settings judged which stored records, and judging them again.
//!
//! Alerts are decided when a record arrives, by the settings in force then. When the settings
//! change (a restart with different AL_RULES, change windows or working hours), records that
//! arrived before keep their old judgement until someone asks to judge them again. Judging
//! again only reads the stored records: the records themselves, and their hash chain, do not
//! change. Alerts, stories and the hourly alert counts are rebuilt in shadow tables and swap in
//! at once, so the UI never shows a half-done state.
//!
//! What was judged with which settings is kept as segments of record IDs: each settings row
//! covers the records that arrived while it was in force, and a finished re-judging covers
//! every record up to the point where it swapped in.

use crate::audit::{Record, Source, Via};
use crate::rules::judge;
use crate::store::{Store, Tables};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;
use std::time::Instant;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS rule_settings (
  id INTEGER PRIMARY KEY, fingerprint TEXT NOT NULL, settings TEXT NOT NULL,
  active_from_ms INTEGER NOT NULL, first_event_id INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS recomputes (
  id INTEGER PRIMARY KEY, settings_id INTEGER NOT NULL, started_ms INTEGER NOT NULL, finished_ms INTEGER,
  events INTEGER NOT NULL DEFAULT 0, upto_event_id INTEGER, from_ms INTEGER, to_ms INTEGER, error TEXT
);
"#;

/// Shadow tables; left over only if the server stopped during a re-judging.
const SHADOW: &str = r#"
DROP TABLE IF EXISTS rc_alerts;
DROP TABLE IF EXISTS rc_stories;
DROP TABLE IF EXISTS rc_hourly;
DROP TABLE IF EXISTS rc_story_of;
"#;

const SHADOW_CREATE: &str = r#"
CREATE TABLE rc_alerts (event_id INTEGER NOT NULL, rule TEXT NOT NULL, severity TEXT NOT NULL);
CREATE TABLE rc_stories (
  id INTEGER PRIMARY KEY, cluster TEXT NOT NULL, actor_key TEXT NOT NULL, via TEXT NOT NULL,
  severity TEXT NOT NULL, first_ms INTEGER NOT NULL, last_ms INTEGER NOT NULL, count INTEGER NOT NULL
);
CREATE INDEX rc_stories_actor ON rc_stories(cluster, actor_key, via, last_ms);
CREATE TABLE rc_hourly (
  cluster TEXT NOT NULL, hour_ms INTEGER NOT NULL, actor_key TEXT NOT NULL, human INTEGER NOT NULL,
  writes INTEGER NOT NULL, alerts INTEGER NOT NULL,
  PRIMARY KEY (cluster, hour_ms, actor_key)
);
CREATE TABLE rc_story_of (event_id INTEGER PRIMARY KEY, story_id INTEGER NOT NULL);
"#;

/// Records judged per second when no run has been timed here yet. The `judging_speed` test
/// measures about 70,000/s on a laptop SSD; this is kept well below so the first estimate
/// errs on the long side. After a run, the measured rate is used.
const DEFAULT_RATE: f64 = 10_000.0;
/// Records judged per lock hold, so collectors and the UI are not blocked for long.
const CHUNK: i64 = 2000;

pub fn prepare(c: &Connection) -> anyhow::Result<()> {
    c.execute_batch(SCHEMA)?;
    c.execute_batch(SHADOW)?;
    // A re-judging cut off by a restart never finished.
    c.execute("UPDATE recomputes SET error='stopped by a restart' WHERE finished_ms IS NULL AND error IS NULL", [])?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub state: &'static str,
    pub done: i64,
    pub total: i64,
    pub started_ms: i64,
    pub finished_ms: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsRow {
    pub id: i64,
    pub fingerprint: String,
    pub active_from_ms: i64,
    pub settings: Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecomputeRow {
    pub id: i64,
    pub settings_id: i64,
    pub started_ms: i64,
    pub finished_ms: Option<i64>,
    pub events: i64,
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Pending {
    /// Stored records judged by other settings than the current ones.
    pub events: i64,
    pub oldest_ms: Option<i64>,
    /// Every stored record is judged again, not only those.
    pub total: i64,
    pub estimate_seconds: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub fingerprint: String,
    pub since_ms: i64,
    pub rules: Value,
    pub change_windows: usize,
    pub pending: Option<Pending>,
    pub job: Option<Job>,
    pub history: Vec<SettingsRow>,
    pub recomputes: Vec<RecomputeRow>,
}

fn record_of(row: &rusqlite::Row) -> rusqlite::Result<Loaded> {
    let ts: String = row.get(3)?;
    let source: String = row.get(20)?;
    let via: String = row.get(7)?;
    let body: Option<String> = row.get(19)?;
    let cluster: String = row.get(1)?;
    let r = Record {
        audit_id: row.get(2)?,
        source: if source == "rancher" { Source::Rancher } else { Source::K8s },
        cluster: Some(cluster.clone()),
        ts: DateTime::parse_from_rfc3339(&ts).map(|t| t.with_timezone(&Utc)).unwrap_or_default(),
        actor: row.get(4)?,
        principal: row.get(5)?,
        user_id: row.get(21)?,
        human: row.get::<_, i64>(6)? == 1,
        via: match via.as_str() {
            "rancher" => Via::Rancher,
            "impersonation" => Via::Impersonation,
            "ace" => Via::Ace,
            _ => Via::Direct,
        },
        caller: row.get(8)?,
        verb: row.get(9)?,
        api_group: row.get(10)?,
        resource: row.get(11)?,
        subresource: row.get(12)?,
        namespace: row.get(13)?,
        name: row.get(14)?,
        code: row.get(15)?,
        source_ip: row.get(16)?,
        user_agent: row.get(17)?,
        request_uri: row.get(18)?,
        request_object: body.and_then(|b| serde_json::from_str(&b).ok()),
    };
    // A Rancher record matched to its downstream record is shown through that one.
    let hidden = source == "rancher" && row.get::<_, Option<i64>>(22)?.is_some();
    Ok((row.get(0)?, cluster, row.get(23)?, hidden, r))
}

/// A stored record loaded for judging: ID, cluster, time, whether it is the hidden Rancher half.
type Loaded = (i64, String, i64, bool, Record);

const SELECT: &str = "SELECT id, cluster, audit_id, ts, actor, principal, human, via, caller, verb, api_group, resource, subresource,
    namespace, name, code, source_ip, user_agent, request_uri, request_object, source, user_id, link_id, ts_ms FROM events";

impl Store {
    /// Note the settings this server started with, if they differ from the last ones.
    pub(crate) fn register_settings(&self, now_ms: i64) -> anyhow::Result<()> {
        let (fp, json) = crate::rules::fingerprint(&self.cfg);
        let c = self.conn.lock().unwrap();
        let last: Option<String> = c.query_row("SELECT fingerprint FROM rule_settings ORDER BY id DESC LIMIT 1", [], |r| r.get(0)).optional()?;
        match last {
            Some(f) if f == fp => {}
            // First start with settings tracking: what is stored was judged by these settings,
            // as far as anyone can tell (earlier versions had nothing else to set).
            None => {
                c.execute(
                    "INSERT INTO rule_settings (fingerprint, settings, active_from_ms, first_event_id) VALUES (?1, ?2, ?3, 0)",
                    params![fp, json.to_string(), now_ms],
                )?;
            }
            Some(_) => {
                c.execute(
                    "INSERT INTO rule_settings (fingerprint, settings, active_from_ms, first_event_id)
                     VALUES (?1, ?2, ?3, (SELECT coalesce(max(id), 0) + 1 FROM events))",
                    params![fp, json.to_string(), now_ms],
                )?;
            }
        }
        Ok(())
    }

    /// Record ID ranges [from, to) and the settings fingerprint that judged them.
    fn segments(c: &Connection) -> anyhow::Result<Vec<(i64, i64, String)>> {
        let rows: Vec<(i64, String)> = c
            .prepare("SELECT first_event_id, fingerprint FROM rule_settings ORDER BY id")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let mut segs: Vec<(i64, i64, String)> = Vec::new();
        for (i, (from, fp)) in rows.iter().enumerate() {
            let to = rows.get(i + 1).map(|n| n.0).unwrap_or(i64::MAX);
            segs.push((*from, to, fp.clone()));
        }
        // A finished re-judging covers everything up to where it swapped in, with the settings
        // in force at the time.
        let done: Vec<(i64, String)> = c
            .prepare(
                "SELECT rc.upto_event_id, s.fingerprint FROM recomputes rc JOIN rule_settings s ON s.id=rc.settings_id
                 WHERE rc.finished_ms IS NOT NULL AND rc.error IS NULL ORDER BY rc.id",
            )?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        for (upto, fp) in done {
            let mut next = vec![(0, upto + 1, fp)];
            for (a, b, f) in segs {
                if b > upto + 1 {
                    next.push((a.max(upto + 1), b, f));
                }
            }
            segs = next;
        }
        Ok(segs)
    }

    fn pending(c: &Connection, current: &str) -> anyhow::Result<Option<Pending>> {
        let mut events = 0;
        let mut oldest: Option<i64> = None;
        for (a, b, fp) in Self::segments(c)? {
            if fp == current {
                continue;
            }
            let (n, o): (i64, Option<i64>) =
                c.query_row("SELECT count(*), min(ts_ms) FROM events WHERE id >= ?1 AND id < ?2", params![a, b], |r| Ok((r.get(0)?, r.get(1)?)))?;
            events += n;
            oldest = match (oldest, o) {
                (Some(x), Some(y)) => Some(x.min(y)),
                (x, y) => x.or(y),
            };
        }
        if events == 0 {
            return Ok(None);
        }
        let total: i64 = c.query_row("SELECT count(*) FROM events", [], |r| r.get(0))?;
        let rate: f64 = c
            .query_row(
                "SELECT events * 1000.0 / max(finished_ms - started_ms, 1) FROM recomputes
                 WHERE finished_ms IS NOT NULL AND error IS NULL AND events >= 1000 ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(DEFAULT_RATE);
        Ok(Some(Pending { events, oldest_ms: oldest, total, estimate_seconds: (total as f64 / rate.max(1.0)).ceil() as i64 }))
    }

    pub fn rule_status(&self) -> anyhow::Result<Status> {
        let (fp, _) = crate::rules::fingerprint(&self.cfg);
        let job = self.job.lock().unwrap().clone();
        let c = self.conn.lock().unwrap();
        let history: Vec<SettingsRow> = c
            .prepare("SELECT id, fingerprint, active_from_ms, settings FROM rule_settings ORDER BY id DESC LIMIT 50")?
            .query_map([], |r| {
                let s: String = r.get(3)?;
                Ok(SettingsRow { id: r.get(0)?, fingerprint: r.get(1)?, active_from_ms: r.get(2)?, settings: serde_json::from_str(&s).unwrap_or_default() })
            })?
            .collect::<Result<_, _>>()?;
        let recomputes: Vec<RecomputeRow> = c
            .prepare("SELECT id, settings_id, started_ms, finished_ms, events, from_ms, to_ms, error FROM recomputes ORDER BY id DESC LIMIT 50")?
            .query_map([], |r| {
                Ok(RecomputeRow {
                    id: r.get(0)?, settings_id: r.get(1)?, started_ms: r.get(2)?, finished_ms: r.get(3)?, events: r.get(4)?,
                    from_ms: r.get(5)?, to_ms: r.get(6)?, error: r.get(7)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        let since_ms = history.iter().find(|h| h.fingerprint == fp).map(|h| h.active_from_ms).unwrap_or_default();
        let running = job.as_ref().is_some_and(|j| j.state == "running");
        Ok(Status {
            pending: if running { None } else { Self::pending(&c, &fp)? },
            fingerprint: fp,
            since_ms,
            rules: serde_json::to_value(&self.cfg.rules.rules)?,
            change_windows: self.cfg.change_windows.len(),
            job,
            history,
            recomputes,
        })
    }

    /// Start judging every stored record again with the current settings, in the background.
    /// Returns false when one is already running or nothing needs it.
    pub fn start_recompute(&self) -> anyhow::Result<bool> {
        {
            let mut job = self.job.lock().unwrap();
            if job.as_ref().is_some_and(|j| j.state == "running") {
                return Ok(false);
            }
            let (fp, _) = crate::rules::fingerprint(&self.cfg);
            let (pending, total) = {
                let c = self.conn.lock().unwrap();
                (Self::pending(&c, &fp)?, c.query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))?)
            };
            if pending.is_none() {
                return Ok(false);
            }
            *job = Some(Job { state: "running", done: 0, total, started_ms: Utc::now().timestamp_millis(), finished_ms: None, error: None });
        }
        let s = self.clone();
        std::thread::spawn(move || {
            let result = s.recompute();
            let mut job = s.job.lock().unwrap();
            if let Some(j) = job.as_mut() {
                j.finished_ms = Some(Utc::now().timestamp_millis());
                match result {
                    Ok(()) => j.state = "done",
                    Err(e) => {
                        tracing::warn!(error = %e, "re-judging stored records failed");
                        j.state = "failed";
                        j.error = Some(e.to_string());
                    }
                }
            }
        });
        Ok(true)
    }

    fn set_progress(&self, done: i64, total: i64) {
        if let Some(j) = self.job.lock().unwrap().as_mut() {
            j.done = done;
            j.total = total.max(done);
        }
    }

    /// Judge loaded records into the shadow tables, the way records are judged on arrival.
    fn judge_rows(&self, c: &Connection, rows: &[Loaded]) -> anyhow::Result<()> {
        let mut al = c.prepare_cached("INSERT INTO rc_alerts (event_id, rule, severity) VALUES (?1, ?2, ?3)")?;
        for (id, cluster, ts_ms, hidden, r) in rows {
            let alerts = judge(r, cluster, &self.cfg, c)?;
            for a in &alerts {
                al.execute(params![id, a.rule, a.severity])?;
            }
            if *hidden {
                continue;
            }
            let key = if r.user_id.is_empty() { r.actor.as_str() } else { r.user_id.as_str() };
            if crate::store::counts(r, &alerts) {
                Store::bump_in(c, Tables::SHADOW, cluster, *ts_ms, key, r.human, 1, !alerts.is_empty() as i64)?;
            }
            if alerts.iter().any(|a| matches!(a.severity, "high" | "medium")) {
                Store::add_to_story_in(c, Tables::SHADOW, *id, cluster, key, r.via.as_str(), *ts_ms, &alerts)?;
            }
        }
        Ok(())
    }

    /// The next chunk of records stored up to `max_id`, in time order after `(ts_ms, id)`.
    fn chunk(c: &Connection, after: (i64, i64), max_id: i64, limit: i64) -> anyhow::Result<Vec<Loaded>> {
        let mut st = c.prepare_cached(&format!("{SELECT} WHERE (ts_ms > ?1 OR (ts_ms = ?1 AND id > ?2)) AND id <= ?3 ORDER BY ts_ms, id LIMIT ?4"))?;
        let rows = st.query_map(params![after.0, after.1, max_id, limit], record_of)?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    fn recompute(&self) -> anyhow::Result<()> {
        let started = Instant::now();
        let started_ms = Utc::now().timestamp_millis();
        let (run_id, max_id, total) = {
            let c = self.conn.lock().unwrap();
            c.execute_batch(SHADOW)?;
            c.execute_batch(SHADOW_CREATE)?;
            let (fp, _) = crate::rules::fingerprint(&self.cfg);
            let sid: i64 = c.query_row("SELECT id FROM rule_settings WHERE fingerprint=?1 ORDER BY id DESC LIMIT 1", params![fp], |r| r.get(0))?;
            c.execute("INSERT INTO recomputes (settings_id, started_ms) VALUES (?1, ?2)", params![sid, started_ms])?;
            let run_id = c.last_insert_rowid();
            let max_id: i64 = c.query_row("SELECT coalesce(max(id), 0) FROM events", [], |r| r.get(0))?;
            let total: i64 = c.query_row("SELECT count(*) FROM events", [], |r| r.get(0))?;
            (run_id, max_id, total)
        };
        let fail = |e: anyhow::Error| -> anyhow::Error {
            let c = self.conn.lock().unwrap();
            let _ = c.execute_batch(SHADOW);
            let _ = c.execute("UPDATE recomputes SET error=?2, finished_ms=?3 WHERE id=?1", params![run_id, e.to_string(), Utc::now().timestamp_millis()]);
            e
        };
        // Records stored before the start, a chunk per lock hold.
        let mut pos = (i64::MIN, i64::MIN);
        let mut done = 0;
        loop {
            let mut step = || -> anyhow::Result<usize> {
                let mut c = self.conn.lock().unwrap();
                let tx = c.transaction()?;
                let rows = Self::chunk(&tx, pos, max_id, CHUNK)?;
                self.judge_rows(&tx, &rows)?;
                tx.commit()?;
                if let Some(last) = rows.last() {
                    pos = (last.2, last.0);
                }
                Ok(rows.len())
            };
            let n = step().map_err(&fail)? as i64;
            done += n;
            self.set_progress(done, total);
            if n < CHUNK {
                break;
            }
        }
        // Then, holding the lock: whatever arrived meanwhile, and the swap.
        let swap = || -> anyhow::Result<i64> {
            let mut c = self.conn.lock().unwrap();
            let tx = c.transaction()?;
            let late: Vec<Loaded> = tx
                .prepare(&format!("{SELECT} WHERE id > ?1 ORDER BY ts_ms, id"))?
                .query_map(params![max_id], record_of)?
                .collect::<Result<_, _>>()?;
            self.judge_rows(&tx, &late)?;
            let upto: i64 = tx.query_row("SELECT coalesce(max(id), 0) FROM events", [], |r| r.get(0))?;
            tx.execute_batch(
                "DELETE FROM alerts;
                 INSERT INTO alerts (event_id, rule, severity) SELECT a.event_id, a.rule, a.severity FROM rc_alerts a JOIN events e ON e.id=a.event_id;
                 DELETE FROM stories;
                 INSERT INTO stories (id, cluster, actor_key, via, severity, first_ms, last_ms, count)
                   SELECT id, cluster, actor_key, via, severity, first_ms, last_ms, count FROM rc_stories;
                 UPDATE events SET story_id=NULL WHERE story_id IS NOT NULL;
                 UPDATE events SET story_id=(SELECT story_id FROM rc_story_of WHERE event_id=events.id) WHERE id IN (SELECT event_id FROM rc_story_of);
                 DELETE FROM hourly;
                 INSERT INTO hourly (cluster, hour_ms, actor_key, human, writes, alerts)
                   SELECT cluster, hour_ms, actor_key, human, writes, alerts FROM rc_hourly;",
            )?;
            tx.execute_batch(SHADOW)?;
            let (from_ms, to_ms): (Option<i64>, Option<i64>) = tx.query_row("SELECT min(ts_ms), max(ts_ms) FROM events", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
            let n = done + late.len() as i64;
            tx.execute(
                "UPDATE recomputes SET finished_ms=?2, events=?3, upto_event_id=?4, from_ms=?5, to_ms=?6 WHERE id=?1",
                params![run_id, started_ms + started.elapsed().as_millis() as i64, n, upto, from_ms, to_ms],
            )?;
            tx.commit()?;
            Ok(n)
        };
        let n = swap().map_err(&fail)?;
        self.set_progress(n, n);
        tracing::info!(records = n, seconds = started.elapsed().as_secs(), "stored records judged again");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::{parse_body, to_record};
    use crate::config::Config;
    use std::sync::Arc;

    fn exec(id: &str, user: &str, ns: &str, ts: &str) -> Record {
        let ev = format!(r#"{{"auditID":"{id}","stage":"ResponseStarted","verb":"create",
          "user":{{"username":"system:serviceaccount:cattle-impersonation-system:cattle-impersonation-u-1"}},
          "impersonatedUser":{{"username":"{user}","extra":{{"username":["{user}"]}}}},
          "objectRef":{{"resource":"pods","subresource":"exec","namespace":"{ns}","name":"web-1"}},
          "responseStatus":{{"code":101}},"stageTimestamp":"{ts}"}}"#);
        to_record(&parse_body(ev.replace('\n', " ").as_bytes()).unwrap()[0]).unwrap()
    }

    fn rancher_put(id: &str, ts: &str) -> Record {
        let line = format!(r#"{{"auditID":"{id}","requestURI":"/k8s/clusters/c1/v1/apps.deployments/shop/web","method":"PATCH",
          "user":{{"name":"u-1","extra":{{"username":["alice"]}}}},"responseCode":200,"responseTimestamp":"{ts}"}}"#);
        crate::rancher::to_record(&crate::rancher::parse_fixture(&line)[0]).unwrap()
    }

    fn patch(id: &str, ts: &str) -> Record {
        let ev = format!(r#"{{"auditID":"{id}","stage":"ResponseComplete","verb":"patch",
          "user":{{"username":"system:serviceaccount:cattle-impersonation-system:cattle-impersonation-u-1"}},
          "impersonatedUser":{{"username":"u-1","extra":{{"username":["alice"]}}}},
          "objectRef":{{"resource":"deployments","namespace":"shop","name":"web","apiGroup":"apps"}},
          "responseStatus":{{"code":200}},"stageTimestamp":"{ts}"}}"#);
        to_record(&parse_body(ev.replace('\n', " ").as_bytes()).unwrap()[0]).unwrap()
    }

    fn with_rules(json: &str) -> Arc<Config> {
        let mut cfg = Config::defaults();
        cfg.rules = crate::rules::RuleSet::parse(json, &cfg.tz).unwrap();
        Arc::new(cfg)
    }

    /// Everything alert-derived, independent of story IDs.
    fn snapshot(s: &Store) -> (Vec<(i64, String, String)>, Vec<(String, String, String, i64, i64, i64, Vec<i64>)>, Vec<(String, i64, String, i64, i64, i64)>) {
        let c = s.conn.lock().unwrap();
        let alerts = c.prepare("SELECT event_id, rule, severity FROM alerts ORDER BY event_id, rule").unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        let stories = c.prepare("SELECT id, cluster, actor_key, severity, first_ms, last_ms, count FROM stories ORDER BY first_ms, actor_key").unwrap()
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))).unwrap()
            .collect::<Result<Vec<(i64, String, String, String, i64, i64, i64)>, _>>().unwrap()
            .into_iter()
            .map(|(id, cl, k, sev, f, l, n)| {
                let evs = c.prepare("SELECT id FROM events WHERE story_id=?1 ORDER BY id").unwrap()
                    .query_map([id], |r| r.get(0)).unwrap().collect::<Result<Vec<i64>, _>>().unwrap();
                (cl, k, sev, f, l, n, evs)
            })
            .collect();
        let hourly = c.prepare("SELECT cluster, hour_ms, actor_key, human, writes, alerts FROM hourly ORDER BY 1,2,3").unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        (alerts, stories, hourly)
    }

    fn mixed(s: &Store) {
        // Pairs (the Rancher half arriving first and last), exec sessions, plain changes.
        s.insert_event("local", &rancher_put("r1", "2026-10-14T02:00:01Z")).unwrap();
        s.insert_event("c1", &patch("k1", "2026-10-14T02:00:00.900Z")).unwrap();
        s.insert_event("c1", &patch("k2", "2026-10-14T03:00:00Z")).unwrap();
        s.insert_event("local", &rancher_put("r2", "2026-10-14T03:00:01Z")).unwrap();
        s.insert_event("c1", &exec("e1", "alice", "shop", "2026-10-14T02:03:00Z")).unwrap();
        s.insert_event("c1", &exec("e2", "alice", "dev-a", "2026-10-14T02:05:00Z")).unwrap();
        s.insert_event("c1", &exec("e3", "bob", "shop", "2026-10-14T13:00:00Z")).unwrap();
        s.insert_event("c1", &patch("k3", "2026-10-14T14:00:00Z")).unwrap();
    }

    fn wait(s: &Store) -> Job {
        for _ in 0..500 {
            if let Some(j) = s.job.lock().unwrap().clone()
                && j.state != "running" {
                    return j;
                }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("re-judging did not finish");
    }

    #[test]
    fn judging_again_with_the_same_settings_changes_nothing() {
        let s = Store::open(":memory:").unwrap();
        mixed(&s);
        let before = snapshot(&s);
        assert!(!before.0.is_empty() && !before.1.is_empty(), "the data has alerts and stories");
        s.recompute().unwrap();
        assert_eq!(snapshot(&s), before);
    }

    #[test]
    fn new_settings_apply_to_new_records_and_old_ones_on_request() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let path = path.to_str().unwrap();
        let s = Store::open_with(path, with_rules("")).unwrap();
        mixed(&s);
        let medium = |s: &Store| snapshot(s).0.iter().filter(|a| a.1 == "pod_exec").map(|a| a.2.clone()).collect::<Vec<_>>();
        assert_eq!(medium(&s), vec!["medium"; 3]);
        assert!(s.rule_status().unwrap().pending.is_none());
        let chain = s.verify_chain(None).unwrap();
        drop(s);

        // Restart with exec made high and dev namespaces exempt.
        let s = Store::open_with(path, with_rules(r#"{"pod_exec": {"severity": "high", "except": [{"namespaces": ["dev-*"], "reason": "debugging"}]}}"#)).unwrap();
        let st = s.rule_status().unwrap();
        let p = st.pending.expect("stored records were judged by the old settings");
        assert_eq!(p.events, 8);
        assert_eq!(st.history.len(), 2);
        assert_eq!(medium(&s), vec!["medium"; 3], "old judgements stay until asked");
        // A record arriving now is judged by the new settings; the old ones are still pending.
        s.insert_event("c1", &exec("e4", "carol", "shop", "2026-10-15T02:00:00Z")).unwrap();
        assert_eq!(s.rule_status().unwrap().pending.unwrap().events, 8);

        assert!(s.start_recompute().unwrap());
        let j = wait(&s);
        assert_eq!((j.state, j.done, j.total), ("done", 9, 9));
        assert_eq!(medium(&s), vec!["high"; 3], "e1, e3 and e4; e2 is exempt now");
        let st = s.rule_status().unwrap();
        assert!(st.pending.is_none());
        assert_eq!(st.recomputes.len(), 1);
        assert!(st.recomputes[0].error.is_none() && st.recomputes[0].from_ms.is_some());
        let stories = snapshot(&s).1;
        assert!(stories.iter().all(|x| x.2 == "high"), "{stories:?}");
        // The records and their chain are untouched.
        let after = s.verify_chain(None).unwrap();
        assert!(after.iter().all(|c| c.first_bad_id.is_none()));
        assert_eq!(after.iter().map(|c| c.verified).sum::<i64>(), chain.iter().map(|c| c.verified).sum::<i64>() + 1);
        assert!(!s.start_recompute().unwrap(), "nothing left to judge again");
    }

    #[test]
    fn going_back_to_earlier_settings_leaves_only_the_records_judged_in_between() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let path = path.to_str().unwrap();
        let a = "";
        let b = r#"{"secret_read": {"enabled": false}}"#;
        let s = Store::open_with(path, with_rules(a)).unwrap();
        s.insert_event("c1", &patch("k1", "2026-10-14T02:00:00Z")).unwrap();
        drop(s);
        let s = Store::open_with(path, with_rules(b)).unwrap();
        s.insert_event("c1", &patch("k2", "2026-10-14T03:00:00Z")).unwrap();
        s.insert_event("c1", &patch("k3", "2026-10-14T04:00:00Z")).unwrap();
        assert_eq!(s.rule_status().unwrap().pending.unwrap().events, 1);
        drop(s);
        let s = Store::open_with(path, with_rules(a)).unwrap();
        assert_eq!(s.rule_status().unwrap().pending.unwrap().events, 2, "only k2 and k3");
        // Restarting with the same settings is not a change.
        drop(s);
        let s = Store::open_with(path, with_rules(a)).unwrap();
        assert_eq!(s.rule_status().unwrap().history.len(), 3);
    }

    #[test]
    fn a_run_cut_off_by_a_restart_is_marked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let path = path.to_str().unwrap();
        let s = Store::open(path).unwrap();
        {
            let c = s.conn.lock().unwrap();
            c.execute("INSERT INTO recomputes (settings_id, started_ms) VALUES (1, 1)", []).unwrap();
            c.execute_batch(SHADOW_CREATE).unwrap();
        }
        drop(s);
        let s = Store::open(path).unwrap();
        let st = s.rule_status().unwrap();
        assert_eq!(st.recomputes[0].error.as_deref(), Some("stopped by a restart"));
        let left: i64 = s.conn.lock().unwrap().query_row("SELECT count(*) FROM sqlite_master WHERE name LIKE 'rc\\_%' ESCAPE '\\'", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 0, "shadow tables are dropped");
    }

    /// Timing for the estimate shown before re-judging: `cargo test --release -- --ignored judging_speed --nocapture`.
    #[test]
    #[ignore]
    fn judging_speed() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().join("db").to_str().unwrap()).unwrap();
        let fleet = serde_json::json!({"metadata":{"name":"web","namespace":"shop","labels":{"fleet.cattle.io/managed-by-kind":"gitrepo"}}});
        let key = crate::store::ObjKey { cluster: "c1".into(), api_group: "apps".into(), resource: "deployments".into(), namespace: "shop".into(), name: "web".into() };
        for n in 0..200 {
            s.insert_version(&key, "Deployment", "u1", &fleet, if n == 0 { "baseline" } else { "modified" }, chrono::Utc::now() - chrono::Duration::hours(30 - n / 10)).unwrap();
        }
        let base = chrono::Utc::now().timestamp_millis() - 86_400_000;
        let mut batch = Vec::new();
        for i in 0..60_000i64 {
            let ts = chrono::DateTime::from_timestamp_millis(base + i * 1000).unwrap().to_rfc3339();
            batch.push(if i % 10 == 0 { exec(&format!("e{i}"), "alice", "shop", &ts) } else { patch(&format!("k{i}"), &ts) });
            if batch.len() == 1000 {
                s.insert_events("c1", &batch).unwrap();
                batch.clear();
            }
        }
        let t = std::time::Instant::now();
        s.recompute().unwrap();
        let secs = t.elapsed().as_secs_f64();
        println!("judged 60000 records in {secs:.2}s: {:.0}/s", 60_000.0 / secs);
    }
}
