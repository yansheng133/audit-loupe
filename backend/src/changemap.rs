//! `/api/changemap` v0 (prototype): one request per page view, answered straight from the
//! existing tables. No summary tables and no tuning; see `docs/api/changemap-v0.md`.

use crate::store::Store;
use chrono::{NaiveDate, TimeZone, Utc};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;

pub const PAGE: usize = 100;
const TICKS: usize = 200;
const DETAIL_OPS: usize = 200;
const EVENTS: usize = 500;
const SLOT_GRACE_MS: i64 = 10 * 60_000;

#[derive(Deserialize, Default, Debug, Clone)]
pub struct Params {
    pub day: Option<String>,
    #[serde(default)]
    pub cluster: Vec<String>,
    pub namespace: Option<String>,
    pub state: Option<String>,
    pub tab: Option<String>,
    pub cursor: Option<String>,
    pub select: Option<String>,
    pub system: Option<String>,
    /// Object key for `/api/changemap/detail`.
    pub key: Option<String>,
}

/// Parse a query string that may repeat `cluster`.
pub fn parse_query(q: &str) -> Params {
    let mut p = Params::default();
    for pair in q.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = decode(v);
        match k {
            "day" => p.day = Some(v),
            "cluster" => p.cluster.extend(v.split(',').filter(|s| !s.is_empty()).map(str::to_string)),
            "namespace" => p.namespace = Some(v),
            "state" => p.state = Some(v),
            "tab" => p.tab = Some(v),
            "cursor" => p.cursor = Some(v),
            "select" => p.select = Some(v),
            "system" => p.system = Some(v),
            "key" => p.key = Some(v),
            _ => {}
        }
    }
    p
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Who did it, in the five categories the page shows (report-ui-facts section B).
pub fn categorize(actor: &str, human: bool, user_agent: &str, principals: Option<&str>) -> (&'static str, &'static str) {
    if let Some(rest) = actor.strip_prefix("system:serviceaccount:") {
        let ns = rest.split(':').next().unwrap_or("");
        return if ns.starts_with("cattle-") {
            ("rancher", "Rancher 安裝的元件所用的 ServiceAccount")
        } else if ns == "kube-system" {
            ("component", "叢集內建元件")
        } else {
            ("serviceaccount", "工作負載的 ServiceAccount")
        };
    }
    match actor {
        "system:admin" => return ("human", "管理者憑證，對不到個人"),
        "system:cattle:error" => return ("unknown", "Rancher 未通過驗證的請求，看不出是誰"),
        "" => return ("unknown", "找不到執行者"),
        _ => {}
    }
    if actor.starts_with("system:") || actor.ends_with("cloud-controller-manager") || user_agent.starts_with("cloud-controller-manager/") {
        return ("component", "叢集內建元件");
    }
    if let Some(p) = principals {
        if p.contains("system://") {
            return ("rancher", "Rancher 內部帳號（叢集佈建）");
        }
        let ids: Vec<String> = serde_json::from_str(p).unwrap_or_default();
        if human && !ids.is_empty() && ids.iter().all(|i| i.starts_with("local://")) {
            return ("human", "Rancher 本機帳號，可能多人共用");
        }
    }
    if human { ("human", "") } else { ("unknown", "看不出是人或程式") }
}

/// Rancher UI housekeeping: temporary RBAC for the kubectl shell and per-user UI settings.
fn ui_maintenance(resource: &str, name: &str) -> bool {
    name.starts_with("pod-impersonation-shell-")
        || name.starts_with("pod-impersonation-helm-")
        || name.starts_with("impersonation-shell-")
        || matches!(resource, "userpreferences" | "userattributes")
}

fn is_write(source: &str, verb: &str, sub: &str) -> bool {
    if source == "rancher" {
        return !matches!(verb, "get" | "list" | "watch");
    }
    matches!(verb, "create" | "update" | "patch" | "delete" | "deletecollection") || matches!(sub, "exec" | "attach" | "portforward")
}

fn kind_from_resource(resource: &str) -> String {
    let known = [
        ("pods", "Pod"), ("deployments", "Deployment"), ("replicasets", "ReplicaSet"), ("daemonsets", "DaemonSet"), ("statefulsets", "StatefulSet"),
        ("services", "Service"), ("configmaps", "ConfigMap"), ("secrets", "Secret"), ("namespaces", "Namespace"), ("nodes", "Node"),
        ("serviceaccounts", "ServiceAccount"), ("roles", "Role"), ("rolebindings", "RoleBinding"), ("clusterroles", "ClusterRole"),
        ("clusterrolebindings", "ClusterRoleBinding"), ("jobs", "Job"), ("cronjobs", "CronJob"), ("leases", "Lease"), ("events", "Event"),
        ("endpointslices", "EndpointSlice"), ("persistentvolumeclaims", "PersistentVolumeClaim"), ("persistentvolumes", "PersistentVolume"),
        ("customresourcedefinitions", "CustomResourceDefinition"), ("apiservices", "APIService"), ("ingresses", "Ingress"),
        ("networkpolicies", "NetworkPolicy"), ("clusters", "Cluster"), ("projects", "Project"), ("users", "User"), ("tokens", "Token"),
    ];
    known.iter().find(|(r, _)| *r == resource).map(|(_, k)| k.to_string()).unwrap_or_else(|| resource.to_string())
}

/// Requests that leave no object behind (API contract v0.1, point 1). They are not changes to
/// an object, so they are counted in `hidden.requestOnly` instead of listed.
const REQUEST_ONLY: [&str; 5] = ["tokenreviews", "subjectaccessreviews", "selfsubjectaccessreviews", "selfsubjectreviews", "localsubjectaccessreviews"];

/// Name shown for a write without a name: `deletecollection`, or a create whose name the
/// server generated (v0.1, point 1).
pub const BATCH: &str = "*";

/// First instant of a local day. Where midnight does not exist (clocks jump forward at 00:00),
/// the day starts at the first local minute that does.
fn day_start(tz: chrono_tz::Tz, d: NaiveDate) -> i64 {
    let mut t = d.and_hms_opt(0, 0, 0).unwrap();
    for _ in 0..24 * 60 {
        if let Some(dt) = tz.from_local_datetime(&t).earliest() {
            return dt.timestamp_millis();
        }
        t += chrono::Duration::minutes(1);
    }
    Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap()).timestamp_millis()
}

/// Page cursor (v0.1, point 2). Opaque to the page: hex of the newest event and version ids
/// seen when the first page was read (so later pages are cut from the same data) and the sort
/// key of the last row shown.
#[derive(Debug, PartialEq)]
struct Cursor {
    ev_max: i64,
    ver_max: i64,
    rank: usize,
    last: i64,
    key: String,
}

fn enc_cursor(c: &Cursor) -> String {
    format!("{}\n{}\n{}\n{}\n{}", c.ev_max, c.ver_max, c.rank, c.last, c.key).bytes().map(|b| format!("{b:02x}")).collect()
}

fn dec_cursor(s: &str) -> Option<Cursor> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect();
    let text = String::from_utf8(bytes?).ok()?;
    let mut it = text.splitn(5, '\n');
    Some(Cursor {
        ev_max: it.next()?.parse().ok()?,
        ver_max: it.next()?.parse().ok()?,
        rank: it.next()?.parse().ok()?,
        last: it.next()?.parse().ok()?,
        key: it.next()?.to_string(),
    })
}

/// An object key `cluster/apiGroup/resource/namespace/name` taken apart.
#[derive(Debug, Clone)]
struct ObjKey {
    cluster: String,
    api_group: String,
    resource: String,
    namespace: String,
    name: String,
}

fn parse_key(k: &str) -> Option<ObjKey> {
    let mut it = k.splitn(5, '/');
    let (cluster, api_group, resource, namespace, name) = (it.next()?, it.next()?, it.next()?, it.next()?, it.next()?);
    if cluster.is_empty() || resource.is_empty() || name.is_empty() {
        return None;
    }
    Some(ObjKey { cluster: cluster.into(), api_group: api_group.into(), resource: resource.into(), namespace: namespace.into(), name: name.into() })
}

const STATES: [&str; 4] = ["look", "unclear", "blind", "clear"];
fn rank(s: &str) -> usize {
    match s {
        "look" => 0,
        "unclear" => 1,
        "blind" => 2,
        "clear" => 3,
        _ => 4,
    }
}

/// One operation on an object: an audit record, or a version nobody could be found for.
#[derive(Clone, Debug)]
struct Op {
    t: i64,
    cluster: String,
    key: String,
    resource: String,
    namespace: String,
    name: String,
    actor: String,
    category: &'static str,
    note: &'static str,
    via: String,
    verb: String,
    severity: Option<String>,
    unattributed: bool,
    hashed: bool,
}

#[derive(Default, Debug)]
struct Obj {
    key: String,
    kind: String,
    cluster: String,
    namespace: String,
    name: String,
    api_group: String,
    resource: String,
    ops: Vec<usize>,
    state: &'static str,
    severity: Option<String>,
    reason: String,
    last: i64,
}

type ChainMap = HashMap<String, (Option<i64>, i64)>;

/// Last chain check (when, result) and whether one is running now.
static CHAIN: Mutex<(Option<(i64, ChainMap)>, bool)> = Mutex::new((None, false));

/// Chain check results. Walking the whole chain takes about a second, so it runs in the
/// background: the page gets the last result (or `None` before the first one finishes,
/// shown as `pending`) and a new check starts when the result is over 10 minutes old.
fn chain_status(store: &Store, now_ms: i64) -> Option<ChainMap> {
    let mut g = CHAIN.lock().unwrap();
    let stale = g.0.as_ref().is_none_or(|(at, _)| now_ms - at >= 600_000);
    if stale && !g.1 {
        g.1 = true;
        let store = store.reader();
        std::thread::spawn(move || {
            let result = store.verify_chain(None);
            let mut g = CHAIN.lock().unwrap();
            g.1 = false;
            match result {
                Ok(rows) => g.0 = Some((Utc::now().timestamp_millis(), rows.into_iter().map(|r| (r.cluster, (r.first_bad_id, r.head_id))).collect())),
                Err(e) => tracing::warn!(error = %e, "chain check failed"),
            }
        });
    }
    g.0.as_ref().map(|(_, m)| m.clone())
}

/// Start the first chain check when the server starts, so the first page view is not `pending`.
pub fn warm(store: &Store) {
    chain_status(store, Utc::now().timestamp_millis());
}

/// One page view. `AL_DATA_NOTE`, when set, is shown as a note on the page (v0.1, point 5).
pub fn changemap(store: &Store, tz: chrono_tz::Tz, p: &Params, now_ms: i64) -> anyhow::Result<Value> {
    let note = std::env::var("AL_DATA_NOTE").ok().filter(|n| !n.is_empty());
    build(store, tz, p, now_ms, None, note.as_deref())
}

/// The detail of one object (v0.1, point 4): what a click on a row needs, without working out
/// the whole page again. `Null` when the object has nothing that day.
pub fn detail(store: &Store, tz: chrono_tz::Tz, p: &Params, now_ms: i64) -> anyhow::Result<Value> {
    let key = p.key.as_deref().and_then(parse_key).ok_or_else(|| anyhow::anyhow!("key: expected cluster/apiGroup/resource/namespace/name"))?;
    let v = build(store, tz, p, now_ms, Some(&key), None)?;
    Ok(v.get("detail").cloned().unwrap_or(Value::Null))
}

fn build(store: &Store, tz: chrono_tz::Tz, p: &Params, now_ms: i64, only: Option<&ObjKey>, note: Option<&str>) -> anyhow::Result<Value> {
    let day = match p.day.as_deref().filter(|d| !d.is_empty()) {
        Some(d) => NaiveDate::parse_from_str(d, "%Y-%m-%d").map_err(|e| anyhow::anyhow!("day: {e}"))?,
        None => Utc::now().with_timezone(&tz).date_naive(),
    };
    let start = day_start(tz, day);
    let end = day_start(tz, day.succ_opt().ok_or_else(|| anyhow::anyhow!("day: out of range"))?);
    let tab = p.tab.as_deref().unwrap_or("overview");
    let with_system = p.system.as_deref() == Some("1");
    let clusters: HashSet<&str> = p.cluster.iter().map(String::as_str).collect();
    let ns_filter = p.namespace.as_deref().filter(|s| !s.is_empty());
    let chains = chain_status(store, now_ms);

    let cursor = match p.cursor.as_deref().filter(|c| !c.is_empty()) {
        Some(s) => Some(dec_cursor(s).ok_or_else(|| anyhow::anyhow!("cursor: not one this server gave out"))?),
        None => None,
    };

    let c = store.conn.lock().unwrap();
    // Later pages are cut from the data the first page saw, so rows neither repeat nor go missing
    // while new records arrive (v0.1, point 2).
    let (ev_max, ver_max): (i64, i64) = match &cursor {
        Some(k) => (k.ev_max, k.ver_max),
        None => (
            c.query_row("SELECT coalesce(max(id), 0) FROM events", [], |r| r.get(0))?,
            c.query_row("SELECT coalesce(max(id), 0) FROM versions", [], |r| r.get(0))?,
        ),
    };
    let principals: HashMap<String, String> =
        c.prepare("SELECT user_id, principal_ids FROM users")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
    // Which collectors each cluster runs decides why a change has no author.
    let mut collectors: HashMap<String, HashSet<String>> = HashMap::new();
    for row in c.prepare("SELECT cluster, collector FROM heartbeats")?.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (cl, col) = row?;
        collectors.entry(cl).or_default().insert(col);
    }
    let mut as_of: BTreeMap<String, i64> = BTreeMap::new();
    for row in c.prepare("SELECT cluster, max(ts_ms) FROM events GROUP BY cluster")?.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (cl, t) = row?;
        as_of.insert(cl, t);
    }
    for row in c.prepare("SELECT cluster, max(observed_ms) FROM versions GROUP BY cluster")?.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (cl, t) = row?;
        let e = as_of.entry(cl).or_insert(t);
        *e = (*e).max(t);
    }
    let kinds: HashMap<(String, String, String), String> = c
        .prepare("SELECT DISTINCT cluster, api_group, resource, kind FROM versions")?
        .query_map([], |r| Ok(((r.get(0)?, r.get(1)?, r.get(2)?), r.get(3)?)))?
        .collect::<Result<_, _>>()?;

    let mut ops: Vec<Op> = Vec::new();
    let mut hidden_system = 0i64;
    let mut hidden_ui = 0i64;
    let mut hidden_request = 0i64;
    let mut objs: HashMap<String, Obj> = HashMap::new();
    let add = |op: Op, api_group: &str, objs: &mut HashMap<String, Obj>, ops: &mut Vec<Op>| {
        let o = objs.entry(op.key.clone()).or_insert_with(|| Obj {
            key: op.key.clone(),
            kind: kinds.get(&(op.cluster.clone(), api_group.to_string(), op.resource.clone())).cloned().unwrap_or_else(|| kind_from_resource(&op.resource)),
            cluster: op.cluster.clone(),
            namespace: op.namespace.clone(),
            name: op.name.clone(),
            api_group: api_group.to_string(),
            resource: op.resource.clone(),
            ..Default::default()
        });
        o.ops.push(ops.len());
        o.last = o.last.max(op.t);
        ops.push(op);
    };

    let mut st = c.prepare(&format!(
        "SELECT e.cluster, e.ts_ms, e.actor, e.human, e.via, e.verb, e.api_group, e.resource, e.subresource, e.namespace, e.name,
                e.user_agent, e.user_id, e.source, e.hash IS NOT NULL,
                (SELECT CASE WHEN max(a.severity='high') THEN 'high' WHEN max(a.severity='medium') THEN 'medium' END FROM alerts a WHERE a.event_id=e.id)
         FROM events e WHERE e.ts_ms >= ?1 AND e.ts_ms < ?2 AND e.id <= ?3 AND {} {}",
        crate::store::SHOWN_PUB,
        if only.is_some() { "AND e.cluster = ?4 AND e.resource = ?5 AND e.namespace = ?6 AND e.name = ?7" } else { "" }
    ))?;
    let only_name = |k: &ObjKey| if k.name == BATCH { String::new() } else { k.name.clone() };
    let mut rows = match only {
        Some(k) => st.query(params![start, end, ev_max, k.cluster, k.resource, k.namespace, only_name(k)])?,
        None => st.query(params![start, end, ev_max])?,
    };
    while let Some(r) = rows.next()? {
        let cluster: String = r.get(0)?;
        if !clusters.is_empty() && !clusters.contains(cluster.as_str()) {
            continue;
        }
        let (verb, sub, source): (String, String, String) = (r.get(5)?, r.get(8)?, r.get(13)?);
        let severity: Option<String> = r.get(15)?;
        if !is_write(&source, &verb, &sub) && severity.is_none() {
            continue;
        }
        let (namespace, mut name, resource, api_group): (String, String, String, String) = (r.get(9)?, r.get(10)?, r.get(7)?, r.get(6)?);
        if ns_filter.is_some_and(|n| n != namespace) || only.is_some_and(|k| k.api_group != api_group) {
            continue;
        }
        // A write without a name is a deletecollection or a create the server named. It is
        // listed as one batch row per resource and namespace; requests that leave no object
        // behind are only counted (v0.1, point 1).
        if name.is_empty() {
            if REQUEST_ONLY.contains(&resource.as_str()) {
                hidden_request += 1;
                continue;
            }
            name = BATCH.to_string();
        }
        let (actor, human, ua, uid): (String, bool, String, String) = (r.get(2)?, r.get::<_, i64>(3)? == 1, r.get(11)?, r.get(12)?);
        let (category, note) = categorize(&actor, human, &ua, principals.get(&uid).map(String::as_str));
        if !with_system {
            if ui_maintenance(&resource, &name) {
                hidden_ui += 1;
                continue;
            }
            if matches!(category, "component" | "rancher") {
                hidden_system += 1;
                continue;
            }
        }
        let key = format!("{cluster}/{api_group}/{resource}/{namespace}/{name}");
        // Shown as the subresource for entering a container (exec, attach, port-forward) whatever
        // the verb: current kubectl opens these over a websocket with GET, older ones with POST
        // (create). Other subresources keep the old rule: only a create is named after them.
        let verb = if matches!(sub.as_str(), "exec" | "attach" | "portforward") || (!sub.is_empty() && verb == "create") { sub.clone() } else { verb };
        let op = Op { t: r.get(1)?, cluster, key, resource, namespace, name, actor, category, note, via: r.get(4)?, verb, severity, unattributed: false, hashed: r.get(14)? };
        add(op, &api_group, &mut objs, &mut ops);
    }
    drop(rows);
    drop(st);

    let mut st = c.prepare(
        "SELECT cluster, observed_ms, api_group, resource, namespace, name, change FROM versions
         WHERE event_id IS NULL AND change <> 'baseline' AND observed_ms >= ?1 AND observed_ms < ?2 AND id <= ?3",
    )?;
    let mut rows = st.query(params![start, end, ver_max])?;
    while let Some(r) = rows.next()? {
        let cluster: String = r.get(0)?;
        if !clusters.is_empty() && !clusters.contains(cluster.as_str()) {
            continue;
        }
        let (api_group, resource, namespace, name): (String, String, String, String) = (r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?);
        if ns_filter.is_some_and(|n| n != namespace)
            || only.is_some_and(|k| k.api_group != api_group || k.resource != resource || k.namespace != namespace || k.name != name)
        {
            continue;
        }
        if !with_system && ui_maintenance(&resource, &name) {
            hidden_ui += 1;
            continue;
        }
        let key = format!("{cluster}/{api_group}/{resource}/{namespace}/{name}");
        let op = Op {
            t: r.get(1)?, cluster, key, resource, namespace, name, actor: String::new(), category: "unknown", note: "找不到執行者",
            via: "unknown".into(), verb: r.get(6)?, severity: None, unattributed: true, hashed: false,
        };
        add(op, &api_group, &mut objs, &mut ops);
    }
    drop(rows);
    drop(st);

    // Collection gaps overlapping the day.
    let mut gaps: Vec<(String, i64, i64, String)> = Vec::new();
    let mut st = c.prepare("SELECT cluster, from_ms, to_ms, collector FROM gaps WHERE to_ms >= ?1 AND from_ms < ?2 ORDER BY from_ms")?;
    for row in st.query_map(params![start, end], |r| Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?, r.get::<_, String>(3)?)))? {
        let g = row?;
        if clusters.is_empty() || clusters.contains(g.0.as_str()) {
            gaps.push(g);
        }
    }
    drop(st);

    let unclear_reason = |cluster: &str| -> String {
        let cols = collectors.get(cluster);
        if cols.is_some_and(|c| !c.contains("file")) {
            "這座叢集沒有收 apiserver audit log：只看得到物件變了，看不到是誰改的".into()
        } else {
            "收集中斷後重新連線比對物件版本才發現的變更，找不到對應的 audit 紀錄".into()
        }
    };
    let in_gap = |cluster: &str, t: i64| gaps.iter().any(|(cl, f, to, _)| cl == cluster && t >= f - SLOT_GRACE_MS && t <= to + SLOT_GRACE_MS);

    for o in objs.values_mut() {
        let sev = o.ops.iter().filter_map(|i| ops[*i].severity.clone()).min_by_key(|s| if s == "high" { 0 } else { 1 });
        if let Some(s) = sev {
            o.state = "look";
            o.reason = if s == "high" { "當天有高風險警示".into() } else { "當天有中風險警示".into() };
            o.severity = Some(s);
        } else if o.ops.iter().any(|i| ops[*i].unattributed) {
            o.state = "unclear";
            o.reason = unclear_reason(&o.cluster);
        } else if o.ops.iter().any(|i| in_gap(&o.cluster, ops[*i].t)) {
            o.state = "blind";
            o.reason = "有紀錄落在收集中斷前後 10 分鐘內，可能不完整".into();
        } else {
            o.state = "clear";
        }
    }

    let ticks = |o: &Obj| -> Vec<Value> {
        let mut v: Vec<&Op> = o.ops.iter().map(|i| &ops[*i]).collect();
        v.sort_by_key(|op| op.t);
        if v.len() > TICKS {
            // Keep every alert and an even spread of the rest.
            let step = v.len() as f64 / TICKS as f64;
            let alerts: Vec<&Op> = v.iter().copied().filter(|op| op.severity.is_some()).collect();
            let mut keep: Vec<&Op> = (0..TICKS).map(|i| v[(i as f64 * step) as usize]).collect();
            keep.extend(alerts);
            keep.sort_by_key(|op| op.t);
            keep.dedup_by_key(|op| (op.t, op.severity.is_some()));
            v = keep;
        }
        v.iter().map(|op| json!({"t": op.t, "alert": op.severity.is_some()})).collect()
    };
    // What the right-hand panel shows for one object.
    let detail_of = |sel: Option<&Obj>| -> anyhow::Result<Value> {
        let Some(o) = sel else { return Ok(Value::Null) };
        let mut list: Vec<&Op> = o.ops.iter().map(|i| &ops[*i]).collect();
        list.sort_by_key(|op| std::cmp::Reverse(op.t));
        let ops_json: Vec<Value> = list
            .iter()
            .take(DETAIL_OPS)
            .map(|op| json!({"t": op.t, "who": op.actor, "category": op.category, "note": op.note, "verb": op.verb, "via": op.via,
                             "raw": {"verb": op.verb, "resource": op.resource, "user": op.actor}}))
            .collect();
        // Objects this one refers to (v0.1, point 3); kept apart from the settings below.
        let mut st = c.prepare_cached(
            "SELECT DISTINCT rel, to_kind, to_ns, to_name FROM relations WHERE cluster=?1 AND from_kind=?2 AND from_ns=?3 AND from_name=?4 LIMIT 50",
        )?;
        let related: Vec<Value> = st
            .query_map(params![o.cluster, o.kind, o.namespace, o.name], |r| {
                Ok(json!({"rel": r.get::<_, String>(0)?, "kind": r.get::<_, String>(1)?, "namespace": r.get::<_, String>(2)?, "name": r.get::<_, String>(3)?}))
            })?
            .collect::<Result<_, _>>()?;
        // Settings that decide what the records can show. v0.1 has one: what was typed inside a
        // container is never in the audit log.
        let mut depends: Vec<Value> = Vec::new();
        if o.kind == "Pod" && list.iter().any(|op| matches!(op.verb.as_str(), "exec" | "attach")) {
            depends.push(json!({
                "setting": "是否啟用 SUSE Security 收集",
                "text": "容器裡輸入的指令不在 audit log。有啟用時，這裡會列出 SUSE Security 判定違規的程序。",
            }));
        }
        let has_versions: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM versions WHERE cluster=?1 AND api_group=?2 AND resource=?3)",
            params![o.cluster, o.api_group, o.resource],
            |r| r.get(0),
        )?;
        let hash = match chains.as_ref().map(|m| m.get(&o.cluster)) {
            Some(Some((Some(_), _))) => "broken",
            _ if list.iter().any(|op| !op.unattributed && !op.hashed) => "pending",
            Some(Some(_)) => "ok",
            _ => "pending",
        };
        Ok(json!({
            "key": o.key, "batch": o.name == BATCH, "state": o.state, "severity": o.severity, "stateReason": o.reason,
            "strip": ticks(o), "ops": ops_json, "dependsOn": depends, "related": related, "hasVersions": has_versions, "hashChain": hash,
        }))
    };
    if let Some(k) = only {
        let key = format!("{}/{}/{}/{}/{}", k.cluster, k.api_group, k.resource, k.namespace, k.name);
        return Ok(json!({"detail": detail_of(objs.get(&key))?}));
    }

    let mut list: Vec<&Obj> = objs.values().collect();
    list.sort_by(|a, b| rank(a.state).cmp(&rank(b.state)).then(b.last.cmp(&a.last)).then(a.key.cmp(&b.key)));
    let mut counts = json!({"objects": list.len(), "ops": ops.len(), "look": 0, "unclear": 0, "blind": 0, "clear": 0});
    for o in &list {
        counts[o.state] = json!(counts[o.state].as_i64().unwrap_or(0) + 1);
    }
    let state_filter = p.state.as_deref().filter(|s| STATES.contains(s));
    let shown: Vec<&Obj> = list.iter().copied().filter(|o| state_filter.is_none_or(|s| o.state == s)).collect();

    // Timeline.
    let slot_min: i64 = if clusters.len() == 1 { 1 } else { 10 };
    let slot_ms = slot_min * 60_000;
    let n_slots = ((end - start) / slot_ms) as usize;
    let mut line_keys: Vec<Option<String>> = if clusters.is_empty() {
        vec![None]
    } else {
        let mut v: Vec<_> = p.cluster.iter().cloned().map(Some).collect();
        v.dedup();
        v
    };
    line_keys.dedup();
    let latest = if clusters.is_empty() { as_of.values().copied().max() } else { clusters.iter().filter_map(|c| as_of.get(*c)).copied().max() };
    let mut lines = Vec::new();
    for lk in &line_keys {
        let mut slots: Vec<(usize, i64, i64)> = vec![(9, 0, 0); n_slots]; // (rank, ops, alerts); 9 = none
        for o in objs.values().filter(|o| lk.as_ref().is_none_or(|c| *c == o.cluster)) {
            for i in &o.ops {
                let op = &ops[*i];
                let s = ((op.t - start) / slot_ms) as usize;
                if s >= n_slots {
                    continue;
                }
                let r = if op.severity.is_some() { 0 } else if op.unattributed { 1 } else if in_gap(&op.cluster, op.t) { 2 } else { 3 };
                slots[s].0 = slots[s].0.min(r);
                slots[s].1 += 1;
                slots[s].2 += op.severity.is_some() as i64;
            }
        }
        let out: Vec<Value> = slots
            .iter()
            .enumerate()
            .map(|(i, (r, n, a))| {
                let t = start + i as i64 * slot_ms;
                let gap = gaps.iter().any(|(cl, f, to, _)| lk.as_ref().is_none_or(|c| c == cl) && *f < t + slot_ms && *to > t);
                let state = match r {
                    0 => "look",
                    1 => "unclear",
                    2 => "blind",
                    3 if gap => "blind",
                    3 => "clear",
                    _ if gap => "blind",
                    _ if latest.is_none_or(|l| t > l) => "unknown",
                    _ => "none",
                };
                json!({"t": t, "state": state, "ops": n, "alerts": a})
            })
            .collect();
        lines.push(json!({"cluster": lk, "slots": out}));
    }
    let gaps_json: Vec<Value> = gaps
        .iter()
        .map(|(cl, f, t, col)| json!({"cluster": cl, "fromMs": f, "toMs": t, "kind": "heartbeat", "reason": format!("收集元件 {col} 沒有回報")}))
        .collect();

    // Prefer the latest op with a known author; fall back to the latest unattributed change.
    let latest_op = |o: &Obj| -> &Op { o.ops.iter().map(|i| &ops[*i]).max_by_key(|op| (!op.unattributed, op.t)).unwrap() };

    let mut resp = json!({
        "day": day.format("%Y-%m-%d").to_string(),
        "tz": tz.name(),
        "asOf": as_of.iter().filter(|(c, _)| clusters.is_empty() || clusters.contains(c.as_str()))
            .map(|(c, t)| (c.clone(), json!(Utc.timestamp_millis_opt(*t).unwrap().to_rfc3339_opts(chrono::SecondsFormat::Millis, true))))
            .collect::<serde_json::Map<_, _>>(),
        "counts": counts,
        "hidden": {"system": hidden_system, "uiMaintenance": hidden_ui, "requestOnly": hidden_request},
        "timeline": {"slotMinutes": slot_min, "lines": lines, "gaps": gaps_json},
    });

    if tab == "overview" {
        let after = cursor.as_ref().map(|k| (k.rank, std::cmp::Reverse(k.last), k.key.as_str()));
        let mut rest: Vec<&Obj> = shown
            .iter()
            .copied()
            .filter(|o| after.is_none_or(|a| (rank(o.state), std::cmp::Reverse(o.last), o.key.as_str()) > a))
            .take(PAGE + 1)
            .collect();
        let more = rest.len() > PAGE;
        rest.truncate(PAGE);
        let next = if more {
            rest.last().map(|o| enc_cursor(&Cursor { ev_max, ver_max, rank: rank(o.state), last: o.last, key: o.key.clone() }))
        } else {
            None
        };
        let page: Vec<Value> = rest
            .iter()
            .map(|o| {
                let l = latest_op(o);
                json!({
                    "key": o.key, "kind": o.kind, "name": o.name, "namespace": o.namespace, "cluster": o.cluster, "batch": o.name == BATCH,
                    "state": o.state, "severity": o.severity, "stateReason": o.reason,
                    "ops": o.ops.len(), "lastMs": o.last, "ticks": ticks(o),
                    "who": {"name": l.actor, "category": l.category, "note": l.note},
                    "via": l.via,
                })
            })
            .collect();
        resp["rows"] = json!(page);
        resp["next"] = json!(next);
    }

    if tab == "relations" {
        let mut idents: HashMap<String, (&'static str, i64)> = HashMap::new();
        let mut e_iv: HashMap<(String, String), (i64, bool)> = HashMap::new();
        let mut e_vc: HashMap<(String, String), i64> = HashMap::new();
        let mut e_co: HashMap<(String, String), i64> = HashMap::new();
        for o in &shown {
            for i in &o.ops {
                let op = &ops[*i];
                let id = if op.actor.is_empty() { "(找不到執行者)".to_string() } else { op.actor.clone() };
                let e = idents.entry(id.clone()).or_insert((op.category, 0));
                e.1 += 1;
                let bypass = op.via == "direct" && matches!(op.category, "human" | "unknown");
                let ev = e_iv.entry((id, op.via.clone())).or_insert((0, bypass));
                ev.0 += 1;
                *e_vc.entry((op.via.clone(), op.cluster.clone())).or_insert(0) += 1;
                *e_co.entry((op.cluster.clone(), o.key.clone())).or_insert(0) += 1;
            }
        }
        let group = idents.len() > 20;
        let ident_node = |id: &str| -> String { if group { format!("group:{}", idents[id].0) } else { format!("identity:{id}") } };
        let mut nodes: BTreeMap<String, Value> = BTreeMap::new();
        for (id, (cat, _)) in &idents {
            let nid = ident_node(id);
            let label = if group { format!("{cat}（群組）") } else { id.clone() };
            nodes.entry(nid.clone()).or_insert(json!({"id": nid, "type": "identity", "label": label, "category": cat}));
        }
        let mut edges: BTreeMap<(String, String), (i64, bool)> = BTreeMap::new();
        for ((id, via), (n, b)) in &e_iv {
            let vid = format!("via:{via}");
            nodes.entry(vid.clone()).or_insert(json!({"id": vid, "type": "via", "label": via}));
            let e = edges.entry((ident_node(id), vid)).or_insert((0, false));
            e.0 += n;
            e.1 |= *b;
        }
        for ((via, cl), n) in &e_vc {
            let cid = format!("cluster:{cl}");
            nodes.entry(cid.clone()).or_insert(json!({"id": cid, "type": "cluster", "label": cl}));
            edges.entry((format!("via:{via}"), cid)).or_insert((0, false)).0 += n;
        }
        // Objects only for the ones worth a look, so the graph stays readable.
        let state_of: HashMap<&str, &Obj> = shown.iter().map(|o| (o.key.as_str(), *o)).collect();
        let mut obj_edges: Vec<_> = e_co.iter().filter(|((_, k), _)| state_of[k.as_str()].state != "clear").collect();
        obj_edges.sort_by(|a, b| rank(state_of[a.0.1.as_str()].state).cmp(&rank(state_of[b.0.1.as_str()].state)).then(b.1.cmp(a.1)));
        for ((cl, k), n) in obj_edges.into_iter().take(50) {
            let o = state_of[k.as_str()];
            let oid = format!("object:{k}");
            nodes.insert(oid.clone(), json!({"id": oid, "type": "object", "label": format!("{} {}", o.kind, o.name), "state": o.state}));
            edges.insert((format!("cluster:{cl}"), oid), (*n, false));
        }
        resp["relations"] = json!({
            "nodes": nodes.into_values().collect::<Vec<_>>(),
            "edges": edges.into_iter().map(|((f, t), (n, b))| json!({"from": f, "to": t, "count": n, "bypass": b})).collect::<Vec<_>>(),
        });
    }

    if tab == "events" {
        let mut evs: Vec<Value> = Vec::new();
        for o in &shown {
            let first = |pred: &dyn Fn(&Op) -> bool| o.ops.iter().map(|i| &ops[*i]).filter(|op| pred(op)).map(|op| op.t).min();
            if let Some(t) = first(&|op| op.severity.is_some()) {
                evs.push(json!({"t": t, "subject": o.key, "from": "clear", "to": "look", "reason": "第一次出現中、高風險警示（近似：v0 以此代替物件狀態轉變）", "source": "state"}));
            }
            if let Some(t) = first(&|op| op.unattributed) {
                evs.push(json!({"t": t, "subject": o.key, "from": "clear", "to": "unclear", "reason": "第一次出現找不到執行者的變更（近似：v0 以此代替物件狀態轉變）", "source": "state"}));
            }
        }
        for (cl, f, t, col) in &gaps {
            evs.push(json!({"t": f, "subject": cl, "from": "ok", "to": "blind", "reason": format!("收集元件 {col} 停止回報"), "source": "heartbeat"}));
            evs.push(json!({"t": t, "subject": cl, "from": "blind", "to": "ok", "reason": format!("收集元件 {col} 恢復回報"), "source": "heartbeat"}));
        }
        evs.sort_by_key(|e| std::cmp::Reverse(e["t"].as_i64().unwrap_or(0)));
        evs.truncate(EVENTS);
        resp["events"] = json!(evs);
    }

    // Detail of the selected object, or the first one in order.
    let sel = p.select.as_deref().and_then(|k| objs.get(k)).or_else(|| shown.first().copied());
    resp["detail"] = detail_of(sel)?;
    if let Some(n) = note {
        resp["meta"] = json!({"dataNote": n, "dataUntilMs": as_of.values().copied().max()});
    }
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::sync::Arc;

    fn store() -> Store {
        Store::open_with(":memory:", Arc::new(Config::test())).unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    fn ev(c: &rusqlite::Connection, id: i64, cluster: &str, ts: i64, actor: &str, human: i64, via: &str, verb: &str, res: &str, ns: &str, name: &str) {
        c.execute(
            "INSERT INTO events (id, cluster, audit_id, ts_ms, ts, actor, principal, human, via, caller, verb, api_group, resource, subresource,
                                 namespace, name, code, source_ip, user_agent, request_uri, source, hash)
             VALUES (?1, ?2, ?1, ?3, '', ?4, '', ?5, ?6, ?4, ?7, '', ?8, '', ?9, ?10, 200, '', '', '', 'k8s', 'h')",
            params![id, cluster, ts, actor, human, via, verb, res, ns, name],
        )
        .unwrap();
    }

    // 2026-10-20 10:00 Asia/Taipei
    const T: i64 = 1_792_461_600_000;

    #[test]
    fn states_follow_the_order_and_system_is_hidden() {
        let s = store();
        {
            let c = s.conn.lock().unwrap();
            ev(&c, 1, "c1", T, "alice", 1, "direct", "patch", "deployments", "app", "web");
            c.execute("INSERT INTO alerts (event_id, rule, severity) VALUES (1, 'bypassed_rancher', 'medium')", []).unwrap();
            ev(&c, 2, "c1", T + 60_000, "alice", 1, "rancher", "update", "configmaps", "app", "cfg");
            ev(&c, 3, "c1", T + 120_000, "system:kube-scheduler", 0, "direct", "create", "bindings", "app", "p1");
            c.execute(
                "INSERT INTO versions (cluster, api_group, resource, kind, namespace, name, uid, observed_ms, observed, change, body)
                 VALUES ('c2', 'apps', 'deployments', 'Deployment', 'demo', 'api', 'u', ?1, '', 'modified', '{}')",
                params![T + 30_000],
            )
            .unwrap();
            c.execute("INSERT INTO heartbeats (cluster, collector, instance, last_ms, details) VALUES ('c2', 'watch', 'w', 0, '{}')", []).unwrap();
            c.execute("INSERT INTO gaps (cluster, collector, instance, from_ms, to_ms) VALUES ('c1', 'file', 'n', ?1, ?2)", params![T + 3_600_000, T + 7_200_000]).unwrap();
            ev(&c, 4, "c1", T + 3_000_000, "bob", 1, "direct", "delete", "pods", "app", "old");
        }
        let p = Params { day: Some("2026-10-20".into()), ..Default::default() };
        let v = changemap(&s, chrono_tz::Asia::Taipei, &p, T).unwrap();
        let rows = v["rows"].as_array().unwrap();
        let states: Vec<&str> = rows.iter().map(|r| r["state"].as_str().unwrap()).collect();
        assert_eq!(states, vec!["look", "unclear", "blind", "clear"]);
        assert_eq!(v["hidden"]["system"], 1);
        assert_eq!(v["counts"]["objects"], 4);
        assert!(rows[1]["stateReason"].as_str().unwrap().contains("沒有收 apiserver audit log"));
        assert_eq!(rows[0]["key"], "c1//deployments/app/web");
        assert_eq!(v["detail"]["key"], "c1//deployments/app/web");
        assert_eq!(v["timeline"]["lines"].as_array().unwrap().len(), 1);
        assert_eq!(v["timeline"]["slotMinutes"], 10);
        assert_eq!(v["timeline"]["gaps"][0]["kind"], "heartbeat");

        let p = Params { day: Some("2026-10-20".into()), cluster: vec!["c1".into()], system: Some("1".into()), state: Some("clear".into()), ..Default::default() };
        let v = changemap(&s, chrono_tz::Asia::Taipei, &p, T).unwrap();
        assert_eq!(v["timeline"]["slotMinutes"], 1);
        assert_eq!(v["timeline"]["lines"][0]["cluster"], "c1");
        assert_eq!(v["timeline"]["lines"][0]["slots"].as_array().unwrap().len(), 1440);
        assert!(v["rows"].as_array().unwrap().iter().all(|r| r["state"] == "clear"));
        assert!(v["rows"].as_array().unwrap().iter().any(|r| r["who"]["category"] == "component"));
    }

    #[test]
    fn tabs_return_their_blocks() {
        let s = store();
        {
            let c = s.conn.lock().unwrap();
            for i in 0..30 {
                ev(&c, i + 1, "c1", T + i * 1000, &format!("user{i}"), 1, "direct", "patch", "configmaps", "app", &format!("cm{i}"));
            }
        }
        let p = Params { day: Some("2026-10-20".into()), tab: Some("relations".into()), ..Default::default() };
        let v = changemap(&s, chrono_tz::Asia::Taipei, &p, T).unwrap();
        assert!(v.get("rows").is_none());
        let nodes = v["relations"]["nodes"].as_array().unwrap();
        assert!(nodes.iter().any(|n| n["id"] == "group:human"), "more than 20 identities are grouped");
        assert!(v["relations"]["edges"].as_array().unwrap().iter().any(|e| e["bypass"] == true));
        let p = Params { day: Some("2026-10-20".into()), tab: Some("events".into()), ..Default::default() };
        let v = changemap(&s, chrono_tz::Asia::Taipei, &p, T).unwrap();
        assert!(v["events"].is_array());
        assert!(v.get("relations").is_none());
    }

    #[test]
    fn pages_and_query_parsing() {
        let s = store();
        {
            let c = s.conn.lock().unwrap();
            for i in 0..150 {
                ev(&c, i + 1, "c1", T + i * 1000, "alice", 1, "rancher", "patch", "configmaps", "app", &format!("cm{i}"));
            }
        }
        let p = Params { day: Some("2026-10-20".into()), ..Default::default() };
        let v = changemap(&s, chrono_tz::Asia::Taipei, &p, T).unwrap();
        assert_eq!(v["rows"].as_array().unwrap().len(), PAGE);
        let next = v["next"].as_str().unwrap().to_string();
        assert!(next.parse::<i64>().is_err(), "the cursor is opaque, not an offset");
        let p = Params { cursor: Some(next), ..p };
        let v = changemap(&s, chrono_tz::Asia::Taipei, &p, T).unwrap();
        assert_eq!(v["rows"].as_array().unwrap().len(), 50);
        assert!(v["next"].is_null());
        let p = Params { cursor: Some("zz".into()), ..p };
        assert!(changemap(&s, chrono_tz::Asia::Taipei, &p, T).is_err(), "a cursor the server did not give out is refused");

        let q = parse_query("day=2026-10-20&cluster=a&cluster=b&select=c%2F%2Fpods%2Fns%2Fx&tab=events");
        assert_eq!(q.cluster, vec!["a", "b"]);
        assert_eq!(q.select.as_deref(), Some("c//pods/ns/x"));
    }

    /// v0.1 point 2: records that arrive between two pages neither repeat rows nor push rows
    /// off the next page.
    #[test]
    fn pages_hold_still_while_records_arrive() {
        let s = store();
        {
            let c = s.conn.lock().unwrap();
            for i in 0..150 {
                ev(&c, i + 1, "c1", T + i * 1000, "alice", 1, "rancher", "patch", "configmaps", "app", &format!("cm{i}"));
            }
        }
        let p = Params { day: Some("2026-10-20".into()), ..Default::default() };
        let v1 = changemap(&s, chrono_tz::Asia::Taipei, &p, T).unwrap();
        let mut keys: Vec<String> = v1["rows"].as_array().unwrap().iter().map(|r| r["key"].as_str().unwrap().to_string()).collect();
        {
            // A new object that sorts first, and a new write to an object already on page 1.
            let c = s.conn.lock().unwrap();
            ev(&c, 1000, "c1", T + 500_000, "bob", 1, "rancher", "patch", "configmaps", "app", "brand-new");
            ev(&c, 1001, "c1", T + 600_000, "bob", 1, "rancher", "patch", "configmaps", "app", "cm149");
        }
        let p2 = Params { cursor: Some(v1["next"].as_str().unwrap().into()), ..p.clone() };
        let v2 = changemap(&s, chrono_tz::Asia::Taipei, &p2, T).unwrap();
        keys.extend(v2["rows"].as_array().unwrap().iter().map(|r| r["key"].as_str().unwrap().to_string()));
        let unique: HashSet<&String> = keys.iter().collect();
        assert_eq!(keys.len(), 150, "every object of the first view, once");
        assert_eq!(unique.len(), 150);
        assert!(!keys.iter().any(|k| k.ends_with("brand-new")), "records after the first page wait for a reload");
        // A reload sees them.
        let v3 = changemap(&s, chrono_tz::Asia::Taipei, &p, T).unwrap();
        assert_eq!(v3["counts"]["objects"], 151);
    }

    /// v0.1 point 1: a deletecollection or a create the server named is one batch row;
    /// requests that leave no object behind are only counted.
    #[test]
    fn batch_writes_are_listed_and_requests_are_counted() {
        let s = store();
        {
            let c = s.conn.lock().unwrap();
            ev(&c, 1, "c1", T, "alice", 1, "direct", "deletecollection", "pods", "app", "");
            ev(&c, 2, "c1", T + 1000, "alice", 1, "direct", "deletecollection", "pods", "app", "");
            ev(&c, 3, "c1", T + 2000, "alice", 1, "rancher", "create", "users", "", "");
            ev(&c, 4, "c1", T + 3000, "alice", 1, "direct", "create", "selfsubjectaccessreviews", "", "");
        }
        let p = Params { day: Some("2026-10-20".into()), ..Default::default() };
        let v = changemap(&s, chrono_tz::Asia::Taipei, &p, T).unwrap();
        let rows = v["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        let pods = rows.iter().find(|r| r["key"] == "c1//pods/app/*").unwrap();
        assert_eq!((pods["name"].as_str(), pods["batch"].as_bool(), pods["ops"].as_i64()), (Some("*"), Some(true), Some(2)));
        assert!(rows.iter().any(|r| r["key"] == "c1//users//*" && r["batch"] == true));
        assert_eq!(v["hidden"]["requestOnly"], 1);
        assert_eq!(v["counts"]["ops"], 3);
    }

    /// v0.1 points 3 and 4: the detail call answers for one object; settings and related
    /// objects are kept apart.
    #[test]
    fn detail_for_one_object() {
        let s = store();
        {
            let c = s.conn.lock().unwrap();
            ev(&c, 1, "c1", T, "alice", 1, "rancher", "patch", "configmaps", "app", "cfg");
            c.execute(
                "INSERT INTO events (id, cluster, audit_id, ts_ms, ts, actor, principal, human, via, caller, verb, api_group, resource, subresource,
                                     namespace, name, code, source_ip, user_agent, request_uri, source, hash)
                 VALUES (2, 'c1', '2', ?1, '', 'alice', '', 1, 'rancher', 'alice', 'create', '', 'pods', 'exec', 'app', 'web', 200, '', '', '', 'k8s', 'h')",
                params![T + 1000],
            )
            .unwrap();
            c.execute(
                "INSERT INTO relations (cluster, from_kind, from_ns, from_name, rel, to_kind, to_ns, to_name) VALUES ('c1', 'Pod', 'app', 'web', 'mounts', 'ConfigMap', 'app', 'cfg')",
                [],
            )
            .unwrap();
            ev(&c, 3, "c1", T + 2000, "alice", 1, "direct", "deletecollection", "pods", "app", "");
        }
        let p = Params { day: Some("2026-10-20".into()), key: Some("c1//pods/app/web".into()), ..Default::default() };
        let d = detail(&s, chrono_tz::Asia::Taipei, &p, T).unwrap();
        assert_eq!(d["key"], "c1//pods/app/web");
        assert_eq!(d["ops"].as_array().unwrap().len(), 1, "only this object's records");
        assert_eq!(d["dependsOn"][0]["setting"], "是否啟用 SUSE Security 收集");
        assert_eq!(d["related"][0], json!({"rel": "mounts", "kind": "ConfigMap", "namespace": "app", "name": "cfg"}));
        // The same detail as the page gives for that object.
        let page = changemap(&s, chrono_tz::Asia::Taipei, &Params { select: Some("c1//pods/app/web".into()), ..p.clone() }, T).unwrap();
        assert_eq!(page["detail"], d);
        // A configmap has no setting to name, and an empty list rather than related objects.
        let d = detail(&s, chrono_tz::Asia::Taipei, &Params { key: Some("c1//configmaps/app/cfg".into()), ..p.clone() }, T).unwrap();
        assert_eq!(d["dependsOn"], json!([]));
        // The batch row of the same namespace answers too.
        let d = detail(&s, chrono_tz::Asia::Taipei, &Params { key: Some("c1//pods/app/*".into()), ..p.clone() }, T).unwrap();
        assert_eq!((d["batch"].as_bool(), d["ops"].as_array().unwrap().len()), (Some(true), 1));
        // Nothing that day, and a malformed key.
        assert!(detail(&s, chrono_tz::Asia::Taipei, &Params { key: Some("c1//pods/app/none".into()), ..p.clone() }, T).unwrap().is_null());
        assert!(detail(&s, chrono_tz::Asia::Taipei, &Params { key: Some("c1/pods".into()), ..p }, T).is_err());
    }

    /// Entering a container is shown as such whatever the verb: current kubectl sends exec as a
    /// GET (websocket), older kubectl as a POST (create). Both name the SUSE Security setting.
    #[test]
    fn exec_by_get_or_create_is_entering_a_container() {
        let s = store();
        {
            let c = s.conn.lock().unwrap();
            for (id, verb, name) in [(1, "get", "web"), (2, "create", "api")] {
                c.execute(
                    "INSERT INTO events (id, cluster, audit_id, ts_ms, ts, actor, principal, human, via, caller, verb, api_group, resource, subresource,
                                         namespace, name, code, source_ip, user_agent, request_uri, source, hash)
                     VALUES (?1, 'c1', ?1, ?2, '', 'alice', '', 1, 'rancher', 'alice', ?3, '', 'pods', 'exec', 'app', ?4, 101, '', '', '', 'k8s', 'h')",
                    params![id, T + id * 1000, verb, name],
                )
                .unwrap();
            }
            // A plain read of a pod's logs stays a read and is not listed.
            c.execute(
                "INSERT INTO events (id, cluster, audit_id, ts_ms, ts, actor, principal, human, via, caller, verb, api_group, resource, subresource,
                                     namespace, name, code, source_ip, user_agent, request_uri, source, hash)
                 VALUES (3, 'c1', '3', ?1, '', 'alice', '', 1, 'rancher', 'alice', 'get', '', 'pods', 'log', 'app', 'logs-only', 200, '', '', '', 'k8s', 'h')",
                params![T + 3000],
            )
            .unwrap();
        }
        for name in ["web", "api"] {
            let p = Params { day: Some("2026-10-20".into()), key: Some(format!("c1//pods/app/{name}")), ..Default::default() };
            let d = detail(&s, chrono_tz::Asia::Taipei, &p, T).unwrap();
            assert_eq!(d["ops"][0]["verb"], "exec", "{name}");
            assert_eq!(d["dependsOn"][0]["setting"], "是否啟用 SUSE Security 收集", "{name}");
        }
        let v = changemap(&s, chrono_tz::Asia::Taipei, &Params { day: Some("2026-10-20".into()), ..Default::default() }, T).unwrap();
        assert!(!v["rows"].as_array().unwrap().iter().any(|r| r["name"] == "logs-only"));
    }

    /// v0.1 point 5: the note is passed on with the time of the newest record.
    #[test]
    fn data_note_is_passed_on() {
        let s = store();
        {
            let c = s.conn.lock().unwrap();
            ev(&c, 1, "c1", T, "alice", 1, "rancher", "patch", "configmaps", "app", "cfg");
        }
        let p = Params { day: Some("2026-10-20".into()), ..Default::default() };
        let v = build(&s, chrono_tz::Asia::Taipei, &p, T, None, Some("原型")).unwrap();
        assert_eq!(v["meta"], json!({"dataNote": "原型", "dataUntilMs": T}));
        let v = build(&s, chrono_tz::Asia::Taipei, &p, T, None, None).unwrap();
        assert!(v.get("meta").is_none());
    }

    /// A day whose midnight does not exist (Havana moved clocks from 00:00 to 01:00 on
    /// 2025-03-09) starts at the first minute that does, instead of stopping the server.
    #[test]
    fn day_without_a_midnight() {
        let tz = chrono_tz::America::Havana;
        let d = NaiveDate::from_ymd_opt(2025, 3, 9).unwrap();
        assert!(tz.from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap()).earliest().is_none(), "the test needs a missing midnight");
        let start = day_start(tz, d);
        assert_eq!(start, tz.from_local_datetime(&d.and_hms_opt(1, 0, 0).unwrap()).earliest().unwrap().timestamp_millis());
        let s = store();
        let p = Params { day: Some("2025-03-09".into()), ..Default::default() };
        let v = changemap(&s, tz, &p, T).unwrap();
        assert_eq!(v["timeline"]["lines"][0]["slots"].as_array().unwrap().len(), 138, "the 23-hour day has 23 hours of slots");
        let p = Params { day: Some("2025-03-08".into()), ..Default::default() };
        let v = changemap(&s, tz, &p, T).unwrap();
        assert_eq!(v["timeline"]["lines"][0]["slots"].as_array().unwrap().len(), 144, "the day before ends where that day starts");
    }

    #[test]
    fn categories() {
        assert_eq!(categorize("system:serviceaccount:cattle-system:cattle", false, "", None).0, "rancher");
        assert_eq!(categorize("system:serviceaccount:kube-system:cilium", false, "", None).0, "component");
        assert_eq!(categorize("system:serviceaccount:al-sample:ci-runner", false, "", None).0, "serviceaccount");
        assert_eq!(categorize("system:admin", true, "", None).0, "human");
        assert_eq!(categorize("system:cattle:error", false, "", None).0, "unknown");
        assert_eq!(categorize("rke2-cloud-controller-manager", true, "", None).0, "component");
        assert_eq!(categorize("u-d67", true, "", Some(r#"["system://provisioning/x","local://u-d67"]"#)).0, "rancher");
        assert_eq!(categorize("admin", true, "", Some(r#"["local://user-kgw7z"]"#)), ("human", "Rancher 本機帳號，可能多人共用"));
    }
}
