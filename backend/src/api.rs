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
    pub watch: crate::watch::Status,
}

fn err(e: anyhow::Error) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response()
}

fn token_ok(headers: &HeaderMap, expected: &str) -> bool {
    use subtle::ConstantTimeEq;
    let got = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("");
    let want = format!("Bearer {expected}");
    got.len() == want.len() && bool::from(got.as_bytes().ct_eq(want.as_bytes()))
}

/// The cluster label always comes from configuration: client-go drops query strings
/// from the webhook URL, and a caller-supplied label could be spoofed.
async fn ingest(State(s): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    if let Some(tok) = &s.cfg.ingest_token {
        if !token_ok(&headers, tok) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
    }
    let events = match parse_body(&body) {
        Ok(e) => e,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    let batch: Vec<_> = events.iter().filter_map(to_record).map(|r| {
        let a = evaluate(&r, &s.cfg);
        (r, a)
    }).collect();
    let received = events.len();
    let (store, cluster) = (s.store.clone(), s.cfg.cluster.clone());
    match tokio::task::spawn_blocking(move || store.insert_events(&cluster, &batch)).await {
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
        "watching": s.cfg.watch_enabled,
        "watchResources": s.cfg.watch_resources,
        "watchStatus": s.watch.lock().unwrap().clone(),
        "timezone": s.cfg.tz.name(),
        "workHours": format!("{}-{}", s.cfg.work_start_hour, s.cfg.work_end_hour),
        "retentionDays": s.cfg.retention_days,
    }))
}

/// Read API for the UI (reached through the Rancher service proxy).
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/info", get(info))
        .route("/api/actors", get(actors))
        .route("/api/timeline", get(timeline))
        .route("/api/objects", get(objects))
        .route("/api/history", get(history))
        .route("/api/alerts", get(alerts))
        .with_state(state)
}

/// Audit webhook receiver, on its own port so a NetworkPolicy can limit it to control-plane nodes.
pub fn ingest_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/ingest/k8s-audit", post(ingest))
        // A batch of up to a few hundred Request-level events can exceed axum's 2 MB default.
        .layer(DefaultBodyLimit::max(64 << 20))
        .with_state(state)
}
