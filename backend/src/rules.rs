//! Rules that flag audit records worth a second look, and the settings that tune them.
//!
//! Each rule detects one kind of action. Whether it is on, how serious it is and which
//! scopes are exempt comes from the rule settings (AL_RULES, rendered by the Helm chart from
//! `server.rules`); anything not set keeps the built-in default. Only people are checked:
//! controllers and other `system:*` callers change things all day.
//!
//! Some rules need what was stored about the object (Fleet markers, namespace labels for
//! change windows); those look it up as it was at the time of the action, so judging a record
//! again later gives the same answer as judging it when it arrived.

use crate::audit::{is_write, Record, Via};
use crate::config::Config;
use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Timelike};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Alert {
    pub rule: &'static str,
    /// "high", "medium" or "low".
    pub severity: &'static str,
}

/// Every rule, in display order, with its built-in severity.
pub const RULES: &[(&str, &str)] = &[
    ("namespace_deleted", "high"),
    ("rbac_changed", "medium"),
    ("pod_exec", "medium"),
    ("bypassed_rancher", "medium"),
    ("gitops_bypassed", "medium"),
    ("outside_change_window", "medium"),
    ("user_deleted", "medium"),
    ("secret_read", "low"),
    ("credential_issued", "low"),
    ("off_hours", "low"),
    ("forbidden", "low"),
    ("login_failed", "low"),
];

const SEVERITIES: &[&str] = &["high", "medium", "low"];

/// Kubernetes RBAC plus Rancher's own role and binding types (management.cattle.io).
const RBAC: &[&str] = &[
    "roles", "rolebindings", "clusterroles", "clusterrolebindings",
    "globalroles", "globalrolebindings", "roletemplates",
    "clusterroletemplatebindings", "projectroletemplatebindings",
];

// ---------------------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleInput {
    enabled: Option<bool>,
    severity: Option<String>,
    #[serde(default)]
    except: Vec<ExceptInput>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ExceptInput {
    #[serde(default)]
    clusters: Vec<String>,
    #[serde(default)]
    namespaces: Vec<String>,
    #[serde(default)]
    namespace_labels: BTreeMap<String, String>,
    #[serde(default)]
    users: Vec<String>,
    #[serde(default)]
    resources: Vec<String>,
    #[serde(default)]
    reason: String,
    until: Option<String>,
}

/// A scope a rule does not apply to. Every field that is set must match (a list matches
/// when any entry does); `*` in a pattern matches any text.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Exception {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub clusters: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub namespaces: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub namespace_labels: BTreeMap<String, String>,
    /// Matches the person's name, Rancher user ID or identity-provider principal.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub users: Vec<String>,
    /// `secrets`, or `deployments.apps` to name the API group too.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<String>,
    pub reason: String,
    /// As written in the settings; the exception covers actions before this time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    #[serde(skip)]
    pub until_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    pub id: &'static str,
    pub enabled: bool,
    pub severity: &'static str,
    pub default_severity: &'static str,
    pub except: Vec<Exception>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RuleSet {
    pub rules: Vec<Rule>,
}

impl Default for RuleSet {
    fn default() -> Self {
        RuleSet {
            rules: RULES
                .iter()
                .map(|(id, sev)| Rule { id, enabled: true, severity: sev, default_severity: sev, except: vec![] })
                .collect(),
        }
    }
}

fn severity_of(s: &str) -> Option<&'static str> {
    SEVERITIES.iter().copied().find(|x| *x == s)
}

/// `2026-12-31` (to the end of that day, in the configured time zone) or a full RFC 3339 time.
fn parse_until(s: &str, tz: &chrono_tz::Tz) -> Option<i64> {
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.timestamp_millis());
    }
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()?.succ_opt()?;
    let t = tz.from_local_datetime(&d.and_hms_opt(0, 0, 0)?).earliest()?;
    Some(t.timestamp_millis())
}

impl RuleSet {
    /// Settings as JSON: `{"pod_exec": {"severity": "high", "except": [...]}, ...}`.
    /// Anything wrong is an error naming where, so a typo cannot silently turn a rule off.
    pub fn parse(json: &str, tz: &chrono_tz::Tz) -> anyhow::Result<RuleSet> {
        let mut set = RuleSet::default();
        if json.trim().is_empty() {
            return Ok(set);
        }
        let input: BTreeMap<String, RuleInput> = serde_json::from_str(json).map_err(|e| anyhow::anyhow!("{e}"))?;
        for (id, ri) in input {
            let Some(rule) = set.rules.iter_mut().find(|r| r.id == id) else {
                let known: Vec<&str> = RULES.iter().map(|(id, _)| *id).collect();
                anyhow::bail!("unknown rule {id:?}; rules are {}", known.join(", "));
            };
            if let Some(e) = ri.enabled {
                rule.enabled = e;
            }
            if let Some(s) = ri.severity {
                rule.severity = severity_of(&s).ok_or_else(|| anyhow::anyhow!("{id}.severity: {s:?} is not high, medium or low"))?;
            }
            for (n, x) in ri.except.into_iter().enumerate() {
                let at = format!("{id}.except[{n}]");
                if x.reason.trim().is_empty() {
                    anyhow::bail!("{at}: reason is required");
                }
                if x.clusters.is_empty() && x.namespaces.is_empty() && x.namespace_labels.is_empty() && x.users.is_empty() && x.resources.is_empty() {
                    anyhow::bail!("{at}: names no scope (clusters, namespaces, namespaceLabels, users or resources); to turn the rule off use enabled: false");
                }
                let until_ms = match &x.until {
                    Some(u) => Some(parse_until(u, tz).ok_or_else(|| anyhow::anyhow!("{at}.until: {u:?} is not a date (2026-12-31) or an RFC 3339 time"))?),
                    None => None,
                };
                rule.except.push(Exception {
                    clusters: x.clusters, namespaces: x.namespaces, namespace_labels: x.namespace_labels, users: x.users,
                    resources: x.resources, reason: x.reason.trim().to_string(), until: x.until, until_ms,
                });
            }
        }
        Ok(set)
    }

    pub fn get(&self, id: &str) -> Option<&Rule> {
        self.rules.iter().find(|r| r.id == id)
    }

    fn needs_labels(&self) -> bool {
        self.rules.iter().any(|r| r.except.iter().any(|x| !x.namespace_labels.is_empty()))
    }
}

/// `*` matches any run of characters; everything else literally.
fn glob(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !text.starts_with(first) || text.len() < first.len() + last.len() || !text.ends_with(last) {
        return false;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    true
}

/// What an exception is matched against.
pub struct Subject<'a> {
    pub cluster: &'a str,
    pub namespace: &'a str,
    pub names: [&'a str; 3],
    pub resource: &'a str,
    pub api_group: &'a str,
    pub ts_ms: i64,
    pub labels: Option<&'a Map<String, Value>>,
}

impl Exception {
    pub fn matches(&self, s: &Subject) -> bool {
        if self.until_ms.is_some_and(|u| s.ts_ms >= u) {
            return false;
        }
        let any = |pats: &[String], text: &str| pats.iter().any(|p| glob(p, text));
        if !self.clusters.is_empty() && !any(&self.clusters, s.cluster) {
            return false;
        }
        // Cluster-scoped objects are in no namespace, so a namespace scope never covers them.
        if !self.namespaces.is_empty() && (s.namespace.is_empty() || !any(&self.namespaces, s.namespace)) {
            return false;
        }
        if !self.namespace_labels.is_empty() {
            let Some(labels) = s.labels else { return false };
            if !self.namespace_labels.iter().all(|(k, v)| labels.get(k).and_then(Value::as_str) == Some(v.as_str())) {
                return false;
            }
        }
        if !self.users.is_empty() && !s.names.iter().any(|n| !n.is_empty() && any(&self.users, n)) {
            return false;
        }
        if !self.resources.is_empty() {
            let full = if s.api_group.is_empty() { s.resource.to_string() } else { format!("{}.{}", s.resource, s.api_group) };
            if !any(&self.resources, s.resource) && !any(&self.resources, &full) {
                return false;
            }
        }
        true
    }
}

// ---------------------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------------------

/// What rules need from stored data, as it was at a given time.
pub trait Lookup {
    fn namespace_labels(&self, cluster: &str, ns: &str, at_ms: i64) -> anyhow::Result<Option<Map<String, Value>>>;
    /// The Fleet source of an object, if it is deployed by Fleet.
    fn fleet_source(&self, cluster: &str, group: &str, resource: &str, ns: &str, name: &str, at_ms: i64) -> anyhow::Result<Option<String>>;
}

/// No stored data: rules that need it do not fire.
#[cfg(test)]
pub struct NoLookup;

#[cfg(test)]
impl Lookup for NoLookup {
    fn namespace_labels(&self, _: &str, _: &str, _: i64) -> anyhow::Result<Option<Map<String, Value>>> {
        Ok(None)
    }
    fn fleet_source(&self, _: &str, _: &str, _: &str, _: &str, _: &str, _: i64) -> anyhow::Result<Option<String>> {
        Ok(None)
    }
}

/// The namespace an action is in; a namespace itself counts as being in itself.
fn namespace_of(r: &Record) -> &str {
    if r.namespace.is_empty() && r.resource == "namespaces" && r.api_group.is_empty() { &r.name } else { &r.namespace }
}

/// Which rules a record trips, before the settings are applied.
fn detect(r: &Record, cluster: &str, cfg: &Config, look: &dyn Lookup) -> anyhow::Result<Vec<&'static str>> {
    let mut out = Vec::new();
    if !r.human {
        return Ok(out);
    }
    // 101 is the protocol switch for exec/attach/port-forward sessions.
    let ok = (200..300).contains(&r.code) || r.code == 101;
    let write = is_write(&r.verb);
    let ts_ms = r.ts.timestamp_millis();

    if r.code == 403 {
        out.push("forbidden");
    }
    if r.verb == "login" && r.code == 401 {
        out.push("login_failed");
    }
    if !ok {
        return Ok(out);
    }
    if r.resource == "namespaces" && r.verb == "delete" {
        out.push("namespace_deleted");
    }
    if write && RBAC.contains(&r.resource.as_str()) {
        out.push("rbac_changed");
    }
    if r.api_group == "management.cattle.io" && r.resource == "users" && r.verb == "delete" {
        out.push("user_deleted");
    }
    if r.verb == "generatekubeconfig" || (r.api_group == "management.cattle.io" && r.resource == "tokens" && r.verb == "create") {
        out.push("credential_issued");
    }
    if r.resource == "pods" && matches!(r.subresource.as_str(), "exec" | "attach" | "portforward") {
        out.push("pod_exec");
    }
    if r.resource == "secrets" && matches!(r.verb.as_str(), "get" | "list") {
        out.push("secret_read");
    }
    if r.via == Via::Direct && (write || matches!(r.subresource.as_str(), "exec" | "attach" | "portforward")) {
        out.push("bypassed_rancher");
    }
    let changed = write && (200..300).contains(&r.code);
    // A person writing to an object Fleet deploys: the change is not in Git.
    if changed && !r.name.is_empty() && look.fleet_source(cluster, &r.api_group, &r.resource, &r.namespace, &r.name, ts_ms)?.is_some() {
        out.push("gitops_bypassed");
    }
    if cfg.change_windows.is_empty() {
        // Without change windows, flag changes outside working hours.
        if write {
            let local = r.ts.with_timezone(&cfg.tz);
            let day = local.weekday().number_from_monday();
            let hour = local.hour();
            if !(cfg.work_days.contains(&day) && hour >= cfg.work_start_hour && hour < cfg.work_end_hour) {
                out.push("off_hours");
            }
        }
    } else if changed {
        let ns = namespace_of(r);
        let labels = if ns.is_empty() { None } else { look.namespace_labels(cluster, ns, ts_ms)? };
        if crate::windows::allowed(&cfg.change_windows, cluster, labels.as_ref(), r.ts, &cfg.tz) == Some(false) {
            out.push("outside_change_window");
        }
    }
    Ok(out)
}

/// The alerts for one record under the current settings. `cluster` is the cluster the record
/// is stored under.
pub fn judge(r: &Record, cluster: &str, cfg: &Config, look: &dyn Lookup) -> anyhow::Result<Vec<Alert>> {
    let hits = detect(r, cluster, cfg, look)?;
    if hits.is_empty() {
        return Ok(vec![]);
    }
    let ts_ms = r.ts.timestamp_millis();
    let ns = namespace_of(r);
    let labels = if cfg.rules.needs_labels() && !ns.is_empty() { look.namespace_labels(cluster, ns, ts_ms)? } else { None };
    let subject = Subject {
        cluster, namespace: ns, names: [&r.actor, &r.user_id, &r.principal], resource: &r.resource, api_group: &r.api_group,
        ts_ms, labels: labels.as_ref(),
    };
    Ok(hits
        .into_iter()
        .filter_map(|id| {
            let rule = cfg.rules.get(id)?;
            (rule.enabled && !rule.except.iter().any(|x| x.matches(&subject))).then_some(Alert { rule: rule.id, severity: rule.severity })
        })
        .collect())
}

/// Judge a record without stored data (rules that need it do not fire).
#[cfg(test)]
pub fn evaluate(r: &Record, cfg: &Config) -> Vec<Alert> {
    let cluster = r.cluster.clone().unwrap_or_default();
    judge(r, &cluster, cfg, &NoLookup).unwrap_or_default()
}

/// When the settings that decide alerts were last changed is told apart by this.
pub fn fingerprint(cfg: &Config) -> (String, Value) {
    use sha2::{Digest, Sha256};
    let v = serde_json::json!({
        "rules": cfg.rules.rules.iter().map(|r| serde_json::json!({
            "id": r.id, "enabled": r.enabled, "severity": r.severity, "except": r.except,
        })).collect::<Vec<_>>(),
        "changeWindows": cfg.change_windows,
        "tz": cfg.tz.name(),
        "workDays": cfg.work_days,
        "workHours": [cfg.work_start_hour, cfg.work_end_hour],
    });
    let h: String = Sha256::digest(v.to_string().as_bytes()).iter().take(8).map(|b| format!("{b:02x}")).collect();
    (h, v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::{parse_body, to_record};

    fn rec(json: &str) -> Record {
        to_record(&parse_body(json.as_bytes()).unwrap()[0]).unwrap()
    }

    #[test]
    fn namespace_delete_at_night_directly() {
        // 15:00 UTC = 23:00 Taipei on a Wednesday
        let r = rec(r#"{"auditID":"x","stage":"ResponseComplete","verb":"delete","user":{"username":"system:admin"},
            "objectRef":{"resource":"namespaces","name":"shop"},"responseStatus":{"code":200},
            "stageTimestamp":"2026-10-14T15:00:00Z"}"#);
        let rules: Vec<_> = evaluate(&r, &Config::test()).into_iter().map(|a| a.rule).collect();
        assert_eq!(rules, vec!["namespace_deleted", "bypassed_rancher", "off_hours"]);
    }

    #[test]
    fn daytime_patch_via_rancher_is_quiet() {
        let r = rec(r#"{"auditID":"y","stage":"ResponseComplete","verb":"patch",
            "user":{"username":"system:serviceaccount:cattle-impersonation-system:cattle-impersonation-u-1"},
            "impersonatedUser":{"username":"u-1","extra":{"username":["alice"]}},
            "objectRef":{"resource":"deployments","namespace":"shop","name":"web","apiGroup":"apps"},
            "responseStatus":{"code":200},"stageTimestamp":"2026-10-14T02:00:00Z"}"#);
        assert!(evaluate(&r, &Config::test()).is_empty());
    }

    #[test]
    fn rancher_management_actions() {
        use crate::rancher::{parse_fixture, to_record as rancher};
        let line = |uri: &str, method: &str, code: i64| format!(
            r#"{{"auditID":"m","requestURI":"{uri}","method":"{method}","user":{{"name":"u-1","extra":{{"username":["alice"]}}}},
               "responseCode":{code},"responseTimestamp":"2026-10-14T10:00:00+08:00"}}"#);
        let rules = |l: String| -> Vec<&str> {
            evaluate(&rancher(&parse_fixture(&l)[0]).unwrap(), &Config::test()).into_iter().map(|a| a.rule).collect()
        };
        assert_eq!(rules(line("/v3/users/u-9", "DELETE", 204)), vec!["user_deleted"]);
        assert_eq!(rules(line("/v3/clusterroletemplatebindings", "POST", 201)), vec!["rbac_changed"]);
        assert_eq!(rules(line("/v3/clusters/c-1?action=generateKubeconfig", "POST", 200)), vec!["credential_issued"]);
        assert_eq!(rules(line("/v3-public/localProviders/local?action=login", "POST", 401)), vec!["login_failed"]);
    }

    #[test]
    fn controllers_are_ignored() {
        let r = rec(r#"{"auditID":"z","stage":"ResponseComplete","verb":"delete",
            "user":{"username":"system:serviceaccount:kube-system:namespace-controller"},
            "objectRef":{"resource":"namespaces","name":"shop"},"responseStatus":{"code":200},
            "stageTimestamp":"2026-10-14T15:00:00Z"}"#);
        assert!(evaluate(&r, &Config::test()).is_empty());
    }

    fn exec_in(ns: &str, user: &str, ts: &str) -> Record {
        rec(&format!(r#"{{"auditID":"e","stage":"ResponseStarted","verb":"create","user":{{"username":"{user}"}},
            "objectRef":{{"resource":"pods","subresource":"exec","namespace":"{ns}","name":"web-1"}},"responseStatus":{{"code":101}},
            "stageTimestamp":"{ts}"}}"#))
    }

    #[test]
    fn settings_change_severity_disable_and_exempt() {
        let mut cfg = Config::test();
        cfg.rules = RuleSet::parse(r#"{
            "pod_exec": {"severity": "high", "except": [
                {"namespaces": ["dev-*"], "reason": "developers debug here"},
                {"users": ["oncall-*"], "clusters": ["c-m-1"], "reason": "on-call", "until": "2026-10-14"}
            ]},
            "bypassed_rancher": {"enabled": false},
            "off_hours": {"enabled": false}
        }"#, &cfg.tz).unwrap();
        let mut r = exec_in("shop", "alice", "2026-10-14T02:00:00Z");
        r.cluster = Some("c-m-1".into());
        assert_eq!(evaluate(&r, &cfg), vec![Alert { rule: "pod_exec", severity: "high" }]);
        let mut d = exec_in("dev-alice", "alice", "2026-10-14T02:00:00Z");
        d.cluster = Some("c-m-1".into());
        assert!(evaluate(&d, &cfg).is_empty(), "dev namespaces are exempt");
        // The on-call exception covers 14 Oct (Taipei) and not after.
        let mut o = exec_in("shop", "oncall-bob", "2026-10-14T15:59:00Z");
        o.cluster = Some("c-m-1".into());
        assert!(evaluate(&o, &cfg).is_empty());
        o.ts = "2026-10-14T16:00:00Z".parse().unwrap();
        assert_eq!(evaluate(&o, &cfg).len(), 1, "exception expired at midnight Taipei");
        o.ts = "2026-10-14T02:00:00Z".parse().unwrap();
        o.cluster = Some("c-m-2".into());
        assert_eq!(evaluate(&o, &cfg).len(), 1, "other clusters are not covered");
    }

    #[test]
    fn bad_settings_say_where() {
        let tz = chrono_tz::Asia::Taipei;
        let err = |s: &str| RuleSet::parse(s, &tz).unwrap_err().to_string();
        assert!(err(r#"{"pod_exek": {}}"#).contains("unknown rule \"pod_exek\""));
        assert!(err(r#"{"pod_exec": {"severity": "critical"}}"#).contains("pod_exec.severity"));
        assert!(err(r#"{"pod_exec": {"except": [{"namespaces": ["a"]}]}}"#).contains("pod_exec.except[0]: reason is required"));
        assert!(err(r#"{"pod_exec": {"except": [{"reason": "x"}]}}"#).contains("names no scope"));
        assert!(err(r#"{"pod_exec": {"except": [{"namespaces": ["a"], "reason": "x", "until": "next week"}]}}"#).contains("until"));
        assert!(err(r#"{"pod_exec": {"enable": false}}"#).contains("unknown field"));
        assert_eq!(RuleSet::parse("", &tz).unwrap(), RuleSet::default());
    }

    #[test]
    fn globs() {
        assert!(glob("dev-*", "dev-a") && glob("*", "") && glob("a*b*c", "aXbYc") && glob("*-prod", "shop-prod"));
        assert!(!glob("dev-*", "prod") && !glob("a*b", "ab-") && !glob("ab*ba", "aba"));
    }

    #[test]
    fn resources_match_with_or_without_group() {
        let x = Exception { clusters: vec![], namespaces: vec![], namespace_labels: BTreeMap::new(), users: vec![],
            resources: vec!["deployments.apps".into()], reason: "r".into(), until: None, until_ms: None };
        let s = |res: &'static str, grp: &'static str| Subject { cluster: "c", namespace: "n", names: ["a", "", ""], resource: res, api_group: grp, ts_ms: 0, labels: None };
        assert!(x.matches(&s("deployments", "apps")));
        assert!(!x.matches(&s("deployments", "extensions")));
    }
}
