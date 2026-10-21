//! Performance gate for the change map (requirements section 3, section 4 point 11): one day of
//! 720,000 records, shaped like the lab's busiest day (report-ui-facts section B) times 11,
//! stored through the real ingest path, then the change map is asked for the views the page
//! opens with. Server-side time only (no HTTP, no browser).
//!
//! Run: `cargo test --release --locked perf_gate -- --ignored --nocapture`.
//! - `AL_PERF_SUMMARY=<file>` appends the result table (Markdown) to that file (CI: the job
//!   summary).
//! - `AL_PERF_ENFORCE=1` fails when a target is missed. Without it the gate only records.
//! - `AL_PERF_SCALE=<n>` changes the multiple of the lab day (default 11, about 720,000).

use crate::audit::{Record, Source, Via};
use crate::changemap::{changemap, changemap_by_scan, detail, Params};
use crate::config::Config;
use crate::store::Store;
use chrono::{TimeZone, Utc};
use std::sync::Arc;
use std::time::Instant;

/// One identity's share of the lab day: how many records, how many of them writes, and on what.
struct Who {
    actor: &'static str,
    /// Rancher user ID when the person came through Rancher (impersonated).
    user_id: &'static str,
    via: Via,
    ua: &'static str,
    per_day: usize,
    writes: usize,
    group: &'static str,
    resource: &'static str,
    namespace: &'static str,
    objects: usize,
    /// Every n-th write is a pod exec (people only), which trips the pod_exec rule.
    exec_every: usize,
}

/// 10/20 in the lab (report-ui-facts section B), rounded; people added so rules have work.
const DAY: &[Who] = &[
    Who { actor: "system:serviceaccount:cattle-fleet-system:fleet-agent", user_id: "", via: Via::Direct, ua: "fleetagent/v0.0.0", per_day: 27_744, writes: 0, group: "fleet.cattle.io", resource: "bundledeployments", namespace: "cattle-fleet-system", objects: 20, exec_every: 0 },
    Who { actor: "system:serviceaccount:local-path-storage:local-path-provisioner", user_id: "", via: Via::Direct, ua: "local-path-provisioner/v0.0.0", per_day: 17_280, writes: 0, group: "", resource: "configmaps", namespace: "local-path-storage", objects: 1, exec_every: 0 },
    Who { actor: "system:serviceaccount:kube-system:rke2-coredns-rke2-coredns-autoscaler", user_id: "", via: Via::Direct, ua: "cluster-proportional-autoscaler/v0.0.0", per_day: 8_640, writes: 0, group: "", resource: "configmaps", namespace: "kube-system", objects: 1, exec_every: 0 },
    Who { actor: "system:serviceaccount:al-sample:app-config", user_id: "", via: Via::Direct, ua: "app-config-sample/1.0", per_day: 5_742, writes: 0, group: "", resource: "configmaps", namespace: "al-sample", objects: 5, exec_every: 0 },
    Who { actor: "system:serviceaccount:cattle-system:cattle", user_id: "", via: Via::Direct, ua: "agent/v0.0.0", per_day: 1_355, writes: 719, group: "", resource: "secrets", namespace: "cattle-system", objects: 40, exec_every: 0 },
    Who { actor: "u-d67owytc34", user_id: "u-d67owytc34", via: Via::Rancher, ua: "cluster-api-controller-manager", per_day: 1_321, writes: 1_321, group: "", resource: "nodes", namespace: "", objects: 3, exec_every: 0 },
    Who { actor: "system:rke2-supervisor", user_id: "", via: Via::Direct, ua: "rke2-supervisor@perf", per_day: 683, writes: 463, group: "helm.cattle.io", resource: "helmcharts", namespace: "kube-system", objects: 30, exec_every: 0 },
    Who { actor: "system:serviceaccount:kube-system:cilium", user_id: "", via: Via::Direct, ua: "cilium-agent/1.19.6", per_day: 589, writes: 589, group: "cilium.io", resource: "ciliumendpoints", namespace: "al-sample", objects: 100, exec_every: 0 },
    Who { actor: "system:serviceaccount:longhorn-system:longhorn-service-account", user_id: "", via: Via::Direct, ua: "longhorn-manager/v0.0.0", per_day: 589, writes: 581, group: "longhorn.io", resource: "volumes", namespace: "longhorn-system", objects: 50, exec_every: 0 },
    Who { actor: "system:serviceaccount:al-sample:ci-runner", user_id: "", via: Via::Direct, ua: "ci-runner-sample/1.0", per_day: 552, writes: 552, group: "apps", resource: "deployments", namespace: "al-sample", objects: 40, exec_every: 0 },
    Who { actor: "system:kube-scheduler", user_id: "", via: Via::Direct, ua: "kube-scheduler/v1.36.4+rke2r1", per_day: 278, writes: 278, group: "", resource: "pods", namespace: "al-sample", objects: 278, exec_every: 0 },
    Who { actor: "system:kube-controller-manager", user_id: "", via: Via::Direct, ua: "kube-controller-manager/v1.36.4", per_day: 65, writes: 65, group: "apps", resource: "replicasets", namespace: "al-sample", objects: 30, exec_every: 0 },
    Who { actor: "system:admin", user_id: "", via: Via::Direct, ua: "kubectl1.36.0 (darwin/arm64)", per_day: 5, writes: 3, group: "", resource: "configmaps", namespace: "al-sample", objects: 3, exec_every: 0 },
    Who { actor: "alice", user_id: "u-perf1", via: Via::Rancher, ua: "Mozilla/5.0", per_day: 60, writes: 40, group: "apps", resource: "deployments", namespace: "shop", objects: 10, exec_every: 10 },
    Who { actor: "bob", user_id: "u-perf2", via: Via::Rancher, ua: "Mozilla/5.0", per_day: 40, writes: 25, group: "", resource: "configmaps", namespace: "shop", objects: 10, exec_every: 0 },
];

const CLUSTER: &str = "c-m-perf";
/// 2026-10-20 00:00 in Taipei.
fn day_start() -> i64 {
    chrono_tz::Asia::Taipei.with_ymd_and_hms(2026, 10, 20, 0, 0, 0).unwrap().timestamp_millis()
}

/// A small deterministic generator, so every run stores the same day.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

/// The day's records, `scale` copies of the lab day; each copy has its own objects
/// (`name-c<copy>`), so there are about `scale` times as many objects too.
pub fn records(scale: usize) -> Vec<Record> {
    let start = day_start();
    let mut rng = Lcg(42);
    let mut out = Vec::new();
    for copy in 0..scale {
        for w in DAY {
            for i in 0..w.per_day {
                let write = i < w.writes;
                let exec = write && w.exec_every > 0 && i % w.exec_every == 0;
                let ts_ms = start + (rng.next() % 86_400_000) as i64;
                let obj = (rng.next() as usize) % w.objects.max(1);
                let (resource, sub, verb, ns) = if exec {
                    ("pods", "exec", "create", w.namespace)
                } else {
                    (w.resource, "", if write { "patch" } else { "get" }, w.namespace)
                };
                out.push(Record {
                    audit_id: format!("perf-{copy}-{}-{i}", w.actor),
                    source: Source::K8s,
                    cluster: None,
                    ts: Utc.timestamp_millis_opt(ts_ms).unwrap(),
                    actor: w.actor.into(),
                    principal: String::new(),
                    user_id: w.user_id.into(),
                    human: crate::audit::is_human(w.actor),
                    via: w.via,
                    caller: if w.via == Via::Rancher { format!("system:serviceaccount:cattle-impersonation-system:cattle-impersonation-{}", w.user_id) } else { w.actor.into() },
                    verb: verb.into(),
                    api_group: if exec { String::new() } else { w.group.into() },
                    resource: resource.into(),
                    subresource: sub.into(),
                    namespace: ns.into(),
                    name: format!("{}-{obj}-c{copy}", w.resource),
                    code: if exec { 101 } else { 200 },
                    source_ip: String::new(),
                    user_agent: w.ua.into(),
                    request_uri: String::new(),
                    request_object: None,
                });
            }
        }
    }
    out.sort_by_key(|r| r.ts);
    out
}

struct Row {
    view: &'static str,
    target_ms: Option<f64>,
    ms: Vec<f64>,
    bytes: usize,
    /// The same view read the old way (every record of the day), p50, when measured.
    scan_ms: Option<f64>,
    /// Whether the response equals the old way's.
    same: Option<bool>,
}

/// The first place two responses differ, for the report.
fn first_diff(a: &serde_json::Value, b: &serde_json::Value, path: &str) -> Option<String> {
    use serde_json::Value as V;
    match (a, b) {
        (V::Object(x), V::Object(y)) => {
            let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
            keys.sort();
            keys.dedup();
            keys.into_iter().find_map(|k| match (x.get(k), y.get(k)) {
                (Some(p), Some(q)) => first_diff(p, q, &format!("{path}.{k}")),
                _ => Some(format!("{path}.{k}")),
            })
        }
        (V::Array(x), V::Array(y)) if x.len() == y.len() => x.iter().zip(y).enumerate().find_map(|(i, (p, q))| first_diff(p, q, &format!("{path}[{i}]"))),
        _ if a == b => None,
        _ => Some(path.to_string()),
    }
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[i.min(sorted.len() - 1)]
}

fn time(runs: usize, mut f: impl FnMut() -> serde_json::Value) -> (Vec<f64>, usize) {
    let mut bytes = 0;
    let _ = f(); // first call warms the caches; not counted
    let ms = (0..runs)
        .map(|_| {
            let t = Instant::now();
            let v = f();
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            bytes = serde_json::to_vec(&v).map(|b| b.len()).unwrap_or(0);
            ms
        })
        .collect();
    (ms, bytes)
}

#[test]
#[ignore]
fn perf_gate() {
    let scale: usize = std::env::var("AL_PERF_SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(11);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("perf.db");
    let cfg = Arc::new(Config::test());
    let store = Store::open_with(path.to_str().unwrap(), cfg.clone()).unwrap();

    let recs = records(scale);
    let t = Instant::now();
    for chunk in recs.chunks(1000) {
        store.insert_events(CLUSTER, chunk).unwrap();
    }
    let ingest_s = t.elapsed().as_secs_f64();
    let n = recs.len();
    let now = day_start() + 86_400_000 + 3_600_000;
    let tz = cfg.tz;
    let day = Some("2026-10-20".to_string());
    let base = Params { day: day.clone(), ..Default::default() };
    let runs = 20;

    // The chain check runs in the background every 10 minutes; time it on its own and keep it
    // out of the page timings below.
    let t = Instant::now();
    store.verify_chain(None).unwrap();
    let chain_s = t.elapsed().as_secs_f64();
    let first = changemap(&store, tz, &base, now).unwrap();
    while crate::changemap::chain_check_running() {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let busiest = first["rows"]
        .as_array()
        .and_then(|rows| rows.iter().max_by_key(|r| r["ops"].as_i64().unwrap_or(0)))
        .and_then(|r| r["key"].as_str())
        .unwrap_or_default()
        .to_string();

    let mut rows = Vec::new();
    let mut diffs = Vec::new();
    let mut add = |view, target_ms, p: Params, is_detail: bool| {
        let (ms, bytes) = time(runs, || if is_detail { detail(&store, tz, &p, now).unwrap() } else { changemap(&store, tz, &p, now).unwrap() });
        let (scan_ms, same) = if is_detail {
            (None, None)
        } else {
            let (mut sm, _) = time(3, || changemap_by_scan(&store, tz, &p, now).unwrap());
            sm.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let (new, old) = (changemap(&store, tz, &p, now).unwrap(), changemap_by_scan(&store, tz, &p, now).unwrap());
            if let Some(d) = first_diff(&new, &old, "") {
                diffs.push(format!("{view}: {d}"));
            }
            (Some(pct(&sm, 0.5)), Some(new == old))
        };
        rows.push(Row { view, target_ms, ms, bytes, scan_ms, same });
    };
    add("總覽首頁（全部叢集）", Some(300.0), base.clone(), false);
    add("總覽首頁（含系統元件）", Some(300.0), Params { system: Some("1".into()), ..base.clone() }, false);
    add("總覽首頁（單一叢集，每分鐘時間列）", Some(300.0), Params { cluster: vec![CLUSTER.into()], ..base.clone() }, false);
    add("關係分頁", Some(300.0), Params { tab: Some("relations".into()), ..base.clone() }, false);
    add("事件分頁", Some(300.0), Params { tab: Some("events".into()), ..base.clone() }, false);
    add("單一物件細節", Some(100.0), Params { key: Some(busiest.clone()), ..base.clone() }, true);
    let next = first["next"].as_str().map(str::to_string);
    add("總覽第二頁", Some(300.0), Params { cursor: next, ..base.clone() }, false);
    add("總覽只看「要看」", Some(300.0), Params { state: Some("look".into()), ..base.clone() }, false);
    add("總覽單一 Namespace", Some(300.0), Params { namespace: Some("al-sample".into()), ..base.clone() }, false);

    let mut md = format!(
        "### 變更地圖效能閘門\n\n一天 {n} 筆（lab 10/20 的 {scale} 倍），寫入 {ingest_s:.1} 秒（每秒 {:.0} 筆）；雜湊鏈完整檢查 {chain_s:.1} 秒（背景，每 10 分鐘一次，不計入下表）。每項 {runs} 次，不含第一次。伺服器端時間，不含網路與瀏覽器。\n\n| 畫面 | p50 (ms) | p95 (ms) | 最慢 (ms) | 回應大小（未壓縮） | 目標 p95 | 結果 | 舊做法 p50 (ms) | 與舊做法相同 |\n|---|---|---|---|---|---|---|---|---|\n",
        n as f64 / ingest_s
    );
    let mut missed = Vec::new();
    for r in &mut rows {
        r.ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let (p50, p95, max) = (pct(&r.ms, 0.5), pct(&r.ms, 0.95), r.ms[r.ms.len() - 1]);
        let verdict = match r.target_ms {
            Some(t) if p95 <= t => "達標".to_string(),
            Some(t) => {
                missed.push(format!("{} p95 {p95:.0} ms > {t:.0} ms", r.view));
                "未達標".to_string()
            }
            None => "—".to_string(),
        };
        md += &format!(
            "| {} | {p50:.0} | {p95:.0} | {max:.0} | {} KB | {} | {verdict} | {} | {} |\n",
            r.view,
            r.bytes / 1024,
            r.target_ms.map(|t| format!("≤ {t:.0} ms")).unwrap_or_else(|| "—".into()),
            r.scan_ms.map(|m| format!("{m:.0}")).unwrap_or_else(|| "—".into()),
            match r.same { Some(true) => "相同", Some(false) => "**不同**", None => "—" },
        );
    }
    println!("{md}");
    if let Ok(p) = std::env::var("AL_PERF_SUMMARY") {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = writeln!(f, "{md}");
        }
    }
    if !diffs.is_empty() {
        md += &format!("\n不同的地方：{diffs:?}\n");
        println!("differences: {diffs:?}");
    }
    assert!(!busiest.is_empty(), "the first page has rows");
    assert!(diffs.is_empty(), "the index must give the same page as reading the whole day: {diffs:?}");
    if std::env::var("AL_PERF_ENFORCE").as_deref() == Ok("1") {
        assert!(missed.is_empty(), "targets missed: {missed:?}");
    }
}

/// The generator itself: the right number of records, the shape of the lab day, the same
/// every run.
#[test]
fn the_day_has_the_lab_shape() {
    let one: usize = DAY.iter().map(|w| w.per_day).sum();
    let recs = records(2);
    assert_eq!(recs.len(), one * 2);
    assert!(recs.windows(2).all(|w| w[0].ts <= w[1].ts), "in time order");
    let again = records(2);
    assert_eq!(recs.iter().map(|r| &r.audit_id).collect::<Vec<_>>(), again.iter().map(|r| &r.audit_id).collect::<Vec<_>>());
    let execs = recs.iter().filter(|r| r.subresource == "exec").count();
    assert_eq!(execs, 2 * 4, "alice opens a shell 4 times a day");
    // Eleven copies of the lab day is the 5-second baseline (about 720,000 records).
    assert!((700_000..=740_000).contains(&(one * 11)), "{}", one * 11);
}
