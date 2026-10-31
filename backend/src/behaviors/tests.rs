use super::*;
use crate::audit::{parse_body, to_record, Record};
use crate::config::Config;
use chrono::TimeZone;
use std::sync::Arc;

const TZ: chrono_tz::Tz = chrono_tz::Asia::Taipei;
const MIN: i64 = 60_000;

/// 2026-10-20 (a Tuesday) at hh:mm, Taipei.
fn at(h: u32, m: u32) -> i64 {
    TZ.with_ymd_and_hms(2026, 10, 20, h, m, 0).unwrap().timestamp_millis()
}

#[allow(clippy::too_many_arguments)]
fn rec(id: &str, user: &str, verb: &str, group: &str, resource: &str, sub: &str, ns: &str, name: &str, t: i64, code: i64) -> Record {
    let stage = if matches!(sub, "exec" | "attach" | "portforward") { "ResponseStarted" } else { "ResponseComplete" };
    let ts = Utc.timestamp_millis_opt(t).unwrap().to_rfc3339();
    let ev = json!({
        "auditID": id, "stage": stage, "verb": verb, "user": {"username": user},
        "objectRef": {"resource": resource, "subresource": sub, "namespace": ns, "name": name, "apiGroup": group},
        "responseStatus": {"code": code}, "stageTimestamp": ts, "requestReceivedTimestamp": ts,
    });
    to_record(&parse_body(ev.to_string().as_bytes()).unwrap()[0]).unwrap()
}

fn rancher_put(id: &str, ts: &str) -> Record {
    let line = format!(r#"{{"auditID":"{id}","requestURI":"/k8s/clusters/c1/v1/apps.deployments/shop/web","method":"PATCH",
      "user":{{"name":"u-1","extra":{{"username":["alice"]}}}},"responseCode":200,"responseTimestamp":"{ts}"}}"#);
    crate::rancher::to_record(&crate::rancher::parse_fixture(&line)[0]).unwrap()
}

fn impersonated_patch(id: &str, ts: &str) -> Record {
    let ev = format!(r#"{{"auditID":"{id}","stage":"ResponseComplete","verb":"patch",
      "user":{{"username":"system:serviceaccount:cattle-impersonation-system:cattle-impersonation-u-1"}},
      "impersonatedUser":{{"username":"u-1","extra":{{"username":["alice"]}}}},
      "objectRef":{{"resource":"deployments","namespace":"shop","name":"web","apiGroup":"apps"}},
      "responseStatus":{{"code":200}},"stageTimestamp":"{ts}"}}"#);
    to_record(&parse_body(ev.replace('\n', " ").as_bytes()).unwrap()[0]).unwrap()
}

/// A person's direct patch of a ConfigMap in `ns`.
fn patch(id: &str, ns: &str, t: i64) -> Record {
    rec(id, "system:admin", "patch", "", "configmaps", "", ns, "cfg", t, 200)
}

fn store_with(f: impl FnOnce(&mut Config)) -> Store {
    let mut cfg = Config::defaults();
    f(&mut cfg);
    Store::open_with(":memory:", Arc::new(cfg)).unwrap()
}

fn day_of(t: i64) -> String {
    Utc.timestamp_millis_opt(t).unwrap().with_timezone(&TZ).format("%Y-%m-%d").to_string()
}

fn page_of(s: &Store, day: &str) -> Value {
    page(s, TZ, &Params { day: Some(day.into()), ..Default::default() }, at(23, 59) + 86_400_000 * 3).unwrap()
}

/// Behaviors of the day as (scope, [(first, last, ops) per segment]) for one actor.
fn of_actor(v: &Value, key: &str) -> Vec<Value> {
    v["behaviors"].as_array().unwrap().iter().filter(|b| b["actor"]["key"] == key).cloned().collect()
}

fn gap(s: &Store, cluster: &str, from: i64, to: i64) {
    let g = crate::collect::Gap { collector: "file".into(), instance: "n1".into(), from_ms: from, to_ms: to, reason: "collector-restart".into(), detail: String::new() };
    assert!(s.record_gap(cluster, &g).unwrap());
}

/// Stored behaviors as comparable values: key, times, count and the records in each.
/// Cluster, actor key, scope, first, last, count, record IDs.
type Kept = (String, String, String, i64, i64, i64, Vec<i64>);

fn stored(s: &Store) -> Vec<Kept> {
    let c = s.conn.lock().unwrap();
    let mut v: Vec<Kept> = c
        .prepare("SELECT id, cluster, actor_key, scope, first_ms, last_ms, ops FROM behaviors")
        .unwrap()
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))
        .unwrap()
        .map(Result::unwrap)
        .map(|(id, cl, k, sc, f, l, n)| {
            let evs: Vec<i64> = c.prepare("SELECT event_id FROM behavior_ops WHERE behavior_id=?1 ORDER BY event_id").unwrap()
                .query_map([id], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
            (cl, k, sc, f, l, n, evs)
        })
        .collect();
    v.sort();
    v
}

/// The rule of section 3 point 5: a gap cuts where it starts; records inside it still count
/// and still join each other. Whether the gap is written before or after the records around
/// it arrive (a heartbeat gap is written when the collector checks in again) gives the same.
#[test]
fn a_gap_cuts_where_it_starts_and_records_inside_it_still_count() {
    for gap_first in [false, true] {
        let s = Store::open(":memory:").unwrap();
        if gap_first {
            gap(&s, "c1", at(10, 6), at(10, 30));
        }
        // 10:06 is the gap start itself: that record is before it.
        for (i, t) in [at(10, 0), at(10, 5), at(10, 6), at(10, 10), at(10, 15), at(10, 40)].into_iter().enumerate() {
            s.insert_event("c1", &patch(&format!("p{i}"), "shop", t)).unwrap();
        }
        if !gap_first {
            gap(&s, "c1", at(10, 6), at(10, 30));
        }
        let v = page_of(&s, "2026-10-20");
        let b = of_actor(&v, "system:admin");
        let runs: Vec<(i64, i64, i64)> = b.iter().map(|b| (b["fromMs"].as_i64().unwrap(), b["toMs"].as_i64().unwrap(), b["ops"].as_i64().unwrap())).collect();
        assert_eq!(runs, vec![(at(10, 0), at(10, 6), 3), (at(10, 10), at(10, 40), 3)], "gap written first: {gap_first}");
        // Inside the second behavior 10:10 and 10:15 share no cell with 10:40: two segments.
        assert_eq!(b[1]["segments"].as_array().unwrap().len(), 2);
    }
}

/// A gap that starts before both records, or after both, cuts nothing; "collection-start"
/// is a marker, not a gap.
#[test]
fn a_gap_outside_the_pair_or_a_start_marker_cuts_nothing() {
    let s = Store::open(":memory:").unwrap();
    gap(&s, "c1", at(9, 0), at(9, 50));
    let start = crate::collect::Gap { collector: "file".into(), instance: "n1".into(), from_ms: at(10, 7), to_ms: at(10, 7), reason: crate::store::START.into(), detail: String::new() };
    s.record_gap("c1", &start).unwrap();
    gap(&s, "c2", at(10, 7), at(10, 8));
    s.insert_event("c1", &patch("a", "shop", at(10, 0))).unwrap();
    s.insert_event("c1", &patch("b", "shop", at(10, 20))).unwrap();
    assert_eq!(of_actor(&page_of(&s, "2026-10-20"), "system:admin").len(), 1);
    // The check above could fail: a real gap of the same cluster between them cuts.
    gap(&s, "c1", at(10, 7), at(10, 8));
    assert_eq!(of_actor(&page_of(&s, "2026-10-20"), "system:admin").len(), 2);
}

#[test]
fn exactly_the_join_gap_joins_and_one_millisecond_more_cuts() {
    let s = Store::open(":memory:").unwrap();
    s.insert_event("c1", &patch("a", "shop", at(10, 0))).unwrap();
    s.insert_event("c1", &patch("b", "shop", at(11, 0))).unwrap();
    s.insert_event("c1", &patch("c", "shop", at(12, 0) + 1)).unwrap();
    let b = of_actor(&page_of(&s, "2026-10-20"), "system:admin");
    assert_eq!(b.len(), 2);
    assert_eq!((b[0]["ops"].as_i64(), b[1]["ops"].as_i64()), (Some(2), Some(1)));
    // 10:00 and 11:00 are in cells six apart: dashed, two segments of one behavior.
    assert_eq!(b[0]["segments"].as_array().unwrap().len(), 2);
    // Arriving out of order, a record between two behaviors joins them into one (smaller ID).
    s.insert_event("c1", &patch("d", "shop", at(11, 30))).unwrap();
    let b = of_actor(&page_of(&s, "2026-10-20"), "system:admin");
    assert_eq!(b.len(), 1);
    assert_eq!(b[0]["ops"], 4);
}

#[test]
fn a_behavior_runs_across_midnight_with_the_same_id() {
    let s = Store::open(":memory:").unwrap();
    s.insert_event("c1", &patch("a", "shop", at(23, 50))).unwrap();
    s.insert_event("c1", &patch("b", "shop", at(23, 50) + 30 * MIN)).unwrap();
    let (d1, d2) = (page_of(&s, "2026-10-20"), page_of(&s, "2026-10-21"));
    let (b1, b2) = (&of_actor(&d1, "system:admin")[0], &of_actor(&d2, "system:admin")[0]);
    assert_eq!(b1["id"], b2["id"]);
    assert_eq!((b1["ops"].as_i64(), b2["ops"].as_i64()), (Some(1), Some(1)), "each day counts its own records");
    assert_eq!((b1["startedBefore"].as_bool(), b1["continuesAfter"].as_bool()), (Some(false), Some(true)));
    assert_eq!((b2["startedBefore"].as_bool(), b2["continuesAfter"].as_bool()), (Some(true), Some(false)));
    // 00:20 is in the cell starting 00:20, aligned in the server zone.
    assert_eq!(b2["segments"][0]["fromMs"], at(23, 50) + 30 * MIN);
}

#[test]
fn a_namespace_counts_as_its_own_and_cluster_scoped_objects_as_none() {
    let s = Store::open(":memory:").unwrap();
    s.insert_event("c1", &rec("p", "system:admin", "delete", "", "pods", "", "shop", "web-1", at(10, 0), 200)).unwrap();
    s.insert_event("c1", &rec("n", "system:admin", "delete", "", "namespaces", "", "", "shop", at(10, 2), 200)).unwrap();
    s.insert_event("c1", &rec("r", "system:admin", "patch", "rbac.authorization.k8s.io", "clusterroles", "", "", "view", at(10, 3), 200)).unwrap();
    // The namespace controller emptying it counts there too (a system component: hidden).
    s.insert_event("c1", &rec("x", NAMESPACE_CONTROLLER, "deletecollection", "", "pods", "", "shop", "", at(10, 4), 200)).unwrap();
    let v = page_of(&s, "2026-10-20");
    let b = of_actor(&v, "system:admin");
    let scopes: Vec<(&str, i64)> = b.iter().map(|b| (b["scope"].as_str().unwrap(), b["ops"].as_i64().unwrap())).collect();
    assert_eq!(scopes, vec![("shop", 2), ("", 1)]);
    // The namespace deletion is the title, and the behavior is "look".
    assert_eq!(b[0]["tier"], "look");
    assert_eq!((b[0]["title"]["verb"].as_str(), b[0]["title"]["kind"].as_str(), b[0]["title"]["name"].as_str()), (Some("delete"), Some("Namespace"), Some("shop")));
    assert_eq!(v["hidden"]["system"], 1);
    let sys = page(&s, TZ, &Params { day: Some("2026-10-20".into()), system: Some("1".into()), ..Default::default() }, at(23, 0)).unwrap();
    let ctl = of_actor(&sys, NAMESPACE_CONTROLLER);
    assert_eq!((ctl[0]["scope"].as_str(), ctl[0]["tier"].as_str()), (Some("shop"), Some("system")));
}

/// Tag-only rules never raise a tier; the title names the real subresource.
#[test]
fn tag_only_rules_do_not_tier_and_titles_name_the_subresource() {
    let s = Store::open(":memory:").unwrap();
    // Direct writes at night: bypassed_rancher (medium) and off_hours (low), both tag-only.
    for (i, t) in [at(2, 0), at(2, 5)].into_iter().enumerate() {
        s.insert_event("c1", &patch(&format!("a{i}"), "shop", t)).unwrap();
    }
    s.insert_event("c1", &rec("f", "system:admin", "get", "", "pods", "portforward", "argocd", "argocd-server-1", at(2, 0), 101)).unwrap();
    let v = page_of(&s, "2026-10-20");
    let b = of_actor(&v, "system:admin");
    let shop = b.iter().find(|b| b["scope"] == "shop").unwrap();
    assert_eq!(shop["tier"], "normal");
    assert!(shop["title"].is_null());
    let rules: Vec<(&str, bool)> = shop["rules"].as_array().unwrap().iter().map(|r| (r["rule"].as_str().unwrap(), r["tagOnly"].as_bool().unwrap())).collect();
    assert_eq!(rules, vec![("bypassed_rancher", true), ("off_hours", true)]);
    assert_eq!(shop["tags"], json!(["direct", "bypassed-rancher", "off-hours"]));
    let pf = b.iter().find(|b| b["scope"] == "argocd").unwrap();
    assert_eq!(pf["tier"], "notable");
    assert_eq!((pf["title"]["verb"].as_str(), pf["title"]["rule"].as_str()), (Some("portforward"), Some("pod_exec")));
    assert_eq!(pf["what"], json!([{"verb": "portforward", "kind": "Pod", "count": 1}]));
    // Not tag-only, the same rule would tier (so the check above checks the setting).
    let s2 = store_with(|c| c.behaviors.tag_only = vec!["off_hours".into()]);
    s2.insert_event("c1", &patch("a", "shop", at(2, 0))).unwrap();
    assert_eq!(of_actor(&page_of(&s2, "2026-10-20"), "system:admin")[0]["tier"], "notable");
    assert_eq!(v["summary"]["byTier"]["notable"], 1);
    assert_eq!(v["summary"]["byTier"]["normal"], 1);
}

#[test]
fn failed_requests_are_counted_not_listed_by_the_setting() {
    for drop in [true, false] {
        let s = store_with(|c| c.behaviors.drop_failed = drop);
        s.insert_event("c1", &patch("ok", "shop", at(10, 0))).unwrap();
        s.insert_event("c1", &rec("bad", "system:admin", "update", "", "configmaps", "", "shop", "cfg", at(10, 1), 409)).unwrap();
        s.insert_event("c1", &rec("sys", "system:kube-scheduler", "patch", "", "pods", "", "kube-system", "x", at(10, 1), 500)).unwrap();
        let v = page_of(&s, "2026-10-20");
        let ops = of_actor(&v, "system:admin")[0]["ops"].as_i64().unwrap();
        if drop {
            assert_eq!((ops, v["hidden"]["failed"].as_i64(), v["hidden"]["failedSystem"].as_i64(), v["hidden"]["system"].as_i64()), (1, Some(1), Some(1), Some(0)));
        } else {
            assert_eq!((ops, v["hidden"]["failed"].as_i64(), v["hidden"]["system"].as_i64()), (2, Some(0), Some(1)));
        }
    }
}

#[test]
fn who_has_no_author_says_why_from_the_collectors_that_report() {
    let s = Store::open(":memory:").unwrap();
    let now = at(10, 0);
    s.heartbeat("local", "watch", "w1", &json!({}), now).unwrap();
    s.heartbeat("local", "rancher", "r1", &json!({}), now).unwrap();
    s.heartbeat("c1", "file", "n1", &json!({}), now).unwrap();
    s.heartbeat("c1", "watch", "w2", &json!({}), now).unwrap();
    {
        let c = s.conn.lock().unwrap();
        for (cl, t) in [("local", at(10, 0)), ("local", at(10, 3)), ("c1", at(10, 0))] {
            c.execute(
                "INSERT INTO versions (cluster, api_group, resource, kind, namespace, name, uid, observed_ms, observed, change, body) VALUES (?1,'','configmaps','ConfigMap','fleet-default','x','u',?2,'','modified','{}')",
                params![cl, t],
            )
            .unwrap();
        }
    }
    s.insert_event("local", &rec("e", "system:cattle:error", "update", "", "secrets", "", "fleet-default", "plan", at(10, 1), 200)).unwrap();
    let v = page_of(&s, "2026-10-20");
    let un = of_actor(&v, "");
    let local = un.iter().find(|b| b["cluster"] == "local").unwrap();
    assert_eq!(local["ops"], 2);
    assert_eq!(local["tier"], "unrecorded");
    assert_eq!(local["actor"]["noteCode"], "no-apiserver-audit");
    assert_eq!(local["actor"]["id"], "~local");
    let ids: Vec<&str> = v["actors"].as_array().unwrap().iter().map(|a| a["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"~local") && ids.contains(&"~c1") && ids.contains(&"system:cattle:error"), "{ids:?}");
    assert_eq!(local["actor"]["dependsOn"], json!({"code": "no-apiserver-audit", "cluster": "local", "collectors": ["rancher", "watch"]}));
    let c1 = un.iter().find(|b| b["cluster"] == "c1").unwrap();
    assert_eq!((c1["actor"]["noteCode"].as_str(), c1["actor"]["dependsOn"].is_null()), (Some("found-on-reconnect"), true));
    let e = &of_actor(&v, "system:cattle:error")[0];
    assert_eq!((e["tier"].as_str(), e["actor"]["group"].as_str(), e["actor"]["noteCode"].as_str()), (Some("unauthenticated"), Some("unauthenticated"), Some("rancher-unauthenticated")));
    assert!(!v.to_string().contains("找不到執行者"));
    // The detail of a behavior cut from versions is found by its ID.
    let d = detail(&s, TZ, &Params { day: Some("2026-10-20".into()), id: local["id"].as_str().map(str::to_string), ..Default::default() }, now).unwrap().unwrap();
    assert_eq!(d["opsTotal"], 2);
    assert!(detail(&s, TZ, &Params { day: Some("2026-10-21".into()), id: local["id"].as_str().map(str::to_string), ..Default::default() }, now).unwrap().is_none());
}

/// Records arriving in any order, gaps written in between, a Rancher half replaced by its
/// downstream record: the stored behaviors equal a build from scratch, a re-judging, and the
/// plain rule (`cut`) applied to all records at once.
#[test]
fn rebuilding_gives_what_was_kept_on_arrival() {
    let s = Store::open(":memory:").unwrap();
    let mut x: u64 = 7;
    let mut rnd = |n: u64| {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (x >> 33) % n
    };
    let users = ["system:admin", "alice-cert", "system:serviceaccount:shop:app"];
    let nss = ["shop", "argocd", ""];
    let mut recs = Vec::new();
    for i in 0..400 {
        let t = at(0, 0) + rnd(2 * 1440) as i64 * MIN + rnd(60_000) as i64;
        let (u, ns) = (users[rnd(3) as usize], nss[rnd(3) as usize]);
        let (res, name) = if ns.is_empty() { ("namespaces", "shop") } else { ("configmaps", "cfg") };
        let code = if rnd(10) == 0 { 409 } else { 200 };
        recs.push(rec(&format!("r{i}"), u, "patch", "", res, "", ns, name, t, code));
    }
    let gaps: Vec<i64> = (0..6).map(|_| at(0, 0) + rnd(2 * 1440) as i64 * MIN).collect();
    for (i, r) in recs.iter().enumerate() {
        s.insert_event("c1", r).unwrap();
        if i % 70 == 0 {
            gap(&s, "c1", gaps[i / 70], gaps[i / 70] + 5 * MIN);
        }
    }
    // A Rancher half first, then its downstream record takes over.
    s.insert_event("local", &rancher_put("rp", "2026-10-20T02:00:01Z")).unwrap();
    s.insert_event("c1", &impersonated_patch("dp", "2026-10-20T02:00:00.900Z")).unwrap();
    {
        let c = s.conn.lock().unwrap();
        let paired: i64 = c.query_row("SELECT count(*) FROM events WHERE link_id IS NOT NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(paired, 2, "the two halves were matched, so taking one back out is tested");
        let n: i64 = c.query_row("SELECT count(*) FROM behavior_ops o JOIN events e ON e.id=o.event_id WHERE e.audit_id IN ('rp','dp')", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "only the downstream half is an operation");
    }
    let live = stored(&s);
    assert!(live.len() > 20, "{}", live.len());

    // The plain rule over every record of each key.
    let c = s.conn.lock().unwrap();
    let all: Vec<(String, String, i64, i64)> = c
        .prepare("SELECT o.event_id, e.actor, e.user_id, e.namespace, e.resource, e.name, o.t FROM behavior_ops o JOIN events e ON e.id=o.event_id WHERE o.kind='op' ORDER BY o.t, o.event_id")
        .unwrap()
        .query_map([], |r| {
            let (id, a, u, ns, res, n, t): (i64, String, String, String, String, String, i64) = (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?);
            let key = if u.is_empty() { a.clone() } else { u };
            Ok((key, scope_of("patch", &a, "", &res, &ns, &n).to_string(), t, id))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let starts: Vec<i64> = c.prepare("SELECT from_ms FROM gaps WHERE cluster='c1'").unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
    drop(c);
    let mut by: BTreeMap<(String, String), Vec<(i64, i64)>> = BTreeMap::new();
    for (k, sc, t, id) in all {
        by.entry((k, sc)).or_default().push((t, id));
    }
    let mut plain = Vec::new();
    for ((k, sc), v) in by {
        for run in cut(&v.iter().map(|x| x.0).collect::<Vec<_>>(), &starts, 60 * MIN) {
            let part = &v[run];
            let mut ids: Vec<i64> = part.iter().map(|x| x.1).collect();
            ids.sort();
            plain.push(("c1".to_string(), k.clone(), sc.clone(), part[0].0, part[part.len() - 1].0, part.len() as i64, ids));
        }
    }
    plain.sort();
    assert_eq!(live, plain, "kept on arrival = the rule applied at once");

    // A build from scratch.
    {
        let c = s.conn.lock().unwrap();
        c.execute("DELETE FROM behavior_state", []).unwrap();
    }
    ensure(&s).unwrap();
    assert_eq!(stored(&s), live, "build from scratch");
    // Judging again (the settings are the same; force it).
    assert!(s.start_recompute(true).unwrap());
    while s.job.lock().unwrap().as_ref().is_some_and(|j| j.state == "running") {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(s.job.lock().unwrap().as_ref().unwrap().state, "done");
    assert_eq!(stored(&s), live, "re-judging");
    // The comparison can fail: a different join gap gives other behaviors.
    let s2 = store_with(|c| c.behaviors.join_minutes = 30);
    for r in &recs {
        s2.insert_event("c1", r).unwrap();
    }
    assert_ne!(stored(&s2), live);
}

/// A first start with other cutting settings builds the table once; the next start does nothing.
#[test]
fn a_changed_setting_builds_once_and_only_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b.db");
    let path = path.to_str().unwrap();
    {
        let s = Store::open_with(path, Arc::new(Config::defaults())).unwrap();
        for (i, t) in [at(10, 0), at(10, 40), at(11, 50)].into_iter().enumerate() {
            s.insert_event("c1", &patch(&format!("a{i}"), "shop", t)).unwrap();
        }
        assert_eq!(stored(&s).len(), 2);
    }
    let mut cfg = Config::defaults();
    cfg.behaviors.join_minutes = 30;
    let s = Store::open_with(path, Arc::new(cfg.clone())).unwrap();
    assert_eq!(stored(&s).len(), 3, "rebuilt with 30 minutes");
    let ids: Vec<i64> = s.conn.lock().unwrap().prepare("SELECT id FROM behaviors ORDER BY id").unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
    drop(s);
    let s = Store::open_with(path, Arc::new(cfg)).unwrap();
    let again: Vec<i64> = s.conn.lock().unwrap().prepare("SELECT id FROM behaviors ORDER BY id").unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
    assert_eq!(ids, again, "same settings: not built again");
}

#[test]
fn settings_are_checked() {
    let ok = Settings::default();
    assert!(ok.check().is_ok());
    for bad in [
        Settings { cell_minutes: 7, ..ok.clone() },
        Settings { join_minutes: 0, ..ok.clone() },
        Settings { look: vec!["critical".into()], ..ok.clone() },
        Settings { tag_only: vec!["no_such_rule".into()], ..ok.clone() },
    ] {
        assert!(bad.check().is_err(), "{bad:?}");
    }
}

#[test]
fn cells_align_in_the_server_zone() {
    // Taipei is +08:00: 10:07 local is in the cell starting 10:00 local.
    let (_, start) = cell(TZ, at(10, 7), 10 * MIN);
    assert_eq!(start, at(10, 0));
    // Kathmandu is +05:45: cells start at :00, :10 local, not on UTC's ten minutes.
    let ktm = chrono_tz::Asia::Kathmandu;
    let t = ktm.with_ymd_and_hms(2026, 10, 20, 10, 7, 0).unwrap().timestamp_millis();
    assert_eq!(cell(ktm, t, 10 * MIN).1, ktm.with_ymd_and_hms(2026, 10, 20, 10, 0, 0).unwrap().timestamp_millis());
}

#[test]
fn the_page_reads_by_index() {
    let s = Store::open(":memory:").unwrap();
    let c = s.conn.lock().unwrap();
    let plan = |sql: &str, n: usize| -> String {
        let p: Vec<i64> = vec![0; n];
        c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap().query_map(rusqlite::params_from_iter(p), |r| r.get::<_, String>(3)).unwrap().map(Result::unwrap).collect::<Vec<_>>().join("; ")
    };
    let p = plan(OPS_SQL, 3);
    assert!(p.contains("behavior_ops_of"), "{p}");
    let p = plan("SELECT id FROM behaviors WHERE last_ms >= ?1 AND first_ms < ?2", 2);
    assert!(p.contains("behaviors_time"), "{p}");
    let _ = day_of(0);
}

/// On a copy of a real database (AL_TEST_DB): cutting the records one by one in the order
/// they arrived (by ID) gives the same behaviors as the build, which goes in time order.
/// Run: `AL_TEST_DB=<copy> cargo test --release real_database_arrival_order -- --ignored`.
#[test]
#[ignore]
fn real_database_arrival_order_gives_the_build() {
    let path = std::env::var("AL_TEST_DB").expect("AL_TEST_DB: path to a copy");
    let s = Store::open(&path).unwrap();
    while building_since(&s).is_some() {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let built = stored(&s);
    let t = std::time::Instant::now();
    {
        let mut c = s.conn.lock().unwrap();
        let tx = c.transaction().unwrap();
        tx.execute_batch("DELETE FROM behaviors; DELETE FROM behavior_ops;").unwrap();
        let sql = format!(
            "{SELECT}, EXISTS (SELECT 1 FROM alerts a WHERE a.event_id=e.id AND a.severity IN ('high','medium')) FROM events e WHERE {} ORDER BY e.id",
            crate::shown::PREDICATE
        );
        let rs = rows(&tx, &sql, []).unwrap();
        add_rows(&tx, LIVE, &s.cfg.behaviors, &rs).unwrap();
        tx.commit().unwrap();
    }
    let by_id = stored(&s);
    let ops: usize = built.iter().map(|b| b.6.len()).sum();
    println!("behaviors {} with {ops} operations; arrival order took {:.1} s", built.len(), t.elapsed().as_secs_f64());
    assert!(built.len() > 100);
    assert_eq!(by_id, built);
}
