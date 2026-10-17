//! Parse Kubernetes audit events (audit.k8s.io/v1) and work out who really acted.
//!
//! Requests that come through the Rancher proxy reach the downstream cluster as a
//! `cattle-impersonation-system` service account impersonating the Rancher user.
//! The Rancher user and their identity-provider name are in `impersonatedUser`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct UserInfo {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub extra: HashMap<String, Vec<String>>,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ObjectRef {
    #[serde(default)]
    pub resource: String,
    #[serde(default)]
    pub namespace: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub api_group: String,
    #[serde(default)]
    pub subresource: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct Status {
    #[serde(default)]
    pub code: i64,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    #[serde(rename = "auditID")]
    pub audit_id: String,
    #[serde(default)]
    pub stage: String,
    #[serde(default)]
    pub verb: String,
    #[serde(default)]
    pub request_uri: String,
    #[serde(default)]
    pub user: UserInfo,
    pub impersonated_user: Option<UserInfo>,
    #[serde(default, rename = "sourceIPs")]
    pub source_ips: Vec<String>,
    #[serde(default)]
    pub user_agent: String,
    pub object_ref: Option<ObjectRef>,
    pub response_status: Option<Status>,
    pub request_object: Option<Value>,
    pub request_received_timestamp: Option<DateTime<Utc>>,
    pub stage_timestamp: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct EventList {
    #[serde(default)]
    pub items: Vec<Event>,
}

/// How the request reached the cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    /// Through the Rancher proxy (Rancher UI, or kubectl with a Rancher kubeconfig).
    Rancher,
    /// Some other impersonation.
    Impersonation,
    /// Rancher user authenticated directly at the cluster (Authorized Cluster Endpoint).
    Ace,
    /// Straight to the API server: authorized cluster endpoint, node kubeconfig, service account.
    Direct,
}

impl Via {
    pub fn as_str(&self) -> &'static str {
        match self {
            Via::Rancher => "rancher",
            Via::Impersonation => "impersonation",
            Via::Ace => "ace",
            Via::Direct => "direct",
        }
    }
}

/// One audit event reduced to what the UI needs.
#[derive(Debug, Clone, Serialize)]
pub struct Record {
    pub audit_id: String,
    pub ts: DateTime<Utc>,
    /// The person (or system component) the action is attributed to.
    pub actor: String,
    /// Identity-provider principal, when Rancher passes one along.
    pub principal: String,
    pub human: bool,
    pub via: Via,
    /// The account that actually called the API server.
    pub caller: String,
    pub verb: String,
    pub api_group: String,
    pub resource: String,
    pub subresource: String,
    pub namespace: String,
    pub name: String,
    pub code: i64,
    pub source_ip: String,
    pub user_agent: String,
    pub request_uri: String,
    /// Request body when the audit level is Request or higher (the patch or YAML sent).
    pub request_object: Option<Value>,
}

/// Rancher's auth proxy impersonates through per-user service accounts in this namespace.
const RANCHER_IMPERSONATOR: &str = "system:serviceaccount:cattle-impersonation-system:";
/// Requests the Rancher UI makes through Steve may impersonate via the cluster agent instead.
const RANCHER_AGENT: &str = "system:serviceaccount:cattle-system:cattle";

/// Rancher user IDs look like `u-xxxxx` (local and external users) or `user-xxxxx` (admin).
fn is_rancher_user_id(name: &str) -> bool {
    name.starts_with("u-") || name.starts_with("user-")
}

fn first(extra: &HashMap<String, Vec<String>>, key: &str) -> Option<String> {
    extra.get(key).and_then(|v| v.first()).cloned().filter(|s| !s.is_empty())
}

/// `system:*` identities are components, except `system:admin`, which is the
/// admin kubeconfig on an RKE2 server node and is always used by a person.
pub fn is_human(name: &str) -> bool {
    !name.starts_with("system:") || name == "system:admin"
}

pub fn to_record(ev: &Event) -> Option<Record> {
    let o = ev.object_ref.clone().unwrap_or_default();
    // exec/attach/port-forward are long-running: ResponseComplete only arrives when the
    // session ends, so take ResponseStarted for those and stamp them with the request time.
    // Both stages share an auditID; the store keeps whichever arrives first.
    let streaming = matches!(o.subresource.as_str(), "exec" | "attach" | "portforward");
    let ts = match ev.stage.as_str() {
        "ResponseStarted" if streaming => ev.request_received_timestamp.or(ev.stage_timestamp)?,
        "ResponseComplete" | "Panic" if streaming => ev.request_received_timestamp.or(ev.stage_timestamp)?,
        "ResponseComplete" | "Panic" => ev.stage_timestamp.or(ev.request_received_timestamp)?,
        _ => return None,
    };
    if ev.verb == "watch" {
        return None;
    }
    let (actor, principal, via) = match &ev.impersonated_user {
        Some(imp) => {
            let actor = first(&imp.extra, "username").unwrap_or_else(|| imp.username.clone());
            let principal = first(&imp.extra, "principalid").unwrap_or_else(|| imp.username.clone());
            let rancher_user = is_rancher_user_id(&imp.username) || imp.extra.contains_key("principalid");
            let via = if ev.user.username.starts_with(RANCHER_IMPERSONATOR)
                || (ev.user.username == RANCHER_AGENT && rancher_user)
            {
                Via::Rancher
            } else {
                Via::Impersonation
            };
            (actor, principal, via)
        }
        None if is_rancher_user_id(&ev.user.username) => (ev.user.username.clone(), String::new(), Via::Ace),
        None => (ev.user.username.clone(), String::new(), Via::Direct),
    };
    let human = match ev.impersonated_user {
        Some(ref imp) => is_human(&imp.username),
        None => is_human(&ev.user.username),
    };
    // For /api/v1/namespaces/<n> the API server sets objectRef.namespace to <n> as well.
    // Namespaces are cluster-scoped, so clear it to match how object versions are keyed.
    let namespace = if o.resource == "namespaces" && o.api_group.is_empty() { String::new() } else { o.namespace };
    Some(Record {
        audit_id: ev.audit_id.clone(),
        ts,
        actor,
        principal,
        human,
        via,
        caller: ev.user.username.clone(),
        verb: ev.verb.clone(),
        api_group: o.api_group,
        resource: o.resource,
        subresource: o.subresource,
        namespace,
        name: o.name,
        code: ev.response_status.as_ref().map(|s| s.code).unwrap_or(0),
        source_ip: ev.source_ips.first().cloned().unwrap_or_default(),
        user_agent: ev.user_agent.clone(),
        request_uri: ev.request_uri.clone(),
        request_object: ev.request_object.clone(),
    })
}

pub fn is_write(verb: &str) -> bool {
    matches!(verb, "create" | "update" | "patch" | "delete" | "deletecollection")
}

/// Accept either an EventList (what the webhook backend sends) or a single Event
/// or newline-delimited events (the log file format), so replaying a log file works too.
pub fn parse_body(body: &[u8]) -> anyhow::Result<Vec<Event>> {
    if let Ok(list) = serde_json::from_slice::<EventList>(body) {
        // An EventList, possibly empty. A single Event also parses as an EventList with
        // no items, so only trust it when the body says it is a list.
        if !list.items.is_empty() || body.windows(7).any(|w| w == b"\"items\"") {
            return Ok(list.items);
        }
    }
    if let Ok(ev) = serde_json::from_slice::<Event>(body) {
        return Ok(vec![ev]);
    }
    let mut out = Vec::new();
    for line in body.split(|b| *b == b'\n') {
        let line = line.trim_ascii();
        if line.is_empty() {
            continue;
        }
        out.push(serde_json::from_slice::<Event>(line)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIA_RANCHER: &str = r#"{"kind":"Event","apiVersion":"audit.k8s.io/v1","level":"Request","auditID":"a1","stage":"ResponseComplete",
      "requestURI":"/apis/apps/v1/namespaces/shop/deployments/web","verb":"patch",
      "user":{"username":"system:serviceaccount:cattle-impersonation-system:cattle-impersonation-u-abc12","groups":["system:serviceaccounts"]},
      "impersonatedUser":{"username":"u-abc12","groups":["system:authenticated","system:cattle:authenticated"],
         "extra":{"principalid":["openldap_user://uid=alice,ou=people"],"username":["alice"],"requesttokenid":["token-x"]}},
      "sourceIPs":["10.42.0.5"],"userAgent":"Mozilla/5.0",
      "objectRef":{"resource":"deployments","namespace":"shop","name":"web","apiGroup":"apps","apiVersion":"v1"},
      "responseStatus":{"metadata":{},"code":200},
      "requestObject":{"spec":{"replicas":5}},
      "requestReceivedTimestamp":"2026-10-14T02:00:00.000000Z","stageTimestamp":"2026-10-14T02:00:00.030000Z"}"#;

    #[test]
    fn rancher_proxy_is_attributed_to_the_person() {
        let evs = parse_body(VIA_RANCHER.as_bytes()).unwrap();
        let r = to_record(&evs[0]).unwrap();
        assert_eq!(r.actor, "alice");
        assert_eq!(r.principal, "openldap_user://uid=alice,ou=people");
        assert_eq!(r.via, Via::Rancher);
        assert!(r.human);
        assert_eq!(r.code, 200);
        assert_eq!(r.request_object.unwrap()["spec"]["replicas"], 5);
    }

    #[test]
    fn direct_and_system_callers() {
        let direct = r#"{"auditID":"a2","stage":"ResponseComplete","verb":"delete",
          "user":{"username":"system:admin"},"objectRef":{"resource":"namespaces","name":"old"},
          "responseStatus":{"code":200},"stageTimestamp":"2026-10-14T15:00:00Z"}"#;
        let r = to_record(&parse_body(direct.as_bytes()).unwrap()[0]).unwrap();
        assert_eq!(r.via, Via::Direct);
        assert!(r.human);
        let sys = r#"{"auditID":"a3","stage":"ResponseComplete","verb":"update",
          "user":{"username":"system:serviceaccount:kube-system:deployment-controller"},
          "objectRef":{"resource":"deployments","namespace":"shop","name":"web","apiGroup":"apps","subresource":"status"},
          "responseStatus":{"code":200},"stageTimestamp":"2026-10-14T15:00:00Z"}"#;
        let r = to_record(&parse_body(sys.as_bytes()).unwrap()[0]).unwrap();
        assert!(!r.human);
    }

    #[test]
    fn namespace_events_are_cluster_scoped() {
        let ev = r#"{"auditID":"n1","stage":"ResponseComplete","verb":"delete","user":{"username":"system:admin"},
          "objectRef":{"resource":"namespaces","namespace":"old","name":"old","apiVersion":"v1"},
          "responseStatus":{"code":200},"stageTimestamp":"2026-10-14T15:00:00Z"}"#;
        let r = to_record(&parse_body(ev.as_bytes()).unwrap()[0]).unwrap();
        assert_eq!(r.namespace, "");
        assert_eq!(r.name, "old");
    }

    #[test]
    fn steve_agent_impersonation_and_ace() {
        let steve = r#"{"auditID":"s1","stage":"ResponseComplete","verb":"update","user":{"username":"system:serviceaccount:cattle-system:cattle"},
          "impersonatedUser":{"username":"u-abc12","extra":{"username":["alice"]}},
          "objectRef":{"resource":"deployments","namespace":"shop","name":"web","apiGroup":"apps"},
          "responseStatus":{"code":200},"stageTimestamp":"2026-10-14T02:00:00Z"}"#;
        let r = to_record(&parse_body(steve.as_bytes()).unwrap()[0]).unwrap();
        assert_eq!((r.via, r.actor.as_str()), (Via::Rancher, "alice"));
        let ace = r#"{"auditID":"s2","stage":"ResponseComplete","verb":"patch","user":{"username":"u-abc12"},
          "objectRef":{"resource":"deployments","namespace":"shop","name":"web","apiGroup":"apps"},
          "responseStatus":{"code":200},"stageTimestamp":"2026-10-14T02:00:00Z"}"#;
        let r = to_record(&parse_body(ace.as_bytes()).unwrap()[0]).unwrap();
        assert_eq!(r.via, Via::Ace);
    }

    #[test]
    fn exec_uses_response_started_and_request_time() {
        let ev = r#"{"auditID":"e1","stage":"ResponseStarted","verb":"create","user":{"username":"system:admin"},
          "objectRef":{"resource":"pods","namespace":"shop","name":"web-1","subresource":"exec"},
          "responseStatus":{"code":101},"requestReceivedTimestamp":"2026-10-14T02:00:00Z","stageTimestamp":"2026-10-14T02:00:01Z"}"#;
        let r = to_record(&parse_body(ev.as_bytes()).unwrap()[0]).unwrap();
        assert_eq!(r.ts.to_rfc3339(), "2026-10-14T02:00:00+00:00");
    }

    #[test]
    fn skips_non_final_stage_and_parses_lists() {
        let list = format!(r#"{{"kind":"EventList","items":[{},{}]}}"#,
            VIA_RANCHER, VIA_RANCHER.replace("ResponseComplete", "RequestReceived"));
        let evs = parse_body(list.as_bytes()).unwrap();
        assert_eq!(evs.len(), 2);
        assert_eq!(evs.iter().filter_map(to_record).count(), 1);
        assert!(parse_body(br#"{"kind":"EventList","items":[]}"#).unwrap().is_empty());
    }
}
