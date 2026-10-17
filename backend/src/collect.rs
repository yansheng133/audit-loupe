//! Collectors: read audit records where they are written and push them to the server.
//!
//! `collect-file` follows the kube-apiserver audit log on an RKE2 control-plane node.
//! It remembers how far it has read (inode + byte offset) in a small state file, so a
//! restart resumes where it stopped, and it only moves that position forward after the
//! server has accepted the batch. Rotation is handled by finishing the rotated file
//! (found again by its inode) before switching to the new one.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
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

/// Follow the audit log forever.
pub async fn run_file() -> anyhow::Result<()> {
    let sender = Sender::from_env("k8s-audit")?;
    let path = PathBuf::from(env_or("AL_AUDIT_LOG", "/var/lib/rancher/rke2/server/logs/audit.log"));
    let state_dir = PathBuf::from(env_or("AL_STATE_DIR", "/var/lib/audit-loupe"));
    fs::create_dir_all(&state_dir)?;
    let state = state_dir.join("position.json");
    let mut pos = Position::load(&state);
    info!(file = %path.display(), ?pos, "following audit log");
    let mut backoff = 1u64;
    loop {
        let (source, restart) = match next_source(&path, &pos) {
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
            }
            Err(e) => {
                warn!(error = %e, retry_in = backoff, "send failed; will resend the same lines");
                tokio::time::sleep(Duration::from_secs(backoff)).await;
                backoff = (backoff * 2).min(60);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
