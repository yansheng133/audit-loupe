//! Who an identity is, in the five classes the change map shows (requirements section 4,
//! point 10): a person, a cluster component, a workload's ServiceAccount, Rancher itself, or
//! unknown.
//!
//! An identity is a name plus the Rancher user ID. It is classified once, when it is first
//! seen, and again when the whitelist (AL_IDENTITIES) or the Rancher user behind it changes.
//! Stored records are never rewritten: `events.human` stays as recorded, because it is part of
//! the hash chain. The rules ask this table whether an identity is a person.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// The classes, as the API and the UI name them.
pub const CLASSES: [&str; 5] = ["human", "component", "serviceaccount", "rancher", "unknown"];
/// Raised when the built-in classification changes, so the rule settings fingerprint changes
/// and stored judgements are offered to be judged again.
pub const BUILTIN_VERSION: u32 = 1;
/// Patterns (entries with `*`) allowed in the whitelist.
pub const MAX_PATTERNS: usize = 1000;

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS identities (
  id INTEGER PRIMARY KEY,
  actor TEXT NOT NULL, user_id TEXT NOT NULL,
  human INTEGER NOT NULL, user_agent TEXT NOT NULL,
  class TEXT NOT NULL, reason TEXT NOT NULL, source TEXT NOT NULL,
  first_ms INTEGER NOT NULL,
  UNIQUE (actor, user_id)
);";

/// One whitelist entry: these names (exact, or with `*`) are this class, for this reason.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Entry {
    pub names: Vec<String>,
    pub class: &'static str,
    pub reason: String,
}

/// Identities the built-in rules cannot tell apart, named by the administrator
/// (for example `rke2-cloud-controller-manager`, known only by its certificate name).
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct Whitelist {
    pub entries: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryInput {
    #[serde(default)]
    names: Vec<String>,
    #[serde(default)]
    class: String,
    #[serde(default)]
    reason: String,
}

impl Whitelist {
    /// Settings as JSON: `[{"names": ["rke2-cloud-controller-manager"], "class": "component",
    /// "reason": "..."}]`. Anything wrong is an error naming where.
    pub fn parse(json: &str) -> anyhow::Result<Whitelist> {
        if json.trim().is_empty() {
            return Ok(Whitelist::default());
        }
        let input: Vec<EntryInput> = serde_json::from_str(json).map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut patterns = 0;
        let mut entries = Vec::new();
        for (n, x) in input.into_iter().enumerate() {
            let at = format!("[{n}]");
            let class = CLASSES
                .iter()
                .copied()
                .find(|c| *c == x.class)
                .ok_or_else(|| anyhow::anyhow!("{at}.class: {:?} is not one of {}", x.class, CLASSES.join(", ")))?;
            if x.reason.trim().is_empty() {
                anyhow::bail!("{at}: reason is required");
            }
            if x.names.is_empty() {
                anyhow::bail!("{at}: names is empty");
            }
            for (i, name) in x.names.iter().enumerate() {
                if name.trim().is_empty() || name.chars().all(|c| c == '*') {
                    anyhow::bail!("{at}.names[{i}]: {name:?} would match everyone");
                }
                if name.contains('*') {
                    patterns += 1;
                }
            }
            entries.push(Entry { names: x.names, class, reason: x.reason.trim().to_string() });
        }
        if patterns > MAX_PATTERNS {
            anyhow::bail!("{patterns} names with *; at most {MAX_PATTERNS}");
        }
        Ok(Whitelist { entries })
    }

    /// The entry naming this identity, by name or Rancher user ID. Exact names win over
    /// patterns; otherwise the first entry in the settings wins.
    pub fn lookup(&self, actor: &str, user_id: &str) -> Option<&Entry> {
        let is = |n: &str, f: &dyn Fn(&str, &str) -> bool| f(n, actor) || (!user_id.is_empty() && f(n, user_id));
        let exact = |n: &str, t: &str| n == t;
        let pattern = |n: &str, t: &str| n.contains('*') && crate::rules::glob(n, t);
        self.entries
            .iter()
            .find(|e| e.names.iter().any(|n| !n.contains('*') && is(n, &exact)))
            .or_else(|| self.entries.iter().find(|e| e.names.iter().any(|n| is(n, &pattern))))
    }
}

/// The class of one identity and why.
#[derive(Debug, Clone, PartialEq)]
pub struct Class {
    pub class: &'static str,
    pub reason: String,
    /// `whitelist` or `builtin`.
    pub source: &'static str,
}

/// Whitelist first, then the built-in rules.
pub fn classify(actor: &str, user_id: &str, human: bool, user_agent: &str, principals: Option<&str>, wl: &Whitelist) -> Class {
    if let Some(e) = wl.lookup(actor, user_id) {
        return Class { class: e.class, reason: e.reason.clone(), source: "whitelist" };
    }
    let (class, reason) = builtin(actor, human, user_agent, principals);
    Class { class, reason: reason.to_string(), source: "builtin" }
}

/// The built-in rules (report-ui-facts section B). `human` is the record's own guess from the
/// name; `principals` the Rancher user's principal IDs (JSON array), when known. Returns the
/// class and a reason code; the page says the reason in the user's language (`reason_text`
/// is the Chinese one, for API clients that show text).
pub fn builtin(actor: &str, human: bool, user_agent: &str, principals: Option<&str>) -> (&'static str, &'static str) {
    if let Some(rest) = actor.strip_prefix("system:serviceaccount:") {
        let ns = rest.split(':').next().unwrap_or("");
        return if ns.starts_with("cattle-") {
            ("rancher", "sa-rancher")
        } else if ns == "kube-system" {
            ("component", "sa-kube-system")
        } else {
            ("serviceaccount", "sa-workload")
        };
    }
    match actor {
        "system:admin" => return ("human", "admin-cert"),
        "system:cattle:error" => return ("unknown", "rancher-unauthenticated"),
        "" => return ("unknown", "no-actor"),
        _ => {}
    }
    if actor.starts_with("system:") || actor.ends_with("cloud-controller-manager") || user_agent.starts_with("cloud-controller-manager/") {
        return ("component", "system-component");
    }
    if let Some(p) = principals {
        if p.contains("system://") {
            return ("rancher", "rancher-provisioning");
        }
        let ids: Vec<String> = serde_json::from_str(p).unwrap_or_default();
        if human && !ids.is_empty() && ids.iter().all(|i| i.starts_with("local://")) {
            return ("human", "rancher-local-user");
        }
    }
    if human { ("human", "") } else { ("unknown", "unclassified") }
}

/// The reason codes `builtin` gives, and what each means (Chinese, for API clients that show
/// text as it comes; the UI has its own wording per language).
pub const REASONS: [(&str, &str); 10] = [
    ("sa-rancher", "Rancher 安裝的元件所用的 ServiceAccount"),
    ("sa-kube-system", "叢集內建元件"),
    ("sa-workload", "工作負載的 ServiceAccount"),
    ("admin-cert", "管理者憑證，對不到個人"),
    ("rancher-unauthenticated", "Rancher 未通過驗證的請求，看不出是誰"),
    ("no-actor", "找不到執行者"),
    ("system-component", "叢集內建元件"),
    ("rancher-provisioning", "Rancher 內部帳號（叢集佈建）"),
    ("rancher-local-user", "Rancher 本機帳號，可能多人共用"),
    ("unclassified", "看不出是人或程式"),
];

/// A reason code as text; anything that is not a code (a whitelist reason) as it is.
pub fn reason_text(code: &str) -> &str {
    REASONS.iter().find(|(c, _)| *c == code).map(|(_, t)| *t).unwrap_or(code)
}

/// The reason code, if `s` is one.
pub fn reason_code(s: &str) -> Option<&'static str> {
    REASONS.iter().find(|(c, _)| *c == s).map(|(c, _)| *c)
}

pub fn class_name(s: &str) -> &'static str {
    CLASSES.iter().copied().find(|c| *c == s).unwrap_or("unknown")
}

/// The newest principal IDs of a Rancher user, when the user directory has them.
fn principals(c: &Connection, user_id: &str) -> rusqlite::Result<Option<String>> {
    if user_id.is_empty() {
        return Ok(None);
    }
    c.prepare_cached("SELECT principal_ids FROM users WHERE user_id=?1 ORDER BY created_ms DESC LIMIT 1")?
        .query_row(params![user_id], |r| r.get(0))
        .optional()
}

/// The class of an identity, adding the identity the first time it is seen.
pub fn resolve(c: &Connection, wl: &Whitelist, actor: &str, user_id: &str, human: bool, user_agent: &str, at_ms: i64) -> anyhow::Result<&'static str> {
    let known: Option<String> = c
        .prepare_cached("SELECT class FROM identities WHERE actor=?1 AND user_id=?2")?
        .query_row(params![actor, user_id], |r| r.get(0))
        .optional()?;
    if let Some(k) = known {
        return Ok(class_name(&k));
    }
    let p = principals(c, user_id)?;
    let cl = classify(actor, user_id, human, user_agent, p.as_deref(), wl);
    c.prepare_cached(
        "INSERT OR IGNORE INTO identities (actor, user_id, human, user_agent, class, reason, source, first_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
    )?
    .execute(params![actor, user_id, human as i64, user_agent, cl.class, cl.reason, cl.source, at_ms])?;
    Ok(cl.class)
}

/// A person, for the rules that only look at people (off hours, bypassing Rancher, ...).
pub fn is_person(class: &str) -> bool {
    class == "human"
}

/// A stored identity: ID, name, Rancher user ID, the record's own guess, User-Agent, and the
/// class, reason and source it has now.
type Stored = (i64, String, String, bool, String, String, String, String);

/// Classify identities again: all of them, or those of one Rancher user. Returns how many
/// changed class or reason.
pub fn reclassify(c: &Connection, wl: &Whitelist, only_user: Option<&str>) -> anyhow::Result<usize> {
    let rows: Vec<Stored> = {
        let mut st = c.prepare(
            "SELECT id, actor, user_id, human, user_agent, class, reason, source FROM identities WHERE ?1 IS NULL OR user_id = ?1",
        )?;
        st.query_map(params![only_user], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, i64>(3)? == 1, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?))
        })?
        .collect::<Result<_, _>>()?
    };
    let mut changed = 0;
    for (id, actor, user_id, human, ua, class, reason, source) in rows {
        let p = principals(c, &user_id)?;
        let cl = classify(&actor, &user_id, human, &ua, p.as_deref(), wl);
        if cl.class != class || cl.reason != reason || cl.source != source {
            c.execute("UPDATE identities SET class=?2, reason=?3, source=?4 WHERE id=?1", params![id, cl.class, cl.reason, cl.source])?;
            changed += 1;
        }
    }
    Ok(changed)
}

/// Create the table; a database from before it gets one identity per name and user ID already
/// stored (one pass over the records, on the first start only).
pub fn prepare(c: &Connection, wl: &Whitelist) -> anyhow::Result<()> {
    c.execute_batch(SCHEMA)?;
    let empty: bool = c.query_row("SELECT NOT EXISTS (SELECT 1 FROM identities)", [], |r| r.get(0))?;
    if empty {
        c.execute_batch(
            "INSERT OR IGNORE INTO identities (actor, user_id, human, user_agent, class, reason, source, first_ms)
               SELECT actor, user_id, max(human), min(user_agent), '', '', '', min(ts_ms) FROM events WHERE true GROUP BY actor, user_id;",
        )?;
    }
    reclassify(c, wl, None)?;
    Ok(())
}

/// Every identity, for the rules page.
pub fn list(c: &Connection) -> anyhow::Result<Vec<serde_json::Value>> {
    let mut st = c.prepare("SELECT actor, user_id, class, reason, source, first_ms FROM identities ORDER BY class, actor")?;
    let rows = st
        .query_map([], |r| {
            Ok(serde_json::json!({
                "name": r.get::<_, String>(0)?, "userId": r.get::<_, String>(1)?, "class": r.get::<_, String>(2)?,
                "reasonCode": reason_code(&r.get::<_, String>(3)?), "reason": reason_text(&r.get::<_, String>(3)?),
                "source": r.get::<_, String>(4)?, "firstMs": r.get::<_, i64>(5)?,
            }))
        })?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The identities of one lab day and the class each should get (report-ui-facts section B).
    #[test]
    fn identities_of_a_lab_day() {
        let none = Whitelist::default();
        let cases: &[(&str, bool, &str, Option<&str>, &str)] = &[
            ("system:serviceaccount:cattle-fleet-system:fleet-agent", false, "fleetagent/v0.0.0", None, "rancher"),
            ("system:serviceaccount:local-path-storage:local-path-provisioner", false, "local-path-provisioner/v0.0.0", None, "serviceaccount"),
            ("system:serviceaccount:kube-system:rke2-coredns-rke2-coredns-autoscaler", false, "cluster-proportional-autoscaler/v0.0.0", None, "component"),
            ("system:serviceaccount:al-sample:app-config", false, "app-config-sample/1.0", None, "serviceaccount"),
            ("system:serviceaccount:cattle-system:cattle", false, "agent/v0.0.0", None, "rancher"),
            ("u-d67owytc34", true, "cluster-api-controller-manager", Some(r#"["system://c-m-26kbxk5f"]"#), "rancher"),
            ("system:rke2-supervisor", false, "rke2-supervisor@x", None, "component"),
            ("system:serviceaccount:kube-system:cilium", false, "cilium-agent/1.19.6", None, "component"),
            ("system:serviceaccount:longhorn-system:longhorn-service-account", false, "longhorn-manager/v0.0.0", None, "serviceaccount"),
            ("system:kube-scheduler", false, "kube-scheduler/v1.36.4+rke2r1", None, "component"),
            ("system:kube-controller-manager", false, "kube-controller-manager/v1.36.4", None, "component"),
            ("system:serviceaccount:kube-system:deployment-controller", false, "kube-controller-manager/v1.36.4", None, "component"),
            ("system:admin", true, "kubectl1.36.0 (darwin/arm64)", None, "human"),
            ("system:serviceaccount:cattle-system:rancher-webhook", false, "webhook/v0.0.0", None, "rancher"),
            ("system:serviceaccount:cattle-neuvector-system:neuvector", false, "curl/8.14.1", None, "rancher"),
            ("admin", true, "Mozilla/5.0", Some(r#"["local://user-kgw7z"]"#), "human"),
            ("system:cattle:error", false, "Mozilla/5.0", None, "unknown"),
            ("rke2-cloud-controller-manager", true, "cloud-controller-manager/v1.36.3", None, "component"),
            ("laoban-cli-test", true, "kubectl1.36.0 (darwin/arm64)", None, "human"),
            ("", false, "", None, "unknown"),
        ];
        for (actor, human, ua, p, want) in cases {
            assert_eq!(classify(actor, "", *human, ua, *p, &none).class, *want, "{actor}");
        }
        assert_eq!(builtin("admin", true, "", Some(r#"["local://user-kgw7z"]"#)), ("human", "rancher-local-user"));
        assert_eq!(reason_text("rancher-local-user"), "Rancher 本機帳號，可能多人共用");
        assert_eq!(reason_text("排程工具"), "排程工具", "a whitelist reason is shown as written");
        assert!(REASONS.iter().all(|(c, _)| reason_code(c) == Some(*c)));
    }

    #[test]
    fn whitelist_names_users_and_patterns() {
        let wl = Whitelist::parse(
            r#"[{"names": ["laoban-cli-*"], "class": "unknown", "reason": "測試用憑證"},
                {"names": ["laoban-cli-test", "u-abc"], "class": "component", "reason": "測試工具"}]"#,
        )
        .unwrap();
        // An exact name wins over an earlier pattern.
        let c = classify("laoban-cli-test", "", true, "", None, &wl);
        assert_eq!((c.class, c.source, c.reason.as_str()), ("component", "whitelist", "測試工具"));
        assert_eq!(classify("laoban-cli-test2", "", true, "", None, &wl).class, "unknown");
        // A Rancher user ID matches too.
        assert_eq!(classify("Alice", "u-abc", true, "", None, &wl).class, "component");
        assert_eq!(classify("Alice", "u-other", true, "", None, &wl), Class { class: "human", reason: String::new(), source: "builtin" });
    }

    #[test]
    fn bad_whitelists_say_where() {
        let err = |j: &str| Whitelist::parse(j).unwrap_err().to_string();
        assert!(err(r#"[{"names": ["a"], "class": "robot", "reason": "x"}]"#).starts_with("[0].class:"));
        assert_eq!(err(r#"[{"names": ["a"], "class": "human", "reason": " "}]"#), "[0]: reason is required");
        assert_eq!(err(r#"[{"names": [], "class": "human", "reason": "x"}]"#), "[0]: names is empty");
        assert!(err(r#"[{"names": ["**"], "class": "human", "reason": "x"}]"#).contains("would match everyone"));
        assert!(err(r#"[{"name": ["a"], "class": "human", "reason": "x"}]"#).contains("unknown field"));
        let many: Vec<String> = (0..=MAX_PATTERNS).map(|i| format!("p{i}-*")).collect();
        let j = serde_json::json!([{"names": many, "class": "component", "reason": "x"}]).to_string();
        assert!(err(&j).contains("at most 1000"));
        assert_eq!(Whitelist::parse("").unwrap(), Whitelist::default());
    }

    #[test]
    fn resolve_adds_once_and_reclassify_follows_the_whitelist_and_users() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE users (user_id TEXT NOT NULL, uid TEXT NOT NULL, display_name TEXT NOT NULL, username TEXT NOT NULL,
               principal_ids TEXT NOT NULL, local INTEGER NOT NULL, created_ms INTEGER NOT NULL, deleted_ms INTEGER);
             CREATE TABLE events (actor TEXT, user_id TEXT, human INTEGER, user_agent TEXT, ts_ms INTEGER);",
        )
        .unwrap();
        prepare(&c, &Whitelist::default()).unwrap();
        let none = Whitelist::default();
        assert_eq!(resolve(&c, &none, "Bob", "u-d67", true, "", 1).unwrap(), "human");
        assert_eq!(resolve(&c, &none, "Bob", "u-d67", true, "", 2).unwrap(), "human");
        let n: i64 = c.query_row("SELECT count(*) FROM identities", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        // The user directory later says this is Rancher's own account.
        c.execute("INSERT INTO users VALUES ('u-d67', 'x', 'Bob', 'bob', '[\"system://c-m-1\"]', 0, 0, NULL)", []).unwrap();
        assert_eq!(reclassify(&c, &none, Some("u-d67")).unwrap(), 1);
        assert_eq!(resolve(&c, &none, "Bob", "u-d67", true, "", 3).unwrap(), "rancher");
        // A whitelist entry wins, and taking it away puts the built-in class back.
        let wl = Whitelist::parse(r#"[{"names": ["u-d67"], "class": "component", "reason": "佈建"}]"#).unwrap();
        assert_eq!(reclassify(&c, &wl, None).unwrap(), 1);
        assert_eq!(resolve(&c, &wl, "Bob", "u-d67", true, "", 4).unwrap(), "component");
        assert_eq!(reclassify(&c, &none, None).unwrap(), 1);
        assert_eq!(reclassify(&c, &none, None).unwrap(), 0, "nothing left to change");
    }

    #[test]
    fn a_database_from_before_gets_its_identities() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE users (user_id TEXT NOT NULL, uid TEXT NOT NULL, display_name TEXT NOT NULL, username TEXT NOT NULL,
               principal_ids TEXT NOT NULL, local INTEGER NOT NULL, created_ms INTEGER NOT NULL, deleted_ms INTEGER);
             CREATE TABLE events (actor TEXT, user_id TEXT, human INTEGER, user_agent TEXT, ts_ms INTEGER);
             INSERT INTO events VALUES ('system:kube-scheduler', '', 0, 'kube-scheduler/v1', 5),
                                       ('system:kube-scheduler', '', 0, 'kube-scheduler/v1', 3),
                                       ('alice', 'u-1', 1, 'Mozilla', 7);",
        )
        .unwrap();
        prepare(&c, &Whitelist::default()).unwrap();
        let rows: Vec<(String, String, i64)> =
            c.prepare("SELECT actor, class, first_ms FROM identities ORDER BY actor").unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(Result::unwrap).collect();
        assert_eq!(rows, vec![("alice".into(), "human".into(), 7), ("system:kube-scheduler".into(), "component".into(), 3)]);
    }
}
