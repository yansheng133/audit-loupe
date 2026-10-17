//! SUSE Security (NeuVector) process incidents, to show next to a `kubectl exec`.
//!
//! What this can and cannot show: SUSE Security records a process only when it is judged a
//! violation (not in the allowed list in Monitor/Protect mode) or matches its built-in
//! suspicious-process detection. It does not keep a full command history, so the UI says
//! "processes SUSE Security flagged", never "commands run".
//!
//! `collect-neuvector` reads the controller's REST API with a read-only API key ("Security
//! Event" read permission is enough) and pushes new process incidents. Incidents have no link
//! to the Kubernetes audit log; they are matched to an exec by namespace, pod and time.
//!
//! Environment: AL_NEUVECTOR_URL (default the controller service in cattle-neuvector-system),
//! AL_NEUVECTOR_APIKEY_FILE ("<access key>:<secret>"), AL_NEUVECTOR_CA_FILE for the
//! controller's certificate.

use crate::collect::{heartbeat, set, Sender};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;
use tracing::{info, warn};

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Incident {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub level: String,
    #[serde(default)]
    pub reported_timestamp: i64,
    #[serde(default)]
    pub workload_id: String,
    #[serde(default)]
    pub workload_name: String,
    #[serde(default)]
    pub workload_domain: String,
    #[serde(default)]
    pub proc_name: String,
    #[serde(default)]
    pub proc_path: String,
    #[serde(default)]
    pub proc_cmd: String,
    #[serde(default)]
    pub proc_parent_name: String,
    #[serde(default)]
    pub proc_effective_user: String,
    #[serde(default)]
    pub action: String,
    #[serde(default, rename = "message")]
    pub msg: String,
}

#[derive(Deserialize)]
struct IncidentList {
    #[serde(default)]
    incidents: Vec<Incident>,
}

/// Processes whose parent is the container runtime were started from outside the
/// container's own process tree, which is what kubectl exec does (probes too).
pub fn started_by_runtime(parent: &str) -> bool {
    matches!(parent, "runc" | "containerd-shim" | "containerd-shim-runc-v2" | "docker-runc" | "crio" | "conmon")
}

#[derive(Serialize, Deserialize, Default)]
struct State {
    since: i64,
    /// IDs already sent at `since`, so the overlap of the next query is not resent.
    seen: Vec<String>,
}

pub async fn run() -> anyhow::Result<()> {
    let sender = Sender::from_env("neuvector")?;
    let health = heartbeat("neuvector")?;
    let base = std::env::var("AL_NEUVECTOR_URL").unwrap_or_else(|_| "https://neuvector-svc-controller.cattle-neuvector-system:10443".into());
    let key_file = std::env::var("AL_NEUVECTOR_APIKEY_FILE").unwrap_or_else(|_| "/etc/audit-loupe/neuvector/apikey".into());
    let key = std::fs::read_to_string(&key_file).with_context(|| format!("reading {key_file}"))?.trim().to_string();
    let mut client = reqwest::Client::builder().timeout(Duration::from_secs(30));
    if let Ok(ca) = std::env::var("AL_NEUVECTOR_CA_FILE") {
        client = client.add_root_certificate(reqwest::Certificate::from_pem(&std::fs::read(&ca).with_context(|| format!("reading {ca}"))?)?);
    }
    let client = client.build()?;
    let state_dir = PathBuf::from(std::env::var("AL_STATE_DIR").unwrap_or_else(|_| "/var/lib/audit-loupe".into()));
    std::fs::create_dir_all(&state_dir)?;
    let state_file = state_dir.join("neuvector.json");
    let mut st: State = std::fs::read(&state_file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    if st.since == 0 {
        // First start: the last hour, not the controller's whole in-memory history.
        st.since = chrono::Utc::now().timestamp() - 3600;
    }
    info!(url = %base, since = st.since, "reading SUSE Security incidents");
    loop {
        let res = client
            .get(format!("{}/v1/log/incident", base.trim_end_matches('/')))
            .query(&[("f_since", st.since.to_string()), ("limit", "1000".into())])
            .header("X-Auth-Apikey", &key)
            .send()
            .await;
        match res {
            Ok(r) if r.status().is_success() => {
                let bytes = r.bytes().await.unwrap_or_default();
                let list: IncidentList = serde_json::from_slice(&bytes).unwrap_or(IncidentList { incidents: vec![] });
                set(&health, "controller", serde_json::json!("ok"));
                let seen: HashSet<String> = st.seen.iter().cloned().collect();
                let mut new: Vec<Incident> = list.incidents.into_iter().filter(|i| !i.proc_name.is_empty() && !seen.contains(&i.id)).collect();
                new.sort_by_key(|i| i.reported_timestamp);
                if !new.is_empty() {
                    let mut body = Vec::new();
                    for i in &new {
                        body.extend(serde_json::to_vec(i)?);
                        body.push(b'\n');
                    }
                    match sender.send(&body).await {
                        Ok(()) => {
                            let max = new.iter().map(|i| i.reported_timestamp).max().unwrap_or(st.since);
                            let at_max: Vec<String> = new.iter().filter(|i| i.reported_timestamp == max).map(|i| i.id.clone()).collect();
                            st.seen = if max == st.since { st.seen.into_iter().chain(at_max).collect() } else { at_max };
                            st.since = max;
                            std::fs::write(&state_file, serde_json::to_vec(&st)?)?;
                            set(&health, "lastSentMs", serde_json::json!(chrono::Utc::now().timestamp_millis()));
                        }
                        Err(e) => warn!(error = %e, "send failed; will retry"),
                    }
                }
            }
            Ok(r) => {
                warn!(status = %r.status(), "controller refused; check the API key's permissions");
                set(&health, "controller", serde_json::json!(format!("HTTP {}", r.status())));
            }
            Err(e) => {
                warn!(error = %e, "controller unreachable");
                set(&health, "controller", serde_json::json!("unreachable"));
            }
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_controller_shape() {
        let body = r#"{"incidents":[{"name":"Process.Profile.Violation","level":"Warning","reported_timestamp":1792200000,
          "reported_at":"2026-10-17T01:20:00Z","cluster_name":"cluster.local","host_id":"h","host_name":"node-1","enforcer_id":"e","enforcer_name":"e",
          "id":"abc","workload_id":"c1","workload_name":"web-6c9f7d-2kq8v","workload_domain":"shop","proc_name":"curl",
          "proc_cmd":"curl http://10.0.0.1/","proc_parent_name":"bash","action":"violate","rule_id":"","message":"Process profile violation"}]}"#;
        let l: IncidentList = serde_json::from_str(body).unwrap();
        assert_eq!((l.incidents[0].proc_name.as_str(), l.incidents[0].workload_domain.as_str()), ("curl", "shop"));
        assert!(started_by_runtime("containerd-shim-runc-v2") && !started_by_runtime("bash"));
    }
}
