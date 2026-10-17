//! Relations between objects, worked out from one object's body when its version is stored:
//! what it is owned by, mounts, reads, runs as, routes to, grants, scales. Label selectors
//! (Service, NetworkPolicy) are kept as selectors and matched against workloads' pod labels
//! when asked, so a later label change cannot leave a stale relation behind.

use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct Rel {
    pub rel: &'static str,
    pub to_kind: String,
    pub to_namespace: String,
    pub to_name: String,
    /// Set for selector relations; the target is every workload in the namespace whose pod
    /// labels match.
    pub selector: Option<Value>,
}

/// Kinds that carry a pod template, and where it is.
pub fn pod_template(kind: &str, obj: &Value) -> Option<Value> {
    let p = match kind {
        "Deployment" | "StatefulSet" | "DaemonSet" | "ReplicaSet" | "Job" => "/spec/template",
        "CronJob" => "/spec/jobTemplate/spec/template",
        _ => return None,
    };
    obj.pointer(p).cloned()
}

fn s<'a>(v: &'a Value, p: &str) -> Option<&'a str> {
    v.pointer(p).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn arr<'a>(v: &'a Value, p: &str) -> impl Iterator<Item = &'a Value> {
    v.pointer(p).and_then(Value::as_array).into_iter().flatten()
}

fn to(rel: &'static str, kind: &str, ns: &str, name: &str) -> Rel {
    Rel { rel, to_kind: kind.into(), to_namespace: ns.into(), to_name: name.into(), selector: None }
}

fn pod_spec_refs(spec: &Value, ns: &str, out: &mut Vec<Rel>) {
    for v in arr(spec, "/volumes") {
        if let Some(n) = s(v, "/configMap/name") {
            out.push(to("mounts", "ConfigMap", ns, n));
        }
        if let Some(n) = s(v, "/secret/secretName") {
            out.push(to("mounts", "Secret", ns, n));
        }
        if let Some(n) = s(v, "/persistentVolumeClaim/claimName") {
            out.push(to("mounts", "PersistentVolumeClaim", ns, n));
        }
        for src in arr(v, "/projected/sources") {
            if let Some(n) = s(src, "/configMap/name") {
                out.push(to("mounts", "ConfigMap", ns, n));
            }
            if let Some(n) = s(src, "/secret/name") {
                out.push(to("mounts", "Secret", ns, n));
            }
        }
    }
    for c in arr(spec, "/containers").chain(arr(spec, "/initContainers")) {
        for e in arr(c, "/envFrom") {
            if let Some(n) = s(e, "/configMapRef/name") {
                out.push(to("reads", "ConfigMap", ns, n));
            }
            if let Some(n) = s(e, "/secretRef/name") {
                out.push(to("reads", "Secret", ns, n));
            }
        }
        for e in arr(c, "/env") {
            if let Some(n) = s(e, "/valueFrom/configMapKeyRef/name") {
                out.push(to("reads", "ConfigMap", ns, n));
            }
            if let Some(n) = s(e, "/valueFrom/secretKeyRef/name") {
                out.push(to("reads", "Secret", ns, n));
            }
        }
    }
    if let Some(n) = s(spec, "/serviceAccountName") {
        out.push(to("runs_as", "ServiceAccount", ns, n));
    }
    for p in arr(spec, "/imagePullSecrets") {
        if let Some(n) = s(p, "/name") {
            out.push(to("pulls_with", "Secret", ns, n));
        }
    }
}

/// Everything this object points at.
pub fn extract(kind: &str, ns: &str, obj: &Value) -> Vec<Rel> {
    let mut out = Vec::new();
    for o in arr(obj, "/metadata/ownerReferences") {
        if let (Some(k), Some(n)) = (s(o, "/kind"), s(o, "/name")) {
            out.push(to("owned_by", k, ns, n));
        }
    }
    if let Some(t) = pod_template(kind, obj)
        && let Some(spec) = t.get("spec") {
            pod_spec_refs(spec, ns, &mut out);
        }
    match kind {
        "Service" => {
            if let Some(sel) = obj.pointer("/spec/selector").and_then(Value::as_object).filter(|m| !m.is_empty()) {
                out.push(Rel { selector: Some(serde_json::json!({ "matchLabels": sel })), ..to("selects", "", ns, "") });
            }
        }
        "NetworkPolicy" => {
            if let Some(sel) = obj.pointer("/spec/podSelector") {
                out.push(Rel { selector: Some(sel.clone()), ..to("applies_to", "", ns, "") });
            }
        }
        "Ingress" => {
            if let Some(n) = s(obj, "/spec/defaultBackend/service/name") {
                out.push(to("routes_to", "Service", ns, n));
            }
            for r in arr(obj, "/spec/rules") {
                for p in arr(r, "/http/paths") {
                    if let Some(n) = s(p, "/backend/service/name") {
                        out.push(to("routes_to", "Service", ns, n));
                    }
                }
            }
            for t in arr(obj, "/spec/tls") {
                if let Some(n) = s(t, "/secretName") {
                    out.push(to("uses_tls", "Secret", ns, n));
                }
            }
        }
        "HTTPRoute" => {
            for r in arr(obj, "/spec/rules") {
                for b in arr(r, "/backendRefs") {
                    if let Some(n) = s(b, "/name") {
                        out.push(to("routes_to", s(b, "/kind").unwrap_or("Service"), s(b, "/namespace").unwrap_or(ns), n));
                    }
                }
            }
            for p in arr(obj, "/spec/parentRefs") {
                if let Some(n) = s(p, "/name") {
                    out.push(to("attached_to", s(p, "/kind").unwrap_or("Gateway"), s(p, "/namespace").unwrap_or(ns), n));
                }
            }
        }
        "RoleBinding" | "ClusterRoleBinding" => {
            if let (Some(k), Some(n)) = (s(obj, "/roleRef/kind"), s(obj, "/roleRef/name")) {
                // A RoleBinding to a Role stays in its namespace; ClusterRoles are cluster-wide.
                out.push(to("grants_role", k, if k == "Role" { ns } else { "" }, n));
            }
            for sub in arr(obj, "/subjects") {
                if let (Some(k), Some(n)) = (s(sub, "/kind"), s(sub, "/name")) {
                    let sns = if k == "ServiceAccount" { s(sub, "/namespace").unwrap_or(ns) } else { "" };
                    out.push(to("grants_to", k, sns, n));
                }
            }
        }
        "HorizontalPodAutoscaler" => {
            if let (Some(k), Some(n)) = (s(obj, "/spec/scaleTargetRef/kind"), s(obj, "/spec/scaleTargetRef/name")) {
                out.push(to("scales", k, ns, n));
            }
        }
        // Rancher permission objects keep their fields at the top level, not under spec.
        "ClusterRoleTemplateBinding" | "ProjectRoleTemplateBinding" | "GlobalRoleBinding" => {
            if let Some(n) = s(obj, "/roleTemplateName") {
                out.push(to("grants_role", "RoleTemplate", "", n));
            }
            if let Some(n) = s(obj, "/globalRoleName") {
                out.push(to("grants_role", "GlobalRole", "", n));
            }
            if let Some(n) = s(obj, "/userName") {
                out.push(to("grants_to", "User", "", n));
            }
            if let Some(n) = s(obj, "/groupPrincipalName") {
                out.push(to("grants_to", "Group", "", n));
            }
            if let Some(n) = s(obj, "/clusterName") {
                out.push(to("on", "Cluster", "", n));
            }
            if let Some((cluster, project)) = s(obj, "/projectName").and_then(|p| p.split_once(':')) {
                out.push(to("on", "Project", cluster, project));
            }
        }
        _ => {}
    }
    out.sort_by(|a, b| (a.rel, &a.to_kind, &a.to_namespace, &a.to_name).cmp(&(b.rel, &b.to_kind, &b.to_namespace, &b.to_name)));
    out.dedup();
    out
}

/// Kubernetes label selector semantics: matchLabels and matchExpressions (In, NotIn, Exists,
/// DoesNotExist). An empty selector matches everything, as NetworkPolicy's podSelector does.
pub fn selector_matches(sel: &Value, labels: &Map<String, Value>) -> bool {
    let get = |k: &str| labels.get(k).and_then(Value::as_str);
    if let Some(ml) = sel.get("matchLabels").and_then(Value::as_object)
        && !ml.iter().all(|(k, v)| get(k) == v.as_str()) {
            return false;
        }
    for e in sel.get("matchExpressions").and_then(Value::as_array).into_iter().flatten() {
        let key = e.get("key").and_then(Value::as_str).unwrap_or("");
        let values: Vec<&str> = e.get("values").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
        let ok = match e.get("operator").and_then(Value::as_str).unwrap_or("") {
            "In" => get(key).is_some_and(|v| values.contains(&v)),
            "NotIn" => get(key).is_none_or(|v| !values.contains(&v)),
            "Exists" => get(key).is_some(),
            "DoesNotExist" => get(key).is_none(),
            _ => false,
        };
        if !ok {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn deployment_refs() {
        let d = json!({"metadata":{"ownerReferences":[]},"spec":{"template":{"metadata":{"labels":{"app":"web"}},"spec":{
            "serviceAccountName":"web","imagePullSecrets":[{"name":"regcred"}],
            "volumes":[{"name":"c","configMap":{"name":"web-config"}},{"name":"t","secret":{"secretName":"web-tls"}}],
            "containers":[{"name":"web","envFrom":[{"secretRef":{"name":"db"}}],"env":[{"name":"X","valueFrom":{"configMapKeyRef":{"name":"web-config","key":"x"}}}]}]}}}});
        let r: Vec<(&str, String, String)> = extract("Deployment", "shop", &d).into_iter().map(|r| (r.rel, r.to_kind, r.to_name)).collect();
        assert_eq!(r, vec![
            ("mounts", "ConfigMap".into(), "web-config".into()),
            ("mounts", "Secret".into(), "web-tls".into()),
            ("pulls_with", "Secret".into(), "regcred".into()),
            ("reads", "ConfigMap".into(), "web-config".into()),
            ("reads", "Secret".into(), "db".into()),
            ("runs_as", "ServiceAccount".into(), "web".into()),
        ]);
    }

    #[test]
    fn routing_and_rbac() {
        let ing = json!({"spec":{"rules":[{"http":{"paths":[{"backend":{"service":{"name":"web"}}}]}}],"tls":[{"secretName":"shop-tls"}]}});
        assert_eq!(extract("Ingress", "shop", &ing).len(), 2);
        let rb = json!({"roleRef":{"kind":"ClusterRole","name":"edit"},"subjects":[{"kind":"Group","name":"shop-dev"},{"kind":"ServiceAccount","name":"ci","namespace":"build"}]});
        let r = extract("RoleBinding", "shop", &rb);
        assert!(r.contains(&to("grants_role", "ClusterRole", "", "edit")));
        assert!(r.contains(&to("grants_to", "ServiceAccount", "build", "ci")));
        let prtb = json!({"projectName":"c-m-1:p-abc","roleTemplateName":"project-owner","userName":"u-1"});
        let r = extract("ProjectRoleTemplateBinding", "p-abc", &prtb);
        assert!(r.contains(&to("on", "Project", "c-m-1", "p-abc")) && r.contains(&to("grants_to", "User", "", "u-1")));
    }

    #[test]
    fn selectors() {
        let labels = json!({"app":"web","tier":"front"});
        let l = labels.as_object().unwrap();
        assert!(selector_matches(&json!({"matchLabels":{"app":"web"}}), l));
        assert!(!selector_matches(&json!({"matchLabels":{"app":"api"}}), l));
        assert!(selector_matches(&json!({"matchExpressions":[{"key":"tier","operator":"In","values":["front","edge"]}]}), l));
        assert!(!selector_matches(&json!({"matchExpressions":[{"key":"tier","operator":"DoesNotExist"}]}), l));
        assert!(selector_matches(&json!({}), l), "an empty podSelector selects every pod");
        let svc = extract("Service", "shop", &json!({"spec":{"selector":{"app":"web"}}}));
        assert!(selector_matches(svc[0].selector.as_ref().unwrap(), l));
    }
}
