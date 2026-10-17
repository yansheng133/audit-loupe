use std::env;

/// Runtime settings, all from environment variables so the Helm chart can set them.
#[derive(Clone, Debug)]
pub struct Config {
    pub db_path: String,
    pub listen: String,
    pub ingest_listen: String,
    /// Name shown in the UI for the cluster this instance runs in.
    pub cluster: String,
    pub watch_enabled: bool,
    /// Entries like "apps/v1/deployments" or "v1/configmaps".
    pub watch_resources: Vec<String>,
    pub tz: chrono_tz::Tz,
    pub work_start_hour: u32,
    pub work_end_hour: u32,
    /// ISO weekday numbers, Monday = 1.
    pub work_days: Vec<u32>,
    pub retention_days: i64,
    /// If set, the audit webhook must send `Authorization: Bearer <token>`.
    pub ingest_token: Option<String>,
}

pub const DEFAULT_WATCH: &str = "apps/v1/deployments,apps/v1/statefulsets,apps/v1/daemonsets,\
batch/v1/cronjobs,v1/services,v1/configmaps,v1/namespaces,\
networking.k8s.io/v1/ingresses,networking.k8s.io/v1/networkpolicies,\
rbac.authorization.k8s.io/v1/roles,rbac.authorization.k8s.io/v1/rolebindings,\
rbac.authorization.k8s.io/v1/clusterroles,rbac.authorization.k8s.io/v1/clusterrolebindings,\
autoscaling/v2/horizontalpodautoscalers,gateway.networking.k8s.io/v1/httproutes";

fn var(name: &str, default: &str) -> String {
    env::var(name).ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| default.to_string())
}

fn range(spec: &str) -> Vec<u32> {
    // "1-5" or "1,2,3"
    let mut out = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if let Some((a, b)) = part.split_once('-') {
            if let (Ok(a), Ok(b)) = (a.trim().parse::<u32>(), b.trim().parse::<u32>()) {
                out.extend(a..=b);
            }
        } else if let Ok(v) = part.parse::<u32>() {
            out.push(v);
        }
    }
    out
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let hours = var("AL_WORK_HOURS", "9-18");
        let (start, end) = hours
            .split_once('-')
            .and_then(|(a, b)| Some((a.trim().parse().ok()?, b.trim().parse().ok()?)))
            .unwrap_or((9, 18));
        let tz: chrono_tz::Tz = var("AL_TZ", "Asia/Taipei")
            .parse()
            .map_err(|e| anyhow::anyhow!("AL_TZ: {e}"))?;
        Ok(Config {
            db_path: var("AL_DB_PATH", "/data/audit-loupe.db"),
            listen: var("AL_LISTEN", "0.0.0.0:8080"),
            ingest_listen: var("AL_INGEST_LISTEN", "0.0.0.0:8081"),
            cluster: var("AL_CLUSTER", "local"),
            watch_enabled: var("AL_WATCH", "true") == "true",
            watch_resources: var("AL_WATCH_RESOURCES", DEFAULT_WATCH)
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            tz,
            work_start_hour: start,
            work_end_hour: end,
            work_days: range(&var("AL_WORK_DAYS", "1-5")),
            retention_days: var("AL_RETENTION_DAYS", "90").parse().unwrap_or(90),
            ingest_token: env::var("AL_INGEST_TOKEN").ok().filter(|v| !v.is_empty()),
        })
    }

    /// Settings for tests.
    pub fn test() -> Self {
        Config {
            db_path: ":memory:".into(),
            listen: "127.0.0.1:0".into(),
            ingest_listen: "127.0.0.1:0".into(),
            cluster: "test".into(),
            watch_enabled: false,
            watch_resources: vec![],
            tz: chrono_tz::Asia::Taipei,
            work_start_hour: 9,
            work_end_hour: 18,
            work_days: vec![1, 2, 3, 4, 5],
            retention_days: 90,
            ingest_token: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::range;
    #[test]
    fn parses_ranges() {
        assert_eq!(range("1-5"), vec![1, 2, 3, 4, 5]);
        assert_eq!(range("1,3, 6"), vec![1, 3, 6]);
    }
}
