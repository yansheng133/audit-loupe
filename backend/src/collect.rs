//! Collectors: read audit records where they are written and push them to the server.
//!
//! `collect-rancher` follows the Rancher API audit log through the Kubernetes log API of each
//! Rancher pod's `rancher-audit-log` sidecar, which copies the log file to its output.
//!
//! `collect-file` follows the kube-apiserver audit log on an RKE2 control-plane node.
//! It remembers how far it has read (inode + byte offset) in a small state file, so a
//! restart resumes where it stopped, and it only moves that position forward after the
//! server has accepted the batch. Rotation is handled by finishing the rotated file
//! (found again by its inode) before switching to the new one.

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

    /// A sender with explicit settings (the replay tool, no custom CA).
    pub fn new(base: &str, token: String, source: &'static str) -> anyhow::Result<Self> {
        Ok(Sender {
            client: reqwest::Client::builder().timeout(Duration::from_secs(120)).build()?,
            url: format!("{}/ingest/v1/batch", base.trim_end_matches('/')),
            token,
            source,
        })
    }

    pub async fn send(&self, lines: &[u8]) -> anyhow::Result<()> {
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
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
pub struct Position {
    pub inode: u64,
    pub offset: u64,
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

/// After rotation the old file lives on under another name in the same directory.
pub fn find_by_inode(dir: &Path, inode: u64) -> Option<PathBuf> {
    fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| fs::metadata(p).map(|m| m.ino() == inode).unwrap_or(false))
}

/// What to read next, given the saved position and the file currently at `path`.
#[derive(Debug, PartialEq)]
pub enum Next {
    /// Keep reading this file from the saved offset.
    Same(PathBuf),
    /// The saved file was rotated away; finish it before switching.
    Rotated(PathBuf),
    /// Start the current file from the beginning (first run of a new file, or truncated).
    Restart,
    /// Nothing to read yet.
    Missing,
}

pub fn next_source(path: &Path, pos: &Position) -> Next {
    let Ok(meta) = fs::metadata(path) else { return Next::Missing };
    if pos.inode == 0 {
        return Next::Restart;
    }
    if meta.ino() == pos.inode {
        if meta.len() < pos.offset { Next::Restart } else { Next::Same(path.to_path_buf()) }
    } else {
        match path.parent().and_then(|d| find_by_inode(d, pos.inode)) {
            Some(old) => Next::Rotated(old),
            None => Next::Restart, // rotated file already removed; nothing left to recover
        }
    }
}

/// What a collector reports about itself every minute (coverage page).
pub type Health = Arc<Mutex<serde_json::Map<String, serde_json::Value>>>;

pub fn set(h: &Health, key: &str, v: serde_json::Value) {
    h.lock().unwrap().insert(key.to_string(), v);
}

/// Send a heartbeat every minute, with or without data, so the server can tell a quiet
/// cluster from a collector that stopped.
pub fn heartbeat(collector: &'static str) -> anyhow::Result<Health> {
    let sender = Sender::from_env("heartbeat")?;
    let health: Health = Default::default();
    let h = health.clone();
    let instance = std::env::var("NODE_NAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "unknown".into());
    tokio::spawn(async move {
        // Let the collector fill in its report before the first beat.
        tokio::time::sleep(Duration::from_secs(3)).await;
        loop {
            let details = serde_json::Value::Object(h.lock().unwrap().clone());
            let body = serde_json::json!({"collector": collector, "instance": instance, "version": env!("CARGO_PKG_VERSION"), "details": details});
            if let Err(e) = sender.send(body.to_string().as_bytes()).await {
                warn!(error = %e, "heartbeat failed");
            }
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });
    Ok(health)
}

/// Follow the audit log forever.
pub async fn run_file() -> anyhow::Result<()> {
    let sender = Sender::from_env("k8s-audit")?;
    let health = heartbeat("file")?;
    let path = PathBuf::from(env_or("AL_AUDIT_LOG", "/var/lib/rancher/rke2/server/logs/audit.log"));
    let state_dir = PathBuf::from(env_or("AL_STATE_DIR", "/var/lib/audit-loupe"));
    fs::create_dir_all(&state_dir)?;
    let state = state_dir.join("position.json");
    let mut pos = Position::load(&state);
    info!(file = %path.display(), ?pos, "following audit log");
    let mut backoff = 1u64;
    loop {
        let next = next_source(&path, &pos);
        set(&health, "auditLog", serde_json::json!(if next == Next::Missing { "missing" } else { "present" }));
        if let Ok(m) = fs::metadata(&path)
            && m.ino() == pos.inode {
                set(&health, "lagBytes", serde_json::json!(m.len().saturating_sub(pos.offset)));
            }
        let (source, restart) = match next {
            Next::Missing => {
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
            Next::Restart => {
                let ino = fs::metadata(&path)?.ino();
                if pos.inode != 0 {
                    warn!(old = pos.inode, new = ino, "starting the current file from the beginning");
                }
                pos = Position { inode: ino, offset: 0 };
                pos.save(&state)?;
                continue;
            }
            Next::Same(p) => (p, false),
            Next::Rotated(p) => (p, true),
        };
        let (chunk, consumed) = read_lines(&source, pos.offset, MAX_BATCH)?;
        if chunk.is_empty() {
            if restart {
                // Rotated file fully read; move to the new one.
                let ino = fs::metadata(&path)?.ino();
                info!(finished = %source.display(), "switching to the new audit log");
                pos = Position { inode: ino, offset: 0 };
                pos.save(&state)?;
                continue;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        match sender.send(&chunk).await {
            Ok(()) => {
                pos.offset += consumed;
                pos.save(&state)?;
                backoff = 1;
                set(&health, "lastSentMs", serde_json::json!(chrono::Utc::now().timestamp_millis()));
            }
            Err(e) => {
                warn!(error = %e, retry_in = backoff, "send failed; will resend the same lines");
                tokio::time::sleep(Duration::from_secs(backoff)).await;
                backoff = (backoff * 2).min(60);
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
    fn rotation_finishes_the_old_file_first() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("audit.log");
        fs::write(&p, "old1\nold2\n").unwrap();
        let ino = fs::metadata(&p).unwrap().ino();
        let pos = Position { inode: ino, offset: 5 };
        assert_eq!(next_source(&p, &pos), Next::Same(p.clone()));
        // kube-apiserver renames the file and starts a new one.
        let rotated = dir.path().join("audit-2026-10-17T01-00-00.000.log");
        fs::rename(&p, &rotated).unwrap();
        fs::write(&p, "new1\n").unwrap();
        assert_eq!(next_source(&p, &pos), Next::Rotated(rotated.clone()));
        let (chunk, _) = read_lines(&rotated, pos.offset, MAX_BATCH).unwrap();
        assert_eq!(chunk, b"old2\n");
    }

    #[test]
    fn truncation_and_first_run_restart() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("audit.log");
        assert_eq!(next_source(&p, &Position::default()), Next::Missing);
        fs::write(&p, "a\n").unwrap();
        assert_eq!(next_source(&p, &Position::default()), Next::Restart);
        let ino = fs::metadata(&p).unwrap().ino();
        assert_eq!(next_source(&p, &Position { inode: ino, offset: 99 }), Next::Restart);
    }

    #[test]
    fn position_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let s = dir.path().join("position.json");
        assert_eq!(Position::load(&s), Position::default());
        let p = Position { inode: 42, offset: 1234 };
        p.save(&s).unwrap();
        assert_eq!(Position::load(&s), p);
    }
}
