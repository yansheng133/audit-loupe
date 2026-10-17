//! Tamper evidence for stored audit records.
//!
//! Every record is chained to the one before it in the same cluster:
//! `hash = SHA-256(previous hash + "\n" + canonical JSON of the stored columns)`.
//! Changing, inserting or removing a stored record breaks the chain from that point on.
//!
//! The chain lives in the same database, so someone who can rewrite the database can also
//! recompute it. That is why the head of each chain is sent out every hour as an anchor
//! (to the SIEM or other forwarding targets): a chain only proves anything when checked
//! against anchors kept somewhere else. Without them it still detects accidental damage.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// The stored columns that make up a record, in the form they are stored.
pub struct Fields<'a> {
    pub cluster: &'a str,
    pub audit_id: &'a str,
    pub source: &'a str,
    pub ts: &'a str,
    pub actor: &'a str,
    pub principal: &'a str,
    pub user_id: &'a str,
    pub human: bool,
    pub via: &'a str,
    pub caller: &'a str,
    pub verb: &'a str,
    pub api_group: &'a str,
    pub resource: &'a str,
    pub subresource: &'a str,
    pub namespace: &'a str,
    pub name: &'a str,
    pub code: i64,
    pub source_ip: &'a str,
    pub user_agent: &'a str,
    pub request_uri: &'a str,
    pub request_object: Option<&'a str>,
}

pub fn canonical(f: &Fields) -> String {
    // serde_json without preserve_order sorts object keys, so this is stable.
    let v: Value = json!({
        "cluster": f.cluster, "auditID": f.audit_id, "source": f.source, "ts": f.ts, "actor": f.actor,
        "principal": f.principal, "userID": f.user_id, "human": f.human, "via": f.via, "caller": f.caller,
        "verb": f.verb, "apiGroup": f.api_group, "resource": f.resource, "subresource": f.subresource,
        "namespace": f.namespace, "name": f.name, "code": f.code, "sourceIP": f.source_ip,
        "userAgent": f.user_agent, "requestURI": f.request_uri, "requestObject": f.request_object,
    });
    v.to_string()
}

pub fn link(prev: &str, f: &Fields) -> String {
    let mut h = Sha256::new();
    h.update(prev.as_bytes());
    h.update(b"\n");
    h.update(canonical(f).as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(name: &str) -> Fields<'_> {
        Fields {
            cluster: "c1", audit_id: "a", source: "k8s", ts: "2026-10-14T02:00:00+00:00", actor: "alice", principal: "", user_id: "u-1",
            human: true, via: "rancher", caller: "x", verb: "patch", api_group: "apps", resource: "deployments", subresource: "",
            namespace: "shop", name, code: 200, source_ip: "", user_agent: "", request_uri: "", request_object: None,
        }
    }

    #[test]
    fn any_change_changes_the_link() {
        let a = link("", &f("web"));
        assert_eq!(a.len(), 64);
        assert_eq!(a, link("", &f("web")), "deterministic");
        assert_ne!(a, link("", &f("api")));
        assert_ne!(a, link("00", &f("web")), "depends on the previous link");
    }
}
