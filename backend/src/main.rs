mod access;
mod api;
mod audit;
mod changemap;
mod collect;
mod config;
mod demo;
mod diff;
mod forward;
mod identity;
mod integrity;
mod neuvector;
#[cfg(test)]
mod perf;
mod rancher;
mod relations;
mod replay;
mod restore;
mod rules;
mod rulestate;
mod shown;
mod store;
mod watch;
mod windows;

use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let cmd = std::env::args().nth(1).unwrap_or_else(|| "server".into());
    match cmd.as_str() {
        "server" | "seed-demo" => {}
        "collect-file" => return collect::run_file().await,
        "collect-rancher" => return collect::run_rancher().await,
        "collect-watch" => return watch::run().await,
        "collect-neuvector" => return neuvector::run().await,
        "replay" => return replay::run().await,
        other => anyhow::bail!("unknown command {other:?}; use server, collect-file, collect-rancher, collect-watch, collect-neuvector, replay or seed-demo"),
    }
    let cfg = Arc::new(config::Config::from_env()?);
    let store = store::Store::open_with(&cfg.db_path, cfg.clone())?;
    if cmd == "seed-demo" {
        demo::seed(&store, &cfg)?;
        info!(db = %cfg.db_path, "demo data written");
        return Ok(());
    }
    info!(cluster = %cfg.cluster, db = %cfg.db_path, collectors = cfg.ingest_tokens.len(), "starting");
    if cfg.ingest_tokens.is_empty() {
        warn!("no collector tokens configured; the ingest endpoint will refuse all batches");
    }
    // Bind first so probes pass and audit batches are accepted while discovery runs.
    let state = api::AppState { store: store.clone(), cfg: cfg.clone() };
    let api_listener = tokio::net::TcpListener::bind(&cfg.listen).await?;
    changemap::warm(&store, cfg.tz);
    let ingest_listener = tokio::net::TcpListener::bind(&cfg.ingest_listen).await?;
    info!(api = %cfg.listen, ingest = %cfg.ingest_listen, "serving");

    // Audit batches arrive after the watch sees the change, so link them periodically.
    let s = store.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let s = s.clone();
            match tokio::task::spawn_blocking(move || s.link_pending(chrono::Utc::now())).await {
                Ok(Err(e)) => warn!(error = %e, "link failed"),
                Err(e) => warn!(error = %e, "link task failed"),
                _ => {}
            }
        }
    });
    // Chain anchors every hour; forwarding sends them out with the records.
    let s = store.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            let s = s.clone();
            if let Ok(Err(e)) = tokio::task::spawn_blocking(move || s.take_anchors(chrono::Utc::now())).await {
                warn!(error = %e, "anchor failed");
            }
        }
    });
    let targets = forward::targets_from_env()?;
    if !targets.is_empty() {
        forward::Forwarder::new(store.clone(), targets)?.spawn();
    }
    let (s, days) = (store.clone(), cfg.retention_days);
    tokio::spawn(async move {
        loop {
            let s2 = s.clone();
            if let Ok(Err(e)) = tokio::task::spawn_blocking(move || s2.prune(days, chrono::Utc::now())).await {
                warn!(error = %e, "prune failed");
            }
            tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
        }
    });

    let api_app = api::router(state.clone()).layer(tower_http::trace::TraceLayer::new_for_http());
    let ingest_app = api::ingest_router(state);
    tokio::try_join!(axum::serve(api_listener, api_app), axum::serve(ingest_listener, ingest_app))?;
    Ok(())
}
