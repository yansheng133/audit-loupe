//! `audit-loupe seed-demo`: fill a database with a made-up but realistic day of
//! activity, for trying the UI before a cluster is wired up. Every name here is fictional.

use crate::audit::{to_record, Event};
use crate::config::Config;
use crate::diff::mask_values;
use crate::rules::evaluate;
use crate::store::{ObjKey, Store};
use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};
use serde_json::{json, Value};

fn event(id: &str, at: DateTime<Utc>, actor: Option<(&str, &str)>, caller: &str, verb: &str, group: &str, resource: &str, sub: &str, ns: &str, name: &str, code: i64, body: Option<Value>) -> Event {
    let mut v = json!({
        "auditID": id, "stage": "ResponseComplete", "verb": verb,
        "requestURI": format!("/apis/{group}/v1/namespaces/{ns}/{resource}/{name}"),
        "user": {"username": caller},
        "sourceIPs": ["10.42.0.12"], "userAgent": "Mozilla/5.0 (Rancher UI)",
        "objectRef": {"apiGroup": group, "resource": resource, "subresource": sub, "namespace": ns, "name": name},
        "responseStatus": {"code": code},
        "stageTimestamp": at.to_rfc3339(),
    });
    if let Some((uid, user)) = actor {
        v["impersonatedUser"] = json!({"username": uid, "extra": {"username": [user], "principalid": [format!("openldap_user://uid={user},ou=people,dc=example,dc=com")]}});
    }
    if let Some(b) = body {
        v["requestObject"] = b;
    }
    serde_json::from_value(v).expect("demo event")
}

fn deployment(replicas: i64, image: &str, mem: &str) -> Value {
    json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"web","namespace":"shop","labels":{"app":"web"}},
      "spec":{"replicas":replicas,"selector":{"matchLabels":{"app":"web"}},
        "template":{"metadata":{"labels":{"app":"web"}},"spec":{"containers":[{"name":"web","image":image,
          "resources":{"limits":{"memory":mem}}}]}}}})
}

pub fn seed(store: &Store, cfg: &Config) -> anyhow::Result<()> {
    let c = cfg.cluster.as_str();
    // Start at 09:00 local time on the most recent weekday before today.
    let mut date = Utc::now().with_timezone(&cfg.tz).date_naive() - Duration::days(1);
    while date.weekday().number_from_monday() > 5 {
        date -= Duration::days(1);
    }
    let day = cfg.tz.from_local_datetime(&date.and_hms_opt(9, 0, 0).unwrap()).single().unwrap().with_timezone(&Utc);
    let rancher = "system:serviceaccount:cattle-impersonation-system:cattle-impersonation-";
    let dep = ObjKey { cluster: c.into(), api_group: "apps".into(), resource: "deployments".into(), namespace: "shop".into(), name: "web".into() };
    let cm = ObjKey { cluster: c.into(), api_group: "".into(), resource: "configmaps".into(), namespace: "shop".into(), name: "web-config".into() };
    let ns = ObjKey { cluster: c.into(), api_group: "".into(), resource: "namespaces".into(), namespace: "".into(), name: "legacy-batch".into() };
    let rb = ObjKey { cluster: c.into(), api_group: "rbac.authorization.k8s.io".into(), resource: "rolebindings".into(), namespace: "shop".into(), name: "dev-edit".into() };

    // Baselines when tracking started.
    store.insert_version(&dep, "Deployment", "d1", &deployment(3, "registry.example.com/shop/web:1.4.2", "512Mi"), "baseline", day)?;
    let mut cm0 = json!({"metadata":{"name":"web-config","namespace":"shop"},"data":{"LOG_LEVEL":"info","PAYMENT_TIMEOUT":"30s"}});
    mask_values(&mut cm0);
    store.insert_version(&cm, "ConfigMap", "m1", &cm0, "baseline", day)?;
    store.insert_version(&ns, "Namespace", "n1", &json!({"metadata":{"name":"legacy-batch"}}), "baseline", day)?;

    let mut evs: Vec<(Event, Option<(ObjKey, &str, &str, Value, &str)>)> = Vec::new();
    let t = |h: i64, m: i64| day + Duration::hours(h) + Duration::minutes(m);

    // alice scales web from 3 to 5 in the Rancher UI.
    evs.push((event("demo-1", t(2, 5), Some(("u-7x2lq", "alice")), &format!("{rancher}u-7x2lq"), "patch", "apps", "deployments", "scale", "shop", "web", 200, Some(json!({"spec":{"replicas":5}}))),
        Some((dep.clone(), "Deployment", "d1", deployment(5, "registry.example.com/shop/web:1.4.2", "512Mi"), "modified"))));
    // alice rolls out a new image and raises the memory limit with kubectl through Rancher.
    evs.push((event("demo-2", t(3, 40), Some(("u-7x2lq", "alice")), &format!("{rancher}u-7x2lq"), "patch", "apps", "deployments", "", "shop", "web", 200,
        Some(json!({"spec":{"template":{"spec":{"containers":[{"name":"web","image":"registry.example.com/shop/web:1.5.0","resources":{"limits":{"memory":"768Mi"}}}]}}}}))),
        Some((dep.clone(), "Deployment", "d1", deployment(5, "registry.example.com/shop/web:1.5.0", "768Mi"), "modified"))));
    // bob changes a ConfigMap value.
    evs.push((event("demo-3", t(5, 12), Some(("u-k9m3p", "bob")), &format!("{rancher}u-k9m3p"), "update", "", "configmaps", "", "shop", "web-config", 200, None),
        Some((cm.clone(), "ConfigMap", "m1", json!({"metadata":{"name":"web-config","namespace":"shop"},"data":{"LOG_LEVEL":"debug","PAYMENT_TIMEOUT":"30s"}}), "modified"))));
    // bob tries to read a secret and is refused.
    evs.push((event("demo-4", t(5, 20), Some(("u-k9m3p", "bob")), &format!("{rancher}u-k9m3p"), "get", "", "secrets", "", "shop", "payment-key", 403, None), None));
    // carol grants edit to a group.
    evs.push((event("demo-5", t(6, 2), Some(("u-c4r0l", "carol")), &format!("{rancher}u-c4r0l"), "create", "rbac.authorization.k8s.io", "rolebindings", "", "shop", "dev-edit", 201, None),
        Some((rb.clone(), "RoleBinding", "r1", json!({"metadata":{"name":"dev-edit","namespace":"shop"},"roleRef":{"kind":"ClusterRole","name":"edit"},"subjects":[{"kind":"Group","name":"shop-dev"}]}), "added"))));
    // ops opens a shell in a pod.
    evs.push((event("demo-6", t(7, 30), Some(("u-0ps01", "ops")), &format!("{rancher}u-0ps01"), "create", "", "pods", "exec", "shop", "web-6c9f7d-2kq8v", 101, None), None));
    // Late night: someone uses the node admin kubeconfig to delete a namespace (bypasses Rancher).
    evs.push((event("demo-7", t(15, 10), None, "system:admin", "delete", "", "namespaces", "", "", "legacy-batch", 200, None),
        Some((ns.clone(), "Namespace", "n1", json!({"metadata":{"name":"legacy-batch"}}), "deleted"))));
    // Controller noise that the default view hides.
    for i in 0..5 {
        evs.push((event(&format!("demo-sys-{i}"), t(2, 6 + i), None, "system:serviceaccount:kube-system:deployment-controller", "update", "apps", "replicasets", "", "shop", "web-6c9f7d", 200, None), None));
    }

    let mut last = day;
    for (ev, ver) in evs {
        let at = ev.stage_timestamp.unwrap();
        if let Some((key, kind, uid, mut obj, change)) = ver {
            if key.resource == "configmaps" { mask_values(&mut obj); }
            store.insert_version(&key, kind, uid, &obj, change, at + Duration::milliseconds(400))?;
        }
        let r = to_record(&ev).unwrap();
        store.insert_event(c, &r, &evaluate(&r, cfg))?;
        last = last.max(at);
    }
    // A change made while the tool was down: nobody to credit.
    let mut cm2 = json!({"metadata":{"name":"web-config","namespace":"shop"},"data":{"LOG_LEVEL":"debug","PAYMENT_TIMEOUT":"45s"}});
    mask_values(&mut cm2);
    store.insert_version(&cm, "ConfigMap", "m1", &cm2, "resync", last + Duration::hours(1))?;
    store.link_since(0)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn demo_links_and_flags() {
        let cfg = crate::config::Config::test();
        let s = crate::store::Store::open(":memory:").unwrap();
        super::seed(&s, &cfg).unwrap();
        let q = crate::store::Query { cluster: None, actor: None, from_ms: 0, to_ms: i64::MAX, humans_only: true, writes_only: true, alerts_only: false, limit: 100 };
        let tl = s.timeline(&q).unwrap();
        assert_eq!(tl.len(), 7, "humans only, controllers hidden");
        let linked = tl.iter().filter(|e| e.version_id.is_some()).count();
        assert_eq!(linked, 5);
        let alerts = s.alerts(0, i64::MAX, 100).unwrap();
        let rules: Vec<String> = alerts.iter().flat_map(|e| e.alerts.clone()).collect();
        for r in ["namespace_deleted", "bypassed_rancher", "rbac_changed", "pod_exec", "forbidden"] {
            assert!(rules.iter().any(|x| x == r), "missing {r}: {rules:?}");
        }
    }
}
