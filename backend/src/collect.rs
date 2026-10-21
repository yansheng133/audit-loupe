//! Collectors: read audit records where they are written and push them to the server.
//!
//! `collect-rancher` follows the Rancher API audit log through the Kubernetes log API of each
//! Rancher pod's `rancher-audit-log` sidecar, which copies the log file to its output.
//!
//! `collect-file` follows the kube-apiserver audit log on an RKE2 control-plane node.
//! It remembers which file it is on (inode plus a hash of the file's first bytes, so a reused
//! inode is not taken for the same file), how far the server has acknowledged, and the time of
//! the last record acknowledged. Only what is read while the collector runs is sent:
//! - A restart starts at the end of the current `audit.log`; what was written while the
//!   collector was down is not read afterwards, and that period is reported to the server as
//!   not collected, from the last record acknowledged to the start.
//! - A rotation while it runs is followed: the rotated file is read to its end, then each newer
//!   rotated file in order, then `audit.log`.
//! - A file that disappears (or is replaced under the same inode) before it was read to the
//!   end is reported as not collected at once, from the last record acknowledged.

use anyhow::Context;
use chrono::{DateTime, Utc};
use futures::{AsyncBufReadExt, TryStreamExt};
use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, ListParams, LogParams};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{info, warn};

/// Largest batch read from the file at once (uncompressed).
pub const MAX_BATCH: usize = 1 << 20;

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| default.to_string())
}

/// Sends batches to the server's ingest endpoint.
pub struct Sender {
    client: reqwest::Client,
    url: String,
    token: String,
    source: &'static str,
}

impl Sender {
    pub fn from_env(source: &'static str) -> anyhow::Result<Self> {
        let base = std::env::var("AL_INGEST_URL").context("AL_INGEST_URL is required, e.g. https://audit-loupe.example.com")?;
        let token_file = env_or("AL_TOKEN_FILE", "/etc/audit-loupe/token/token");
        let token = fs::read_to_string(&token_file).with_context(|| format!("reading {token_file}"))?.trim().to_string();
        let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(30));
        if let Ok(ca) = std::env::var("AL_INGEST_CA_FILE") {
            let pem = fs::read(&ca).with_context(|| format!("reading {ca}"))?;
            builder = builder.add_root_certificate(reqwest::Certificate::from_pem(&pem)?);
        }
        Ok(Sender {
            client: builder.build()?,
            url: format!("{}/ingest/v1/batch", base.trim_end_matches('/')),
            token,
            source,
        })
    }

    pub async fn send(&self, lines: &[u8]) -> anyhow::Result<()> {
        self.send_for_reply(lines, &[]).await.map(|_| ())
    }

    /// Ask the server whether it has heard from this collector instance before.
    pub async fn known(&self, collector: &str, instance: &str) -> anyhow::Result<Option<i64>> {
        let url = self.url.replace("/ingest/v1/batch", "/ingest/v1/known");
        let res = self.client.get(&url).bearer_auth(&self.token).query(&[("collector", collector), ("instance", instance)]).send().await?;
        let status = res.status();
        if !status.is_success() {
            anyhow::bail!("server answered {status}");
        }
        let v: serde_json::Value = serde_json::from_str(&res.text().await?)?;
        Ok(v.get("lastMs").and_then(|t| t.as_i64()).or(if v["known"] == true { Some(0) } else { None }))
    }

    /// Like `send`, with extra headers, and returns the server's answer (for ingest:
    /// received/skipped/stored).
    pub async fn send_for_reply(&self, lines: &[u8], extra: &[(&str, String)]) -> anyhow::Result<String> {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(lines)?;
        let mut req = self.client.post(&self.url);
        for (k, v) in extra {
            req = req.header(*k, v);
        }
        let res = req
            .bearer_auth(&self.token)
            .header("x-audit-loupe-source", self.source)
            .header("content-encoding", "gzip")
            .header("content-type", "application/x-ndjson")
            .body(enc.finish()?)
            .send()
            .await?;
        let status = res.status();
        if !status.is_success() {
            let text = res.text().await.unwrap_or_default();
            anyhow::bail!("server answered {status}: {}", text.chars().take(200).collect::<String>());
        }
        Ok(res.text().await.unwrap_or_default())
    }
}

/// How many bytes at the start of a file identify it, together with its inode.
const HEAD_BYTES: u64 = 1024;

/// Where collect-file stands. `head` identifies the file beyond its inode; `last_ms` is the
/// time of the last record the server acknowledged (where a "not collected" period starts);
/// `pending` holds such reports until the server has taken them.
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
pub struct Position {
    pub inode: u64,
    pub offset: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<Head>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<Gap>,
    /// When the collector last found nothing left to read: it had everything up to then, even
    /// if the last record is older (a quiet cluster).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caught_up_ms: Option<i64>,
    /// The name the file being read was last seen under (after a rotation: `audit-<time>.log`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// SHA-256 of a file's first `len` bytes (up to HEAD_BYTES).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Head {
    pub len: u64,
    pub sha256: String,
}

/// A period the collector did not collect, as sent to the server (source "gap").
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Gap {
    pub collector: String,
    pub instance: String,
    pub from_ms: i64,
    pub to_ms: i64,
    pub reason: String,
    #[serde(default)]
    pub detail: String,
}

impl Position {
    pub fn load(path: &Path) -> Position {
        fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }
    /// Write to a temp file and rename, so a crash never leaves a half-written position.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, serde_json::to_vec(self)?)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }
    /// Start `file` from `offset`, keeping the time of the last record and pending reports.
    fn at(&self, file: &Path, offset: u64) -> anyhow::Result<Position> {
        let mut f = File::open(file)?;
        Ok(self.on(&mut f, offset)?)
    }
    /// Like `at`, for a file already open: inode and first bytes come from the same handle.
    fn on(&self, f: &mut File, offset: u64) -> std::io::Result<Position> {
        Ok(Position { inode: f.metadata()?.ino(), offset, head: Some(head_of_file(f)?), name: None, ..self.clone() })
    }
    /// Where a "not collected" period starts: the later of the last record acknowledged and
    /// the last time everything had been read.
    fn collected_until(&self) -> Option<i64> {
        match (self.last_ms, self.caught_up_ms) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }
}

fn sha256_hex(b: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

/// `head_of` for an open file (leaves the read position at the end of the head).
fn head_of_file(f: &mut File) -> std::io::Result<Head> {
    let mut buf = Vec::new();
    f.seek(SeekFrom::Start(0))?;
    Read::by_ref(f).take(HEAD_BYTES).read_to_end(&mut buf)?;
    Ok(Head { len: buf.len() as u64, sha256: sha256_hex(&buf) })
}

fn head_matches(f: &mut File, head: &Head) -> bool {
    let mut buf = Vec::new();
    f.seek(SeekFrom::Start(0)).is_ok() && Read::by_ref(f).take(head.len).read_to_end(&mut buf).is_ok() && buf.len() as u64 == head.len && sha256_hex(&buf) == head.sha256
}

/// Files rotated out of `path` (`audit-<UTC time>.log` next to `audit.log`), oldest first;
/// the time in the name sorts as text.
pub fn rotated_files(path: &Path) -> Vec<PathBuf> {
    let (Some(dir), Some(stem)) = (path.parent(), path.file_stem().and_then(|s| s.to_str())) else { return vec![] };
    let prefix = format!("{stem}-");
    let suffix = path.extension().and_then(|e| e.to_str()).map(|e| format!(".{e}")).unwrap_or_default();
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(&prefix) && n.ends_with(&suffix)))
        .collect();
    v.sort();
    v
}

/// Rotated files oldest first, then the live file: the order the records were written in.
fn in_write_order(path: &Path) -> Vec<PathBuf> {
    let mut v = rotated_files(path);
    test_delay();
    if path.exists() {
        v.push(path.to_path_buf());
    }
    v
}

#[cfg(test)]
/// The file the position refers to: same inode and, when known, the same first bytes.
pub fn locate(path: &Path, pos: &Position) -> Option<PathBuf> {
    locate_open(path, pos).map(|(p, _)| p)
}

/// `locate`, returning the file opened: inode (fstat) and first bytes are checked on that
/// handle, so a rename between the listing and the check cannot swap the file under it.
fn locate_open(path: &Path, pos: &Position) -> Option<(PathBuf, File)> {
    in_write_order(path).into_iter().find_map(|p| {
        let mut f = File::open(&p).ok()?;
        let ok = f.metadata().is_ok_and(|m| m.ino() == pos.inode) && pos.head.as_ref().is_none_or(|h| head_matches(&mut f, h));
        ok.then_some((p, f))
    })
}

#[cfg(test)]
/// The files written after `file`, in order.
fn newer_than(path: &Path, file: &Path) -> Vec<PathBuf> {
    let all = in_write_order(path);
    match all.iter().position(|p| p == file) {
        Some(i) => all[i + 1..].to_vec(),
        None => vec![],
    }
}

/// The time of an audit record (the one `audit::to_record` stamps it with, near enough).
fn record_ms(line: &[u8]) -> Option<i64> {
    let v: serde_json::Value = serde_json::from_slice(line).ok()?;
    let t = v.get("stageTimestamp").or_else(|| v.get("requestReceivedTimestamp"))?.as_str()?;
    Some(DateTime::parse_from_rfc3339(t).ok()?.timestamp_millis())
}

/// The time of the last record in a chunk of whole lines.
fn last_record_ms(chunk: &[u8]) -> Option<i64> {
    chunk.split(|b| *b == b'\n').rev().find_map(record_ms)
}

/// The time of the first record in a file.
fn first_record_ms(path: &Path) -> Option<i64> {
    let (chunk, _) = read_lines(path, 0, 64 << 10).ok()?;
    chunk.split(|b| *b == b'\n').find_map(record_ms)
}

/// Offset just after the last complete line: where a restart starts reading.
pub fn end_of_last_line(path: &Path) -> anyhow::Result<u64> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    let mut end = len;
    let mut buf = vec![0u8; 64 << 10];
    while end > 0 {
        let start = end.saturating_sub(buf.len() as u64);
        let n = (end - start) as usize;
        f.seek(SeekFrom::Start(start))?;
        f.read_exact(&mut buf[..n])?;
        if let Some(i) = buf[..n].iter().rposition(|b| *b == b'\n') {
            return Ok(start + i as u64 + 1);
        }
        end = start;
    }
    Ok(0)
}

/// Read complete lines starting at `offset`, at most `max` bytes.
/// Returns the bytes and how many were consumed; a trailing partial line is left for later.
/// A single line longer than `max` is returned whole so it cannot block the file forever.
pub fn read_lines(path: &Path, offset: u64, max: usize) -> anyhow::Result<(Vec<u8>, u64)> {
    read_lines_from(&mut File::open(path)?, offset, max)
}

/// `read_lines` on an open file.
pub fn read_lines_from(f: &mut File, offset: u64, max: usize) -> anyhow::Result<(Vec<u8>, u64)> {
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = Vec::new();
    Read::by_ref(f).take(max as u64).read_to_end(&mut buf)?;
    match buf.iter().rposition(|b| *b == b'\n') {
        Some(i) => {
            buf.truncate(i + 1);
            let n = buf.len() as u64;
            Ok((buf, n))
        }
        None if buf.len() == max => {
            // One very long line: read until its end.
            let mut rest = Vec::new();
            let mut byte = [0u8; 1];
            loop {
                if f.read(&mut byte)? == 0 {
                    return Ok((Vec::new(), 0)); // still incomplete
                }
                rest.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
            buf.extend(rest);
            let n = buf.len() as u64;
            Ok((buf, n))
        }
        None => Ok((Vec::new(), 0)),
    }
}

/// What a collector reports about itself (coverage page): every minute, or every
/// AL_HEARTBEAT_SECS seconds.
pub type Health = Arc<Mutex<serde_json::Map<String, serde_json::Value>>>;

pub fn set(h: &Health, key: &str, v: serde_json::Value) {
    h.lock().unwrap().insert(key.to_string(), v);
}

fn instance_name() -> String {
    std::env::var("NODE_NAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "unknown".into())
}

/// Send a heartbeat every minute, with or without data, so the server can tell a quiet
/// cluster from a collector that stopped.
pub fn heartbeat(collector: &'static str) -> anyhow::Result<Health> {
    let sender = Sender::from_env("heartbeat")?;
    let health: Health = Default::default();
    let h = health.clone();
    let instance = instance_name();
    // The server counts three missed minutes (180 s) as a gap; beats further apart than that
    // would show gaps that are not there.
    let every = env_or("AL_HEARTBEAT_SECS", "60").parse::<u64>().unwrap_or(60).clamp(1, 120);
    tokio::spawn(async move {
        // Let the collector fill in its report before the first beat.
        tokio::time::sleep(Duration::from_secs(3.min(every))).await;
        loop {
            let details = serde_json::Value::Object(h.lock().unwrap().clone());
            let body = serde_json::json!({"collector": collector, "instance": instance, "version": env!("CARGO_PKG_VERSION"), "details": details});
            if let Err(e) = sender.send(body.to_string().as_bytes()).await {
                warn!(error = %e, "heartbeat failed");
            }
            tokio::time::sleep(Duration::from_secs(every)).await;
        }
    });
    Ok(health)
}

/// Where the file with inode `ino` stands now: the name it has, and the bytes of every file
/// written after it (for lagBytes). None when it is no longer in the directory.
fn place(path: &Path, ino: u64) -> (Option<String>, u64) {
    let all = in_write_order(path);
    match all.iter().position(|p| fs::metadata(p).is_ok_and(|m| m.ino() == ino)) {
        Some(i) => {
            let name = all[i].file_name().map(|n| n.to_string_lossy().into_owned());
            (name, all[i + 1..].iter().map(|p| fs::metadata(p).map(|m| m.len()).unwrap_or(0)).sum())
        }
        None => (None, 0),
    }
}

/// Bytes not yet acknowledged: the rest of the file being read (`remaining`, from its open
/// handle) and every file written after it.
#[cfg(test)]
fn lag_bytes(path: &Path, ino: u64, remaining: u64) -> u64 {
    remaining + place(path, ino).1
}

/// The UTC time in a rotated file's name (`audit-2026-10-20T06-00-00.000.log`): when it was
/// rotated, which is also when the next file got its first record.
fn rotated_at_ms(name: &str) -> Option<i64> {
    let stamp = name.get(name.find('-')? + 1..name.len() - 4)?;
    let dt = chrono::NaiveDateTime::parse_from_str(stamp, "%Y-%m-%dT%H-%M-%S%.3f").ok()?;
    Some(dt.and_utc().timestamp_millis())
}

/// How far the first record of the next file may come after the rotation time and still be
/// the next file (kube-apiserver rotates on the write that goes into the new file).
const NEXT_FILE_SLACK_MS: i64 = 5_000;

/// The file written after the one being read (`pos`), opened, and a report when files in
/// between were removed before they could be read. None while there is no next file yet (or
/// the listing caught a rename half-way; the caller looks again).
fn next_after(path: &Path, pos: &Position) -> Option<(PathBuf, File, Option<Gap>)> {
    let all = in_write_order(path);
    let ours = all.iter().position(|p| fs::metadata(p).is_ok_and(|m| m.ino() == pos.inode));
    let (next, lost) = match ours {
        Some(i) => (all.get(i + 1).cloned()?, None),
        None => {
            // Ours left the directory (rotated out past maxbackup) after it was read to the end
            // through the open handle. Files go oldest first, so the oldest one left newer than
            // ours comes next; if it does not start where ours was rotated, files in between
            // went too, unread.
            let after = |p: &PathBuf| match (&pos.name, p.file_name().and_then(|n| n.to_str())) {
                (Some(ours), Some(n)) if rotated_at_ms(ours).is_some() => p == path || n > ours.as_str(),
                _ => last_record_ms_of(p).is_some_and(|t| pos.last_ms.is_none_or(|l| t > l)),
            };
            let next = all.iter().find(|p| after(p)).cloned().unwrap_or_else(|| path.to_path_buf());
            let first = first_record_ms(&next);
            let rotated = pos.name.as_deref().and_then(rotated_at_ms);
            let lost = match (rotated, first) {
                (Some(t), Some(f)) if f > t + NEXT_FILE_SLACK_MS => Some(gap(t, f, "file-removed", "rotated files removed before they were read")),
                (None, Some(f)) => pos.collected_until().filter(|c| f > c + NEXT_FILE_SLACK_MS).map(|c| gap(c, f, "file-removed", "rotated name not seen; may be overstated")),
                _ => None,
            };
            (next, lost)
        }
    };
    let f = File::open(&next).ok()?;
    (f.metadata().ok()?.ino() != pos.inode).then_some((next, f, lost))
}

/// Test hook: widen the window between listing the directory and opening a file.
fn test_delay() {
    if let Some(ms) = std::env::var("AL_TEST_LIST_DELAY_MS").ok().and_then(|v| v.parse::<u64>().ok()) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

/// The file to go on with after a loss: the oldest one holding a record after the last one
/// acknowledged (audit times are not strictly increasing, so the file's last record decides,
/// not its first), else `audit.log`. Without a time, `audit.log`.
fn resume_after_loss(path: &Path, last_ms: Option<i64>) -> PathBuf {
    let Some(l) = last_ms else { return path.to_path_buf() };
    let after = |p: &PathBuf| last_record_ms_of(p).is_some_and(|t| t > l);
    in_write_order(path).into_iter().find(after).unwrap_or_else(|| path.to_path_buf())
}

/// The time of the last record in a file (from its last 64 KiB).
fn last_record_ms_of(path: &Path) -> Option<i64> {
    let mut f = File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(64 << 10))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    last_record_ms(&buf)
}

fn gap(from_ms: i64, to_ms: i64, reason: &str, detail: &str) -> Gap {
    // A loss always covers some time: never report a zero-length period.
    let to_ms = if reason == crate::store::START { to_ms.max(from_ms) } else { to_ms.max(from_ms + 1) };
    Gap { collector: "file".into(), instance: instance_name(), from_ms, to_ms, reason: reason.into(), detail: detail.into() }
}

/// What `collect-file` does when it has no state at all and the server has never heard from
/// this node (AL_ON_FIRST_START): "fromNow" (default) starts at the end of `audit.log`;
/// "readExisting" reads the current `audit.log` from its start (rotated files are not read).
/// Either way the server is told where collection starts.
fn read_existing() -> bool {
    env_or("AL_ON_FIRST_START", "fromNow").eq_ignore_ascii_case("readExisting")
}

/// Follow the audit log forever.
pub async fn run_file() -> anyhow::Result<()> {
    let sender = Sender::from_env("k8s-audit")?;
    let gaps = Sender::from_env("gap")?;
    let path = PathBuf::from(env_or("AL_AUDIT_LOG", "/var/lib/rancher/rke2/server/logs/audit.log"));
    let state_dir = PathBuf::from(env_or("AL_STATE_DIR", "/var/lib/audit-loupe"));
    fs::create_dir_all(&state_dir)?;
    let state = state_dir.join("position.json");
    let instance = instance_name();
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut pos = Position::load(&state);
    if pos == Position::default() {
        // No state: a first install, or the state directory was lost. Only the server can tell;
        // ask before the first heartbeat makes this node known.
        let known = loop {
            match gaps.known("file", &instance).await {
                Ok(k) => break k,
                Err(e) => {
                    warn!(error = %e, "cannot ask the server whether this node is new; retrying");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        };
        while !path.exists() {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        let end = end_of_last_line(&path)?;
        let started_ms = Utc::now().timestamp_millis();
        let file = name(&path);
        pos = match known {
            Some(last) => {
                let from = Some(last).filter(|t| *t > 0).unwrap_or(started_ms);
                warn!(from_ms = from, "state lost but the server knows this node: reading from now on; the time since its last check-in is reported as not collected");
                pos.pending.push(gap(from, started_ms, "collector-state-lost", &file));
                Position { last_ms: Some(started_ms), ..pos }.at(&path, end)?
            }
            None if read_existing() => {
                let from = first_record_ms(&path).unwrap_or(started_ms);
                info!("first start: reading the existing audit.log from its start (rotated files are not read)");
                pos.pending.push(gap(from, started_ms, crate::store::START, "readExisting"));
                pos.at(&path, 0)?
            }
            None => {
                info!("first start: collecting from now on");
                pos.pending.push(gap(started_ms, started_ms, crate::store::START, "fromNow"));
                Position { last_ms: Some(started_ms), ..pos }.at(&path, end)?
            }
        };
        pos.save(&state)?;
    } else if pos.inode != 0 {
        // A restart: what was written while the collector was down is not read. It had
        // everything up to the later of its last record and the last time it was caught up;
        // a state file from before this has neither, so its last write stands in.
        let end = if path.exists() { Some(end_of_last_line(&path)?) } else { None };
        let started_ms = Utc::now().timestamp_millis();
        let from = pos.collected_until().or_else(|| {
            fs::metadata(&state).ok()?.modified().ok().map(|t| DateTime::<Utc>::from(t).timestamp_millis())
        });
        let from = from.unwrap_or(started_ms);
        pos.pending.push(gap(from, started_ms, "collector-restart", &name(&path)));
        pos.last_ms = Some(started_ms);
        pos.caught_up_ms = None;
        pos = match end {
            Some(end) => pos.at(&path, end)?,
            // No audit.log yet: the next one to appear is read from its start.
            None => Position { last_ms: pos.last_ms, pending: pos.pending, ..Default::default() },
        };
        warn!(from_ms = from, to_ms = started_ms, offset = pos.offset, "restarted: reading from now on; the downtime is reported as not collected");
        pos.save(&state)?;
    }
    let health = heartbeat("file")?;
    info!(file = %path.display(), offset = pos.offset, "following audit log");
    // The file being read stays open: a rename (rotation) or removal does not take it away,
    // and its inode and first bytes are always checked on this same handle.
    let mut cur: Option<(PathBuf, File)> = None;
    let mut backoff = 1u64;
    let (mut gap_backoff, mut gap_next) = (1u64, tokio::time::Instant::now());
    let mut caught_up_saved = 0i64;
    loop {
        if !pos.pending.is_empty() && tokio::time::Instant::now() >= gap_next {
            let before = pos.pending.len();
            send_gaps(&gaps, &mut pos.pending).await;
            if pos.pending.len() < before {
                save_or_warn(&pos, &state);
            }
            if pos.pending.is_empty() {
                gap_backoff = 1;
            } else {
                gap_next = tokio::time::Instant::now() + Duration::from_secs(gap_backoff);
                gap_backoff = (gap_backoff * 2).min(300);
            }
        }
        set(&health, "auditLog", serde_json::json!(if path.exists() { "present" } else { "missing" }));
        if pos.inode == 0 {
            // A restart that found no file: read the next audit.log from its start.
            match File::open(&path) {
                Ok(mut f) => match pos.on(&mut f, 0) {
                    Ok(p) => {
                        pos = p;
                        cur = Some((path.clone(), f));
                        save_or_warn(&pos, &state);
                    }
                    Err(e) => warn!(error = %e, "cannot read the new audit.log yet; will retry"),
                },
                Err(_) => tokio::time::sleep(Duration::from_secs(5)).await,
            }
            continue;
        }
        if cur.is_none() {
            // Find the file by inode and first bytes; a rotation half-way through the listing
            // can hide it, so look once more before calling it lost.
            cur = locate_open(&path, &pos);
            if cur.is_none() {
                tokio::time::sleep(Duration::from_millis(300)).await;
                cur = locate_open(&path, &pos);
            }
            if cur.is_none() {
                let replaced = in_write_order(&path).iter().any(|p| fs::metadata(p).is_ok_and(|m| m.ino() == pos.inode));
                lose(&mut pos, &path, if replaced { "file-replaced" } else { "file-removed" });
                save_or_warn(&pos, &state);
                continue;
            }
        }
        let (source, f) = cur.as_mut().expect("set above");
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < pos.offset {
            // Shorter than what was already read: truncated in place, not the same file.
            cur = None;
            lose(&mut pos, &path, "file-replaced");
            save_or_warn(&pos, &state);
            continue;
        }
        if pos.head.as_ref().is_some_and(|h| !head_matches(f, h)) {
            // Same inode, other first bytes: rewritten in place, not the file that was read.
            cur = None;
            lose(&mut pos, &path, "file-replaced");
            save_or_warn(&pos, &state);
            continue;
        }
        let (seen_as, after) = place(&path, pos.inode);
        if seen_as.is_some() {
            pos.name = seen_as;
        }
        set(&health, "lagBytes", serde_json::json!(len - pos.offset + after));
        if pos.head.as_ref().is_none_or(|h| h.len < HEAD_BYTES) {
            // The first bytes are recorded as the file grows, up to HEAD_BYTES.
            if let Ok(h) = head_of_file(f) {
                pos.head = Some(h);
            }
        }
        let (chunk, consumed) = match read_lines_from(f, pos.offset, MAX_BATCH) {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, file = %source.display(), "reading failed; will find the file again");
                cur = None;
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        let at = |source: &Path, offset: u64| vec![("x-audit-loupe-instance", instance.clone()), ("x-audit-loupe-file", name(source)), ("x-audit-loupe-offset", offset.to_string())];
        if chunk.is_empty() {
            let live = fs::metadata(&path).map(|m| m.ino()).ok();
            if live == Some(pos.inode) {
                // Caught up with audit.log: everything written until now has been read.
                let now = Utc::now().timestamp_millis();
                pos.caught_up_ms = Some(now);
                if now - caught_up_saved > 30_000 {
                    save_or_warn(&pos, &state);
                    caught_up_saved = now;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            // Rotated out and read to its end. Bytes after the last newline are a record cut
            // short; the server sets it aside (recorded, never silently dropped).
            if len > pos.offset {
                let mut tail = Vec::new();
                if f.seek(SeekFrom::Start(pos.offset)).is_ok() && Read::by_ref(f).take(len - pos.offset).read_to_end(&mut tail).is_ok() {
                    tail.push(b'\n');
                    match sender.send_for_reply(&tail, &at(source, pos.offset)).await {
                        Ok(_) => warn!(file = %source.display(), bytes = tail.len() - 1, "rotated file ended with an incomplete line; set aside"),
                        Err(e) => {
                            warn!(error = %e, "send failed; will retry");
                            tokio::time::sleep(Duration::from_secs(backoff)).await;
                            backoff = (backoff * 2).min(60);
                            continue;
                        }
                    }
                }
            }
            match next_after(&path, &pos) {
                Some((next, mut nf, lost)) => {
                    info!(finished = %source.display(), next = %next.display(), "rotated file read to the end");
                    if let Some(g) = lost {
                        warn!(from_ms = g.from_ms, to_ms = g.to_ms, "rotated files were removed before they were read; reporting the loss");
                        pos.pending.push(g);
                    }
                    match pos.on(&mut nf, 0) {
                        Ok(p) => {
                            pos = p;
                            cur = Some((next, nf));
                        }
                        // Gone again before it could be read: look for the next file once more.
                        Err(e) => warn!(error = %e, next = %next.display(), "cannot read the next file; will look again"),
                    }
                    save_or_warn(&pos, &state);
                }
                None => tokio::time::sleep(Duration::from_millis(300)).await,
            }
            continue;
        }
        match sender.send_for_reply(&chunk, &at(source, pos.offset)).await {
            Ok(reply) => {
                pos.offset += consumed;
                if let Some(t) = last_record_ms(&chunk) {
                    pos.last_ms = Some(t);
                }
                save_or_warn(&pos, &state);
                backoff = 1;
                set(&health, "lastSentMs", serde_json::json!(Utc::now().timestamp_millis()));
                let skipped = serde_json::from_str::<serde_json::Value>(&reply).ok().and_then(|v| v.get("skipped")?.as_u64()).unwrap_or(0);
                if skipped > 0 {
                    warn!(skipped, file = %source.display(), "the server set aside lines it could not read; they are recorded there");
                }
            }
            Err(e) => {
                warn!(error = %e, retry_in = backoff, "send failed; will resend the same lines");
                tokio::time::sleep(Duration::from_secs(backoff)).await;
                backoff = (backoff * 2).min(60);
            }
        }
    }
}

/// The file being read is gone before it was read to the end: removed (rotated out past
/// maxbackup) or replaced. What was left of it is lost; report it now and go on with the
/// next file that has newer records.
fn lose(pos: &mut Position, path: &Path, reason: &str) {
    let next = resume_after_loss(path, pos.last_ms);
    let now = Utc::now().timestamp_millis();
    let from = pos.collected_until().unwrap_or(now);
    let to = first_record_ms(&next).unwrap_or(now);
    warn!(reason, from_ms = from, to_ms = to, next = %next.display(), "the file being read is gone before it was read to the end; reporting the loss");
    pos.pending.push(gap(from, to, reason, ""));
    pos.caught_up_ms = None;
    *pos = match pos.at(&next, 0) {
        Ok(p) => p,
        // Nothing to open yet: wait for audit.log as after a restart without a file.
        Err(_) => Position { last_ms: pos.last_ms, pending: std::mem::take(&mut pos.pending), ..Default::default() },
    };
}

/// A failed save is retried with the next one; the collector does not stop for it.
fn save_or_warn(pos: &Position, state: &Path) {
    if let Err(e) = pos.save(state) {
        warn!(error = %e, "could not save the read position; will try again");
    }
}

/// Send each report; one the server took is removed at once, so a later failure does not
/// resend it (the server also ignores a report it already has).
async fn send_gaps(sender: &Sender, pending: &mut Vec<Gap>) {
    while let Some(g) = pending.first() {
        let body = match serde_json::to_string(g) {
            Ok(b) => b,
            Err(_) => {
                pending.remove(0);
                continue;
            }
        };
        match sender.send(body.as_bytes()).await {
            Ok(()) => {
                pending.remove(0);
            }
            Err(e) => {
                warn!(error = %e, "reporting a not-collected period failed; will retry");
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// collect-rancher
// ---------------------------------------------------------------------------------------

/// How far each Rancher pod's log has been delivered: pod UID -> kubelet time of the last
/// line the server acknowledged. A new pod has a new UID and is read from its start.
pub type Positions = HashMap<String, DateTime<Utc>>;

fn load_positions(path: &Path) -> Positions {
    fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_positions(path: &Path, p: &Positions) -> anyhow::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec(p)?)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// With `timestamps=true` every line starts with the kubelet's RFC3339Nano time and a space.
pub fn split_timestamp(line: &str) -> Option<(DateTime<Utc>, &str)> {
    let (ts, rest) = line.split_once(' ')?;
    Some((DateTime::parse_from_rfc3339(ts).ok()?.with_timezone(&Utc), rest))
}

/// One JSON line for the server. `mark` is (stream, time) for collectors that resume by time:
/// once the line is acknowledged, that stream's position moves up to `time`.
pub struct Line {
    pub mark: Option<(String, DateTime<Utc>)>,
    pub text: String,
}

/// Follow one pod's sidecar output until the stream ends (container restart, pod gone).
async fn follow_pod(pods: Api<Pod>, name: String, uid: String, container: String, since: Option<DateTime<Utc>>, tx: tokio::sync::mpsc::Sender<Line>) -> anyhow::Result<()> {
    // sinceTime goes out rounded to the second, so start a second early and drop what was
    // already delivered here. Lines with the same time as the position are sent again;
    // the server keeps one copy per auditID.
    let lp = LogParams {
        container: Some(container),
        follow: true,
        timestamps: true,
        since_time: since.map(|t| jiff::Timestamp::from_second(t.timestamp() - 1)).transpose()?,
        ..Default::default()
    };
    let mut lines = pods.log_stream(&name, &lp).await?.lines();
    while let Some(line) = lines.try_next().await? {
        let Some((ts, text)) = split_timestamp(&line) else { continue };
        if since.is_some_and(|s| ts < s) {
            continue;
        }
        // The sidecar is `tail -F`; its own notices ("file truncated") are not entries.
        if !text.trim_start().starts_with('{') {
            continue;
        }
        tx.send(Line { mark: Some((uid.clone(), ts)), text: text.to_string() }).await?;
    }
    Ok(())
}

/// Collect lines into batches (up to MAX_BATCH, or 2 s after the first line), send each until
/// the server accepts it, then record the new positions if the collector keeps any. While a
/// batch is being retried the channel fills up and the producers wait, so nothing is dropped.
pub async fn batch_and_send(mut rx: tokio::sync::mpsc::Receiver<Line>, sender: Sender, positions: Option<(Arc<Mutex<Positions>>, PathBuf)>) {
    let mut buf: Vec<u8> = Vec::new();
    let mut marks: Positions = HashMap::new();
    let add = |l: Line, buf: &mut Vec<u8>, marks: &mut Positions| {
        buf.extend_from_slice(l.text.as_bytes());
        buf.push(b'\n');
        if let Some((stream, ts)) = l.mark {
            let m = marks.entry(stream).or_insert(ts);
            *m = (*m).max(ts);
        }
    };
    loop {
        let Some(first) = rx.recv().await else { return };
        add(first, &mut buf, &mut marks);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while buf.len() < MAX_BATCH {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(l)) => add(l, &mut buf, &mut marks),
                _ => break,
            }
        }
        let mut backoff = 1u64;
        while let Err(e) = sender.send(&buf).await {
            warn!(error = %e, retry_in = backoff, "send failed; will resend the same lines");
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(60);
        }
        if let Some((positions, state)) = &positions {
            let snapshot = {
                let mut p = positions.lock().unwrap();
                for (stream, ts) in marks.drain() {
                    let e = p.entry(stream).or_insert(ts);
                    *e = (*e).max(ts);
                }
                p.clone()
            };
            if let Err(e) = save_positions(state, &snapshot) {
                warn!(error = %e, "could not save positions; a restart will resend some lines");
            }
        }
        marks.clear();
        buf.clear();
    }
}

fn container_running(pod: &Pod, container: &str) -> bool {
    pod.status
        .as_ref()
        .and_then(|s| s.container_statuses.as_ref())
        .is_some_and(|cs| cs.iter().any(|c| c.name == container && c.state.as_ref().is_some_and(|st| st.running.is_some())))
}

/// Find the Rancher pods every 15 s and keep one follower per pod.
pub async fn run_rancher() -> anyhow::Result<()> {
    let sender = Sender::from_env("rancher-audit")?;
    let health = heartbeat("rancher")?;
    let ns = env_or("AL_RANCHER_NAMESPACE", "cattle-system");
    let selector = env_or("AL_RANCHER_SELECTOR", "app=rancher");
    let container = env_or("AL_RANCHER_CONTAINER", "rancher-audit-log");
    let state_dir = PathBuf::from(env_or("AL_STATE_DIR", "/var/lib/audit-loupe"));
    fs::create_dir_all(&state_dir)?;
    let state = state_dir.join("rancher-positions.json");
    let positions = Arc::new(Mutex::new(load_positions(&state)));
    let pods: Api<Pod> = Api::namespaced(kube::Client::try_default().await?, &ns);
    let (tx, rx) = tokio::sync::mpsc::channel::<Line>(4096);
    tokio::spawn(batch_and_send(rx, sender, Some((positions.clone(), state))));
    let following: Arc<Mutex<HashSet<String>>> = Default::default();
    info!(namespace = %ns, selector = %selector, container = %container, "following the Rancher audit log");
    let mut warned = false;
    loop {
        match pods.list(&ListParams::default().labels(&selector)).await {
            Err(e) => warn!(error = %e, "listing Rancher pods failed"),
            Ok(list) => {
                let mut seen = HashSet::new();
                for pod in list.items {
                    let (Some(name), Some(uid)) = (pod.metadata.name.clone(), pod.metadata.uid.clone()) else { continue };
                    seen.insert(uid.clone());
                    if !container_running(&pod, &container) {
                        if !warned {
                            warn!(pod = %name, container = %container, "no running audit sidecar; is auditLog.enabled set on Rancher?");
                            warned = true;
                        }
                        continue;
                    }
                    if !following.lock().unwrap().insert(uid.clone()) {
                        continue;
                    }
                    let since = positions.lock().unwrap().get(&uid).copied();
                    info!(pod = %name, ?since, "following");
                    let (pods, tx, following, container) = (pods.clone(), tx.clone(), following.clone(), container.clone());
                    tokio::spawn(async move {
                        if let Err(e) = follow_pod(pods, name.clone(), uid.clone(), container, since, tx).await {
                            warn!(pod = %name, error = %e, "log stream ended");
                        }
                        following.lock().unwrap().remove(&uid);
                    });
                }
                set(&health, "podsFollowing", serde_json::json!(following.lock().unwrap().len()));
                set(&health, "sidecarMissing", serde_json::json!(warned));
                // Forget pods that are gone; their logs went with them.
                positions.lock().unwrap().retain(|uid, _| seen.contains(uid));
            }
        }
        tokio::time::sleep(Duration::from_secs(15)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kubelet_timestamps_are_split_off() {
        let (ts, rest) = split_timestamp("2026-10-17T01:02:03.123456789Z {\"auditID\":\"a\"}").unwrap();
        assert_eq!(ts.timestamp_subsec_nanos(), 123_456_789);
        assert_eq!(rest, "{\"auditID\":\"a\"}");
        assert!(split_timestamp("tail: file truncated").is_none());
    }

    #[test]
    fn positions_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("rancher-positions.json");
        assert!(load_positions(&p).is_empty());
        let mut m = Positions::new();
        m.insert("uid-1".into(), "2026-10-17T01:02:03.5Z".parse().unwrap());
        save_positions(&p, &m).unwrap();
        assert_eq!(load_positions(&p), m);
    }

    #[test]
    fn partial_last_line_waits() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("audit.log");
        fs::write(&p, "{\"a\":1}\n{\"b\":2}\n{\"c\":").unwrap();
        let (chunk, n) = read_lines(&p, 0, MAX_BATCH).unwrap();
        assert_eq!(chunk, b"{\"a\":1}\n{\"b\":2}\n");
        assert_eq!(n, 16);
        let (chunk, n) = read_lines(&p, 16, MAX_BATCH).unwrap();
        assert!(chunk.is_empty() && n == 0);
    }

    #[test]
    fn batches_respect_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("audit.log");
        fs::write(&p, "aaaa\nbbbb\ncccc\n").unwrap();
        let (chunk, n) = read_lines(&p, 0, 12).unwrap();
        assert_eq!(chunk, b"aaaa\nbbbb\n");
        let (chunk, _) = read_lines(&p, n, 12).unwrap();
        assert_eq!(chunk, b"cccc\n");
        // A line longer than the limit still comes through whole.
        fs::write(&p, "0123456789abcdef\nx\n").unwrap();
        let (chunk, _) = read_lines(&p, 0, 4).unwrap();
        assert_eq!(chunk, b"0123456789abcdef\n");
    }

    #[test]
    fn rotated_files_in_write_order() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("audit.log");
        for n in ["audit-2026-10-20T02-00-00.000.log", "audit-2026-10-19T23-00-00.000.log", "other.log", "audit.log"] {
            fs::write(dir.path().join(n), "x\n").unwrap();
        }
        let names: Vec<String> = in_write_order(&p).iter().map(|f| f.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["audit-2026-10-19T23-00-00.000.log", "audit-2026-10-20T02-00-00.000.log", "audit.log"]);
        let first = dir.path().join("audit-2026-10-19T23-00-00.000.log");
        assert_eq!(newer_than(&p, &first).len(), 2);
        assert!(newer_than(&p, &p).is_empty());
    }

    #[test]
    fn rotation_is_followed_by_inode_and_first_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("audit.log");
        fs::write(&p, "old1\nold2\n").unwrap();
        let pos = Position::default().at(&p, 5).unwrap();
        assert_eq!(locate(&p, &pos), Some(p.clone()));
        // kube-apiserver renames the file and starts a new one.
        let rotated = dir.path().join("audit-2026-10-17T01-00-00.000.log");
        fs::rename(&p, &rotated).unwrap();
        fs::write(&p, "new1\n").unwrap();
        assert_eq!(locate(&p, &pos), Some(rotated.clone()));
        assert_eq!(lag_bytes(&p, pos.inode, 10 - pos.offset), 5 + 5);
        // Same inode, other content: not the file that was being read.
        fs::write(&rotated, "else\nelse\n").unwrap();
        assert_eq!(locate(&p, &pos), None);
    }

    #[test]
    fn restart_starts_after_the_last_complete_line() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("audit.log");
        fs::write(&p, "{\"a\":1}\n{\"b\":2}\n{\"c\":").unwrap();
        assert_eq!(end_of_last_line(&p).unwrap(), 16);
        fs::write(&p, "no newline yet").unwrap();
        assert_eq!(end_of_last_line(&p).unwrap(), 0);
    }

    #[test]
    fn record_times_come_from_the_audit_event() {
        let chunk = b"{\"stageTimestamp\":\"2026-10-20T00:00:01.5Z\"}\n{\"requestReceivedTimestamp\":\"2026-10-20T00:00:02Z\"}\nbroken\n";
        assert_eq!(last_record_ms(chunk), Some(1792454402000));
        assert_eq!(record_ms(b"broken"), None);
    }

    #[test]
    fn position_round_trips_and_reads_the_old_format() {
        let dir = tempfile::tempdir().unwrap();
        let s = dir.path().join("position.json");
        assert_eq!(Position::load(&s), Position::default());
        let p = Position { inode: 42, offset: 1234, head: Some(Head { len: 3, sha256: "ab".into() }), last_ms: Some(7), pending: vec![], caught_up_ms: Some(9), name: Some("audit.log".into()) };
        p.save(&s).unwrap();
        assert_eq!(Position::load(&s), p);
        // Written by 0.3.1 and earlier.
        fs::write(&s, r#"{"inode":42,"offset":1234}"#).unwrap();
        assert_eq!(Position::load(&s), Position { inode: 42, offset: 1234, ..Default::default() });
    }
}
