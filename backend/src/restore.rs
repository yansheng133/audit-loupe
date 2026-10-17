//! Restore YAML: the object as it was just before a change, ready for `kubectl apply`.
//!
//! Audit Loupe only produces the text. It never applies it and holds no write access to
//! any cluster. What it cannot restore it says so in the file: ConfigMap values are kept
//! only as hashes, and Secrets are never stored.

use serde_json::{Map, Value};

/// Fields the cluster assigns; applying them back would fail or fight the controllers.
const DROP_META: &[&str] = &["uid", "resourceVersion", "generation", "creationTimestamp", "deletionTimestamp", "deletionGracePeriodSeconds", "managedFields", "selfLink", "ownerReferences", "finalizers"];

pub fn clean(body: &Value) -> Value {
    let mut v = body.clone();
    if let Some(m) = v.as_object_mut() {
        m.remove("status");
        if let Some(meta) = m.get_mut("metadata").and_then(Value::as_object_mut) {
            for k in DROP_META {
                meta.remove(*k);
            }
        }
    }
    v
}

/// Values that are only stored as hashes ("sha256:xxxx (n bytes)").
pub fn masked_paths(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for field in ["data", "binaryData"] {
        if let Some(m) = v.get(field).and_then(Value::as_object) {
            for (k, x) in m {
                if x.as_str().is_some_and(|s| s.starts_with("sha256:")) {
                    out.push(format!("{field}.{k}"));
                }
            }
        }
    }
    out
}

fn scalar(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        // Always quoted (JSON string syntax is valid YAML), so "on", "0012" or "a: b" stay strings.
        Value::String(s) => serde_json::to_string(s).unwrap_or_default(),
        _ => unreachable!(),
    }
}

/// YAML 1.1 (which kubectl's parser follows) reads these as booleans or null.
const RESERVED: &[&str] = &["y", "n", "yes", "no", "on", "off", "true", "false", "null", "~"];

fn key(k: &str) -> String {
    let plain = !k.is_empty()
        && k.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
        && !k.starts_with(['-', '.'])
        && !k.chars().all(|c| c.is_ascii_digit() || c == '.')
        && !RESERVED.contains(&k.to_ascii_lowercase().as_str());
    if plain {
        k.to_string()
    } else {
        serde_json::to_string(k).unwrap_or_default()
    }
}

fn emit(v: &Value, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    match v {
        Value::Object(m) => emit_map(m, indent, out),
        Value::Array(a) => {
            for item in a {
                match item {
                    Value::Object(m) if !m.is_empty() => {
                        out.push_str(&format!("{pad}- "));
                        let mut inner = String::new();
                        emit_map(m, indent + 2, &mut inner);
                        out.push_str(inner.trim_start());
                    }
                    Value::Array(x) if !x.is_empty() => {
                        out.push_str(&format!("{pad}-\n"));
                        emit(item, indent + 2, out);
                    }
                    Value::Object(_) => out.push_str(&format!("{pad}- {{}}\n")),
                    Value::Array(_) => out.push_str(&format!("{pad}- []\n")),
                    _ => out.push_str(&format!("{pad}- {}\n", scalar(item))),
                }
            }
        }
        _ => out.push_str(&format!("{pad}{}\n", scalar(v))),
    }
}

fn emit_map(m: &Map<String, Value>, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    for (k, v) in m {
        match v {
            Value::Object(x) if !x.is_empty() => {
                out.push_str(&format!("{pad}{}:\n", key(k)));
                emit_map(x, indent + 2, out);
            }
            Value::Array(x) if !x.is_empty() => {
                out.push_str(&format!("{pad}{}:\n", key(k)));
                emit(v, indent + 2, out);
            }
            Value::Object(_) => out.push_str(&format!("{pad}{}: {{}}\n", key(k))),
            Value::Array(_) => out.push_str(&format!("{pad}{}: []\n", key(k))),
            _ => out.push_str(&format!("{pad}{}: {}\n", key(k), scalar(v))),
        }
    }
}

pub fn to_yaml(v: &Value) -> String {
    let mut out = String::new();
    emit(v, 0, &mut out);
    out
}

/// The full restore file: comments saying what it is and what it cannot restore, then YAML.
pub fn restore_file(before: &Value, header: &[String]) -> String {
    let obj = clean(before);
    let mut out = String::new();
    for line in header {
        out.push_str(&format!("# {line}\n"));
    }
    let masked = masked_paths(&obj);
    if !masked.is_empty() {
        out.push_str("# NOT RESTORABLE: these values were never stored (only a hash of each). Fill them in\n");
        out.push_str("# from the original source before applying, or remove them to leave the current values:\n");
        for p in &masked {
            out.push_str(&format!("#   {p}\n"));
        }
    }
    out.push_str(&to_yaml(&obj));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn yaml_round_trips_through_a_parser() {
        let v = json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"web","namespace":"shop","uid":"x","resourceVersion":"9",
            "labels":{"app.kubernetes.io/name":"web"}},"spec":{"replicas":3,"selector":{"matchLabels":{"app":"web"}},
            "template":{"metadata":{"labels":{"app":"web"}},"spec":{"containers":[{"name":"web","image":"r/web:1.4","args":["--port","8080"],"env":[],
            "ports":[{"containerPort":8080}]}],"nodeSelector":{}}}},"status":{"readyReplicas":3}});
        let y = to_yaml(&clean(&v));
        assert!(!y.contains("uid") && !y.contains("status") && !y.contains("resourceVersion"));
        // Check with Python's YAML parser, as kubectl would read it.
        let parsed = std::process::Command::new("python3")
            .args(["-c", "import sys,yaml,json; print(json.dumps(yaml.safe_load(sys.stdin.read()), sort_keys=True))"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut p| {
                use std::io::Write;
                p.stdin.take().unwrap().write_all(y.as_bytes())?;
                p.wait_with_output()
            });
        if let Ok(o) = parsed {
            if o.status.success() {
                let back: Value = serde_json::from_slice(&o.stdout).unwrap();
                assert_eq!(back, clean(&v), "{y}");
            }
        }
    }

    #[test]
    fn masked_configmap_values_are_called_out() {
        let cm = json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"web-config"},"data":{"LOG_LEVEL":"sha256:3f2a90 (4 bytes)","on":"yes"}});
        let f = restore_file(&cm, &["restore of ConfigMap shop/web-config".into()]);
        assert!(f.contains("NOT RESTORABLE") && f.contains("#   data.LOG_LEVEL"));
        assert!(f.contains("\"on\": \"yes\""), "{f}");
    }
}
