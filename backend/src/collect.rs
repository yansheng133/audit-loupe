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
        self.send_for_reply(lines).await.map(|_| ())
    }

    /// Like `send`, and returns the server's answer (for ingest: received/skipped/stored).
    pub async fn send_for_reply(&self, lines: &[u8]) -> anyhow::Result<String> {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(lines)?;
        let res = self
            .client
            .post(&self.url)
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
        Ok(Position { inode: fs::metadata(file)?.ino(), offset, head: Some(head_of(file)?), last_ms: self.last_ms, pending: self.pending.clone() })
    }
}

fn sha256_hex(b: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

fn first_bytes(path: &Path, n: u64) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    File::open(path)?.take(n).read_to_end(&mut buf)?;
    Ok(buf)
}

pub fn head_of(path: &Path) -> std::io::Result<Head> {
    let buf = first_bytes(path, HEAD_BYTES)?;
    Ok(Head { len: buf.len() as u64, sha256: sha256_hex(&buf) })
}

/// Whether `path` still starts with the recorded bytes. Shorter than recorded: another file.
pub fn same_head(path: &Path, head: &Head) -> bool {
    first_bytes(path, head.len).is_ok_and(|b| b.len() as u64 == head.len && sha256_hex(&b) == head.sha256)
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
    if path.exists() {
        v.push(path.to_path_buf());
    }
    v
}

/// The file the position refers to: same inode and, when known, the same first bytes.
pub fn locate(path: &Path, pos: &Position) -> Option<PathBuf> {
    in_write_order(path).into_iter().find(|p| {
        fs::metadata(p).is_ok_and(|m| m.ino() == pos.inode) && pos.head.as_ref().is_none_or(|h| same_head(p, h))
    })
}

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
    let mut f = File::open(path)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = Vec::new();
    Read::by_ref(&mut f).take(max as u64).read_to_end(&mut buf)?;
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
    let every = env_or("AL_HEARTBEAT_SECS", "60").parse::<u64>().unwrap_or(60).max(1);
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

/// Bytes not yet acknowledged: the rest of the file being read and every file written after it.
fn lag_bytes(path: &Path, source: &Path, offset: u64) -> u64 {
    let len = |p: &Path| fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    len(source).saturating_sub(offset) + newer_than(path, source).iter().map(|p| len(p)).sum::<u64>()
}

/// The file to go on with after a loss: the oldest one whose first record comes after the last
/// record acknowledged (so newer rotated files still on disk are read), else `audit.log`.
fn resume_after_loss(path: &Path, last_ms: Option<i64>) -> PathBuf {
    let after = |p: &PathBuf| match (last_ms, first_record_ms(p)) {
        (Some(l), Some(f)) => f > l,
        _ => false,
    };
    in_write_order(path).into_iter().find(after).unwrap_or_else(|| path.to_path_buf())
}

fn gap(from_ms: i64, to_ms: i64, reason: &str, detail: &str) -> Gap {
    Gap { collector: "file".into(), instance: instance_name(), from_ms, to_ms: to_ms.max(from_ms), reason: reason.into(), detail: detail.into() }
}

/// Follow the audit log forever.
pub async fn run_file() -> anyhow::Result<()> {
    let sender = Sender::from_env("k8s-audit")?;
    let gaps = Sender::from_env("gap")?;
    let health = heartbeat("file")?;
    let path = PathBuf::from(env_or("AL_AUDIT_LOG", "/var/lib/rancher/rke2/server/logs/audit.log"));
    let state_dir = PathBuf::from(env_or("AL_STATE_DIR", "/var/lib/audit-loupe"));
    fs::create_dir_all(&state_dir)?;
    let state = state_dir.join("position.json");
    let started_ms = Utc::now().timestamp_millis();
    let mut pos = Position::load(&state);
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if pos.inode != 0 {
        // A restart: what was written while the collector was down is not read. The last
        // record acknowledged dates the stop; a state file from before 0.3.2 has no such time,
        // so its last write (made when the server acknowledged a batch) stands in for it.
        let from = pos.last_ms.or_else(|| {
            fs::metadata(&state).ok()?.modified().ok().map(|t| DateTime::<Utc>::from(t).timestamp_millis())
        });
        let from = from.unwrap_or(started_ms);
        pos.pending.push(gap(from, started_ms, "collector-restart", &name(&path)));
        pos.last_ms = Some(started_ms);
        pos = if path.exists() {
            let end = end_of_last_line(&path)?;
            pos.at(&path, end)?
        } else {
            // No audit.log yet: the next one to appear is read from its start, as on a first run.
            Position { last_ms: pos.last_ms, pending: pos.pending, ..Default::default() }
        };
        warn!(from_ms = from, to_ms = started_ms, offset = pos.offset, "restarted: reading from now on; the downtime is reported as not collected");
        pos.save(&state)?;
    }
    info!(file = %path.display(), offset = pos.offset, "following audit log");
    let mut backoff = 1u64;
    loop {
        if !pos.pending.is_empty() {
            match send_gaps(&gaps, &pos.pending).await {
                Ok(()) => {
                    pos.pending.clear();
                    pos.save(&state)?;
                }
                Err(e) => warn!(error = %e, "reporting a not-collected period failed; will retry"),
            }
        }
        set(&health, "auditLog", serde_json::json!(if path.exists() { "present" } else { "missing" }));
        if !path.exists() && rotated_files(&path).is_empty() {
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }
        if pos.inode == 0 {
            // First run (read the current file from its start), or a restart that found no file.
            if !path.exists() {
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
            pos = pos.at(&path, 0)?;
            pos.save(&state)?;
            continue;
        }
        // Shorter than what was already read: truncated or replaced in place, not the same file.
        let len = |p: &Path| fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let Some(source) = locate(&path, &pos).filter(|p| len(p) >= pos.offset) else {
            // The file being read is gone: removed (rotated out past maxbackup) or replaced under
            // the same inode. What was left of it is lost; say so now, from the last record.
            let replaced = in_write_order(&path).iter().any(|p| fs::metadata(p).is_ok_and(|m| m.ino() == pos.inode));
            let reason = if replaced { "file-replaced" } else { "file-removed" };
            let next = resume_after_loss(&path, pos.last_ms);
            let now = Utc::now().timestamp_millis();
            let from = pos.last_ms.unwrap_or(now);
            let to = first_record_ms(&next).unwrap_or(now);
            warn!(reason, from_ms = from, to_ms = to, next = %next.display(), "the file being read is gone before it was read to the end; reporting the loss");
            pos.pending.push(gap(from, to, reason, ""));
            pos = pos.at(&next, 0)?;
            pos.save(&state)?;
            continue;
        };
        set(&health, "lagBytes", serde_json::json!(lag_bytes(&path, &source, pos.offset)));
        // The first bytes are recorded as the file grows, up to HEAD_BYTES.
        if pos.head.as_ref().is_none_or(|h| h.len < HEAD_BYTES) {
            pos.head = Some(head_of(&source)?);
        }
        let (chunk, consumed) = read_lines(&source, pos.offset, MAX_BATCH)?;
        if chunk.is_empty() {
            if let Some(next) = newer_than(&path, &source).into_iter().next() {
                // A rotated file read to its end: go on with the next one written.
                info!(finished = %source.display(), next = %next.display(), "rotated file read to the end");
                pos = pos.at(&next, 0)?;
                pos.save(&state)?;
                continue;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        match sender.send_for_reply(&chunk).await {
            Ok(reply) => {
                pos.offset += consumed;
                if let Some(t) = last_record_ms(&chunk) {
                    pos.last_ms = Some(t);
                }
                pos.save(&state)?;
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

async fn send_gaps(sender: &Sender, gaps: &[Gap]) -> anyhow::Result<()> {
    for g in gaps {
        sender.send(serde_json::to_string(g)?.as_bytes()).await?;
    }
    Ok(())
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
        assert_eq!(lag_bytes(&p, &rotated, pos.offset), 5 + 5);
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
        let p = Position { inode: 42, offset: 1234, head: Some(Head { len: 3, sha256: "ab".into() }), last_ms: Some(7), pending: vec![] };
        p.save(&s).unwrap();
        assert_eq!(Position::load(&s), p);
        // Written by 0.3.1 and earlier.
        fs::write(&s, r#"{"inode":42,"offset":1234}"#).unwrap();
        assert_eq!(Position::load(&s), Position { inode: 42, offset: 1234, ..Default::default() });
    }
}
