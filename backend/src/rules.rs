//! Rules that flag audit records worth a second look. Only people are checked;
//! controllers and other `system:*` callers are expected to change things all day.

use crate::audit::{is_write, Record, Via};
use crate::config::Config;
use chrono::{Datelike, Timelike};
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Alert {
    pub rule: &'static str,
    /// "high", "medium" or "low".
    pub severity: &'static str,
}

/// Kubernetes RBAC plus Rancher's own role and binding types (management.cattle.io).
const RBAC: &[&str] = &[
    "roles", "rolebindings", "clusterroles", "clusterrolebindings",
    "globalroles", "globalrolebindings", "roletemplates",
    "clusterroletemplatebindings", "projectroletemplatebindings",
];

pub fn evaluate(r: &Record, cfg: &Config) -> Vec<Alert> {
    let mut out = Vec::new();
    if !r.human {
        return out;
    }
    // 101 is the protocol switch for exec/attach/port-forward sessions.
    let ok = (200..300).contains(&r.code) || r.code == 101;
    let write = is_write(&r.verb);

    if r.code == 403 {
        out.push(Alert { rule: "forbidden", severity: "low" });
    }
    if r.verb == "login" && r.code == 401 {
        out.push(Alert { rule: "login_failed", severity: "low" });
    }
    if !ok {
        return out;
    }
    if r.resource == "namespaces" && r.verb == "delete" {
        out.push(Alert { rule: "namespace_deleted", severity: "high" });
    }
    if write && RBAC.contains(&r.resource.as_str()) {
        out.push(Alert { rule: "rbac_changed", severity: "medium" });
    }
    if r.api_group == "management.cattle.io" && r.resource == "users" && r.verb == "delete" {
        out.push(Alert { rule: "user_deleted", severity: "medium" });
    }
    if r.verb == "generatekubeconfig" || (r.api_group == "management.cattle.io" && r.resource == "tokens" && r.verb == "create") {
        out.push(Alert { rule: "credential_issued", severity: "low" });
    }
    if r.resource == "pods" && matches!(r.subresource.as_str(), "exec" | "attach" | "portforward") {
        out.push(Alert { rule: "pod_exec", severity: "medium" });
    }
    if r.resource == "secrets" && matches!(r.verb.as_str(), "get" | "list") {
        out.push(Alert { rule: "secret_read", severity: "low" });
    }
    if r.via == Via::Direct && (write || matches!(r.subresource.as_str(), "exec" | "attach" | "portforward")) {
        out.push(Alert { rule: "bypassed_rancher", severity: "medium" });
    }
    if write {
        let local = r.ts.with_timezone(&cfg.tz);
        let day = local.weekday().number_from_monday();
        let hour = local.hour();
        let in_hours = cfg.work_days.contains(&day) && hour >= cfg.work_start_hour && hour < cfg.work_end_hour;
        if !in_hours {
            out.push(Alert { rule: "off_hours", severity: "low" });
        }
    }
    out
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
}
