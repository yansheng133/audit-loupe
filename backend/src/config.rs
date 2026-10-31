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
    /// Rule settings (AL_RULES): on or off, severity, exempt scopes.
    pub rules: crate::rules::RuleSet,
    /// Identities the built-in classification cannot tell apart (AL_IDENTITIES).
    pub identities: crate::identity::Whitelist,
    /// How the change map v6 cuts operations into behaviors (AL_BEHAVIOR_*).
    pub behaviors: crate::behaviors::Settings,
}

fn var(name: &str, default: &str) -> String {
    env::var(name).ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| default.to_string())
}

/// ISO weekdays from "1-5" or "1,3,5" (Monday = 1). Anything else is an error, so a setting
/// like "Mon-Fri" stops the config check instead of meaning "no working days".
pub fn days(spec: &str) -> anyhow::Result<Vec<u32>> {
    let day = |v: &str| match v.trim().parse::<u32>() {
        Ok(d @ 1..=7) => Ok(d),
        _ => Err(anyhow::anyhow!("\"{spec}\": days are numbers 1 (Monday) to 7 (Sunday), as \"1-5\" or \"1,3,5\"")),
    };
    let mut out = Vec::new();
    // Empty parts ("1-5,") are skipped, as before; a list with no day at all is refused.
    for part in spec.split(',').filter(|p| !p.trim().is_empty()) {
        match part.split_once('-') {
            Some((a, b)) => {
                let (a, b) = (day(a)?, day(b)?);
                if a > b {
                    anyhow::bail!("\"{spec}\": {a}-{b} runs backwards");
                }
                out.extend(a..=b);
            }
            None => out.push(day(part)?),
        }
    }
    if out.is_empty() {
        anyhow::bail!("\"{spec}\": no days");
    }
    Ok(out)
}

/// Start and end hour from "9-18" (0 to 24). The end may be before the start ("22-2"): the
/// hours then run past midnight.
pub fn hours(spec: &str) -> anyhow::Result<(u32, u32)> {
    let hour = |v: &str| v.trim().parse::<u32>().ok().filter(|h| *h <= 24);
    match spec.split_once('-').map(|(a, b)| (hour(a), hour(b))) {
        Some((Some(a), Some(b))) if a != b => Ok((a, b)),
        _ => anyhow::bail!("\"{spec}\": hours are whole hours 0 to 24, as \"9-18\""),
    }
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
        let (start, end) = hours(&var("AL_WORK_HOURS", "9-18"))
            .and_then(|(a, b)| if a < b { Ok((a, b)) } else { anyhow::bail!("working hours cannot run past midnight") })
            .map_err(|e| anyhow::anyhow!("AL_WORK_HOURS: {e}"))?;
        let tz: chrono_tz::Tz = var("AL_TZ", "Asia/Taipei")
            .parse()
            .map_err(|e| anyhow::anyhow!("AL_TZ: {e}"))?;
        Ok(Config {
            db_path: var("AL_DB_PATH", "/data/audit-loupe.db"),
            listen: var("AL_LISTEN", "0.0.0.0:8080"),
            ingest_listen: var("AL_INGEST_LISTEN", "0.0.0.0:8081"),
            cluster: var("AL_CLUSTER", "local"),
            work_start_hour: start,
            work_end_hour: end,
            work_days: days(&var("AL_WORK_DAYS", "1-5")).map_err(|e| anyhow::anyhow!("AL_WORK_DAYS: {e}"))?,
            retention_days: match var("AL_RETENTION_DAYS", "30").trim().parse::<i64>() {
                Ok(d) if d > 0 => d,
                _ => anyhow::bail!("AL_RETENTION_DAYS: a whole number of days greater than 0, as \"30\""),
            },
            ingest_tokens: load_tokens(&var("AL_INGEST_TOKENS_FILE", "/etc/audit-loupe/tokens/tokens"))?,
            rancher_cluster: var("AL_RANCHER_CLUSTER", "local"),
            change_windows: crate::windows::parse(&env::var("AL_CHANGE_WINDOWS").unwrap_or_default()).map_err(|e| anyhow::anyhow!("AL_CHANGE_WINDOWS: {e}"))?,
            rules: crate::rules::RuleSet::parse(&env::var("AL_RULES").unwrap_or_default(), &tz).map_err(|e| anyhow::anyhow!("AL_RULES: {e}"))?,
            identities: crate::identity::Whitelist::parse(&env::var("AL_IDENTITIES").unwrap_or_default()).map_err(|e| anyhow::anyhow!("AL_IDENTITIES: {e}"))?,
            behaviors: crate::behaviors::Settings::from_env()?,
            tz,
        })
    }

    /// Built-in settings, for a store opened without any.
    #[cfg(test)]
    pub fn defaults() -> Self {
        Config {
            db_path: ":memory:".into(),
            listen: "127.0.0.1:0".into(),
            ingest_listen: "127.0.0.1:0".into(),
            cluster: "local".into(),
            tz: chrono_tz::Asia::Taipei,
            work_start_hour: 9,
            work_end_hour: 18,
            work_days: vec![1, 2, 3, 4, 5],
            retention_days: 30,
            ingest_tokens: vec![],
            rancher_cluster: "local".into(),
            change_windows: vec![],
            rules: crate::rules::RuleSet::default(),
            identities: crate::identity::Whitelist::default(),
            behaviors: crate::behaviors::Settings::default(),
        }
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
            rules: crate::rules::RuleSet::default(),
            identities: crate::identity::Whitelist::default(),
            behaviors: crate::behaviors::Settings::default(),
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
    fn parses_days_and_hours() {
        assert_eq!(days("1-5").unwrap(), vec![1, 2, 3, 4, 5]);
        assert_eq!(days("1,3, 6").unwrap(), vec![1, 3, 6]);
        assert_eq!(hours("9-18").unwrap(), (9, 18));
        assert_eq!(hours("22-2").unwrap(), (22, 2));
    }

    /// A setting the server cannot read stops the config check; it used to fall back
    /// quietly ("Mon-Fri" meant no working days, "90d" meant 30 days kept).
    #[test]
    fn unreadable_settings_are_refused() {
        assert_eq!(days("1-5,").unwrap(), vec![1, 2, 3, 4, 5], "a trailing comma was accepted before");
        for bad in ["Mon-Fri", "0-5", "1-8", "5-1", ",", ""] {
            assert!(days(bad).is_err(), "{bad}");
        }
        for bad in ["9:00-18:00", "9", "9-25", "9-9", "a-b"] {
            assert!(hours(bad).is_err(), "{bad}");
        }
    }
}
