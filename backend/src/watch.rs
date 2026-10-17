//! Watch selected resources and store a version every time their spec changes.
//! Audit logs say who sent a change; the stored versions say what it changed from.

use crate::diff::mask_values;
use crate::store::{ObjKey, Store};
use chrono::{DateTime, Duration as CDuration, Utc};
use futures::StreamExt;
use kube::api::{Api, DynamicObject};
use kube::discovery::{self, ApiResource};
use kube::runtime::watcher::{self, Event};
use kube::runtime::WatchStreamExt;
use kube::Client;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{info, warn};

/// Per-resource watch state, reported by /api/info so a silent failure is visible.
pub type Status = Arc<Mutex<BTreeMap<String, String>>>;

/// Never watched, whatever the configuration says.
const DENY: &[&str] = &["secrets"];

/// "apps/v1/deployments" -> ("apps", "v1", "deployments"); "v1/configmaps" -> ("", "v1", "configmaps")
pub fn split_spec(spec: &str) -> Option<(String, String, String)> {
    let parts: Vec<&str> = spec.split('/').collect();
    match parts.as_slice() {
        [v, r] => Some((String::new(), v.to_string(), r.to_string())),
        [g, v, r] => Some((g.to_string(), v.to_string(), r.to_string())),
        _ => None,
    }
}

/// Use the API server's clock rather than ours: the latest managedFields time, or the
/// deletionTimestamp. Fall back to now if missing or implausible (clock skew, old objects).
fn change_time(obj: &Value, now: DateTime<Utc>) -> DateTime<Utc> {
    let mut best: Option<DateTime<Utc>> = obj
        .pointer("/metadata/deletionTimestamp")
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok());
    if let Some(fields) = obj.pointer("/metadata/managedFields").and_then(Value::as_array) {
        for f in fields {
            if let Some(t) = f.get("time").and_then(Value::as_str).and_then(|s| s.parse::<DateTime<Utc>>().ok()) {
                best = Some(best.map_or(t, |b| b.max(t)));
            }
        }
    }
    match best {
        Some(t) if (now - t).abs() < CDuration::minutes(2) => t,
        _ => now,
    }
}

async fn resolve(client: &Client, spec: &str) -> anyhow::Result<Option<ApiResource>> {
    let Some((group, version, plural)) = split_spec(spec) else {
        anyhow::bail!("expected group/version/resource");
    };
    if DENY.contains(&plural.as_str()) {
        anyhow::bail!("{plural} are never watched");
    }
    // Only the one group, so an unavailable aggregated API elsewhere does not matter.
    let g = match discovery::group(client, &group).await {
        Ok(g) => g,
        Err(kube::Error::Api(e)) if e.code == 404 => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    Ok(g.versioned_resources(&version).into_iter().find(|(ar, _)| ar.plural == plural).map(|(ar, _)| ar))
}

fn store_obj(store: &Store, cluster: &str, ar: &ApiResource, obj: &DynamicObject, seen: &str) -> Option<(String, String)> {
    let key = ObjKey {
        cluster: cluster.to_string(),
        api_group: ar.group.clone(),
        resource: ar.plural.clone(),
        namespace: obj.metadata.namespace.clone().unwrap_or_default(),
        name: obj.metadata.name.clone().unwrap_or_default(),
    };
    let uid = obj.metadata.uid.clone().unwrap_or_default();
    let mut value = serde_json::to_value(obj).ok()?;
    if ar.plural == "configmaps" && ar.group.is_empty() {
        mask_values(&mut value);
    }
    let at = if seen == "deleted" { Utc::now() } else { change_time(&value, Utc::now()) };
    if let Err(e) = store.insert_version(&key, &ar.kind, &uid, &value, seen, at) {
        warn!(error = %e, "store version failed");
    }
    Some((key.namespace, key.name))
}

async fn watch_one(client: Client, ar: ApiResource, cluster: String, store: Store, status: Status, label: String) {
    let api: Api<DynamicObject> = Api::all_with(client, &ar);
    let first = !store.has_any(&cluster, &ar.group, &ar.plural).unwrap_or(false);
    let mut listed_once = false;
    let mut seen_in_list: HashSet<(String, String)> = HashSet::new();
    let mut stream = watcher::watcher(api, watcher::Config::default()).default_backoff().boxed();
    status.lock().unwrap().insert(label.clone(), "listing".into());
    while let Some(item) = stream.next().await {
        match item {
            Ok(Event::Init) => seen_in_list.clear(),
            Ok(Event::InitApply(o)) => {
                let seen = if first && !listed_once { "baseline" } else { "resync" };
                if let Some(k) = store_obj(&store, &cluster, &ar, &o, seen) {
                    seen_in_list.insert(k);
                }
            }
            Ok(Event::InitDone) => {
                // Anything we thought was alive but is missing from the fresh list was deleted
                // while we were not watching.
                if listed_once || !first {
                    if let Ok(alive) = store.alive(&cluster, &ar.group, &ar.plural) {
                        for (ns, name, uid, kind, body) in alive {
                            if !seen_in_list.contains(&(ns.clone(), name.clone())) {
                                let key = ObjKey { cluster: cluster.clone(), api_group: ar.group.clone(), resource: ar.plural.clone(), namespace: ns, name };
                                let _ = store.insert_version(&key, &kind, &uid, &body, "deleted", Utc::now());
                            }
                        }
                    }
                }
                listed_once = true;
                status.lock().unwrap().insert(label.clone(), "watching".into());
            }
            Ok(Event::Apply(o)) => {
                store_obj(&store, &cluster, &ar, &o, "modified");
            }
            Ok(Event::Delete(o)) => {
                store_obj(&store, &cluster, &ar, &o, "deleted");
            }
            Err(e) => {
                // default_backoff() spaces out retries; just record it.
                warn!(resource = %label, error = %e, "watch error");
                status.lock().unwrap().insert(label.clone(), format!("error: {e}"));
            }
        }
    }
}

/// Resolve each configured resource (retrying until the API is reachable) and watch it.
pub fn run(specs: Vec<String>, cluster: String, store: Store, status: Status) {
    tokio::spawn(async move {
        let client = loop {
            match Client::try_default().await {
                Ok(c) => break c,
                Err(e) => {
                    warn!(error = %e, "kubernetes client unavailable, retrying");
                    tokio::time::sleep(Duration::from_secs(15)).await;
                }
            }
        };
        for spec in specs {
            let (client, cluster, store, status) = (client.clone(), cluster.clone(), store.clone(), status.clone());
            tokio::spawn(async move {
                let mut delay = 5;
                loop {
                    match resolve(&client, &spec).await {
                        Ok(Some(ar)) => {
                            info!(resource = %spec, "watching");
                            watch_one(client, ar, cluster, store, status, spec).await;
                            return;
                        }
                        Ok(None) => {
                            status.lock().unwrap().insert(spec.clone(), "not served by this cluster".into());
                            return;
                        }
                        Err(e) => {
                            status.lock().unwrap().insert(spec.clone(), format!("error: {e}"));
                            if e.to_string().contains("never watched") || e.to_string().contains("expected group") {
                                return;
                            }
                            tokio::time::sleep(Duration::from_secs(delay)).await;
                            delay = (delay * 2).min(300);
                        }
                    }
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    #[test]
    fn specs() {
        assert_eq!(split_spec("v1/configmaps"), Some(("".into(), "v1".into(), "configmaps".into())));
        assert_eq!(split_spec("apps/v1/deployments"), Some(("apps".into(), "v1".into(), "deployments".into())));
        assert_eq!(split_spec("x"), None);
    }

    #[test]
    fn uses_api_server_time_when_plausible() {
        let now = Utc.with_ymd_and_hms(2026, 10, 14, 2, 0, 10).unwrap();
        let obj = json!({"metadata":{"managedFields":[{"time":"2026-10-14T01:00:00Z"},{"time":"2026-10-14T02:00:03Z"}]}});
        assert_eq!(change_time(&obj, now).to_rfc3339(), "2026-10-14T02:00:03+00:00");
        let stale = json!({"metadata":{"managedFields":[{"time":"2026-10-01T00:00:00Z"}]}});
        assert_eq!(change_time(&stale, now), now);
    }
}
