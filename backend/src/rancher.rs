//! Parse the Rancher API audit log and reduce it to the same [`Record`] as kube-apiserver events.
//!
//! Each line is one JSON entry written by Rancher's audit middleware. Unlike the Kubernetes
//! format there is no verb or object reference: both are worked out from the HTTP method and
//! the request URI, which can be
//!
//! - `/k8s/clusters/<id>/api/...` or `/apis/...`: the Kubernetes API of a cluster, proxied by
//!   Rancher (Rancher UI, or kubectl with a Rancher kubeconfig). The same action also appears
//!   in that cluster's own audit log, impersonated, and the two are matched in the store.
//! - `/k8s/clusters/<id>/v1/<type>/...` or `/v1/<type>/...`: Steve, the API the Rancher UI uses.
//! - `/v3/<type>/<id>?action=<x>`: Norman, Rancher's management API (users, bindings, tokens).
//! - `/v3-public/<provider>?action=login`: logins.
//!
//! Timestamps have one-second precision. Requests whose connection is hijacked (exec, attach,
//! port-forward, the UI shell) are not logged by Rancher at all.

use crate::audit::{is_human, is_rancher_user_id, is_write, Record, Source, Via};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub principal_id: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Impersonator {
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct User {
    /// Rancher user ID, or a system identity such as `system:unauthenticated`.
    #[serde(default)]
    pub name: String,
    /// Newer Rancher releases (audit-log-username feature) add who the person is.
    pub identity: Option<Identity>,
    #[serde(default)]
    pub extra: HashMap<String, Vec<String>>,
    pub impersonated_by: Option<Impersonator>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    #[serde(rename = "auditID", default)]
    pub audit_id: String,
    #[serde(default, rename = "requestURI")]
    pub request_uri: String,
    pub user: Option<User>,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub remote_addr: String,
    #[serde(default)]
    pub response_code: i64,
    /// Set on local logins, from the login form.
    #[serde(default)]
    pub user_login_name: String,
    #[serde(default)]
    pub request_timestamp: String,
    #[serde(default)]
    pub response_timestamp: String,
    #[serde(default)]
    pub request_header: HashMap<String, Vec<String>>,
    pub request_body: Option<Value>,
}

/// What a request URI points at.
#[derive(Debug, Default, PartialEq, Clone)]
pub struct Target {
    /// Cluster ID from `/k8s/clusters/<id>/`; None means Rancher's own (local) cluster.
    pub cluster: Option<String>,
    pub verb: String,
    pub api_group: String,
    pub resource: String,
    pub subresource: String,
    pub namespace: String,
    pub name: String,
}

fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        (k == key).then_some(v)
    })
}

fn verb_for(method: &str, has_name: bool, query: &str) -> String {
    let watch = matches!(query_param(query, "watch"), Some("true" | "1"));
    match method {
        "GET" if watch => "watch",
        "GET" if has_name => "get",
        "GET" => "list",
        "POST" => "create",
        "PUT" => "update",
        "PATCH" => "patch",
        "DELETE" if has_name => "delete",
        "DELETE" => "deletecollection",
        _ => "",
    }
    .to_string()
}

/// `/api/v1/...` or `/apis/<group>/<version>/...` after the version.
fn kube_path(group: &str, rest: &[&str], method: &str, query: &str) -> Option<Target> {
    let mut t = Target { api_group: group.to_string(), ..Default::default() };
    let rest = match rest {
        // /namespaces/<ns> is the namespace object itself (cluster-scoped).
        ["namespaces", ns] if group.is_empty() => {
            t.resource = "namespaces".into();
            t.name = ns.to_string();
            &[][..]
        }
        ["namespaces", ns, tail @ ..] => {
            t.namespace = ns.to_string();
            tail
        }
        _ => rest,
    };
    match rest {
        [] if !t.resource.is_empty() => {}
        [res] => t.resource = res.to_string(),
        [res, name] => (t.resource, t.name) = (res.to_string(), name.to_string()),
        [res, name, sub, ..] => (t.resource, t.name, t.subresource) = (res.to_string(), name.to_string(), sub.to_string()),
        _ => return None,
    }
    t.verb = verb_for(method, !t.name.is_empty(), query);
    Some(t)
}

/// Steve: `/v1/<type>`, `/v1/<type>/<name>`, `/v1/<type>/<namespace>/<name>`.
/// The type is the schema ID: `pods`, `apps.deployments`, `management.cattle.io.clusters`.
/// Two segments are ambiguous (a namespace to list or create in, or a cluster-scoped name);
/// only writes matter here, and a create always names a namespace while update and delete
/// of two segments are cluster-scoped objects.
fn steve_path(segs: &[&str], method: &str, query: &str) -> Option<Target> {
    let (ty, rest) = segs.split_first()?;
    let (group, resource) = match ty.rsplit_once('.') {
        Some((g, r)) => (g.to_string(), r.to_string()),
        None => (String::new(), ty.to_string()),
    };
    let mut t = Target { api_group: group, resource, ..Default::default() };
    match rest {
        [] => {}
        [one] if method == "POST" || method == "GET" => t.namespace = one.to_string(),
        [one] => t.name = one.to_string(),
        [ns, name, ..] => (t.namespace, t.name) = (ns.to_string(), name.to_string()),
    }
    t.verb = verb_for(method, !t.name.is_empty(), query);
    Some(t)
}

/// Norman: `/v3/<type>` or `/v3/<type>/<id>`, with `?action=<name>` for actions.
fn norman_path(segs: &[&str], method: &str, query: &str) -> Option<Target> {
    let (ty, rest) = segs.split_first()?;
    let mut t = Target { api_group: "management.cattle.io".into(), resource: ty.to_ascii_lowercase(), ..Default::default() };
    if let Some(id) = rest.first() {
        t.name = id.to_string();
    }
    t.verb = match query_param(query, "action") {
        Some(a) if method == "POST" && !a.is_empty() => a.to_ascii_lowercase(),
        _ => verb_for(method, !t.name.is_empty(), query),
    };
    Some(t)
}

pub fn parse_target(uri: &str, method: &str) -> Option<Target> {
    let (path, query) = uri.split_once('?').unwrap_or((uri, ""));
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let (cluster, segs) = match segs.as_slice() {
        ["k8s", "clusters", id, rest @ ..] => (Some(id.to_string()), rest),
        _ => (None, &segs[..]),
    };
    let mut t = match segs {
        ["api", "v1", rest @ ..] => kube_path("", rest, method, query)?,
        ["apis", group, _version, rest @ ..] => kube_path(group, rest, method, query)?,
        ["v1", rest @ ..] => steve_path(rest, method, query)?,
        ["v3", rest @ ..] if cluster.is_none() => norman_path(rest, method, query)?,
        ["v3-public", .., provider] if cluster.is_none() && query_param(query, "action") == Some("login") => Target {
            verb: "login".into(),
            resource: "login".into(),
            name: provider.to_ascii_lowercase(),
            ..Default::default()
        },
        _ => return None,
    };
    // The local cluster is Rancher's own; keep one name for it.
    t.cluster = cluster.filter(|c| c != "local");
    Some(t)
}

/// The smallest non-empty value, as Rancher does, so the same user always gets the same name.
fn smallest(extra: &HashMap<String, Vec<String>>, key: &str) -> Option<String> {
    extra.get(key)?.iter().filter(|s| !s.is_empty()).min().cloned()
}

fn header<'a>(h: &'a HashMap<String, Vec<String>>, name: &str) -> Option<&'a str> {
    h.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).and_then(|(_, v)| v.first()).map(String::as_str)
}

fn client_ip(e: &Entry) -> String {
    // Behind an ingress, remoteAddr is the ingress; the first forwarded address is the client.
    if let Some(xff) = header(&e.request_header, "X-Forwarded-For")
        && let Some(first) = xff.split(',').map(str::trim).find(|s| !s.is_empty()) {
            return first.to_string();
        }
    match e.remote_addr.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host.trim_matches(['[', ']']).to_string(),
        _ => e.remote_addr.clone(),
    }
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|t| t.with_timezone(&Utc))
}

/// Only what the timeline shows is kept: changes, actions, logins and Secret reads.
/// Everything else (the UI polls constantly) would only cost space.
fn worth_keeping(t: &Target) -> bool {
    match t.verb.as_str() {
        "login" => true,
        "get" | "list" => t.resource == "secrets" && t.api_group.is_empty(),
        "watch" | "" => false,
        v if is_write(v) => true,
        // Norman actions (generatekubeconfig, setpassword, ...)
        _ => true,
    }
}

pub fn to_record(e: &Entry) -> Option<Record> {
    if e.audit_id.is_empty() {
        return None;
    }
    let t = parse_target(&e.request_uri, &e.method)?;
    if !worth_keeping(&t) {
        return None;
    }
    let ts = parse_ts(&e.response_timestamp).or_else(|| parse_ts(&e.request_timestamp))?;
    let user = e.user.clone().unwrap_or_default();
    let identity = user.identity.clone().unwrap_or_default();
    let login = t.verb == "login";
    let actor = [
        identity.username.clone(),
        smallest(&user.extra, "username").unwrap_or_default(),
        if login { e.user_login_name.clone() } else { String::new() },
        user.name.clone(),
    ]
    .into_iter()
    .find(|s| !s.is_empty())
    .unwrap_or_else(|| "system:unauthenticated".into());
    let principal = Some(identity.principal_id.clone())
        .filter(|s| !s.is_empty())
        .or_else(|| smallest(&user.extra, "principalid"))
        .unwrap_or_default();
    let user_id = if is_rancher_user_id(&user.name) { user.name.clone() } else { String::new() };
    let human = login || if user.name.is_empty() { false } else { is_human(&user.name) };
    let caller = user.impersonated_by.as_ref().map(|i| i.name.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| user.name.clone());
    Some(Record {
        audit_id: e.audit_id.clone(),
        source: Source::Rancher,
        cluster: t.cluster.clone(),
        ts,
        actor,
        principal,
        user_id,
        human,
        via: Via::Rancher,
        caller,
        verb: t.verb,
        api_group: t.api_group,
        resource: t.resource,
        subresource: t.subresource,
        namespace: t.namespace,
        name: t.name,
        code: e.response_code,
        source_ip: client_ip(e),
        user_agent: header(&e.request_header, "User-Agent").unwrap_or_default().to_string(),
        request_uri: e.request_uri.clone(),
        request_object: e.request_body.clone(),
    })
}

/// JSON lines. Lines that are not entries (the sidecar's `tail` notices, a torn line after a
/// restart) are skipped and counted rather than failing the whole batch.
pub fn parse_lines(body: &[u8]) -> (Vec<Entry>, usize) {
    let mut out = Vec::new();
    let mut bad = 0;
    for line in body.split(|b| *b == b'\n') {
        let line = line.trim_ascii();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_slice::<Entry>(line) {
            Ok(e) => out.push(e),
            Err(_) => bad += 1,
        }
    }
    (out, bad)
}

/// Test fixtures are written over several lines for reading; the log has one entry per line.
#[cfg(test)]
pub fn parse_fixture(s: &str) -> Vec<Entry> {
    let (es, bad) = parse_lines(s.replace('\n', " ").as_bytes());
    assert_eq!(bad, 0);
    es
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(uri: &str, method: &str) -> Target {
        parse_target(uri, method).unwrap()
    }

    #[test]
    fn proxied_kube_paths() {
        let x = t("/k8s/clusters/c-m-7h2kx/apis/apps/v1/namespaces/shop/deployments/web", "PATCH");
        assert_eq!(x.cluster.as_deref(), Some("c-m-7h2kx"));
        assert_eq!((x.verb.as_str(), x.api_group.as_str(), x.resource.as_str(), x.namespace.as_str(), x.name.as_str()),
            ("patch", "apps", "deployments", "shop", "web"));
        let x = t("/k8s/clusters/c-m-7h2kx/api/v1/namespaces/legacy", "DELETE");
        assert_eq!((x.verb.as_str(), x.resource.as_str(), x.namespace.as_str(), x.name.as_str()), ("delete", "namespaces", "", "legacy"));
        let x = t("/k8s/clusters/c1/api/v1/namespaces/shop/secrets?limit=500", "GET");
        assert_eq!((x.verb.as_str(), x.resource.as_str(), x.namespace.as_str()), ("list", "secrets", "shop"));
        let x = t("/k8s/clusters/c1/apis/apps/v1/namespaces/shop/deployments/web/scale", "PUT");
        assert_eq!((x.verb.as_str(), x.subresource.as_str()), ("update", "scale"));
        let x = t("/k8s/clusters/local/api/v1/namespaces/shop/configmaps/a", "PUT");
        assert_eq!(x.cluster, None, "local is Rancher's own cluster");
        let x = t("/k8s/clusters/c1/api/v1/pods?watch=true", "GET");
        assert_eq!(x.verb, "watch");
    }

    #[test]
    fn steve_paths() {
        let x = t("/k8s/clusters/c1/v1/apps.deployments/shop/web", "PUT");
        assert_eq!((x.verb.as_str(), x.api_group.as_str(), x.resource.as_str(), x.namespace.as_str(), x.name.as_str()),
            ("update", "apps", "deployments", "shop", "web"));
        let x = t("/v1/management.cattle.io.clusters/c-m-7h2kx", "DELETE");
        assert_eq!((x.cluster.clone(), x.api_group.as_str(), x.resource.as_str(), x.name.as_str()),
            (None, "management.cattle.io", "clusters", "c-m-7h2kx"));
        let x = t("/k8s/clusters/c1/v1/secrets/shop", "POST");
        assert_eq!((x.verb.as_str(), x.resource.as_str(), x.namespace.as_str(), x.name.as_str()), ("create", "secrets", "shop", ""));
    }

    #[test]
    fn norman_and_login() {
        let x = t("/v3/clusterroletemplatebindings/c-m-7h2kx:crtb-abc", "DELETE");
        assert_eq!((x.verb.as_str(), x.resource.as_str(), x.name.as_str()), ("delete", "clusterroletemplatebindings", "c-m-7h2kx:crtb-abc"));
        let x = t("/v3/clusters/c-m-7h2kx?action=generateKubeconfig", "POST");
        assert_eq!(x.verb, "generatekubeconfig");
        let x = t("/v3-public/localProviders/local?action=login", "POST");
        assert_eq!((x.verb.as_str(), x.name.as_str()), ("login", "local"));
        assert!(parse_target("/v3-public/authProviders", "GET").is_none());
        assert!(parse_target("/meta/proxy/example.com", "GET").is_none());
    }

    const UI_PATCH: &str = r#"{"auditID":"r1","requestURI":"/k8s/clusters/c-m-7h2kx/v1/apps.deployments/shop/web","method":"PUT",
      "user":{"name":"u-abc12","group":["system:authenticated"],"extra":{"principalid":["openldap_user://uid=alice,ou=people"],"username":["alice"]}},
      "remoteAddr":"10.42.0.9:51234","responseCode":200,
      "requestTimestamp":"2026-10-14T10:00:00+08:00","responseTimestamp":"2026-10-14T10:00:01+08:00",
      "requestHeader":{"User-Agent":["Mozilla/5.0"],"X-Forwarded-For":["192.168.10.23, 10.0.0.1"]}}"#;

    #[test]
    fn record_from_a_ui_edit() {
        let (es, bad) = parse_lines(format!("{}\ntail: file truncated\n", UI_PATCH.replace('\n', " ")).as_bytes());
        assert_eq!((es.len(), bad), (1, 1));
        let r = to_record(&es[0]).unwrap();
        assert_eq!(r.source, Source::Rancher);
        assert_eq!(r.cluster.as_deref(), Some("c-m-7h2kx"));
        assert_eq!((r.actor.as_str(), r.user_id.as_str()), ("alice", "u-abc12"));
        assert_eq!(r.principal, "openldap_user://uid=alice,ou=people");
        assert_eq!(r.ts.to_rfc3339(), "2026-10-14T02:00:01+00:00");
        assert_eq!((r.source_ip.as_str(), r.user_agent.as_str()), ("192.168.10.23", "Mozilla/5.0"));
        assert!(r.human);
    }

    #[test]
    fn identity_wins_and_reads_are_dropped() {
        let line = UI_PATCH.replace(r#""user":{"name":"u-abc12","#,
            r#""user":{"name":"u-abc12","identity":{"username":"alice.chen","provider":"openldap","principalId":"openldap_user://uid=alice"},"#);
        let r = to_record(&parse_fixture(&line)[0]).unwrap();
        assert_eq!((r.actor.as_str(), r.principal.as_str()), ("alice.chen", "openldap_user://uid=alice"));
        let get = UI_PATCH.replace(r#""method":"PUT""#, r#""method":"GET""#);
        assert!(to_record(&parse_fixture(&get)[0]).is_none());
    }

    #[test]
    fn failed_login_keeps_the_name_typed() {
        let line = r#"{"auditID":"l1","requestURI":"/v3-public/localProviders/local?action=login","method":"POST",
          "user":{"name":"system:unauthenticated"},"userLoginName":"admin","remoteAddr":"[fd00::5]:443","responseCode":401,
          "responseTimestamp":"2026-10-14T02:00:00Z"}"#;
        let r = to_record(&parse_fixture(line)[0]).unwrap();
        assert_eq!((r.actor.as_str(), r.verb.as_str(), r.code), ("admin", "login", 401));
        assert_eq!((r.user_id.as_str(), r.source_ip.as_str()), ("", "fd00::5"));
        assert!(r.human);
    }
}
