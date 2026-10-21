//! `replay`: send recorded audit logs through the normal ingest endpoint, for local
//! prototypes and tests. The files are only read.
//!
//! ```text
//! audit-loupe replay --url http://127.0.0.1:18093 --tokens FILE --cluster c-m-xxxx \
//!     --source k8s-audit [--no-wait] [--speed 600] [--shift 3d | --shift-to 2026-10-21T00:00:00Z] FILE...
//! ```
//!
//! The token for `--cluster` is taken from a server tokens file ("<cluster> <token>" lines)
//! and is never printed. Without `--no-wait` the gaps between records are kept, divided by
//! `--speed` and capped at 5 seconds per pause.
//!
//! `--shift` moves every record by a fixed amount (`90s`, `30m`, `-2h`, `3d`); `--shift-to` moves
//! them so the first record lands at the given time. Shifted records get a new audit ID
//! (`<id>-shift<seconds>`), so the same log can be sent again next to the original.

use crate::collect::{MAX_BATCH, Sender};
use anyhow::Context;
use std::io::{BufRead, BufReader};
use std::time::Duration;

struct Opts {
    url: String,
    tokens: String,
    cluster: String,
    source: &'static str,
    no_wait: bool,
    speed: f64,
    shift: Shift,
    files: Vec<String>,
}

fn parse_args(args: &[String]) -> anyhow::Result<Opts> {
    let mut o = Opts { url: String::new(), tokens: String::new(), cluster: String::new(), source: "k8s-audit", no_wait: false, speed: 600.0, shift: Shift::None, files: vec![] };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--url" => o.url = val()?,
            "--tokens" => o.tokens = val()?,
            "--cluster" => o.cluster = val()?,
            "--source" => {
                o.source = match val()?.as_str() {
                    "k8s-audit" => "k8s-audit",
                    "rancher-audit" => "rancher-audit",
                    other => anyhow::bail!("--source must be k8s-audit or rancher-audit, not {other:?}"),
                }
            }
            "--no-wait" => o.no_wait = true,
            "--speed" => o.speed = val()?.parse().context("--speed")?,
            "--shift" => o.shift = Shift::By(parse_duration_ms(&val()?)?),
            "--shift-to" => {
                let t = chrono::DateTime::parse_from_rfc3339(&val()?).context("--shift-to needs an RFC 3339 time")?;
                o.shift = Shift::FirstTo(t.timestamp_millis());
            }
            f if f.starts_with("--") => anyhow::bail!("unknown option {f}"),
            f => o.files.push(f.to_string()),
        }
    }
    if o.url.is_empty() || o.tokens.is_empty() || o.cluster.is_empty() || o.files.is_empty() {
        anyhow::bail!("usage: replay --url URL --tokens FILE --cluster NAME [--source k8s-audit|rancher-audit] [--no-wait] [--speed N] [--shift D | --shift-to T] FILE...");
    }
    Ok(o)
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Shift {
    None,
    /// Milliseconds to add.
    By(i64),
    /// Move the first record to this time; becomes `By` once the first record is seen.
    FirstTo(i64),
}

/// `90s`, `30m`, `-2h`, `3d`.
fn parse_duration_ms(s: &str) -> anyhow::Result<i64> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.len().saturating_sub(1));
    let n: i64 = num.parse().with_context(|| format!("bad duration {s:?}"))?;
    let mul = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => anyhow::bail!("duration {s:?} must end in s, m, h or d"),
    };
    Ok(n * mul)
}

/// Fields that hold a time, in Kubernetes and Rancher audit records.
const TIME_FIELDS: [&str; 4] = ["requestReceivedTimestamp", "stageTimestamp", "requestTimestamp", "responseTimestamp"];

/// Move a record's times by `delta_ms` and give it a new audit ID.
fn shift_line(line: &[u8], delta_ms: i64) -> Option<Vec<u8>> {
    let mut v: serde_json::Value = serde_json::from_slice(line).ok()?;
    let obj = v.as_object_mut()?;
    for k in TIME_FIELDS {
        if let Some(t) = obj.get(k).and_then(|x| x.as_str()).and_then(|x| chrono::DateTime::parse_from_rfc3339(x).ok()) {
            let moved = t.with_timezone(&chrono::Utc) + chrono::Duration::milliseconds(delta_ms);
            obj.insert(k.to_string(), moved.to_rfc3339_opts(chrono::SecondsFormat::Micros, true).into());
        }
    }
    if let Some(id) = obj.get("auditID").and_then(|x| x.as_str()) {
        let id = format!("{id}-shift{}", delta_ms / 1000);
        obj.insert("auditID".into(), id.into());
    }
    let mut out = serde_json::to_vec(&v).ok()?;
    out.push(b'\n');
    Some(out)
}

/// The token of one cluster from a server tokens file.
fn token_for(path: &str, cluster: &str) -> anyhow::Result<String> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .find_map(|l| {
            let mut p = l.split_whitespace();
            (p.next() == Some(cluster)).then(|| p.next().map(str::to_string)).flatten()
        })
        .with_context(|| format!("no token for cluster {cluster:?} in {path}"))
}

/// When a record happened, for pacing. Kubernetes and Rancher name the field differently.
fn record_ms(line: &[u8]) -> Option<i64> {
    let v: serde_json::Value = serde_json::from_slice(line).ok()?;
    let ts = ["requestReceivedTimestamp", "requestTimestamp", "stageTimestamp"].iter().find_map(|k| v.get(*k)?.as_str())?;
    chrono::DateTime::parse_from_rfc3339(ts).ok().map(|t| t.timestamp_millis())
}

pub async fn run() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(2).collect();
    let o = parse_args(&args)?;
    let sender = Sender::new(&o.url, token_for(&o.tokens, &o.cluster)?, o.source)?;
    let (mut sent, mut batches) = (0usize, 0usize);
    let mut last_ms: Option<i64> = None;
    let mut shift = o.shift;
    for path in &o.files {
        let f = std::fs::File::open(path).with_context(|| format!("opening {path}"))?;
        let mut reader = BufReader::new(f);
        let mut batch: Vec<u8> = Vec::new();
        let mut line = Vec::new();
        loop {
            line.clear();
            let n = reader.read_until(b'\n', &mut line)?;
            if n > 0 && line.trim_ascii().is_empty() {
                continue;
            }
            if n > 0
                && !o.no_wait
                && let Some(t) = record_ms(&line)
            {
                if let Some(prev) = last_ms {
                    let pause = ((t - prev).max(0) as f64 / o.speed).min(5000.0);
                    if pause >= 1.0 {
                        if !batch.is_empty() {
                            sender.send(&batch).await.with_context(|| format!("sending a batch from {path}"))?;
                            batches += 1;
                            batch.clear();
                        }
                        tokio::time::sleep(Duration::from_millis(pause as u64)).await;
                    }
                }
                last_ms = Some(t);
            }
            if n == 0 || batch.len() + line.len() > MAX_BATCH {
                if !batch.is_empty() {
                    sender.send(&batch).await.with_context(|| format!("sending a batch from {path}"))?;
                    batches += 1;
                    batch.clear();
                }
                if n == 0 {
                    break;
                }
            }
            if shift != Shift::None {
                if let Shift::FirstTo(to) = shift {
                    let first = record_ms(&line).with_context(|| format!("{path}: first record has no time to shift from"))?;
                    shift = Shift::By(to - first);
                }
                if let Shift::By(d) = shift {
                    match shift_line(&line, d) {
                        Some(l) => line = l,
                        None => anyhow::bail!("{path}: a record could not be shifted (not a JSON object)"),
                    }
                }
            }
            if !line.ends_with(b"\n") {
                line.push(b'\n');
            }
            batch.extend_from_slice(&line);
            sent += 1;
        }
        eprintln!("replay: {path}: done ({sent} records, {batches} batches so far)");
    }
    eprintln!("replay: finished, {sent} records in {batches} batches to cluster {}", o.cluster);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_options_and_tokens() {
        let a: Vec<String> = ["--url", "http://x", "--tokens", "t", "--cluster", "c1", "--no-wait", "a.log", "b.log"].iter().map(|s| s.to_string()).collect();
        let o = parse_args(&a).unwrap();
        assert!(o.no_wait);
        assert_eq!(o.files, vec!["a.log", "b.log"]);
        assert!(parse_args(&a[..2]).is_err());
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("tokens");
        std::fs::write(&p, "# c\nlocal aaaaaaaaaaaaaaaaaaaa\nc1 bbbbbbbbbbbbbbbbbbbb\n").unwrap();
        assert_eq!(token_for(p.to_str().unwrap(), "c1").unwrap(), "bbbbbbbbbbbbbbbbbbbb");
        assert!(token_for(p.to_str().unwrap(), "c2").is_err());
    }
    #[test]
    fn shifts_times_and_ids() {
        assert_eq!(parse_duration_ms("3d").unwrap(), 3 * 86_400_000);
        assert_eq!(parse_duration_ms("-90s").unwrap(), -90_000);
        assert!(parse_duration_ms("5w").is_err());
        let line = br#"{"auditID":"a1","requestReceivedTimestamp":"2026-10-18T12:00:00.000000Z","stageTimestamp":"2026-10-18T12:00:00.500000Z","verb":"get"}"#;
        let out = shift_line(line, 86_400_000).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["auditID"], "a1-shift86400");
        assert_eq!(v["requestReceivedTimestamp"], "2026-10-19T12:00:00.000000Z");
        assert_eq!(v["stageTimestamp"], "2026-10-19T12:00:00.500000Z");
        assert_eq!(v["verb"], "get");
        assert_eq!(record_ms(&out), Some(1_792_324_800_000 + 86_400_000));
        let a: Vec<String> = ["--url", "u", "--tokens", "t", "--cluster", "c", "--shift-to", "2026-10-21T00:00:00Z", "f"].iter().map(|s| s.to_string()).collect();
        assert!(matches!(parse_args(&a).unwrap().shift, Shift::FirstTo(_)));
    }
    #[test]
    fn finds_record_time() {
        assert_eq!(record_ms(br#"{"requestReceivedTimestamp":"2026-10-18T12:00:00.000Z"}"#), Some(1_792_324_800_000));
        assert_eq!(record_ms(br#"{"x":1}"#), None);
    }
}
