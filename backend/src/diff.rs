//! Normalise Kubernetes objects and compute field-level differences between versions.

use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

const DROP_METADATA: &[&str] = &[
    "resourceVersion",
    "managedFields",
    "generation",
    "uid",
    "creationTimestamp",
    "selfLink",
];

const DROP_ANNOTATIONS: &[&str] = &[
    "kubectl.kubernetes.io/last-applied-configuration",
    "deployment.kubernetes.io/revision",
    "field.cattle.io/publicEndpoints",
];

/// Keep only what a person changes: drop status and bookkeeping fields.
pub fn normalize(obj: &Value) -> Value {
    let mut v = obj.clone();
    if let Some(m) = v.as_object_mut() {
        m.remove("status");
        if let Some(meta) = m.get_mut("metadata").and_then(Value::as_object_mut) {
            for k in DROP_METADATA {
                meta.remove(*k);
            }
            if let Some(ann) = meta.get_mut("annotations").and_then(Value::as_object_mut) {
                for k in DROP_ANNOTATIONS {
                    ann.remove(*k);
                }
                if ann.is_empty() {
                    meta.remove("annotations");
                }
            }
        }
    }
    v
}

/// Replace ConfigMap values with a short hash so the UI can show *that* a key changed
/// without storing or serving its content.
pub fn mask_values(obj: &mut Value) {
    for field in ["data", "binaryData"] {
        if let Some(m) = obj.get_mut(field).and_then(Value::as_object_mut) {
            for v in m.values_mut() {
                let s = match &*v { Value::String(s) => s.clone(), other => other.to_string() };
                let h = Sha256::digest(s.as_bytes());
                let hex: String = h.iter().take(6).map(|b| format!("{b:02x}")).collect();
                *v = Value::String(format!("sha256:{hex} ({} bytes)", s.len()));
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Change {
    pub path: String,
    /// "added", "removed" or "changed".
    pub op: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new: Option<Value>,
}

fn join(base: &str, key: &str) -> String {
    if base.is_empty() { key.to_string() } else { format!("{base}.{key}") }
}

/// Containers and similar lists are keyed by `name` so a reorder or an insert
/// does not show up as every element changing.
fn keyed(list: &[Value]) -> Option<Map<String, Value>> {
    let mut m = Map::new();
    for item in list {
        let name = item.get("name")?.as_str()?.to_string();
        if m.insert(name, item.clone()).is_some() {
            return None;
        }
    }
    Some(m)
}

fn walk(path: &str, a: &Value, b: &Value, out: &mut Vec<Change>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for (k, va) in x {
                match y.get(k) {
                    Some(vb) => walk(&join(path, k), va, vb, out),
                    None => out.push(Change { path: join(path, k), op: "removed", old: Some(va.clone()), new: None }),
                }
            }
            for (k, vb) in y {
                if !x.contains_key(k) {
                    out.push(Change { path: join(path, k), op: "added", old: None, new: Some(vb.clone()) });
                }
            }
        }
        (Value::Array(x), Value::Array(y)) if x != y => {
            if let (Some(kx), Some(ky)) = (keyed(x), keyed(y)) {
                for (k, va) in &kx {
                    let p = format!("{path}[{k}]");
                    match ky.get(k) {
                        Some(vb) => walk(&p, va, vb, out),
                        None => out.push(Change { path: p, op: "removed", old: Some(va.clone()), new: None }),
                    }
                }
                for (k, vb) in &ky {
                    if !kx.contains_key(k) {
                        out.push(Change { path: format!("{path}[{k}]"), op: "added", old: None, new: Some(vb.clone()) });
                    }
                }
            } else if x.len() == y.len() {
                for (i, (va, vb)) in x.iter().zip(y).enumerate() {
                    walk(&format!("{path}[{i}]"), va, vb, out);
                }
            } else {
                out.push(Change { path: path.to_string(), op: "changed", old: Some(a.clone()), new: Some(b.clone()) });
            }
        }
        _ if a != b => out.push(Change { path: path.to_string(), op: "changed", old: Some(a.clone()), new: Some(b.clone()) }),
        _ => {}
    }
}

pub fn diff(old: &Value, new: &Value) -> Vec<Change> {
    let mut out = Vec::new();
    walk("", &without_types(&normalize(old)), &without_types(&normalize(new)), &mut out);
    out
}

/// Top-level apiVersion and kind are absent from LIST items sent by collectors before 0.2.1
/// and present in watch events; nobody changes them, so they never count as a change.
fn without_types(v: &Value) -> Value {
    let mut v = v.clone();
    if let Some(m) = v.as_object_mut() {
        m.remove("apiVersion");
        m.remove("kind");
    }
    v
}

/// Same content for change detection (normalised bodies; apiVersion/kind ignored).
pub fn same_content(a: &Value, b: &Value) -> bool {
    without_types(a) == without_types(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn replicas_and_image() {
        let a = json!({"metadata":{"name":"web","resourceVersion":"1","managedFields":[]},
            "spec":{"replicas":3,"template":{"spec":{"containers":[{"name":"app","image":"web:1.0"},{"name":"side","image":"s:1"}]}}},
            "status":{"readyReplicas":3}});
        let b = json!({"metadata":{"name":"web","resourceVersion":"2"},
            "spec":{"replicas":5,"template":{"spec":{"containers":[{"name":"side","image":"s:1"},{"name":"app","image":"web:1.1"}]}}},
            "status":{"readyReplicas":4}});
        let d = diff(&a, &b);
        assert_eq!(d.len(), 2, "{d:?}");
        assert!(d.iter().any(|c| c.path == "spec.replicas" && c.new == Some(json!(5))));
        assert!(d.iter().any(|c| c.path == "spec.template.spec.containers[app].image" && c.old == Some(json!("web:1.0"))));
    }

    #[test]
    fn status_only_change_is_empty() {
        let a = json!({"metadata":{"resourceVersion":"1"},"spec":{"x":1},"status":{"a":1}});
        let b = json!({"metadata":{"resourceVersion":"2"},"spec":{"x":1},"status":{"a":2}});
        assert!(diff(&a, &b).is_empty());
    }

    #[test]
    fn masked_values_still_show_which_key_changed() {
        let mut a = json!({"data":{"A":"secret-1","B":"x"}});
        let mut b = json!({"data":{"A":"secret-2","B":"x"}});
        mask_values(&mut a);
        mask_values(&mut b);
        let d = diff(&a, &b);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].path, "data.A");
        assert!(!a.to_string().contains("secret-1"));
    }

    #[test]
    fn added_and_removed_keys() {
        let a = json!({"data":{"a":"1","b":"2"}});
        let b = json!({"data":{"a":"1","c":"3"}});
        let d = diff(&a, &b);
        assert!(d.contains(&Change{path:"data.b".into(),op:"removed",old:Some(json!("2")),new:None}));
        assert!(d.contains(&Change{path:"data.c".into(),op:"added",old:None,new:Some(json!("3"))}));
    }
}
