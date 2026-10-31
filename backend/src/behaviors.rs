//! Behaviors for the change map v6 (`docs/api/behaviors-v0.md`): every operation of one
//! identity on one cluster in one target namespace, cut where two operations are more than
//! the join gap apart or a collection gap starts between them.
//!
//! The cutting is kept at write time, like `stories`: each operation the page could list goes
//! into `behavior_ops` with the behavior it belongs to (or, for operations that are only
//! counted, none and a kind), and `behaviors` holds each behavior's key and first and last
//! time. A page view reads the behaviors of the day and the operations of those it shows, by
//! index; it never cuts anything or reads the whole day of records.
//!
//! Changes nobody could be found for (versions without an audit record) have no record to hang
//! on; there are a few hundred a day, read from the versions index and cut by the same rule
//! when the page is built.
//!
//! What goes where does not depend on the identity classes or the tier settings: those are
//! applied when the page is read, so a whitelist change needs no rebuild. It does depend on
//! the join gap, the failed-request setting and the alerts (a read is an operation only with a
//! medium or high alert), so the tables are rebuilt when rules are judged again
//! (`rulestate::recompute`) and on the first start with different settings (`ensure`).

use crate::changemap::{application_namespace, day_start, is_write, kind_from_resource, request_only, ui_maintenance, NAMESPACE_CONTROLLER};
use crate::store::Store;
use chrono::{NaiveDate, Offset, TimeZone, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Mutex;

/// Raised when the way operations are cut changes, so stored behaviors are built again.
const ALGORITHM: u32 = 1;
/// Records built in place (not in the background) up to this many.
const BUILD_INLINE: i64 = 50_000;
/// Records cut per lock hold in a background build.
const CHUNK: i64 = 5000;
/// Operations and objects a detail lists at most.
const DETAIL_MAX: usize = 500;
/// Objects listed with each behavior on the page.
const TOP_OBJECTS: usize = 20;

// ---------------------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub cell_minutes: i64,
    pub join_minutes: i64,
    pub drop_failed: bool,
    pub look: Vec<String>,
    pub notable: Vec<String>,
    pub tag_only: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            cell_minutes: 10,
            join_minutes: 60,
            drop_failed: true,
            look: vec!["high".into()],
            notable: vec!["medium".into()],
            tag_only: vec!["bypassed_rancher".into(), "off_hours".into()],
        }
    }
}

fn list(v: &str) -> Vec<String> {
    v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

impl Settings {
    pub fn from_env() -> anyhow::Result<Settings> {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let d = Settings::default();
        let s = Settings {
            cell_minutes: match get("AL_BEHAVIOR_CELL_MINUTES") {
                Some(v) => v.trim().parse().map_err(|_| anyhow::anyhow!("AL_BEHAVIOR_CELL_MINUTES: {v:?} is not a whole number of minutes"))?,
                None => d.cell_minutes,
            },
            join_minutes: match get("AL_BEHAVIOR_JOIN_MINUTES") {
                Some(v) => v.trim().parse().map_err(|_| anyhow::anyhow!("AL_BEHAVIOR_JOIN_MINUTES: {v:?} is not a whole number of minutes"))?,
                None => d.join_minutes,
            },
            drop_failed: match get("AL_BEHAVIOR_DROP_FAILED").map(|v| v.trim().to_ascii_lowercase()) {
                None => d.drop_failed,
                Some(v) if v == "true" || v == "1" => true,
                Some(v) if v == "false" || v == "0" => false,
                Some(v) => anyhow::bail!("AL_BEHAVIOR_DROP_FAILED: {v:?} is not true or false"),
            },
            look: get("AL_BEHAVIOR_LOOK").map(|v| list(&v)).unwrap_or(d.look),
            notable: get("AL_BEHAVIOR_NOTABLE").map(|v| list(&v)).unwrap_or(d.notable),
            tag_only: get("AL_BEHAVIOR_TAG_ONLY").map(|v| list(&v)).unwrap_or(d.tag_only),
        };
        s.check()?;
        Ok(s)
    }

    pub fn check(&self) -> anyhow::Result<()> {
        if !(1..=1440).contains(&self.cell_minutes) || 1440 % self.cell_minutes != 0 {
            anyhow::bail!("AL_BEHAVIOR_CELL_MINUTES: {} does not divide a day (1440 minutes), as 5, 10 or 15", self.cell_minutes);
        }
        if !(1..=1440).contains(&self.join_minutes) {
            anyhow::bail!("AL_BEHAVIOR_JOIN_MINUTES: {} is not 1 to 1440", self.join_minutes);
        }
        for (k, v) in [("AL_BEHAVIOR_LOOK", &self.look), ("AL_BEHAVIOR_NOTABLE", &self.notable)] {
            if let Some(bad) = v.iter().find(|s| !matches!(s.as_str(), "high" | "medium" | "low")) {
                anyhow::bail!("{k}: {bad:?} is not high, medium or low");
            }
        }
        if let Some(bad) = self.tag_only.iter().find(|r| !crate::rules::RULES.iter().any(|(id, _)| id == r)) {
            anyhow::bail!("AL_BEHAVIOR_TAG_ONLY: {bad:?} is not a rule");
        }
        Ok(())
    }

    pub fn join_ms(&self) -> i64 {
        self.join_minutes * 60_000
    }

    pub fn cell_ms(&self) -> i64 {
        self.cell_minutes * 60_000
    }

    /// What the stored cutting depends on; the tiers are applied when reading.
    fn fingerprint(&self) -> String {
        format!("v{ALGORITHM};join={};dropFailed={}", self.join_minutes, self.drop_failed)
    }

    fn json(&self) -> Value {
        json!({"cellMinutes": self.cell_minutes, "joinMinutes": self.join_minutes, "dropFailed": self.drop_failed,
               "look": self.look, "notable": self.notable, "tagOnly": self.tag_only})
    }
}

// ---------------------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------------------

/// The live tables, or the shadow ones a rebuild fills before swapping them in.
#[derive(Clone, Copy)]
pub(crate) struct Tb {
    pub behaviors: &'static str,
    pub ops: &'static str,
}

pub(crate) const LIVE: Tb = Tb { behaviors: "behaviors", ops: "behavior_ops" };
/// Filled while rules are judged again (`rulestate::recompute`).
pub(crate) const RECOMPUTE: Tb = Tb { behaviors: "rc_behaviors", ops: "rc_behavior_ops" };
/// Filled by the build on a first start (`ensure`).
const BUILD: Tb = Tb { behaviors: "rb_behaviors", ops: "rb_behavior_ops" };

fn ddl(tb: Tb, if_not: bool) -> String {
    let ine = if if_not { "IF NOT EXISTS " } else { "" };
    format!(
        "CREATE TABLE {ine}{b} (
           id INTEGER PRIMARY KEY, cluster TEXT NOT NULL, actor_key TEXT NOT NULL, actor TEXT NOT NULL, user_id TEXT NOT NULL,
           scope TEXT NOT NULL, first_ms INTEGER NOT NULL, last_ms INTEGER NOT NULL, ops INTEGER NOT NULL);
         CREATE INDEX {ine}{b}_key ON {b}(cluster, actor_key, scope, last_ms);
         CREATE INDEX {ine}{b}_time ON {b}(last_ms);
         CREATE TABLE {ine}{o} (event_id INTEGER PRIMARY KEY, behavior_id INTEGER, cluster TEXT NOT NULL, t INTEGER NOT NULL, kind TEXT NOT NULL);
         CREATE INDEX {ine}{o}_of ON {o}(behavior_id, t) WHERE behavior_id IS NOT NULL;
         CREATE INDEX {ine}{o}_counted ON {o}(t) WHERE behavior_id IS NULL;",
        b = tb.behaviors,
        o = tb.ops
    )
}

fn drop_sql(tb: Tb) -> String {
    format!("DROP TABLE IF EXISTS {}; DROP TABLE IF EXISTS {};", tb.behaviors, tb.ops)
}

/// Create the tables. A build cut off by a restart left its shadow tables; they go.
pub fn prepare(c: &Connection) -> anyhow::Result<()> {
    c.execute_batch(&ddl(LIVE, true))?;
    c.execute_batch(&drop_sql(BUILD))?;
    c.execute_batch(&drop_sql(RECOMPUTE))?;
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS behavior_state (k TEXT PRIMARY KEY, v TEXT NOT NULL);
         CREATE INDEX IF NOT EXISTS gaps_start ON gaps(cluster, from_ms);",
    )?;
    Ok(())
}

fn state(c: &Connection, k: &str) -> anyhow::Result<Option<String>> {
    Ok(c.query_row("SELECT v FROM behavior_state WHERE k=?1", params![k], |r| r.get(0)).optional()?)
}

fn set_state(c: &Connection, k: &str, v: &str) -> anyhow::Result<()> {
    c.execute("INSERT INTO behavior_state (k, v) VALUES (?1, ?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v", params![k, v])?;
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Write side
// ---------------------------------------------------------------------------------------

/// The fields of a record that decide where it goes.
pub(crate) struct OpIn<'a> {
    pub t: i64,
    pub source: &'a str,
    pub verb: &'a str,
    pub sub: &'a str,
    pub api_group: &'a str,
    pub resource: &'a str,
    pub namespace: &'a str,
    pub name: &'a str,
    pub code: i64,
    pub actor: &'a str,
    pub user_id: &'a str,
}

impl<'a> OpIn<'a> {
    pub fn of(r: &'a crate::audit::Record) -> OpIn<'a> {
        OpIn {
            t: r.ts.timestamp_millis(), source: r.source.as_str(), verb: &r.verb, sub: &r.subresource, api_group: &r.api_group,
            resource: &r.resource, namespace: &r.namespace, name: &r.name, code: r.code, actor: &r.actor, user_id: &r.user_id,
        }
    }
}

/// What a record is to the behaviors: an operation ("op"), only counted ("request", "ui",
/// "failed"), or nothing (a read without a medium or high alert). The same records the change
/// map lists, in the same order of checks.
fn kind_of(o: &OpIn, flagged: bool, s: &Settings) -> Option<&'static str> {
    if !is_write(o.source, o.verb, o.sub) && !flagged {
        return None;
    }
    let name_ref = if o.name.is_empty() { crate::changemap::BATCH } else { o.name };
    if request_only(o.resource, o.verb, o.name) {
        return Some("request");
    }
    if ui_maintenance(o.resource, name_ref) {
        return Some("ui");
    }
    // A refused request with a medium or high alert stays: the alert is about the refusal.
    if s.drop_failed && o.code >= 400 && !flagged {
        return Some("failed");
    }
    Some("op")
}

/// The target namespace: a Namespace counts as its own, the namespace controller emptying a
/// deleted namespace counts there, other cluster-scoped objects are "".
pub(crate) fn scope_of<'a>(verb: &str, actor: &str, api_group: &str, resource: &str, namespace: &'a str, name: &'a str) -> &'a str {
    if verb == "deletecollection" && actor == NAMESPACE_CONTROLLER && !namespace.is_empty() {
        return namespace;
    }
    if resource == "namespaces" && api_group.is_empty() && !name.is_empty() {
        return name;
    }
    namespace
}

/// Whether a collection gap of the cluster starts at or after `a` and before `b`: the
/// operation at `a` is before it, the one at `b` after it.
fn gap_starts(c: &Connection, cluster: &str, a: i64, b: i64) -> anyhow::Result<bool> {
    Ok(c.prepare_cached("SELECT EXISTS (SELECT 1 FROM gaps WHERE cluster=?1 AND from_ms >= ?2 AND from_ms < ?3 AND reason <> ?4)")?
        .query_row(params![cluster, a, b, crate::store::START], |r| r.get(0))?)
}

/// Put one stored record where it belongs. `flagged`: it has a medium or high alert.
pub(crate) fn add(c: &Connection, tb: Tb, s: &Settings, event_id: i64, cluster: &str, o: &OpIn, flagged: bool) -> anyhow::Result<()> {
    let Some(kind) = kind_of(o, flagged, s) else { return Ok(()) };
    if kind != "op" {
        c.prepare_cached(&format!("INSERT OR REPLACE INTO {} (event_id, behavior_id, cluster, t, kind) VALUES (?1, NULL, ?2, ?3, ?4)", tb.ops))?
            .execute(params![event_id, cluster, o.t, kind])?;
        return Ok(());
    }
    let key = if o.user_id.is_empty() { o.actor } else { o.user_id };
    let scope = scope_of(o.verb, o.actor, o.api_group, o.resource, o.namespace, o.name);
    let (t, join) = (o.t, s.join_ms());
    let near: Vec<(i64, i64, i64)> = c
        .prepare_cached(&format!(
            "SELECT id, first_ms, last_ms FROM {} WHERE cluster=?1 AND actor_key=?2 AND scope=?3 AND last_ms >= ?4 AND first_ms <= ?5 ORDER BY id",
            tb.behaviors
        ))?
        .query_map(params![cluster, key, scope, t - join, t + join], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    let mut join_to = Vec::new();
    for (id, first, last) in near {
        let ok = if t > last {
            !gap_starts(c, cluster, last, t)?
        } else if t < first {
            !gap_starts(c, cluster, t, first)?
        } else {
            true
        };
        if ok {
            join_to.push(id);
        }
    }
    let bid = match join_to.split_first() {
        None => {
            c.prepare_cached(&format!(
                "INSERT INTO {} (cluster, actor_key, actor, user_id, scope, first_ms, last_ms, ops) VALUES (?1,?2,?3,?4,?5,?6,?6,0)",
                tb.behaviors
            ))?
            .execute(params![cluster, key, o.actor, o.user_id, scope, t])?;
            c.last_insert_rowid()
        }
        Some((keep, rest)) => {
            // This record joins behaviors that were apart: they become one, under the smaller ID.
            for other in rest {
                c.prepare_cached(&format!("UPDATE {} SET behavior_id=?1 WHERE behavior_id=?2", tb.ops))?.execute(params![keep, other])?;
                c.prepare_cached(&format!(
                    "UPDATE {b} SET first_ms=min(first_ms, (SELECT first_ms FROM {b} WHERE id=?2)),
                       actor=CASE WHEN (SELECT last_ms FROM {b} WHERE id=?2) > last_ms THEN (SELECT actor FROM {b} WHERE id=?2) ELSE actor END,
                       last_ms=max(last_ms, (SELECT last_ms FROM {b} WHERE id=?2)), ops=ops+(SELECT ops FROM {b} WHERE id=?2) WHERE id=?1",
                    b = tb.behaviors
                ))?
                .execute(params![keep, other])?;
                c.prepare_cached(&format!("DELETE FROM {} WHERE id=?1", tb.behaviors))?.execute(params![other])?;
            }
            *keep
        }
    };
    c.prepare_cached(&format!(
        "UPDATE {} SET actor=CASE WHEN ?2 >= last_ms THEN ?3 ELSE actor END, first_ms=min(first_ms, ?2), last_ms=max(last_ms, ?2), ops=ops+1 WHERE id=?1",
        tb.behaviors
    ))?
    .execute(params![bid, t, o.actor])?;
    c.prepare_cached(&format!("INSERT OR REPLACE INTO {} (event_id, behavior_id, cluster, t, kind) VALUES (?1, ?2, ?3, ?4, 'op')", tb.ops))?
        .execute(params![event_id, bid, cluster, t])?;
    Ok(())
}

/// Count, first and last time of a behavior again from its operations; gone when it has none.
fn refresh(c: &Connection, tb: Tb, id: i64) -> anyhow::Result<()> {
    c.prepare_cached(&format!(
        "UPDATE {b} SET ops=(SELECT count(*) FROM {o} WHERE behavior_id=?1), first_ms=coalesce((SELECT min(t) FROM {o} WHERE behavior_id=?1), first_ms),
           last_ms=coalesce((SELECT max(t) FROM {o} WHERE behavior_id=?1), last_ms) WHERE id=?1",
        b = tb.behaviors,
        o = tb.ops
    ))?
    .execute(params![id])?;
    c.prepare_cached(&format!("DELETE FROM {} WHERE id=?1 AND ops<=0", tb.behaviors))?.execute(params![id])?;
    Ok(())
}

/// Take a record back out (a Rancher half that turned out to be shown through its downstream
/// record). The downstream half arrives within seconds of it and takes its place, so a
/// behavior it held together is not cut again here.
pub(crate) fn remove(c: &Connection, tb: Tb, event_id: i64) -> anyhow::Result<()> {
    let bid: Option<Option<i64>> =
        c.prepare_cached(&format!("SELECT behavior_id FROM {} WHERE event_id=?1", tb.ops))?.query_row(params![event_id], |r| r.get(0)).optional()?;
    c.prepare_cached(&format!("DELETE FROM {} WHERE event_id=?1", tb.ops))?.execute(params![event_id])?;
    if let Some(Some(b)) = bid {
        refresh(c, tb, b)?;
    }
    Ok(())
}

/// Cut every behavior of the cluster that runs across the start of a gap: what comes after it
/// becomes a behavior of its own.
fn split_at(c: &Connection, tb: Tb, cluster: &str, g: i64) -> anyhow::Result<()> {
    let hit: Vec<(i64, String, String, String, String)> = c
        .prepare_cached(&format!("SELECT id, actor_key, actor, user_id, scope FROM {} WHERE cluster=?1 AND first_ms <= ?2 AND last_ms > ?2", tb.behaviors))?
        .query_map(params![cluster, g], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
        .collect::<Result<_, _>>()?;
    for (id, key, actor, uid, scope) in hit {
        c.prepare_cached(&format!(
            "INSERT INTO {} (cluster, actor_key, actor, user_id, scope, first_ms, last_ms, ops) VALUES (?1,?2,?3,?4,?5,?6,?6,0)",
            tb.behaviors
        ))?
        .execute(params![cluster, key, actor, uid, scope, g])?;
        let nid = c.last_insert_rowid();
        c.prepare_cached(&format!("UPDATE {} SET behavior_id=?1 WHERE behavior_id=?2 AND t > ?3", tb.ops))?.execute(params![nid, id, g])?;
        refresh(c, tb, id)?;
        refresh(c, tb, nid)?;
    }
    Ok(())
}

/// Cut behaviors at every gap written since the last call (gaps are written when a collector
/// checks in again, after the records around them may already be stored).
pub(crate) fn sync_gaps(c: &Connection) -> anyhow::Result<()> {
    let done: i64 = state(c, "gap_id")?.and_then(|v| v.parse().ok()).unwrap_or(0);
    let max: i64 = c.query_row("SELECT coalesce(max(id), 0) FROM gaps", [], |r| r.get(0))?;
    if max <= done {
        return Ok(());
    }
    split_gaps(c, LIVE, done, max)?;
    set_state(c, "gap_id", &max.to_string())
}

fn split_gaps(c: &Connection, tb: Tb, after_id: i64, upto_id: i64) -> anyhow::Result<()> {
    let gaps: Vec<(String, i64)> = c
        .prepare("SELECT cluster, from_ms FROM gaps WHERE id > ?1 AND id <= ?2 AND reason <> ?3 ORDER BY id")?
        .query_map(params![after_id, upto_id, crate::store::START], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (cl, g) in gaps {
        split_at(c, tb, &cl, g)?;
    }
    Ok(())
}

/// Records that arrived before `retention` cut: their operations go, and behaviors that began
/// before it start at their first kept operation.
pub(crate) fn prune(c: &Connection, cutoff: i64) -> anyhow::Result<()> {
    c.execute("DELETE FROM behavior_ops WHERE t < ?1", params![cutoff])?;
    c.execute("DELETE FROM behaviors WHERE last_ms < ?1", params![cutoff])?;
    let ids: Vec<i64> = c.prepare("SELECT id FROM behaviors WHERE first_ms < ?1")?.query_map(params![cutoff], |r| r.get(0))?.collect::<Result<_, _>>()?;
    for id in ids {
        refresh(c, LIVE, id)?;
    }
    Ok(())
}

// --- rebuilding -------------------------------------------------------------------------

const SELECT: &str = "SELECT e.id, e.cluster, e.ts_ms, e.source, e.verb, e.subresource, e.api_group, e.resource, e.namespace, e.name, e.code,
        e.actor, e.user_id, e.source='rancher' AND e.link_id IS NOT NULL";

type Row = (i64, String, i64, String, String, String, String, String, String, String, i64, String, String, bool, bool);

fn rows(c: &Connection, sql: &str, p: impl rusqlite::Params) -> anyhow::Result<Vec<Row>> {
    let mut st = c.prepare_cached(sql)?;
    let v = st
        .query_map(p, |r| {
            Ok((
                r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?,
                r.get(11)?, r.get(12)?, r.get(13)?, r.get(14)?,
            ))
        })?
        .collect::<Result<_, _>>()?;
    Ok(v)
}

fn add_rows(c: &Connection, tb: Tb, s: &Settings, rs: &[Row]) -> anyhow::Result<()> {
    for (id, cl, t, source, verb, sub, group, res, ns, name, code, actor, uid, hidden_half, flagged) in rs {
        if *hidden_half {
            continue;
        }
        let o = OpIn { t: *t, source, verb, sub, api_group: group, resource: res, namespace: ns, name, code: *code, actor, user_id: uid };
        add(c, tb, s, *id, cl, &o, *flagged)?;
    }
    Ok(())
}

/// When a build started, per database connection, while it runs.
static BUILDING: Mutex<Option<HashMap<usize, i64>>> = Mutex::new(None);

fn conn_id(store: &Store) -> usize {
    std::sync::Arc::as_ptr(&store.conn) as usize
}

pub fn building_since(store: &Store) -> Option<i64> {
    BUILDING.lock().unwrap().as_ref().and_then(|m| m.get(&conn_id(store)).copied())
}

fn set_building(store: &Store, on: bool) -> bool {
    let mut g = BUILDING.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::new);
    if on {
        if m.contains_key(&conn_id(store)) {
            return false;
        }
        m.insert(conn_id(store), Utc::now().timestamp_millis());
    } else {
        m.remove(&conn_id(store));
    }
    true
}

/// Make sure the tables were cut with the current settings; build them once if not (the first
/// start after upgrading, or a changed join gap or failed-request setting). Small databases
/// are built in place; larger ones in the background, the page saying so meanwhile.
pub fn ensure(store: &Store) -> anyhow::Result<()> {
    let fp = store.cfg.behaviors.fingerprint();
    let n: i64 = {
        let c = store.conn.lock().unwrap();
        if state(&c, "fingerprint")?.as_deref() == Some(fp.as_str()) {
            return Ok(());
        }
        c.query_row("SELECT count(*) FROM events", [], |r| r.get(0))?
    };
    if !set_building(store, true) {
        return Ok(());
    }
    if n <= BUILD_INLINE {
        let r = build(store);
        set_building(store, false);
        return r;
    }
    tracing::info!(records = n, "building the behavior table in the background");
    let s = store.clone();
    std::thread::spawn(move || {
        let t0 = std::time::Instant::now();
        // Another build on its own connection (the change map index after an upgrade) can hold
        // the write lock longer than the busy timeout: try again rather than leave the page empty.
        for attempt in 1..=5 {
            match build(&s) {
                Ok(()) => {
                    tracing::info!(ms = t0.elapsed().as_millis() as u64, "behavior table ready");
                    break;
                }
                Err(e) if attempt < 5 => {
                    tracing::warn!(error = %e, attempt, "building the behavior table failed; trying again");
                    std::thread::sleep(std::time::Duration::from_secs(10));
                }
                Err(e) => tracing::warn!(error = %e, "building the behavior table failed; the behaviors page stays empty until the next start"),
            }
        }
        set_building(&s, false);
    });
    Ok(())
}

/// Cut every stored record into shadow tables, a chunk per lock hold, then swap them in with
/// whatever arrived meanwhile.
fn build(store: &Store) -> anyhow::Result<()> {
    let s = store.cfg.behaviors.clone();
    let flagged = "EXISTS (SELECT 1 FROM alerts a WHERE a.event_id=e.id AND a.severity IN ('high','medium'))";
    let (max_id, gap_id) = {
        let c = store.conn.lock().unwrap();
        c.execute_batch(&drop_sql(BUILD))?;
        c.execute_batch(&ddl(BUILD, false))?;
        let m: i64 = c.query_row("SELECT coalesce(max(id), 0) FROM events", [], |r| r.get(0))?;
        let g: i64 = c.query_row("SELECT coalesce(max(id), 0) FROM gaps", [], |r| r.get(0))?;
        (m, g)
    };
    let fail = |e: anyhow::Error| -> anyhow::Error {
        let _ = store.conn.lock().unwrap().execute_batch(&drop_sql(BUILD));
        e
    };
    let chunk_sql = format!(
        "{SELECT}, {flagged} FROM events e WHERE (e.ts_ms > ?1 OR (e.ts_ms = ?1 AND e.id > ?2)) AND e.id <= ?3 AND {} ORDER BY e.ts_ms, e.id LIMIT ?4",
        crate::shown::PREDICATE
    );
    let mut pos = (i64::MIN, i64::MIN);
    loop {
        let mut step = || -> anyhow::Result<usize> {
            let mut c = store.conn.lock().unwrap();
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let rs = rows(&tx, &chunk_sql, params![pos.0, pos.1, max_id, CHUNK])?;
            add_rows(&tx, BUILD, &s, &rs)?;
            tx.commit()?;
            if let Some(l) = rs.last() {
                pos = (l.2, l.0);
            }
            Ok(rs.len())
        };
        if step().map_err(fail)? < CHUNK as usize {
            break;
        }
        // A mutex is not fair: without a pause this thread takes the lock straight back and
        // ingest and page views wait for the whole build.
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let swap = || -> anyhow::Result<()> {
        let mut c = store.conn.lock().unwrap();
        // Judging again rebuilds these tables too; if it ran meanwhile, its result stands.
        if store.job.lock().unwrap().as_ref().is_some_and(|j| j.started_ms >= building_since(store).unwrap_or(i64::MAX) || j.state == "running") {
            c.execute_batch(&drop_sql(BUILD))?;
            return Ok(());
        }
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let late = rows(&tx, &format!("{SELECT}, {flagged} FROM events e WHERE e.id > ?1 AND {} ORDER BY e.ts_ms, e.id", crate::shown::PREDICATE), params![max_id])?;
        add_rows(&tx, BUILD, &s, &late)?;
        // A Rancher half cut early on its own may have been matched to its downstream record.
        let paired: Vec<i64> = tx
            .prepare(&format!("SELECT o.event_id FROM {} o JOIN events e ON e.id=o.event_id WHERE e.source='rancher' AND e.link_id IS NOT NULL", BUILD.ops))?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for id in paired {
            remove(&tx, BUILD, id)?;
        }
        let gmax: i64 = tx.query_row("SELECT coalesce(max(id), 0) FROM gaps", [], |r| r.get(0))?;
        split_gaps(&tx, BUILD, gap_id, gmax)?;
        swap_in(&tx, BUILD)?;
        set_state(&tx, "gap_id", &gmax.to_string())?;
        set_state(&tx, "fingerprint", &s.fingerprint())?;
        tx.commit()?;
        Ok(())
    };
    swap().map_err(fail)
}

/// Replace the live tables with a shadow pair, which is dropped.
fn swap_in(c: &Connection, tb: Tb) -> anyhow::Result<()> {
    c.execute_batch(&format!(
        "DELETE FROM behaviors; INSERT INTO behaviors SELECT * FROM {b};
         DELETE FROM behavior_ops; INSERT INTO behavior_ops SELECT * FROM {o};",
        b = tb.behaviors,
        o = tb.ops
    ))?;
    c.execute_batch(&drop_sql(tb))?;
    Ok(())
}

/// For `rulestate::recompute`: fresh shadow tables, and the last gap they already know.
pub(crate) fn recompute_start(c: &Connection) -> anyhow::Result<i64> {
    c.execute_batch(&drop_sql(RECOMPUTE))?;
    c.execute_batch(&ddl(RECOMPUTE, false))?;
    Ok(c.query_row("SELECT coalesce(max(id), 0) FROM gaps", [], |r| r.get(0))?)
}

/// For `rulestate::recompute`, holding the lock at its swap: cut at gaps written during the
/// run and swap the shadow tables in.
pub(crate) fn recompute_swap(c: &Connection, s: &Settings, gap_id: i64) -> anyhow::Result<()> {
    let gmax: i64 = c.query_row("SELECT coalesce(max(id), 0) FROM gaps", [], |r| r.get(0))?;
    split_gaps(c, RECOMPUTE, gap_id, gmax)?;
    swap_in(c, RECOMPUTE)?;
    set_state(c, "gap_id", &gmax.to_string())?;
    set_state(c, "fingerprint", &s.fingerprint())
}

pub(crate) fn recompute_drop(c: &Connection) {
    let _ = c.execute_batch(&drop_sql(RECOMPUTE));
}

// ---------------------------------------------------------------------------------------
// Read side
// ---------------------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct Params {
    pub day: Option<String>,
    pub cluster: Vec<String>,
    pub system: Option<String>,
    pub select: Option<String>,
    pub id: Option<String>,
}

pub fn parse_query(q: &str) -> Params {
    let mut p = Params::default();
    for pair in q.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = crate::changemap::decode(v);
        match k {
            "day" => p.day = Some(v),
            "cluster" => p.cluster.extend(v.split(',').filter(|s| !s.is_empty()).map(str::to_string)),
            "system" => p.system = Some(v),
            "select" => p.select = Some(v),
            "id" => p.id = Some(v),
            _ => {}
        }
    }
    p
}

/// The cell an instant falls in, aligned in the server's time zone: its number (cells next to
/// each other differ by one) and where it starts.
fn cell(tz: chrono_tz::Tz, t: i64, cell_ms: i64) -> (i64, i64) {
    let off = Utc.timestamp_millis_opt(t).single().map(|u| tz.offset_from_utc_datetime(&u.naive_utc()).fix().local_minus_utc() as i64 * 1000).unwrap_or(0);
    let n = (t + off).div_euclid(cell_ms);
    (n, n * cell_ms - off)
}

/// One operation as the page reads it.
#[derive(Clone, Debug)]
struct Op {
    t: i64,
    verb: String,
    api_group: String,
    resource: String,
    namespace: String,
    name: String,
    via: String,
    code: Option<i64>,
    /// (rule, severity)
    alerts: Vec<(String, String)>,
    cascade: Option<String>,
}

/// The verb the page shows: entering a container is named after the subresource whatever the
/// verb; other subresources only for a create (as `detail.ops[].verb` in the change map).
fn shown_verb(verb: &str, sub: &str) -> String {
    if matches!(sub, "exec" | "attach" | "portforward") || (!sub.is_empty() && verb == "create") { sub.to_string() } else { verb.to_string() }
}

fn sev_rank(s: &str) -> u8 {
    match s {
        "high" => 0,
        "medium" => 1,
        "low" => 2,
        _ => 3,
    }
}

const TIERS: [&str; 7] = ["look", "notable", "normal", "unrecorded", "unauthenticated", "routine", "system"];
fn tier_rank(t: &str) -> usize {
    TIERS.iter().position(|x| *x == t).unwrap_or(TIERS.len())
}
const GROUPS: [&str; 5] = ["people", "unrecorded", "unauthenticated", "routine", "system"];

/// Which group an identity is in (contract table), from its class and reason and the target.
fn group_of(class: &str, reason: &str, scope: &str, has_actor: bool) -> &'static str {
    if !has_actor {
        return "unrecorded";
    }
    match class {
        "human" => "people",
        "serviceaccount" => "routine",
        "rancher" if application_namespace(scope) => "routine",
        "rancher" | "component" => "system",
        _ if reason == "rancher-unauthenticated" => "unauthenticated",
        _ if reason == "no-actor" => "unrecorded",
        _ => "people",
    }
}

/// One behavior as built for the page.
struct Beh {
    id: String,
    cluster: String,
    actor_key: String,
    actor: Value,
    group: &'static str,
    scope: String,
    started_before: bool,
    continues_after: bool,
    ops: Vec<Op>,
}

struct Ctx {
    tz: chrono_tz::Tz,
    s: Settings,
    kinds: HashMap<(String, String, String), String>,
}

impl Ctx {
    fn kind(&self, cluster: &str, group: &str, resource: &str) -> String {
        self.kinds.get(&(cluster.to_string(), group.to_string(), resource.to_string())).cloned().unwrap_or_else(|| kind_from_resource(resource))
    }
}

/// The tier, and the operation the title names, by the settings.
fn tier_of(b: &Beh, s: &Settings) -> (&'static str, Option<usize>) {
    let mut best: Option<(u8, i64, usize, &'static str)> = None;
    for (i, op) in b.ops.iter().enumerate() {
        for (rule, sev) in &op.alerts {
            if s.tag_only.iter().any(|r| r == rule) {
                continue;
            }
            let tier = if s.look.iter().any(|x| x == sev) {
                "look"
            } else if s.notable.iter().any(|x| x == sev) {
                "notable"
            } else {
                continue;
            };
            let k = (tier_rank(tier) as u8, op.t, i, tier);
            if best.is_none_or(|b| (k.0, k.1) < (b.0, b.1)) {
                best = Some(k);
            }
        }
    }
    match best {
        Some((_, _, i, t)) => (t, Some(i)),
        None => (
            match b.group {
                "people" => "normal",
                "unrecorded" => "unrecorded",
                "unauthenticated" => "unauthenticated",
                "routine" => "routine",
                _ => "system",
            },
            None,
        ),
    }
}

fn op_json(ctx: &Ctx, cluster: &str, op: &Op) -> Value {
    let (rules, sev): (Vec<&String>, Option<&String>) =
        (op.alerts.iter().map(|(r, _)| r).collect(), op.alerts.iter().map(|(_, s)| s).min_by_key(|s| sev_rank(s)));
    json!({"t": op.t, "verb": op.verb, "kind": ctx.kind(cluster, &op.api_group, &op.resource), "apiGroup": op.api_group, "resource": op.resource,
           "namespace": op.namespace, "name": op.name, "via": op.via, "code": op.code, "severity": sev, "rules": rules, "cascade": op.cascade})
}

fn obj_key(op: &Op) -> (String, String, String, String) {
    (op.api_group.clone(), op.resource.clone(), op.namespace.clone(), if op.name.is_empty() { crate::changemap::BATCH.to_string() } else { op.name.clone() })
}

/// Distinct objects with their operation counts, most first.
fn objects(b: &Beh) -> Vec<((String, String, String, String), i64)> {
    let mut m: HashMap<(String, String, String, String), i64> = HashMap::new();
    for op in &b.ops {
        *m.entry(obj_key(op)).or_default() += 1;
    }
    let mut v: Vec<_> = m.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

fn behavior_json(ctx: &Ctx, b: &Beh) -> (Value, &'static str) {
    let (tier, head) = tier_of(b, &ctx.s);
    let cell_ms = ctx.s.cell_ms();
    // Runs of cells next to each other.
    let mut segs: Vec<(i64, i64, i64, i64, i64, i64)> = Vec::new(); // (first cell n, last cell n, cell start, firstMs, lastMs, ops)
    for op in &b.ops {
        let (n, start) = cell(ctx.tz, op.t, cell_ms);
        match segs.last_mut() {
            Some(s) if n <= s.1 + 1 => {
                s.1 = n;
                s.4 = op.t;
                s.5 += 1;
            }
            _ => segs.push((n, n, start, op.t, op.t, 1)),
        }
    }
    let segments: Vec<Value> = segs
        .iter()
        .map(|(n0, n1, start, f, l, k)| json!({"fromMs": start, "toMs": start + (n1 - n0 + 1) * cell_ms, "firstMs": f, "lastMs": l, "ops": k}))
        .collect();
    let mut what: HashMap<(String, String), i64> = HashMap::new();
    let mut rules: BTreeMap<(u8, String), (String, i64)> = BTreeMap::new();
    let mut via: BTreeSet<String> = BTreeSet::new();
    for op in &b.ops {
        *what.entry((op.verb.clone(), ctx.kind(&b.cluster, &op.api_group, &op.resource))).or_default() += 1;
        for (r, s) in &op.alerts {
            rules.entry((sev_rank(s), r.clone())).or_insert((s.clone(), 0)).1 += 1;
        }
        via.insert(op.via.clone());
    }
    let mut what: Vec<_> = what.into_iter().collect();
    what.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let is_tag = |r: &str| ctx.s.tag_only.iter().any(|x| x == r);
    let mut tags: Vec<String> = Vec::new();
    if b.group != "routine" && via.contains("direct") {
        tags.push("direct".into());
    }
    if via.contains("rancher") {
        tags.push("via-rancher".into());
    }
    for (_, r) in rules.keys() {
        if is_tag(r) {
            tags.push(r.replace('_', "-"));
        }
    }
    let objs = objects(b);
    let top: Vec<Value> = objs
        .iter()
        .take(TOP_OBJECTS)
        .map(|((g, r, ns, n), k)| json!({"kind": ctx.kind(&b.cluster, g, r), "apiGroup": g, "resource": r, "namespace": ns, "name": n, "ops": k}))
        .collect();
    let title = head.map(|i| {
        let op = &b.ops[i];
        let (rule, sev) = op
            .alerts
            .iter()
            .filter(|(r, _)| !is_tag(r))
            .min_by_key(|(_, s)| sev_rank(s))
            .cloned()
            .unwrap_or_default();
        json!({"verb": op.verb, "kind": ctx.kind(&b.cluster, &op.api_group, &op.resource), "apiGroup": op.api_group, "resource": op.resource,
               "namespace": op.namespace, "name": op.name, "t": op.t, "rule": rule, "severity": sev})
    });
    let v = json!({
        "id": b.id, "cluster": b.cluster, "actor": b.actor, "scope": b.scope, "tier": tier, "title": title,
        "what": what.iter().map(|((v, k), n)| json!({"verb": v, "kind": k, "count": n})).collect::<Vec<_>>(),
        "fromMs": b.ops.first().map(|o| o.t), "toMs": b.ops.last().map(|o| o.t),
        "startedBefore": b.started_before, "continuesAfter": b.continues_after,
        "segments": segments, "ops": b.ops.len(), "objects": objs.len(), "objectsTop": top,
        "via": via, "tags": tags,
        "rules": rules.iter().map(|((_, r), (s, n))| json!({"rule": r, "severity": s, "count": n, "tagOnly": is_tag(r)})).collect::<Vec<_>>(),
        "truncated": b.ops.len() > DETAIL_MAX,
    });
    (v, tier)
}

fn detail_json(ctx: &Ctx, b: &Beh) -> Value {
    let objs = objects(b);
    json!({
        "id": b.id,
        "ops": b.ops.iter().take(DETAIL_MAX).map(|o| op_json(ctx, &b.cluster, o)).collect::<Vec<_>>(),
        "opsTotal": b.ops.len(), "truncated": b.ops.len() > DETAIL_MAX,
        "objects": objs.iter().take(DETAIL_MAX).map(|((g, r, ns, n), k)| json!({
            "kind": ctx.kind(&b.cluster, g, r), "apiGroup": g, "resource": r, "namespace": ns, "name": n, "ops": k,
            "key": format!("{}/{g}/{r}/{ns}/{n}", b.cluster)})).collect::<Vec<_>>(),
        "objectsTotal": objs.len(),
    })
}

/// Everything a page view needs from the database, read under one lock.
struct Loaded {
    behs: Vec<Beh>,
    hidden: BTreeMap<&'static str, i64>,
    gaps: Vec<crate::changemap::GapRow>,
}

fn alerts_of(agg: Option<String>) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = agg
        .as_deref()
        .map(|s| s.split(',').filter_map(|x| x.split_once(':')).map(|(r, s)| (r.to_string(), s.to_string())).collect())
        .unwrap_or_default();
    v.sort();
    v.dedup();
    v
}

const OPS_SQL: &str = "SELECT b.t, e.verb, e.subresource, e.api_group, e.resource, e.namespace, e.name, e.via, e.code, e.actor,
        (SELECT group_concat(a.rule || ':' || a.severity) FROM alerts a WHERE a.event_id=e.id)
 FROM behavior_ops b JOIN events e ON e.id=b.event_id WHERE b.behavior_id=?1 AND b.t >= ?2 AND b.t < ?3 ORDER BY b.t, b.event_id";

fn stored_ops(c: &Connection, id: i64, start: i64, end: i64) -> anyhow::Result<Vec<Op>> {
    let mut st = c.prepare_cached(OPS_SQL)?;
    let v = st
        .query_map(params![id, start, end], |r| {
            let (verb, sub, actor): (String, String, String) = (r.get(1)?, r.get(2)?, r.get(9)?);
            let cascade = (verb == "deletecollection" && actor == NAMESPACE_CONTROLLER).then(|| r.get::<_, String>(4)).transpose()?;
            Ok(Op {
                t: r.get(0)?, verb: shown_verb(&verb, &sub), api_group: r.get(3)?, resource: r.get(4)?, namespace: r.get(5)?, name: r.get(6)?,
                via: r.get(7)?, code: Some(r.get(8)?), alerts: alerts_of(r.get(10)?), cascade,
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(v)
}

/// The identity of a behavior as the page shows it.
#[allow(clippy::too_many_arguments)]
fn actor_json(
    c: &Connection, idents: &HashMap<(String, String), (&'static str, String)>, collectors: &HashMap<String, BTreeSet<String>>,
    cluster: &str, key: &str, actor: &str, uid: &str, scope: &str,
) -> anyhow::Result<(Value, &'static str)> {
    if key.is_empty() && actor.is_empty() {
        let cols = collectors.get(cluster);
        let no_audit = cols.is_some_and(|c| !c.contains("file"));
        let code = if no_audit { "no-apiserver-audit" } else { "found-on-reconnect" };
        let note = if no_audit { "這座叢集沒有收 apiserver audit log：只看得到物件變了，看不到是誰改的" } else { "收集中斷後重新連線比對物件版本才發現的變更，找不到對應的 audit 紀錄" };
        let depends = no_audit.then(|| json!({"code": "no-apiserver-audit", "cluster": cluster, "collectors": cols.map(|c| c.iter().collect::<Vec<_>>()).unwrap_or_default()}));
        return Ok((json!({"id": format!("~{cluster}"), "key": "", "name": "", "category": "unknown", "group": "unrecorded", "noteCode": code, "note": note, "dependsOn": depends}), "unrecorded"));
    }
    let (class, reason) = match idents.get(&(actor.to_string(), uid.to_string())) {
        Some((c, r)) => (*c, r.clone()),
        None => {
            let p: Option<String> = c.query_row("SELECT principal_ids FROM users WHERE user_id=?1 ORDER BY created_ms DESC LIMIT 1", params![uid], |r| r.get(0)).optional()?;
            let (c, r) = crate::identity::builtin(actor, false, "", p.as_deref());
            (c, r.to_string())
        }
    };
    let group = group_of(class, &reason, scope, true);
    let name: Option<String> = if uid.is_empty() {
        None
    } else {
        c.prepare_cached("SELECT display_name FROM users WHERE user_id=?1 ORDER BY created_ms DESC LIMIT 1")?.query_row(params![uid], |r| r.get(0)).optional()?
    };
    Ok((
        json!({"id": key, "key": key, "name": name.unwrap_or_else(|| actor.to_string()), "category": class, "group": group,
               "noteCode": crate::identity::reason_code(&reason), "note": crate::identity::reason_text(&reason), "dependsOn": null}),
        group,
    ))
}

/// Changes nobody could be found for, cut by the same rule as stored behaviors: the day plus
/// the join gap on each side, so a behavior across midnight is seen to go on.
fn unrecorded(
    c: &Connection, ctx: &Ctx, clusters: &HashSet<&str>, start: i64, end: i64, collectors: &HashMap<String, BTreeSet<String>>, hidden_ui: &mut i64,
) -> anyhow::Result<Vec<Beh>> {
    let join = ctx.s.join_ms();
    let mut by_key: BTreeMap<(String, String), Vec<Op>> = BTreeMap::new();
    let mut st = c.prepare_cached(
        "SELECT cluster, observed_ms, api_group, resource, namespace, name, change FROM versions
         WHERE event_id IS NULL AND change <> 'baseline' AND observed_ms >= ?1 AND observed_ms < ?2 ORDER BY observed_ms, id",
    )?;
    let mut rows = st.query(params![start - join, end + join])?;
    while let Some(r) = rows.next()? {
        let cl: String = r.get(0)?;
        if !clusters.is_empty() && !clusters.contains(cl.as_str()) {
            continue;
        }
        let (t, g, res, ns, name, change): (i64, String, String, String, String, String) = (r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?);
        if ui_maintenance(&res, &name) {
            *hidden_ui += (t >= start && t < end) as i64;
            continue;
        }
        let scope = scope_of(&change, "", &g, &res, &ns, &name).to_string();
        by_key.entry((cl, scope)).or_default().push(Op {
            t, verb: change, api_group: g, resource: res, namespace: ns, name, via: "unknown".into(), code: None, alerts: vec![], cascade: None,
        });
    }
    drop(rows);
    let mut out = Vec::new();
    for ((cl, scope), ops) in by_key {
        let gaps: Vec<i64> = c
            .prepare_cached("SELECT from_ms FROM gaps WHERE cluster=?1 AND reason <> ?2 AND from_ms >= ?3 AND from_ms < ?4 ORDER BY from_ms")?
            .query_map(params![cl, crate::store::START, start - join, end + join], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for run in cut(&ops.iter().map(|o| o.t).collect::<Vec<_>>(), &gaps, join) {
            let all = &ops[run.clone()];
            let day: Vec<Op> = all.iter().filter(|o| o.t >= start && o.t < end).cloned().collect();
            if day.is_empty() {
                continue;
            }
            let (actor, group) = actor_json(c, &HashMap::new(), collectors, &cl, "", "", "", &scope)?;
            out.push(Beh {
                id: format!("u~{cl}~{scope}~{}", day[0].t), cluster: cl.clone(), actor_key: String::new(), actor, group, scope: scope.clone(),
                started_before: all[0].t < start, continues_after: all[all.len() - 1].t >= end, ops: day,
            });
        }
    }
    Ok(out)
}

/// Cut sorted times into behaviors: apart when more than `join` apart or a gap starts in
/// between (`a <= gap < b`). Returns index ranges.
pub(crate) fn cut(times: &[i64], gap_starts: &[i64], join: i64) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut from = 0;
    for i in 1..times.len() {
        let (a, b) = (times[i - 1], times[i]);
        if b - a > join || gap_starts.iter().any(|g| a <= *g && *g < b) {
            out.push(from..i);
            from = i;
        }
    }
    if !times.is_empty() {
        out.push(from..times.len());
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn load(store: &Store, ctx: &Ctx, clusters: &HashSet<&str>, with_system: bool, start: i64, end: i64, now_ms: i64, only: Option<&str>) -> anyhow::Result<Loaded> {
    let c = store.conn.lock().unwrap();
    let mut idents: HashMap<(String, String), (&'static str, String)> = HashMap::new();
    for row in c
        .prepare_cached("SELECT actor, user_id, class, reason FROM identities")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, crate::identity::class_name(&r.get::<_, String>(2)?), r.get::<_, String>(3)?)))?
    {
        let (a, u, cl, re) = row?;
        idents.insert((a, u), (cl, re));
    }
    let mut collectors: HashMap<String, BTreeSet<String>> = HashMap::new();
    for row in c.prepare_cached("SELECT cluster, collector FROM heartbeats")?.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (cl, col) = row?;
        collectors.entry(cl).or_default().insert(col);
    }
    let mut hidden: BTreeMap<&'static str, i64> = ["system", "uiMaintenance", "requestOnly", "failed", "failedSystem"].into_iter().map(|k| (k, 0)).collect();
    let mut behs = Vec::new();
    let only_stored = only.and_then(|id| id.strip_prefix('b')).and_then(|n| n.parse::<i64>().ok());
    type Stored = (i64, String, String, String, String, String, i64, i64);
    let stored: Vec<Stored> = c
        .prepare_cached(
            "SELECT id, cluster, actor_key, actor, user_id, scope, first_ms, last_ms FROM behaviors
             WHERE last_ms >= ?1 AND first_ms < ?2 AND (?3 IS NULL OR id = ?3) ORDER BY id",
        )?
        .query_map(params![start, end, only_stored], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)))?
        .collect::<Result<_, _>>()?;
    if only.is_none() || only_stored.is_some() {
        for (id, cl, key, actor, uid, scope, first, last) in stored {
            if !clusters.is_empty() && !clusters.contains(cl.as_str()) {
                continue;
            }
            let (aj, group) = actor_json(&c, &idents, &collectors, &cl, &key, &actor, &uid, &scope)?;
            if group == "system" && !with_system && only.is_none() {
                let n: i64 = c
                    .prepare_cached("SELECT count(*) FROM behavior_ops WHERE behavior_id=?1 AND t >= ?2 AND t < ?3")?
                    .query_row(params![id, start, end], |r| r.get(0))?;
                *hidden.get_mut("system").unwrap() += n;
                continue;
            }
            let ops = stored_ops(&c, id, start, end)?;
            if ops.is_empty() {
                continue;
            }
            behs.push(Beh {
                id: format!("b{id}"), cluster: cl, actor_key: key, actor: aj, group, scope, started_before: first < start, continues_after: last >= end, ops,
            });
        }
    }
    let mut ui = 0i64;
    if only.is_none_or(|id| id.starts_with("u~")) {
        let mut un = unrecorded(&c, ctx, clusters, start, end, &collectors, &mut ui)?;
        if let Some(id) = only {
            un.retain(|b| b.id == id);
        }
        behs.extend(un);
    }
    if only.is_none() {
        *hidden.get_mut("uiMaintenance").unwrap() += ui;
        let mut st = c.prepare_cached(
            "SELECT b.kind, b.cluster, e.actor, e.user_id, e.verb, e.api_group, e.resource, e.namespace, e.name, count(*) FROM behavior_ops b JOIN events e ON e.id=b.event_id
             WHERE b.behavior_id IS NULL AND b.t >= ?1 AND b.t < ?2 GROUP BY 1, 2, 3, 4, 5, 6, 7, 8, 9",
        )?;
        let mut rows = st.query(params![start, end])?;
        while let Some(r) = rows.next()? {
            let (kind, cl, actor, uid, verb, g, res, ns, name, n): (String, String, String, String, String, String, String, String, String, i64) =
                (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?);
            if !clusters.is_empty() && !clusters.contains(cl.as_str()) {
                continue;
            }
            let k = match kind.as_str() {
                "request" => "requestOnly",
                "ui" => "uiMaintenance",
                _ => {
                    let (class, reason) = idents.get(&(actor.clone(), uid)).map(|(c, r)| (*c, r.clone())).unwrap_or(("unknown", String::new()));
                    let scope = scope_of(&verb, &actor, &g, &res, &ns, &name);
                    if group_of(class, &reason, scope, true) == "system" { "failedSystem" } else { "failed" }
                }
            };
            *hidden.get_mut(k).unwrap() += n;
        }
    }
    let gaps = crate::changemap::day_gaps(&c, start, end, clusters, now_ms)?;
    Ok(Loaded { behs, hidden, gaps })
}

fn context(store: &Store, tz: chrono_tz::Tz) -> anyhow::Result<Ctx> {
    let c = store.conn.lock().unwrap();
    let kinds = c
        .prepare_cached("SELECT cluster, api_group, resource, kind FROM resource_kinds")?
        .query_map([], |r| Ok(((r.get(0)?, r.get(1)?, r.get(2)?), r.get(3)?)))?
        .collect::<Result<_, _>>()?;
    Ok(Ctx { tz, s: store.cfg.behaviors.clone(), kinds })
}

fn day_bounds(tz: chrono_tz::Tz, day: Option<&str>) -> anyhow::Result<(NaiveDate, i64, i64)> {
    let d = match day.filter(|d| !d.is_empty()) {
        Some(d) => NaiveDate::parse_from_str(d, "%Y-%m-%d").map_err(|e| anyhow::anyhow!("day: {e}"))?,
        None => Utc::now().with_timezone(&tz).date_naive(),
    };
    Ok((d, day_start(tz, d), day_start(tz, d.succ_opt().ok_or_else(|| anyhow::anyhow!("day: out of range"))?)))
}

fn sort_key(v: &Value) -> (usize, i64, String) {
    (tier_rank(v["tier"].as_str().unwrap_or("")), v["fromMs"].as_i64().unwrap_or(0), v["id"].as_str().unwrap_or("").to_string())
}

/// `GET /api/behaviors`.
pub fn page(store: &Store, tz: chrono_tz::Tz, p: &Params, now_ms: i64) -> anyhow::Result<Value> {
    let (day, start, end) = day_bounds(tz, p.day.as_deref())?;
    let ctx = context(store, tz)?;
    let clusters: HashSet<&str> = p.cluster.iter().map(String::as_str).collect();
    let with_system = p.system.as_deref() == Some("1");
    let building = building_since(store);
    let mut l = if building.is_some() {
        let c = store.conn.lock().unwrap();
        Loaded { behs: vec![], hidden: BTreeMap::new(), gaps: crate::changemap::day_gaps(&c, start, end, &clusters, now_ms)? }
    } else {
        load(store, &ctx, &clusters, with_system, start, end, now_ms, None)?
    };
    let cell_ms = ctx.s.cell_ms();

    let mut list: Vec<(Value, &Beh)> = l.behs.iter().map(|b| (behavior_json(&ctx, b).0, b)).collect();
    list.sort_by_key(|(v, _)| sort_key(v));

    // Summary.
    let mut by_tier: BTreeMap<&str, i64> = TIERS[..6].iter().map(|t| (*t, 0)).collect();
    for (v, _) in &list {
        if let Some(n) = by_tier.get_mut(v["tier"].as_str().unwrap_or("")) {
            *n += 1;
        }
    }
    let look: Vec<Value> = list.iter().filter(|(v, _)| v["tier"] == "look").map(|(v, _)| json!({"id": v["id"], "title": v["title"], "fromMs": v["fromMs"], "toMs": v["toMs"]})).collect();
    let mut long_gaps = Vec::new();
    let mut per: BTreeMap<&str, Vec<&crate::changemap::GapRow>> = BTreeMap::new();
    for g in &l.gaps {
        per.entry(g.0.as_str()).or_default().push(g);
    }
    for (cl, mut gs) in per {
        gs.sort_by_key(|g| g.1);
        let mut merged: Vec<(i64, i64, BTreeSet<&str>, bool)> = Vec::new();
        for g in gs {
            match merged.last_mut() {
                Some(m) if g.1 <= m.1 => {
                    m.1 = m.1.max(g.2);
                    m.2.insert(g.3.as_str());
                    m.3 |= g.5;
                }
                _ => merged.push((g.1, g.2, BTreeSet::from([g.3.as_str()]), g.5)),
            }
        }
        for (f, t, cols, open) in merged {
            if t - f >= ctx.s.join_ms() {
                long_gaps.push(json!({"cluster": cl, "fromMs": f, "toMs": t, "collectors": cols, "open": open}));
            }
        }
    }

    // Actors, scopes, strip, overlaps.
    struct Agg {
        v: Value,
        clusters: BTreeSet<String>,
        behaviors: i64,
        ops: i64,
        look: i64,
        best: usize,
        group: &'static str,
    }
    let mut actors: BTreeMap<(String, String), Agg> = BTreeMap::new();
    // Per (cluster, scope): who, behaviors, operations, look, most serious tier.
    type Place = (BTreeSet<String>, i64, i64, i64, usize);
    let mut scopes: BTreeMap<(String, String), Place> = BTreeMap::new();
    let mut strip: BTreeMap<i64, BTreeMap<&str, i64>> = BTreeMap::new();
    let mut people_cells: BTreeMap<String, BTreeSet<i64>> = BTreeMap::new();
    for (v, b) in &list {
        let tier = v["tier"].as_str().unwrap_or("");
        let is_look = (tier == "look") as i64;
        // Changes nobody could be found for: one row per cluster, as the reason depends on it.
        let ak = (b.actor_key.clone(), if b.actor_key.is_empty() && b.group == "unrecorded" { b.cluster.clone() } else { String::new() });
        let a = actors.entry(ak).or_insert_with(|| Agg { v: b.actor.clone(), clusters: BTreeSet::new(), behaviors: 0, ops: 0, look: 0, best: 99, group: b.group });
        a.clusters.insert(b.cluster.clone());
        a.behaviors += 1;
        a.ops += b.ops.len() as i64;
        a.look += is_look;
        a.best = a.best.min(tier_rank(tier));
        let s = scopes.entry((b.cluster.clone(), b.scope.clone())).or_insert((BTreeSet::new(), 0, 0, 0, 99));
        s.0.insert(b.actor_key.clone());
        s.1 += 1;
        s.2 += b.ops.len() as i64;
        s.3 += is_look;
        s.4 = s.4.min(tier_rank(tier));
        for op in &b.ops {
            let (_, cs) = cell(tz, op.t, cell_ms);
            *strip.entry(cs).or_default().entry(b.group).or_default() += 1;
            if b.group == "people" {
                people_cells.entry(b.actor_key.clone()).or_default().insert(cs);
            }
        }
    }
    let mut actors: Vec<Agg> = actors.into_values().collect();
    actors.sort_by(|a, b| {
        let g = |x: &Agg| GROUPS.iter().position(|g| *g == x.group).unwrap_or(9);
        g(a).cmp(&g(b)).then(a.best.cmp(&b.best)).then(b.ops.cmp(&a.ops)).then(a.v["key"].as_str().cmp(&b.v["key"].as_str()))
    });
    let actors_json: Vec<Value> = actors
        .into_iter()
        .map(|a| {
            let mut v = a.v;
            v["clusters"] = json!(a.clusters);
            v["behaviors"] = json!(a.behaviors);
            v["ops"] = json!(a.ops);
            v["look"] = json!(a.look);
            v
        })
        .collect();
    let mut scopes: Vec<_> = scopes.into_iter().collect();
    scopes.sort_by(|a, b| a.0.0.cmp(&b.0.0).then(a.1.4.cmp(&b.1.4)).then(b.1.1.cmp(&a.1.1)).then(a.0.1.cmp(&b.0.1)));
    let scopes_json: Vec<Value> = scopes
        .into_iter()
        .map(|((cl, sc), (who, nb, no, lk, best))| {
            json!({"cluster": cl, "scope": sc, "actors": who.len(), "behaviors": nb, "ops": no, "look": lk, "tier": TIERS.get(best).copied().unwrap_or("system")})
        })
        .collect();
    let strip_json: Vec<Value> = strip
        .into_iter()
        .map(|(t, m)| {
            let mut v = json!({"t": t, "people": 0, "routine": 0, "unrecorded": 0, "unauthenticated": 0});
            for (g, n) in m {
                v[g] = json!(n);
            }
            v
        })
        .collect();
    let keys: Vec<&String> = people_cells.keys().collect();
    let mut overlaps = Vec::new();
    for i in 0..keys.len() {
        for j in i + 1..keys.len() {
            let both: Vec<&i64> = people_cells[keys[i]].intersection(&people_cells[keys[j]]).collect();
            if let (Some(f), Some(t)) = (both.first(), both.last()) {
                overlaps.push(json!({"actors": [keys[i], keys[j]], "cells": both.len(), "fromMs": f, "toMs": **t + cell_ms}));
            }
        }
    }

    let sel = p.select.as_deref().and_then(|id| list.iter().find(|(v, _)| v["id"] == id)).or_else(|| list.first());
    let detail = sel.map(|(_, b)| detail_json(&ctx, b));
    let selected = sel.map(|(v, _)| v["id"].clone());
    let summary_n: usize = list.iter().filter(|(v, _)| v["tier"] != "system").count();
    let behaviors: Vec<Value> = std::mem::take(&mut list).into_iter().map(|(v, _)| v).collect();
    let mut resp = json!({
        "day": day.format("%Y-%m-%d").to_string(), "tz": tz.name(), "dayStartMs": start, "dayEndMs": end,
        "settings": ctx.s.json(),
        "summary": {"behaviors": summary_n, "byTier": by_tier, "look": look, "longGaps": long_gaps},
        "hidden": l.hidden, "gaps": crate::changemap::gaps_json(&l.gaps),
        "behaviors": behaviors, "actors": actors_json, "scopes": scopes_json,
        "strip": {"cellMinutes": ctx.s.cell_minutes, "cells": strip_json}, "overlaps": overlaps,
        "selected": selected, "detail": detail,
    });
    if building.is_some() {
        resp["hidden"] = json!({"system": 0, "uiMaintenance": 0, "requestOnly": 0, "failed": 0, "failedSystem": 0});
    }
    let mut progress = Vec::new();
    if let Some(since) = building {
        progress.push(json!({"what": "behaviors", "startedMs": since, "done": null, "total": null}));
    }
    if let Some(j) = store.job.lock().unwrap().as_ref().filter(|j| j.state == "running") {
        progress.push(json!({"what": "recompute", "startedMs": j.started_ms, "done": j.done, "total": j.total}));
    }
    let note = std::env::var("AL_DATA_NOTE").ok().filter(|n| !n.is_empty());
    if note.is_some() || !progress.is_empty() {
        resp["meta"] = json!({});
        if let Some(n) = note {
            let c = store.conn.lock().unwrap();
            let until: Option<i64> = c.query_row("SELECT max(coalesce(last_event_ms, last_version_ms)) FROM cluster_stats", [], |r| r.get(0)).optional()?.flatten();
            resp["meta"]["dataNote"] = json!(n);
            resp["meta"]["dataUntilMs"] = json!(until);
        }
        if !progress.is_empty() {
            resp["meta"]["progress"] = json!(progress);
        }
    }
    l.behs.clear();
    Ok(resp)
}

/// `GET /api/behaviors/detail`: `None` when the behavior has nothing that day.
pub fn detail(store: &Store, tz: chrono_tz::Tz, p: &Params, now_ms: i64) -> anyhow::Result<Option<Value>> {
    let id = p.id.as_deref().filter(|s| !s.is_empty()).ok_or_else(|| anyhow::anyhow!("id: required"))?;
    let (_, start, end) = day_bounds(tz, p.day.as_deref())?;
    let ctx = context(store, tz)?;
    let l = load(store, &ctx, &HashSet::new(), true, start, end, now_ms, Some(id))?;
    Ok(l.behs.iter().find(|b| b.id == id).map(|b| detail_json(&ctx, b)))
}

#[cfg(test)]
mod tests;
