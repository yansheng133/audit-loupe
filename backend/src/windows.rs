//! Change windows: when changes to a scope are approved. A person's change outside every
//! window that covers the object is flagged. Without any windows configured, the simpler
//! "outside working hours" rule applies instead.
//!
//! Configured as JSON in AL_CHANGE_WINDOWS (the Helm chart renders it from values):
//! [{"name": "prod-nightly", "clusters": ["c-m-1"], "namespaceLabels": {"env": "prod"},
//!   "days": "1-5", "hours": "22-24"},
//!  {"name": "CHG-1234", "clusters": ["c-m-1"], "ranges": [{"from": "...", "to": "..."}]}]
//! A window with ranges is open only in those ranges; otherwise on those days and hours
//! (both default to always). Empty clusters or labels mean every cluster or namespace;
//! a window with namespace labels does not cover cluster-scoped objects.

use chrono::{DateTime, Datelike, FixedOffset, Timelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Range {
    pub from: DateTime<FixedOffset>,
    pub to: DateTime<FixedOffset>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Window {
    pub name: String,
    #[serde(default)]
    pub clusters: Vec<String>,
    #[serde(default)]
    pub namespace_labels: BTreeMap<String, String>,
    pub days: Option<String>,
    pub hours: Option<String>,
    #[serde(default)]
    pub ranges: Vec<Range>,
}

fn days_of(spec: &str) -> Vec<u32> {
    let mut out = Vec::new();
    for part in spec.split(',').map(str::trim) {
        match part.split_once('-') {
            Some((a, b)) => {
                if let (Ok(a), Ok(b)) = (a.trim().parse::<u32>(), b.trim().parse::<u32>()) {
                    out.extend(a..=b);
                }
            }
            None => out.extend(part.parse::<u32>().ok()),
        }
    }
    out
}

impl Window {
    /// Whether this window governs an object in this cluster and namespace (labels of the
    /// namespace as last seen; None for cluster-scoped objects).
    pub fn covers(&self, cluster: &str, ns_labels: Option<&Map<String, Value>>) -> bool {
        if !self.clusters.is_empty() && !self.clusters.iter().any(|c| c == cluster) {
            return false;
        }
        if self.namespace_labels.is_empty() {
            return true;
        }
        let Some(labels) = ns_labels else { return false };
        self.namespace_labels.iter().all(|(k, v)| labels.get(k).and_then(Value::as_str) == Some(v.as_str()))
    }

    pub fn open_at(&self, t: DateTime<Utc>, tz: &chrono_tz::Tz) -> bool {
        if !self.ranges.is_empty() {
            return self.ranges.iter().any(|r| t >= r.from && t < r.to);
        }
        let local = t.with_timezone(tz);
        let day_ok = self.days.as_deref().is_none_or(|d| days_of(d).contains(&local.weekday().number_from_monday()));
        let hour_ok = match self.hours.as_deref().and_then(|h| h.split_once('-')) {
            Some((a, b)) => {
                let (a, b) = (a.trim().parse::<u32>().unwrap_or(0), b.trim().parse::<u32>().unwrap_or(24));
                let h = local.hour();
                if a <= b { h >= a && h < b } else { h >= a || h < b }
            }
            None => true,
        };
        day_ok && hour_ok
    }
}

pub fn parse(json: &str) -> anyhow::Result<Vec<Window>> {
    if json.trim().is_empty() {
        return Ok(vec![]);
    }
    Ok(serde_json::from_str(json)?)
}

/// None when no window covers the object (no opinion); Some(true) when one is open.
pub fn allowed(windows: &[Window], cluster: &str, ns_labels: Option<&Map<String, Value>>, t: DateTime<Utc>, tz: &chrono_tz::Tz) -> Option<bool> {
    let covering: Vec<&Window> = windows.iter().filter(|w| w.covers(cluster, ns_labels)).collect();
    if covering.is_empty() {
        return None;
    }
    Some(covering.iter().any(|w| w.open_at(t, tz)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    #[test]
    fn scope_days_hours_and_ranges() {
        let ws = parse(r#"[
          {"name":"prod-nightly","clusters":["c-m-1"],"namespaceLabels":{"env":"prod"},"days":"1-5","hours":"22-2"},
          {"name":"CHG-1","clusters":["c-m-1"],"namespaceLabels":{"env":"prod"},"ranges":[{"from":"2026-10-17T10:00:00+08:00","to":"2026-10-17T12:00:00+08:00"}]}
        ]"#).unwrap();
        let tz = chrono_tz::Asia::Taipei;
        let prod = json!({"env":"prod"});
        let prod = prod.as_object();
        // Wednesday 23:00 Taipei: inside the nightly window.
        let wed_23 = Utc.with_ymd_and_hms(2026, 10, 14, 15, 0, 0).unwrap();
        assert_eq!(allowed(&ws, "c-m-1", prod, wed_23, &tz), Some(true));
        // Wednesday 15:00 Taipei: outside.
        let wed_15 = Utc.with_ymd_and_hms(2026, 10, 14, 7, 0, 0).unwrap();
        assert_eq!(allowed(&ws, "c-m-1", prod, wed_15, &tz), Some(false));
        // Saturday 11:00 Taipei: the one-off change window CHG-1 is open.
        let sat_11 = Utc.with_ymd_and_hms(2026, 10, 17, 3, 0, 0).unwrap();
        assert_eq!(allowed(&ws, "c-m-1", prod, sat_11, &tz), Some(true));
        // Not covered: another cluster, a dev namespace, a cluster-scoped object.
        assert_eq!(allowed(&ws, "local", prod, wed_15, &tz), None);
        assert_eq!(allowed(&ws, "c-m-1", json!({"env":"dev"}).as_object(), wed_15, &tz), None);
        assert_eq!(allowed(&ws, "c-m-1", None, wed_15, &tz), None);
        assert!(parse("").unwrap().is_empty());
    }
}
