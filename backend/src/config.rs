use std::env;

/// Runtime settings, all from environment variables so the Helm chart can set them.
#[derive(Clone, Debug)]
pub struct Config {
    pub db_path: String,
    pub listen: String,
    pub ingest_listen: String,
    /// Name shown in the UI for the cluster this instance runs in.
    pub cluster: String,
    pub tz: chrono_tz::Tz,
    pub work_start_hour: u32,
    pub work_end_hour: u32,
    /// ISO weekday numbers, Monday = 1.
    pub work_days: Vec<u32>,
    pub retention_days: i64,
    /// One entry per cluster allowed to push: (cluster id, sha256 of its token).
    /// The cluster a batch belongs to is decided by which token it carries.
    pub ingest_tokens: Vec<(String, [u8; 32])>,
    /// The cluster Rancher runs in. Only its token may send Rancher API audit records, since
    /// those can name any downstream cluster as their target.
    pub rancher_cluster: String,
    /// Approved change windows; when set they replace the working-hours rule.
    pub change_windows: Vec<crate::windows::Window>,
}

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

pub fn hash_token(token: &str) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(token.trim().as_bytes()).into()
}

/// Lines of "<cluster-id> <token>"; blank lines and # comments are ignored.
/// A missing file means no cluster may push (the read API still works).
pub fn parse_tokens(text: &str) -> anyhow::Result<Vec<(String, [u8; 32])>> {
    let mut out: Vec<(String, [u8; 32])> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(cluster), Some(token), None) = (parts.next(), parts.next(), parts.next()) else {
            anyhow::bail!("tokens line {}: expected \"<cluster-id> <token>\"", n + 1);
        };
        if token.len() < 16 {
            anyhow::bail!("tokens line {}: token for {cluster} is shorter than 16 characters", n + 1);
        }
        let h = hash_token(token);
        if out.iter().any(|(c, t)| c == cluster || *t == h) {
            anyhow::bail!("tokens line {}: duplicate cluster or token", n + 1);
        }
        out.push((cluster.to_string(), h));
    }
    Ok(out)
}

fn load_tokens(path: &str) -> anyhow::Result<Vec<(String, [u8; 32])>> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_tokens(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(anyhow::anyhow!("{path}: {e}")),
    }
}

impl Config {
    /// Which cluster a bearer token belongs to. Every entry is compared so timing
    /// does not reveal how many clusters are configured or which one matched.
    pub fn cluster_for_token(&self, token: &str) -> Option<String> {
        use subtle::ConstantTimeEq;
        let h = hash_token(token);
        let mut found = None;
        for (cluster, t) in &self.ingest_tokens {
            if bool::from(t.ct_eq(&h)) {
                found = Some(cluster.clone());
            }
        }
        found
    }

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
            tz,
            work_start_hour: start,
            work_end_hour: end,
            work_days: range(&var("AL_WORK_DAYS", "1-5")),
            retention_days: var("AL_RETENTION_DAYS", "30").parse().unwrap_or(30),
            ingest_tokens: load_tokens(&var("AL_INGEST_TOKENS_FILE", "/etc/audit-loupe/tokens/tokens"))?,
            rancher_cluster: var("AL_RANCHER_CLUSTER", "local"),
            change_windows: crate::windows::parse(&env::var("AL_CHANGE_WINDOWS").unwrap_or_default()).map_err(|e| anyhow::anyhow!("AL_CHANGE_WINDOWS: {e}"))?,
        })
    }

    /// Settings for tests.
    #[cfg(test)]
    pub fn test() -> Self {
        Config {
            db_path: ":memory:".into(),
            listen: "127.0.0.1:0".into(),
            ingest_listen: "127.0.0.1:0".into(),
            cluster: "test".into(),
            tz: chrono_tz::Asia::Taipei,
            work_start_hour: 9,
            work_end_hour: 18,
            work_days: vec![1, 2, 3, 4, 5],
            retention_days: 90,
            ingest_tokens: vec![
                ("test".into(), crate::config::hash_token("test-token")),
                ("local".into(), crate::config::hash_token("rancher-token")),
            ],
            rancher_cluster: "local".into(),
            change_windows: vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tokens_map_to_clusters() {
        let mut cfg = Config::test();
        cfg.ingest_tokens = parse_tokens("# comment\nlocal aaaaaaaaaaaaaaaaaaaa\nc-m-7h2kx  bbbbbbbbbbbbbbbbbbbb\n").unwrap();
        assert_eq!(cfg.cluster_for_token("bbbbbbbbbbbbbbbbbbbb").as_deref(), Some("c-m-7h2kx"));
        assert_eq!(cfg.cluster_for_token("aaaaaaaaaaaaaaaaaaaa").as_deref(), Some("local"));
        assert_eq!(cfg.cluster_for_token("cccccccccccccccccccc"), None);
        assert!(parse_tokens("x short").is_err());
        assert!(parse_tokens("a aaaaaaaaaaaaaaaaaaaa\na aaaaaaaaaaaaaaaaaaab").is_err());
        assert!(parse_tokens("a aaaaaaaaaaaaaaaaaaaa extra").is_err());
    }
    #[test]
    fn parses_ranges() {
        assert_eq!(range("1-5"), vec![1, 2, 3, 4, 5]);
        assert_eq!(range("1,3, 6"), vec![1, 3, 6]);
    }
}
