mod api;
mod audit;
mod config;
mod demo;
mod diff;
mod rules;
mod store;
mod watch;

use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let cfg = Arc::new(config::Config::from_env()?);
    let store = store::Store::open(&cfg.db_path)?;
    if std::env::args().nth(1).as_deref() == Some("seed-demo") {
        demo::seed(&store, &cfg)?;
        info!(db = %cfg.db_path, "demo data written");
        return Ok(());
    }
    info!(cluster = %cfg.cluster, db = %cfg.db_path, "starting");
    let status: watch::Status = Default::default();

    // Bind first so probes pass and audit batches are accepted while discovery runs.
    let state = api::AppState { store: store.clone(), cfg: cfg.clone(), watch: status.clone() };
    let api_listener = tokio::net::TcpListener::bind(&cfg.listen).await?;
    let ingest_listener = tokio::net::TcpListener::bind(&cfg.ingest_listen).await?;
    info!(api = %cfg.listen, ingest = %cfg.ingest_listen, "serving");

    if cfg.watch_enabled {
        watch::run(cfg.watch_resources.clone(), cfg.cluster.clone(), store.clone(), status.clone());
    }

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
