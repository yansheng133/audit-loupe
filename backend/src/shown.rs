//! The index the change map reads (requirements section 3: the whole page in 5 seconds at
//! about 720,000 records a day).
//!
//! The page only lists writes and records with a medium or high alert. In a day of the lab
//! times eleven that is about 50,000 of 714,000 records; the rest are reads by controllers.
//! Reading the whole day to find them took about a second. This partial, covering index holds
//! only the records the page could ever list, with the columns it reads, so a page view reads
//! those and nothing else. It is kept by SQLite on every insert; nothing else has to keep it.
//!
//! "Could ever list" does not depend on the rule settings, so judging records again changes
//! nothing here: every write, plus every read a rule can flag (secrets, refused requests,
//! logins). Whether a read really has an alert is still looked up per record.
//!
//! A database from before the index gets it on the first start. On a large database that
//! takes a while, so it is built on a second connection in the background; until it is ready
//! the page reads the day the old way and says the index is being built.

use rusqlite::Connection;
use std::sync::Mutex;

pub const NAME: &str = "events_shown";

/// Records the page could list, whatever the rule settings. Used verbatim in the index and in
/// the page's query, so SQLite can tell the index covers the query.
pub const PREDICATE: &str = "(verb IN ('create','update','patch','delete','deletecollection') OR subresource IN ('exec','attach','portforward') OR (source='rancher' AND verb NOT IN ('get','list','watch')) OR resource='secrets' OR code=403 OR verb='login')";

/// Built in place (not in the background) up to this many records.
const BUILD_INLINE: i64 = 200_000;

/// When the background build started, while it runs.
static BUILDING: Mutex<Option<i64>> = Mutex::new(None);

pub fn ddl() -> String {
    format!(
        "CREATE INDEX IF NOT EXISTS {NAME} ON events(ts_ms, cluster, source, link_id, actor, user_id, human, via, verb, api_group, resource, subresource, namespace, name, code) WHERE {PREDICATE}"
    )
}

pub fn ready(c: &Connection) -> bool {
    c.query_row("SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type='index' AND name=?1)", [NAME], |r| r.get(0)).unwrap_or(false)
}

/// When the background build started, if one is running.
pub fn building_since() -> Option<i64> {
    *BUILDING.lock().unwrap()
}

/// Make sure the index exists or is being built. `path` is the database file (`None` in memory).
pub fn prepare(c: &Connection, path: Option<String>) -> anyhow::Result<()> {
    if ready(c) {
        return Ok(());
    }
    let n: i64 = c.query_row("SELECT count(*) FROM events", [], |r| r.get(0))?;
    let Some(path) = path.filter(|_| n > BUILD_INLINE) else {
        c.execute_batch(&ddl())?;
        return Ok(());
    };
    {
        let mut b = BUILDING.lock().unwrap();
        if b.is_some() {
            return Ok(());
        }
        *b = Some(chrono::Utc::now().timestamp_millis());
    }
    tracing::info!(records = n, "building the change map index in the background");
    std::thread::spawn(move || {
        let t0 = std::time::Instant::now();
        let result = Connection::open(&path).and_then(|c| {
            // Ingest waits for the write lock this holds; collectors retry what times out.
            c.busy_timeout(std::time::Duration::from_secs(60))?;
            c.execute_batch(&ddl())
        });
        match result {
            Ok(()) => tracing::info!(seconds = t0.elapsed().as_secs(), "change map index ready"),
            Err(e) => tracing::warn!(error = %e, "building the change map index failed; the page reads the day without it"),
        }
        *BUILDING.lock().unwrap() = None;
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::changemap::{changemap, changemap_by_scan, Params};

    /// Reading the index gives the same page as reading every record of the day, for every
    /// tab and filter the page has.
    #[test]
    fn the_index_gives_the_same_page_as_the_whole_day() {
        let cfg = crate::config::Config::test();
        let s = crate::store::Store::open(":memory:").unwrap();
        crate::demo::seed(&s, &cfg).unwrap();
        let day = {
            let c = s.conn.lock().unwrap();
            let t: i64 = c.query_row("SELECT max(ts_ms) FROM events", [], |r| r.get(0)).unwrap();
            chrono::DateTime::from_timestamp_millis(t).unwrap().with_timezone(&cfg.tz).format("%Y-%m-%d").to_string()
        };
        let now = chrono::Utc::now().timestamp_millis();
        let base = Params { day: Some(day), ..Default::default() };
        let views = [
            base.clone(),
            Params { system: Some("1".into()), ..base.clone() },
            Params { cluster: vec![cfg.cluster.clone()], ..base.clone() },
            Params { tab: Some("relations".into()), ..base.clone() },
            Params { tab: Some("events".into()), ..base.clone() },
            Params { state: Some("look".into()), ..base.clone() },
            Params { namespace: vec!["shop".into()], ..base.clone() },
        ];
        for p in views {
            let (a, b) = (changemap(&s, cfg.tz, &p, now).unwrap(), changemap_by_scan(&s, cfg.tz, &p, now).unwrap());
            assert!(a["rows"].as_array().is_none_or(|r| !r.is_empty()) || p.state.is_some(), "{p:?}");
            assert_eq!(a, b, "{p:?}");
        }
    }

    /// The timeline adds up: its slots hold every operation the page counts, a slot with an
    /// alert is "look", the clusters' lines add up to the merged line, and a picked range counts
    /// exactly the slots it covers.
    #[test]
    fn the_timeline_adds_up() {
        let cfg = crate::config::Config::test();
        let s = crate::store::Store::open(":memory:").unwrap();
        crate::demo::seed(&s, &cfg).unwrap();
        let (day, clusters) = {
            let c = s.conn.lock().unwrap();
            let t: i64 = c.query_row("SELECT max(ts_ms) FROM events", [], |r| r.get(0)).unwrap();
            let day = chrono::DateTime::from_timestamp_millis(t).unwrap().with_timezone(&cfg.tz).format("%Y-%m-%d").to_string();
            let cl: Vec<String> = c.prepare("SELECT DISTINCT cluster FROM events ORDER BY 1").unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
            (day, cl)
        };
        let now = chrono::Utc::now().timestamp_millis();
        let slots = |v: &serde_json::Value, i: usize| v["timeline"]["lines"][i]["slots"].as_array().unwrap().clone();
        let sum = |sl: &[serde_json::Value]| sl.iter().map(|x| x["ops"].as_i64().unwrap()).sum::<i64>();
        for system in [None, Some("1".to_string())] {
            let base = Params { day: Some(day.clone()), system: system.clone(), ..Default::default() };
            let v = changemap(&s, cfg.tz, &base, now).unwrap();
            let merged = slots(&v, 0);
            assert!(sum(&merged) > 0);
            assert!(merged.iter().any(|x| x["state"] == "look"), "the demo day has alerts, so the check below checks something");
            assert_eq!(sum(&merged), v["counts"]["ops"].as_i64().unwrap(), "every counted operation is on the timeline ({system:?})");
            for x in &merged {
                if x["alerts"].as_i64().unwrap() > 0 {
                    assert_eq!(x["state"], "look", "{x}");
                }
                if x["state"] == "look" {
                    assert!(x["alerts"].as_i64().unwrap() > 0, "{x}");
                }
            }
            // One line per picked cluster, at one-minute slots; together they hold what the
            // merged ten-minute line holds (the demo has one cluster, so this checks the binning).
            let per = changemap(&s, cfg.tz, &Params { cluster: clusters.clone(), ..base.clone() }, now).unwrap();
            let total: i64 = (0..clusters.len()).map(|i| sum(&slots(&per, i))).sum();
            assert_eq!(total, sum(&merged));
            // A range of whole slots counts exactly the operations of those slots, and the
            // timeline stays the whole day.
            let busy = merged.iter().position(|x| x["ops"].as_i64().unwrap() > 0).unwrap();
            let (a, b) = (merged[busy]["t"].as_i64().unwrap(), merged[(busy + 6).min(merged.len() - 1)]["t"].as_i64().unwrap());
            let r = changemap(&s, cfg.tz, &Params { from: Some(a), to: Some(b), ..base.clone() }, now).unwrap();
            let inside: i64 = merged.iter().filter(|x| (a..b).contains(&x["t"].as_i64().unwrap())).map(|x| x["ops"].as_i64().unwrap()).sum();
            assert_eq!(r["counts"]["ops"].as_i64().unwrap(), inside, "{system:?}");
            assert_eq!(r["timeline"], v["timeline"]);
        }
    }

    /// Every reason on the page also comes as a code the page can say in its own language,
    /// and nothing on it is worded for developers (v0.2).
    #[test]
    fn reasons_come_as_codes() {
        let cfg = crate::config::Config::test();
        let s = crate::store::Store::open(":memory:").unwrap();
        crate::demo::seed(&s, &cfg).unwrap();
        let day = {
            let c = s.conn.lock().unwrap();
            let t: i64 = c.query_row("SELECT max(ts_ms) FROM events", [], |r| r.get(0)).unwrap();
            chrono::DateTime::from_timestamp_millis(t).unwrap().with_timezone(&cfg.tz).format("%Y-%m-%d").to_string()
        };
        let now = chrono::Utc::now().timestamp_millis();
        let page = changemap(&s, cfg.tz, &Params { day: Some(day.clone()), ..Default::default() }, now).unwrap();
        let rows = page["rows"].as_array().unwrap();
        for r in rows {
            let code = r["stateReasonCode"].as_str().unwrap_or("");
            match r["state"].as_str().unwrap() {
                "look" => assert!(code.starts_with("alert-"), "{r}"),
                "unclear" => assert!(["no-apiserver-audit", "found-on-reconnect"].contains(&code), "{r}"),
                "blind" => assert_eq!(code, "near-gap"),
                _ => assert_eq!(code, ""),
            }
            if r["who"]["name"].as_str().is_some_and(|n| !n.is_empty()) && r["who"]["category"] != "human" {
                assert!(r["who"]["noteCode"].is_string(), "{r}");
            }
        }
        let events = changemap(&s, cfg.tz, &Params { day: Some(day), tab: Some("events".into()), ..Default::default() }, now).unwrap();
        assert!(events["events"].as_array().unwrap().iter().all(|e| e["reasonCode"].is_string()), "{events}");
        assert!(!events.to_string().contains("v0") && !page.to_string().contains("近似"));
    }

    /// The page's query must be answered from the index alone, without reading the table.
    #[test]
    fn the_page_query_reads_only_the_index() {
        let s = crate::store::Store::open(":memory:").unwrap();
        let c = s.conn.lock().unwrap();
        assert!(ready(&c));
        let plan: Vec<String> = c
            .prepare(&format!("EXPLAIN QUERY PLAN {}", crate::changemap::shown_query()))
            .unwrap()
            .query_map([0i64, 0, 0], |r| r.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(plan.iter().any(|p| p.contains(&format!("USING COVERING INDEX {NAME}"))), "{plan:?}");
    }
}
