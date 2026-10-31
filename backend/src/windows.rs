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
        let mut day = local.weekday();
        let hour_ok = match self.hours.as_deref().map(crate::config::hours) {
            Some(Ok((a, b))) => {
                let h = local.hour();
                // Hours past midnight belong to the window that opened the day before, so
                // Saturday 01:00 in "1-5" "22-2" is still Friday's window.
                if a > b && h < b {
                    day = day.pred();
                }
                if a < b { h >= a && h < b } else { h >= a || h < b }
            }
            _ => true,
        };
        let day_ok = self
            .days
            .as_deref()
            .is_none_or(|d| crate::config::days(d).unwrap_or_default().contains(&day.number_from_monday()));
        day_ok && hour_ok
    }
}

pub fn parse(json: &str) -> anyhow::Result<Vec<Window>> {
    if json.trim().is_empty() {
        return Ok(vec![]);
    }
    let windows: Vec<Window> = serde_json::from_str(json)?;
    // Checked here, once, so a typo fails the config check instead of making a window
    // silently open on no day or at every hour.
    for w in &windows {
        if let Some(d) = &w.days {
            crate::config::days(d).map_err(|e| anyhow::anyhow!("window {}: days {e}", w.name))?;
        }
        if let Some(h) = &w.hours {
            crate::config::hours(h).map_err(|e| anyhow::anyhow!("window {}: hours {e}", w.name))?;
        }
    }
    Ok(windows)
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

    /// Hours past midnight count for the day the window opened (the values.yaml example,
    /// "1-5" "22-2"): Saturday 01:00 is still Friday's window, Monday 01:00 is outside
    /// (no window opened on Sunday), Friday 01:00 is Thursday's.
    #[test]
    fn hours_past_midnight_belong_to_the_day_before() {
        let ws = parse(r#"[{"name":"nightly","days":"1-5","hours":"22-2"}]"#).unwrap();
        let tz = chrono_tz::Asia::Taipei;
        let at = |d, h| allowed(&ws, "c", None, tz.with_ymd_and_hms(2026, 10, d, h, 0, 0).unwrap().with_timezone(&Utc), &tz);
        assert_eq!(at(17, 1), Some(true), "Saturday 01:00");
        assert_eq!(at(19, 1), Some(false), "Monday 01:00");
        assert_eq!(at(16, 1), Some(true), "Friday 01:00");
        assert_eq!(at(17, 23), Some(false), "Saturday 23:00");
        assert_eq!(at(16, 23), Some(true), "Friday 23:00");
    }

    #[test]
    fn a_window_with_unreadable_days_or_hours_is_refused() {
        for bad in [r#"[{"name":"w","days":"Mon-Fri"}]"#, r#"[{"name":"w","hours":"22:00-02:00"}]"#, r#"[{"name":"w","days":"0-8"}]"#] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
