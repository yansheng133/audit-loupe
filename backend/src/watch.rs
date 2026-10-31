//! `collect-watch`: watch selected resources in the cluster it runs in and push every spec
//! change to the server. Audit logs say who sent a change; the stored versions say what it
//! changed from.
//!
//! Everything that must not leave the cluster is removed here, before sending: ConfigMap
//! values become short hashes, local users' password hashes are dropped, and Secrets are
//! never watched. Status-only updates are skipped here too, which is most of the traffic.

use crate::collect::{batch_and_send, Line, Sender};
use crate::diff::{mask_values, normalize};
use chrono::{DateTime, Duration as CDuration, Utc};
use futures::StreamExt;
use kube::api::{Api, DynamicObject, TypeMeta};
use kube::discovery::{self, ApiResource};
use kube::runtime::watcher::{self, Event};
use kube::runtime::WatchStreamExt;
use kube::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{info, warn};

/// Never watched, whatever the configuration says.
const DENY: &[&str] = &["secrets", "tokens"];

pub const DEFAULT_WATCH: &str = "apps/v1/deployments,apps/v1/statefulsets,apps/v1/daemonsets,\
batch/v1/cronjobs,v1/services,v1/configmaps,v1/namespaces,v1/serviceaccounts,\
networking.k8s.io/v1/ingresses,networking.k8s.io/v1/networkpolicies,\
rbac.authorization.k8s.io/v1/roles,rbac.authorization.k8s.io/v1/rolebindings,\
rbac.authorization.k8s.io/v1/clusterroles,rbac.authorization.k8s.io/v1/clusterrolebindings,\
autoscaling/v2/horizontalpodautoscalers,gateway.networking.k8s.io/v1/httproutes";

/// Added on the Rancher cluster: who may do what in Rancher (for the access review).
pub const RANCHER_WATCH: &str = "management.cattle.io/v3/globalroles,management.cattle.io/v3/globalrolebindings,\
management.cattle.io/v3/roletemplates,management.cattle.io/v3/clusterroletemplatebindings,\
management.cattle.io/v3/projectroletemplatebindings,management.cattle.io/v3/projects,\
management.cattle.io/v3/users,management.cattle.io/v3/userattributes";

/// One message from `collect-watch`, a JSON line in a `k8s-object` batch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ObjMsg {
    /// A full list of this resource type begins (collector start, or the watch was lost).
    ListStart { group: String, resource: String },
    /// An object as listed or as changed. `seen` is "list", "modified" or "deleted".
    Version {
        group: String,
        resource: String,
        kind: String,
        namespace: String,
        name: String,
        uid: String,
        seen: String,
        at: DateTime<Utc>,
        object: Value,
    },
    /// The list is complete; anything stored as alive but not listed was deleted meanwhile.
    ListDone { group: String, resource: String, listed: Vec<(String, String)> },
}

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

/// Remove what must not leave the cluster and what changes without anyone acting.
pub fn sanitize(group: &str, resource: &str, obj: &mut Value) {
    if let Some(meta) = obj.get_mut("metadata").and_then(Value::as_object_mut) {
        meta.remove("managedFields");
    }
    match (group, resource) {
        ("", "configmaps") => mask_values(obj),
        ("management.cattle.io", "users") => {
            if let Some(m) = obj.as_object_mut() {
                m.remove("password");
            }
        }
        // Refreshed on every login and hourly; only the group lists matter.
        ("management.cattle.io", "userattributes") => {
            if let Some(m) = obj.as_object_mut() {
                for k in ["LastRefresh", "NeedsRefresh", "LastLogin", "lastRefresh", "needsRefresh", "lastLogin"] {
                    m.remove(k);
                }
            }
        }
        _ => {}
    }
}

fn digest(v: &Value) -> [u8; 32] {
    Sha256::digest(v.to_string().as_bytes()).into()
}

async fn resolve(client: &Client, spec: &str) -> anyhow::Result<Option<ApiResource>> {
    let Some((group, version, plural)) = split_spec(spec) else {
        anyhow::bail!("expected group/version/resource");
    };
    if DENY.contains(&plural.as_str()) {
        anyhow::bail!("{plural} are never watched");
    }
    // Only the one group, so an unavailable aggregated API elsewhere does not matter.
    // A group the cluster does not serve (Gateway API not installed, Rancher types on a
    // downstream cluster) is a discovery error, not a 404.
    let g = match discovery::group(client, &group).await {
        Ok(g) => g,
        Err(kube::Error::Api(e)) if e.code == 404 => return Ok(None),
        Err(kube::Error::Discovery(kube::error::DiscoveryError::MissingApiGroup(_))) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    Ok(g.versioned_resources(&version).into_iter().find(|(ar, _)| ar.plural == plural).map(|(ar, _)| ar))
}

fn with_types(obj: &mut DynamicObject, ar: &ApiResource) {
    if obj.types.is_none() {
        obj.types = Some(TypeMeta { api_version: ar.api_version.clone(), kind: ar.kind.clone() });
    }
}

fn line(m: &ObjMsg) -> Line {
    Line { mark: None, text: serde_json::to_string(m).expect("serializable") }
}

/// What the coverage page shows for a watch that keeps failing: a refused request (403) is
/// almost always the chart's ClusterRole not covering a type added to `resources`.
fn failure(e: &watcher::Error) -> &'static str {
    let api = match e {
        watcher::Error::InitialListFailed(kube::Error::Api(a)) | watcher::Error::WatchStartFailed(kube::Error::Api(a)) | watcher::Error::WatchFailed(kube::Error::Api(a)) => Some(a.code),
        watcher::Error::WatchError(a) => Some(a.code),
        _ => None,
    };
    match api {
        Some(403) => "forbidden",
        _ => "failing",
    }
}

async fn watch_one(client: Client, ar: ApiResource, tx: tokio::sync::mpsc::Sender<Line>, health: crate::collect::Health, spec: &str) -> anyhow::Result<()> {
    let api: Api<DynamicObject> = Api::all_with(client, &ar);
    let (group, resource) = (ar.group.clone(), ar.plural.clone());
    // Last digest sent per object, to skip updates that change nothing a person sets.
    let mut sent: HashMap<(String, String), [u8; 32]> = HashMap::new();
    let mut listed: Vec<(String, String)> = Vec::new();
    let mut stream = watcher::watcher(api, watcher::Config::default()).default_backoff().boxed();
    // Health says "watching" only while the stream works; a failure is shown until it recovers.
    let mut failing = false;
    while let Some(item) = stream.next().await {
        if failing && item.is_ok() {
            failing = false;
            crate::collect::set(&health, spec, serde_json::json!("watching"));
        }
        let (mut obj, seen) = match item {
            Ok(Event::Init) => {
                listed.clear();
                tx.send(line(&ObjMsg::ListStart { group: group.clone(), resource: resource.clone() })).await?;
                continue;
            }
            Ok(Event::InitDone) => {
                let done = ObjMsg::ListDone { group: group.clone(), resource: resource.clone(), listed: std::mem::take(&mut listed) };
                tx.send(line(&done)).await?;
                continue;
            }
            Ok(Event::InitApply(o)) => (o, "list"),
            Ok(Event::Apply(o)) => (o, "modified"),
            Ok(Event::Delete(o)) => (o, "deleted"),
            Err(e) => {
                // default_backoff() spaces out retries; the watcher re-lists by itself.
                warn!(resource = %resource, error = %e, "watch error");
                if !failing {
                    failing = true;
                    crate::collect::set(&health, spec, serde_json::json!(failure(&e)));
                }
                continue;
            }
        };
        // Items of a LIST response carry no apiVersion/kind; watch events do. Without this every
        // re-list shows a change on those two fields, and restore YAML would lack them.
        with_types(&mut obj, &ar);
        let namespace = obj.metadata.namespace.clone().unwrap_or_default();
        let name = obj.metadata.name.clone().unwrap_or_default();
        let uid = obj.metadata.uid.clone().unwrap_or_default();
        let mut value = serde_json::to_value(&obj)?;
        let now = Utc::now();
        let at = if seen == "deleted" { now } else { change_time(&value, now) };
        sanitize(&group, &resource, &mut value);
        let key = (namespace.clone(), name.clone());
        if seen == "list" {
            listed.push(key.clone());
        }
        let d = digest(&normalize(&value));
        if seen == "deleted" {
            sent.remove(&key);
        } else if seen == "modified" && sent.get(&key) == Some(&d) {
            continue;
        } else {
            sent.insert(key, d);
        }
        let kind = obj.types.as_ref().map(|t| t.kind.clone()).unwrap_or_else(|| ar.kind.clone());
        let msg = ObjMsg::Version { group: group.clone(), resource: resource.clone(), kind, namespace, name, uid, seen: seen.into(), at, object: value };
        tx.send(line(&msg)).await?;
    }
    Ok(())
}

pub fn resources_from_env() -> Vec<String> {
    let base = std::env::var("AL_WATCH_RESOURCES").ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| DEFAULT_WATCH.to_string());
    let mut specs: Vec<String> = base.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    if std::env::var("AL_WATCH_RANCHER").is_ok_and(|v| v == "true") {
        specs.extend(RANCHER_WATCH.split(',').map(str::to_string));
    }
    specs
}

/// Resolve each configured resource (retrying until the API serves it) and watch it.
pub async fn run() -> anyhow::Result<()> {
    let sender = Sender::from_env("k8s-object")?;
    let health = crate::collect::heartbeat("watch")?;
    let specs = resources_from_env();
    let client = Client::try_default().await?;
    let (tx, rx) = tokio::sync::mpsc::channel::<Line>(4096);
    let sending = tokio::spawn(batch_and_send(rx, sender, None));
    info!(resources = specs.len(), "watching");
    let mut tasks = Vec::new();
    for spec in specs {
        let (client, tx, health) = (client.clone(), tx.clone(), health.clone());
        tasks.push(tokio::spawn(async move {
            let mut delay = 5;
            loop {
                match resolve(&client, &spec).await {
                    Ok(Some(ar)) => {
                        info!(resource = %spec, "watching");
                        crate::collect::set(&health, &spec, serde_json::json!("watching"));
                        if let Err(e) = watch_one(client.clone(), ar, tx.clone(), health.clone(), &spec).await {
                            warn!(resource = %spec, error = %e, "watch stopped");
                        }
                        return;
                    }
                    Ok(None) => {
                        info!(resource = %spec, "not served by this cluster; skipped");
                        crate::collect::set(&health, &spec, serde_json::json!("not served"));
                        return;
                    }
                    Err(e) if e.to_string().contains("never watched") || e.to_string().contains("expected group") => {
                        warn!(resource = %spec, error = %e, "skipped");
                        return;
                    }
                    Err(e) => {
                        warn!(resource = %spec, error = %e, retry_in = delay, "cannot resolve yet");
                        tokio::time::sleep(Duration::from_secs(delay)).await;
                        delay = (delay * 2).min(300);
                    }
                }
            }
        }));
    }
    drop(tx);
    for t in tasks {
        let _ = t.await;
    }
    let _ = sending.await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    /// A refused watch shows as "forbidden" on the coverage page, anything else as "failing".
    #[test]
    fn a_failing_watch_says_why() {
        let status = |code: u16| -> kube::core::Status { serde_json::from_value(json!({"status": "Failure", "code": code})).unwrap() };
        assert_eq!(failure(&watcher::Error::InitialListFailed(kube::Error::Api(status(403).into()))), "forbidden");
        assert_eq!(failure(&watcher::Error::WatchError(status(410).into())), "failing");
    }

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

    #[test]
    fn nothing_sensitive_leaves_the_cluster() {
        let mut cm = json!({"metadata":{"name":"a","managedFields":[{}]},"data":{"DB_PASSWORD":"hunter2"}});
        sanitize("", "configmaps", &mut cm);
        assert!(!cm.to_string().contains("hunter2"));
        assert!(cm["metadata"].get("managedFields").is_none());
        let mut user = json!({"metadata":{"name":"u-abc"},"username":"admin","password":"$2a$10$xyz","principalIds":["local://u-abc"]});
        sanitize("management.cattle.io", "users", &mut user);
        assert!(user.get("password").is_none());
        assert_eq!(user["username"], "admin");
        let mut ua = json!({"metadata":{"name":"u-abc"},"GroupPrincipals":{"openldap":{"items":[]}},"LastRefresh":"2026-10-17T01:00:00Z","NeedsRefresh":false});
        sanitize("management.cattle.io", "userattributes", &mut ua);
        assert!(ua.get("LastRefresh").is_none() && ua.get("GroupPrincipals").is_some());
    }

    #[test]
    fn messages_round_trip() {
        let m = ObjMsg::ListDone { group: "apps".into(), resource: "deployments".into(), listed: vec![("shop".into(), "web".into())] };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.starts_with(r#"{"type":"list_done""#), "{s}");
        assert_eq!(serde_json::from_str::<ObjMsg>(&s).unwrap(), m);
    }
}
