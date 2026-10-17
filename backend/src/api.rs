use crate::audit::{parse_body, to_record};
use crate::config::Config;
use crate::rules::evaluate;
use crate::store::{ObjKey, Query, Store};
use axum::body::Bytes;
use axum::extract::DefaultBodyLimit;
use axum::extract::{Query as Q, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub cfg: Arc<Config>,
}

fn err(e: anyhow::Error) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response()
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers.get("authorization")?.to_str().ok()?.strip_prefix("Bearer ")
}

/// Batches from collectors. The cluster is decided by the token, never by the caller.
/// Body: the source's records as JSON lines (or a JSON list), optionally gzip-encoded.
async fn ingest(State(s): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    if s.cfg.ingest_tokens.is_empty() {
        return (StatusCode::SERVICE_UNAVAILABLE, "no collector tokens configured").into_response();
    }
    let Some(cluster) = bearer(&headers).and_then(|t| s.cfg.cluster_for_token(t)) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let source = headers.get("x-audit-loupe-source").and_then(|v| v.to_str().ok()).unwrap_or("");
    let (records, received, skipped) = match source {
        "k8s-audit" => match parse_body(&body) {
            Ok(evs) => (evs.iter().filter_map(to_record).collect::<Vec<_>>(), evs.len(), 0),
            Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
        },
        "rancher-audit" if cluster == s.cfg.rancher_cluster => {
            let (entries, bad) = crate::rancher::parse_lines(&body);
            (entries.iter().filter_map(crate::rancher::to_record).collect(), entries.len() + bad, bad)
        }
        "rancher-audit" => {
            return (StatusCode::FORBIDDEN, "only the Rancher cluster's collector may send Rancher records").into_response();
        }
        "k8s-object" => return ingest_objects(s, cluster, &body).await,
        _ => return (StatusCode::BAD_REQUEST, format!("unsupported source {source:?}")).into_response(),
    };
    let store = s.store.clone();
    let cfg = s.cfg.clone();
    let work = move || -> anyhow::Result<usize> {
        let mut batch = Vec::with_capacity(records.len());
        for r in records {
            let mut a = evaluate(&r, &cfg);
            if !cfg.change_windows.is_empty() && r.human && crate::audit::is_write(&r.verb) && (200..300).contains(&r.code) {
                let cl = r.cluster.clone().unwrap_or_else(|| cluster.clone());
                let labels = if r.namespace.is_empty() { None } else { store.namespace_labels(&cl, &r.namespace)? };
                if crate::windows::allowed(&cfg.change_windows, &cl, labels.as_ref(), r.ts, &cfg.tz) == Some(false) {
                    a.push(crate::rules::Alert { rule: "outside_change_window", severity: "medium" });
                }
            }
            batch.push((r, a));
        }
        store.insert_events(&cluster, &batch)
    };
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(stored)) => Json(json!({"received": received, "skipped": skipped, "stored": stored})).into_response(),
        Ok(Err(e)) => err(e),
        Err(e) => err(e.into()),
    }
}

async fn ingest_objects(s: AppState, cluster: String, body: &[u8]) -> Response {
    let mut msgs = Vec::new();
    for line in body.split(|b| *b == b'\n') {
        let line = line.trim_ascii();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_slice::<crate::watch::ObjMsg>(line) {
            Ok(m) => msgs.push(m),
            // A malformed line would be resent forever; refuse the batch so it is visible.
            Err(e) => return (StatusCode::BAD_REQUEST, format!("bad object message: {e}")).into_response(),
        }
    }
    let received = msgs.len();
    let store = s.store.clone();
    match tokio::task::spawn_blocking(move || store.apply_objects(&cluster, &msgs)).await {
        Ok(Ok(stored)) => Json(json!({"received": received, "stored": stored})).into_response(),
        Ok(Err(e)) => err(e),
        Err(e) => err(e.into()),
    }
}

#[derive(Deserialize)]
struct Range {
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    /// Shortcut instead of from: look back this many hours.
    hours: Option<i64>,
}

impl Range {
    fn bounds(&self) -> (i64, i64) {
        let to = self.to.unwrap_or_else(Utc::now);
        let hours = self.hours.unwrap_or(24).clamp(1, 24 * 400);
        let from = self.from.unwrap_or_else(|| to - Duration::hours(hours));
        (from.timestamp_millis(), to.timestamp_millis())
    }
}

#[derive(Deserialize)]
struct TimelineParams {
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    hours: Option<i64>,
    actor: Option<String>,
    cluster: Option<String>,
    /// all=1 also shows system components and read requests.
    all: Option<u8>,
    limit: Option<i64>,
}

async fn timeline(State(s): State<AppState>, Q(p): Q<TimelineParams>) -> Response {
    let (from_ms, to_ms) = Range { from: p.from, to: p.to, hours: p.hours }.bounds();
    let all = p.all == Some(1);
    let q = Query {
        cluster: p.cluster.filter(|c| !c.is_empty()),
        actor: p.actor.filter(|a| !a.is_empty()),
        from_ms,
        to_ms,
        humans_only: !all,
        writes_only: !all,
        alerts_only: false,
        limit: p.limit.unwrap_or(200).clamp(1, 2000),
    };
    match s.store.timeline(&q) {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

async fn actors(State(s): State<AppState>, Q(r): Q<Range>) -> Response {
    let (f, t) = r.bounds();
    match s.store.actors(f, t) {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

/// The user directory: every lifetime of every Rancher user seen, deleted ones included.
async fn users(State(s): State<AppState>) -> Response {
    match s.store.users() {
        Ok(rows) => Json(rows.into_iter().map(|(id, lives)| json!({"userId": id, "lifetimes": lives})).collect::<Vec<_>>()).into_response(),
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct ObjectsParams {
    cluster: Option<String>,
    q: Option<String>,
    limit: Option<i64>,
}

async fn objects(State(s): State<AppState>, Q(p): Q<ObjectsParams>) -> Response {
    match s.store.objects(p.cluster.as_deref().filter(|c| !c.is_empty()), p.q.as_deref(), p.limit.unwrap_or(200).clamp(1, 2000)) {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct HistoryParams {
    cluster: Option<String>,
    #[serde(default)]
    group: String,
    resource: String,
    #[serde(default)]
    namespace: String,
    name: String,
}

async fn history(State(s): State<AppState>, Q(p): Q<HistoryParams>) -> Response {
    let key = ObjKey { cluster: p.cluster.filter(|c| !c.is_empty()).unwrap_or_else(|| s.cfg.cluster.clone()), api_group: p.group, resource: p.resource, namespace: p.namespace, name: p.name };
    match s.store.history(&key) {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct ChainParams {
    cluster: Option<String>,
}

/// Verify every cluster's chain of records.
async fn integrity(State(s): State<AppState>, Q(p): Q<ChainParams>) -> Response {
    let store = s.store.clone();
    match tokio::task::spawn_blocking(move || store.verify_chain(p.cluster.as_deref().filter(|c| !c.is_empty()))).await {
        Ok(Ok(rows)) => Json(rows).into_response(),
        Ok(Err(e)) => err(e),
        Err(e) => err(e.into()),
    }
}

/// Changes people made to objects Fleet deploys, and whether Fleet put them back.
async fn gitops(State(s): State<AppState>, Q(r): Q<Range>) -> Response {
    let (f, t) = r.bounds();
    match s.store.gitops_bypasses(f, t) {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

/// Forwarding targets: last success, last error, records still to send.
async fn forwarding(State(s): State<AppState>) -> Response {
    match s.store.forward_status() {
        Ok(rows) => Json(rows.into_iter().map(|(t, ok, e, pending)| json!({"target": t, "lastOkMs": ok, "lastError": e, "pending": pending})).collect::<Vec<_>>()).into_response(),
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct AnchorParams {
    cluster: String,
    event_id: i64,
    hash: String,
}

/// Check an anchor kept outside (e.g. in the SIEM) against what is stored now.
async fn integrity_anchor(State(s): State<AppState>, Q(p): Q<AnchorParams>) -> Response {
    let store = s.store.clone();
    match tokio::task::spawn_blocking(move || store.check_anchor(&p.cluster, p.event_id, &p.hash)).await {
        Ok(Ok(result)) => Json(json!({"result": result})).into_response(),
        Ok(Err(e)) => err(e),
        Err(e) => err(e.into()),
    }
}

#[derive(Deserialize)]
struct AtParams {
    at: Option<DateTime<Utc>>,
    user: Option<String>,
}

fn access_snapshot(s: &AppState, at_ms: i64) -> anyhow::Result<crate::access::Snapshot> {
    let cl = &s.cfg.rancher_cluster;
    let objs = s.store.access_objects(cl, at_ms)?;
    let users = s.store.access_users(cl, at_ms)?;
    Ok(crate::access::snapshot(at_ms, &objs, users))
}

/// Who held which Rancher permission at a time (default now).
async fn access(State(s): State<AppState>, Q(p): Q<AtParams>) -> Response {
    let at = p.at.unwrap_or_else(Utc::now).timestamp_millis();
    match access_snapshot(&s, at) {
        Ok(mut snap) => {
            if let Some(u) = p.user.filter(|u| !u.is_empty()) {
                snap.users.retain(|x| x.user_id == u);
            }
            Json(snap).into_response()
        }
        Err(e) => err(e),
    }
}

async fn access_csv(State(s): State<AppState>, Q(p): Q<AtParams>) -> Response {
    let at = p.at.unwrap_or_else(Utc::now);
    match access_snapshot(&s, at.timestamp_millis()) {
        Ok(snap) => (
            [
                (axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
                (axum::http::header::CONTENT_DISPOSITION, format!("attachment; filename=\"access-{}.csv\"", at.format("%Y%m%d-%H%M"))),
            ],
            // A byte-order mark so spreadsheet programs read the UTF-8 names correctly.
            format!("\u{feff}{}", crate::access::to_csv(&snap)),
        )
            .into_response(),
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct DiffParams {
    from: DateTime<Utc>,
    to: Option<DateTime<Utc>>,
}

/// Permissions added and removed between two times, who did it, and role definition changes.
async fn access_diff(State(s): State<AppState>, Q(p): Q<DiffParams>) -> Response {
    let (from, to) = (p.from.timestamp_millis(), p.to.unwrap_or_else(Utc::now).timestamp_millis());
    let run = || -> anyhow::Result<serde_json::Value> {
        let cl = &s.cfg.rancher_cluster;
        let before = crate::access::grants(&s.store.access_objects(cl, from)?);
        let after = crate::access::grants(&s.store.access_objects(cl, to)?);
        let (added, removed) = crate::access::diff(&before, &after);
        let mut changes = Vec::new();
        for g in added {
            let (at, by) = (Some(g.granted_ms), g.granted_by.clone());
            changes.push(crate::access::Change { change: "added", grant: g, at_ms: at, by, reason: None });
        }
        for g in removed {
            let resource = match g.binding_kind.as_str() {
                "GlobalRoleBinding" => "globalrolebindings",
                "ClusterRoleTemplateBinding" => "clusterroletemplatebindings",
                _ => "projectroletemplatebindings",
            };
            let (at, by, reason) = s.store.access_removal(cl, resource, &g.binding_namespace, &g.binding_name, &g.subject, from, to)?;
            changes.push(crate::access::Change { change: "removed", grant: g, at_ms: at, by, reason });
        }
        Ok(json!({"from": from, "to": to, "changes": changes, "roleChanges": s.store.role_changes(cl, from, to)?}))
    };
    match run() {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct ClusterRange {
    cluster: Option<String>,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    hours: Option<i64>,
}

async fn hourly(State(s): State<AppState>, Q(p): Q<ClusterRange>) -> Response {
    let (f, t) = Range { from: p.from, to: p.to, hours: p.hours }.bounds();
    match s.store.hourly(p.cluster.as_deref().filter(|c| !c.is_empty()), f, t) {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

async fn unattributed(State(s): State<AppState>, Q(r): Q<Range>) -> Response {
    let (f, t) = r.bounds();
    match s.store.unattributed(f, t) {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

async fn clusters(State(s): State<AppState>, Q(r): Q<Range>) -> Response {
    let (f, t) = r.bounds();
    match s.store.clusters(f, t) {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

/// Which Rancher project each namespace belongs to, per cluster.
async fn projects(State(s): State<AppState>, Q(p): Q<ClusterRange>) -> Response {
    match s.store.namespace_projects(p.cluster.as_deref().filter(|c| !c.is_empty())) {
        Ok(rows) => Json(rows.into_iter().map(|(c, n, p)| json!({"cluster": c, "namespace": n, "projectId": p})).collect::<Vec<_>>()).into_response(),
        Err(e) => err(e),
    }
}

async fn stories(State(s): State<AppState>, Q(p): Q<ClusterRange>) -> Response {
    let (f, t) = Range { from: p.from, to: p.to, hours: p.hours }.bounds();
    match s.store.stories(p.cluster.as_deref().filter(|c| !c.is_empty()), f, t) {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct RelatedParams {
    cluster: String,
    kind: String,
    #[serde(default)]
    namespace: String,
    name: String,
}

async fn related(State(s): State<AppState>, Q(p): Q<RelatedParams>) -> Response {
    match s.store.related(&p.cluster, &p.kind, &p.namespace, &p.name) {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct AlertParams {
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    hours: Option<i64>,
    limit: Option<i64>,
}

async fn alerts(State(s): State<AppState>, Q(p): Q<AlertParams>) -> Response {
    let (f, t) = Range { from: p.from, to: p.to, hours: p.hours }.bounds();
    match s.store.alerts(f, t, p.limit.unwrap_or(200).clamp(1, 2000)) {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

async fn info(State(s): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "name": "audit-loupe",
        "version": env!("CARGO_PKG_VERSION"),
        "cluster": s.cfg.cluster,
        "timezone": s.cfg.tz.name(),
        "workHours": format!("{}-{}", s.cfg.work_start_hour, s.cfg.work_end_hour),
        "retentionDays": s.cfg.retention_days,
        "changeWindows": s.cfg.change_windows.iter().map(|w| w.name.as_str()).collect::<Vec<_>>(),
    }))
}

/// Read API for the UI (reached through the Rancher service proxy).
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/info", get(info))
        .route("/api/actors", get(actors))
        .route("/api/users", get(users))
        .route("/api/timeline", get(timeline))
        .route("/api/objects", get(objects))
        .route("/api/history", get(history))
        .route("/api/related", get(related))
        .route("/api/hourly", get(hourly))
        .route("/api/integrity", get(integrity))
        .route("/api/gitops", get(gitops))
        .route("/api/forwarding", get(forwarding))
        .route("/api/integrity/anchor", get(integrity_anchor))
        .route("/api/access", get(access))
        .route("/api/access/diff", get(access_diff))
        .route("/api/access/csv", get(access_csv))
        .route("/api/unattributed", get(unattributed))
        .route("/api/clusters", get(clusters))
        .route("/api/projects", get(projects))
        .route("/api/stories", get(stories))
        .route("/api/alerts", get(alerts))
        .with_state(state)
}

/// Collector entry point, on its own port so it can be exposed (Ingress/LoadBalancer)
/// without exposing the read API.
pub fn ingest_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/ingest/v1/batch", post(ingest))
        // Limit applies to the decompressed body.
        .layer(DefaultBodyLimit::max(64 << 20))
        .layer(tower_http::decompression::RequestDecompressionLayer::new())
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn state() -> AppState {
        AppState { store: Store::open(":memory:").unwrap(), cfg: Arc::new(Config::test()) }
    }
    const LINE: &str = r#"{"auditID":"a1","stage":"ResponseComplete","verb":"delete","user":{"username":"system:admin"},"objectRef":{"resource":"namespaces","name":"legacy-batch"},"responseStatus":{"code":200},"stageTimestamp":"2026-10-15T13:40:00Z"}"#;

    async fn post(app: Router, token: Option<&str>, source: &str, body: Vec<u8>, gzip: bool) -> (StatusCode, String) {
        let mut req = Request::post("/ingest/v1/batch").header("x-audit-loupe-source", source);
        if let Some(t) = token { req = req.header("authorization", format!("Bearer {t}")); }
        if gzip { req = req.header("content-encoding", "gzip"); }
        let res = app.oneshot(req.body(Body::from(body)).unwrap()).await.unwrap();
        let code = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        (code, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn token_decides_the_cluster() {
        let st = state();
        let (code, body) = post(ingest_router(st.clone()), Some("test-token"), "k8s-audit", LINE.into(), false).await;
        assert_eq!(code, StatusCode::OK, "{body}");
        let rows = st.store.timeline(&Query { cluster: None, actor: None, from_ms: 0, to_ms: i64::MAX, humans_only: false, writes_only: false, alerts_only: false, limit: 10 }).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cluster, "test");
        // Same auditID again is not stored twice.
        let (_, body) = post(ingest_router(st.clone()), Some("test-token"), "k8s-audit", LINE.into(), false).await;
        assert!(body.contains("\"stored\":0"), "{body}");
    }

    #[tokio::test]
    async fn rejects_bad_token_and_unknown_source() {
        let (code, _) = post(ingest_router(state()), Some("wrong-token-0000"), "k8s-audit", LINE.into(), false).await;
        assert_eq!(code, StatusCode::UNAUTHORIZED);
        let (code, _) = post(ingest_router(state()), None, "k8s-audit", LINE.into(), false).await;
        assert_eq!(code, StatusCode::UNAUTHORIZED);
        let (code, _) = post(ingest_router(state()), Some("test-token"), "syslog", LINE.into(), false).await;
        assert_eq!(code, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn rancher_records_only_from_the_rancher_cluster() {
        let st = state();
        let line = r#"{"auditID":"r1","requestURI":"/k8s/clusters/c-m-1/api/v1/namespaces/legacy","method":"DELETE",
          "user":{"name":"u-1","extra":{"username":["alice"]}},"responseCode":200,"responseTimestamp":"2026-10-15T13:40:00Z"}"#;
        let line = line.replace('\n', " ");
        let (code, _) = post(ingest_router(st.clone()), Some("test-token"), "rancher-audit", line.clone().into(), false).await;
        assert_eq!(code, StatusCode::FORBIDDEN);
        let (code, body) = post(ingest_router(st.clone()), Some("rancher-token"), "rancher-audit", line.into(), false).await;
        assert_eq!(code, StatusCode::OK, "{body}");
        let rows = st.store.timeline(&Query { cluster: None, actor: None, from_ms: 0, to_ms: i64::MAX, humans_only: false, writes_only: false, alerts_only: false, limit: 10 }).unwrap();
        assert_eq!((rows[0].cluster.as_str(), rows[0].source.as_str()), ("c-m-1", "rancher"), "the target cluster comes from the URI");
    }

    #[tokio::test]
    async fn changes_outside_a_window_are_flagged() {
        let mut cfg = Config::test();
        cfg.change_windows = crate::windows::parse(r#"[{"name":"w","days":"6","hours":"0-24"}]"#).unwrap();
        let st = AppState { store: Store::open(":memory:").unwrap(), cfg: Arc::new(cfg) };
        // A Thursday: the only window is Saturdays.
        let (code, _) = post(ingest_router(st.clone()), Some("test-token"), "k8s-audit", LINE.into(), false).await;
        assert_eq!(code, StatusCode::OK);
        let rows = st.store.timeline(&Query { cluster: None, actor: None, from_ms: 0, to_ms: i64::MAX, humans_only: false, writes_only: false, alerts_only: false, limit: 10 }).unwrap();
        assert!(rows[0].alerts.contains(&"outside_change_window".to_string()), "{:?}", rows[0].alerts);
        assert!(!rows[0].alerts.contains(&"off_hours".to_string()), "windows replace the working-hours rule");
    }

    #[tokio::test]
    async fn accepts_gzip() {
        use std::io::Write;
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(LINE.as_bytes()).unwrap();
        let (code, body) = post(ingest_router(state()), Some("test-token"), "k8s-audit", enc.finish().unwrap(), true).await;
        assert_eq!(code, StatusCode::OK, "{body}");
        assert!(body.contains("\"stored\":1"), "{body}");
    }
}
