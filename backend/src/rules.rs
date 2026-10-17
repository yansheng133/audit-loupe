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

const RBAC: &[&str] = &["roles", "rolebindings", "clusterroles", "clusterrolebindings"];

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
    if !ok {
        return out;
    }
    if r.resource == "namespaces" && r.verb == "delete" {
        out.push(Alert { rule: "namespace_deleted", severity: "high" });
    }
    if write && RBAC.contains(&r.resource.as_str()) {
        out.push(Alert { rule: "rbac_changed", severity: "medium" });
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
    fn controllers_are_ignored() {
        let r = rec(r#"{"auditID":"z","stage":"ResponseComplete","verb":"delete",
            "user":{"username":"system:serviceaccount:kube-system:namespace-controller"},
            "objectRef":{"resource":"namespaces","name":"shop"},"responseStatus":{"code":200},
            "stageTimestamp":"2026-10-14T15:00:00Z"}"#);
        assert!(evaluate(&r, &Config::test()).is_empty());
    }
}
