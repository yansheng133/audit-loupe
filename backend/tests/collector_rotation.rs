//! Rotation and restart behaviour of `collect-file`, run as the real binary against a stub
//! ingest endpoint. Each test drives a small fake audit log directory the way kube-apiserver's
//! log rotation does (rename to `audit-<time>.log`, start a new `audit.log`).
//!
//! The stub records which audit IDs arrive, the heartbeats and the "not collected" reports,
//! and can refuse batches to keep the collector behind. Nothing leaves 127.0.0.1.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Default)]
struct Seen {
    accept: bool,
    /// Audit IDs in arrival order; a line that is not JSON is recorded as "<bad>".
    ids: Vec<String>,
    heartbeats: Vec<serde_json::Value>,
    gaps: Vec<serde_json::Value>,
}

struct Stub {
    port: u16,
    seen: Arc<Mutex<Seen>>,
}

impl Stub {
    fn start() -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Seen { accept: true, ..Default::default() }));
        let s = seen.clone();
        thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let s = s.clone();
                thread::spawn(move || {
                    let _ = serve(conn, &s);
                });
            }
        });
        Stub { port, seen }
    }
    fn accept(&self, yes: bool) {
        self.seen.lock().unwrap().accept = yes;
    }
    fn count(&self, prefix: &str) -> usize {
        self.seen.lock().unwrap().ids.iter().filter(|i| i.starts_with(&format!("{prefix}-"))).count()
    }
    fn bad(&self) -> usize {
        self.seen.lock().unwrap().ids.iter().filter(|i| *i == "<bad>").count()
    }
    fn gaps(&self) -> Vec<serde_json::Value> {
        self.seen.lock().unwrap().gaps.clone()
    }
    fn last_lag(&self) -> Option<u64> {
        self.seen.lock().unwrap().heartbeats.last().and_then(|h| h.pointer("/details/lagBytes")).and_then(|v| v.as_u64())
    }
}

fn serve(mut conn: TcpStream, seen: &Arc<Mutex<Seen>>) -> std::io::Result<()> {
    conn.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(conn.try_clone()?);
    let mut headers: HashMap<String, String> = HashMap::new();
    let mut line = String::new();
    reader.read_line(&mut line)?;
    loop {
        line.clear();
        reader.read_line(&mut line)?;
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut raw = vec![0u8; len];
    reader.read_exact(&mut raw)?;
    let body = if headers.get("content-encoding").map(|v| v == "gzip").unwrap_or(false) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(&raw[..]).read_to_end(&mut out)?;
        out
    } else {
        raw
    };
    let source = headers.get("x-audit-loupe-source").cloned().unwrap_or_default();
    let (status, reply) = {
        let mut s = seen.lock().unwrap();
        match source.as_str() {
            "heartbeat" => {
                s.heartbeats.push(serde_json::from_slice(&body).unwrap_or_default());
                ("204 No Content", String::new())
            }
            "gap" => {
                s.gaps.push(serde_json::from_slice(&body).unwrap_or_default());
                ("204 No Content", String::new())
            }
            "k8s-audit" if !s.accept => ("503 Service Unavailable", "stub refuses for now".to_string()),
            "k8s-audit" => {
                let mut n = 0;
                for l in body.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
                    let id = serde_json::from_slice::<serde_json::Value>(l)
                        .ok()
                        .and_then(|v| v.get("auditID").and_then(|x| x.as_str()).map(str::to_string))
                        .unwrap_or_else(|| "<bad>".to_string());
                    s.ids.push(id);
                    n += 1;
                }
                ("200 OK", format!(r#"{{"received":{n},"skipped":0,"stored":{n}}}"#))
            }
            _ => ("400 Bad Request", format!("unsupported source {source:?}")),
        }
    };
    write!(conn, "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}", reply.len())?;
    conn.flush()
}

struct Lab {
    dir: tempfile::TempDir,
    stub: Stub,
    child: Option<Child>,
}

impl Lab {
    fn new() -> Lab {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("logs")).unwrap();
        fs::create_dir_all(dir.path().join("state")).unwrap();
        fs::write(dir.path().join("token"), "collector-test-token").unwrap();
        Lab { dir, stub: Stub::start(), child: None }
    }
    fn log(&self) -> PathBuf {
        self.dir.path().join("logs/audit.log")
    }
    fn rotated(&self, n: u32) -> PathBuf {
        self.dir.path().join(format!("logs/audit-2026-10-20T0{n}-00-00.000.log"))
    }
    fn start(&mut self) {
        let out = File::options().create(true).append(true).open(self.dir.path().join("collector.log")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_audit-loupe"))
            .arg("collect-file")
            .env("AL_AUDIT_LOG", self.log())
            .env("AL_STATE_DIR", self.dir.path().join("state"))
            .env("AL_INGEST_URL", format!("http://127.0.0.1:{}", self.stub.port))
            .env("AL_TOKEN_FILE", self.dir.path().join("token"))
            .env("AL_HEARTBEAT_SECS", "1")
            .env("HOSTNAME", "node-test")
            .env("RUST_LOG", "info")
            .stdout(Stdio::from(out.try_clone().unwrap()))
            .stderr(Stdio::from(out))
            .spawn()
            .unwrap();
        self.child = Some(child);
    }
    /// Like a pod being killed: no chance to save anything on the way out.
    fn kill(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
    fn collector_log(&self) -> String {
        fs::read_to_string(self.dir.path().join("collector.log")).unwrap_or_default()
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        self.kill();
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// One audit event as kube-apiserver writes it. `pad` changes the line length.
fn event(id: &str, ts_ms: i64, pad: usize) -> String {
    let ts = chrono::DateTime::from_timestamp_millis(ts_ms).unwrap().format("%Y-%m-%dT%H:%M:%S%.6fZ");
    format!(
        r#"{{"kind":"Event","apiVersion":"audit.k8s.io/v1","level":"Metadata","auditID":"{id}","stage":"ResponseComplete","requestURI":"/api/v1/namespaces/shop/configmaps/{p}","verb":"get","user":{{"username":"system:serviceaccount:shop:app"}},"objectRef":{{"resource":"configmaps","namespace":"shop","name":"settings"}},"responseStatus":{{"code":200}},"requestReceivedTimestamp":"{ts}","stageTimestamp":"{ts}"}}"#,
        p = "x".repeat(pad)
    )
}

/// Append `n` events `<prefix>-<i>`; returns the time of the last one.
fn append(path: &Path, prefix: &str, n: usize, pad: usize) -> i64 {
    let mut f = OpenOptions::new().create(true).append(true).open(path).unwrap();
    let mut last = 0;
    let mut buf = String::new();
    for i in 0..n {
        last = now_ms();
        buf.push_str(&event(&format!("{prefix}-{i}"), last, pad));
        buf.push('\n');
    }
    f.write_all(buf.as_bytes()).unwrap();
    last
}

fn wait_until(secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if f() {
            return true;
        }
        thread::sleep(Duration::from_millis(200));
    }
    f()
}

fn gap_ms(g: &serde_json::Value, key: &str) -> i64 {
    g.get(key).and_then(|v| v.as_i64()).unwrap_or(-1)
}

/// Defect 1 and the new rule: after a restart, nothing written while the collector was down is
/// read (not the rest of the old file, not the files rotated in between, not the part of the
/// current file written before the start); the downtime is reported as not collected, from the
/// last record read to the start.
#[test]
fn restart_reads_from_now_and_reports_the_downtime() {
    let mut lab = Lab::new();
    let last_a = append(&lab.log(), "A", 10, 40);
    lab.start();
    assert!(wait_until(15, || lab.stub.count("A") == 10), "A not collected: {}", lab.collector_log());
    lab.kill();

    // While it is down: more lines in the file it was reading, then two rotations.
    append(&lab.log(), "AT", 5, 40);
    fs::rename(lab.log(), lab.rotated(1)).unwrap();
    append(&lab.log(), "B", 10, 40);
    fs::rename(lab.log(), lab.rotated(2)).unwrap();
    append(&lab.log(), "C", 10, 40);

    let started = now_ms();
    lab.start();
    thread::sleep(Duration::from_secs(3));
    append(&lab.log(), "D", 5, 40);
    assert!(wait_until(15, || lab.stub.count("D") == 5), "D (written after the restart) not collected: {}", lab.collector_log());

    let (at, b, c) = (lab.stub.count("AT"), lab.stub.count("B"), lab.stub.count("C"));
    assert_eq!((at, b, c), (0, 0, 0), "lines written while the collector was down must not be read (AT, B, C)");
    let gaps = lab.stub.gaps();
    let g = gaps.iter().find(|g| g.get("reason").and_then(|r| r.as_str()) == Some("collector-restart"));
    let g = g.unwrap_or_else(|| panic!("no 'not collected' report for the downtime; got {gaps:?}"));
    assert_eq!(gap_ms(g, "fromMs"), last_a, "the gap starts at the last record read before the stop");
    let to = gap_ms(g, "toMs");
    assert!(to >= started - 1000 && to <= started + 5000, "the gap ends when the collector started again ({to} vs {started})");
}

/// Defect 2: the file being read is removed before it was read to the end. The loss is
/// reported at once (not after the 180 s heartbeat silence), starting at the last record read.
#[test]
fn removed_file_is_reported_at_once() {
    let mut lab = Lab::new();
    let last_a = append(&lab.log(), "A", 10, 40);
    lab.start();
    assert!(wait_until(15, || lab.stub.count("A") == 10), "A not collected: {}", lab.collector_log());

    // The server stops taking batches, so B is read but never acknowledged.
    lab.stub.accept(false);
    append(&lab.log(), "B", 10, 40);
    thread::sleep(Duration::from_millis(1500));
    fs::rename(lab.log(), lab.rotated(1)).unwrap();
    append(&lab.log(), "C", 10, 40);
    fs::remove_file(lab.rotated(1)).unwrap();
    let removed = Instant::now();
    lab.stub.accept(true);

    assert!(
        wait_until(20, || lab.stub.gaps().iter().any(|g| g.get("reason").and_then(|r| r.as_str()) == Some("file-removed"))),
        "no 'not collected' report after the file was removed; got {:?}; log: {}",
        lab.stub.gaps(),
        lab.collector_log()
    );
    assert!(removed.elapsed() < Duration::from_secs(30), "reported only after {:?}", removed.elapsed());
    let gaps = lab.stub.gaps();
    let g = gaps.iter().find(|g| g.get("reason").and_then(|r| r.as_str()) == Some("file-removed")).unwrap();
    assert_eq!(gap_ms(g, "fromMs"), last_a, "the gap starts at the last record acknowledged");
    assert!(wait_until(20, || lab.stub.count("C") == 10), "C (the new file) not collected: {}", lab.collector_log());
    assert_eq!(lab.stub.count("B"), 0, "B was never acknowledged and its file is gone");
}

/// Defect 3 and live rotation: while the collector is behind, lagBytes covers the rest of the
/// file it is on plus every newer file; once it catches up, each rotated file is read to the end
/// and every line arrives exactly once, including a file rotated twice over while it lagged.
#[test]
fn lag_counts_rotated_files_and_live_rotation_loses_nothing() {
    let mut lab = Lab::new();
    lab.stub.accept(false);
    append(&lab.log(), "A", 20, 200);
    lab.start();
    thread::sleep(Duration::from_millis(800));
    fs::rename(lab.log(), lab.rotated(1)).unwrap();
    append(&lab.log(), "B", 20, 200);
    fs::rename(lab.log(), lab.rotated(2)).unwrap();
    append(&lab.log(), "C", 20, 200);
    let total = fs::metadata(lab.rotated(1)).unwrap().len() + fs::metadata(lab.rotated(2)).unwrap().len() + fs::metadata(lab.log()).unwrap().len();

    let lag_ok = wait_until(10, || lab.stub.last_lag() == Some(total));
    let lag_seen = lab.stub.last_lag();

    lab.stub.accept(true);
    let all = wait_until(20, || lab.stub.count("A") + lab.stub.count("B") + lab.stub.count("C") >= 60);
    thread::sleep(Duration::from_secs(3));
    let counts = (lab.stub.count("A"), lab.stub.count("B"), lab.stub.count("C"));
    let caught_up = wait_until(10, || lab.stub.last_lag() == Some(0));

    assert!(lag_ok, "lagBytes while behind should be {total} (rest of the rotated files and audit.log), last heartbeat said {lag_seen:?}");
    assert!(all && counts == (20, 20, 20), "every line of every rotated file arrives once; got A, B, C = {counts:?}; log: {}", lab.collector_log());
    assert_eq!(lab.stub.bad(), 0);
    assert!(caught_up, "lagBytes back to 0 after catching up, last heartbeat said {:?}", lab.stub.last_lag());
}

/// Defect 4: the same inode, but a different file (here rewritten in place, as happens when
/// the inode of a removed file is given to the new audit.log). The collector must notice from
/// the start of the file, read the new file from its beginning, and report the loss; it must
/// never continue from the old offset in the middle of a line.
#[test]
fn same_inode_with_other_content_is_a_new_file() {
    let mut lab = Lab::new();
    let last_a = append(&lab.log(), "A", 10, 40);
    lab.start();
    assert!(wait_until(15, || lab.stub.count("A") == 10), "A not collected: {}", lab.collector_log());
    thread::sleep(Duration::from_millis(2500));
    let ino = std::os::unix::fs::MetadataExt::ino(&fs::metadata(lab.log()).unwrap());

    let mut buf = String::new();
    for i in 0..20 {
        buf.push_str(&event(&format!("E-{i}"), now_ms(), 333));
        buf.push('\n');
    }
    let mut f = OpenOptions::new().write(true).truncate(true).open(lab.log()).unwrap();
    f.write_all(buf.as_bytes()).unwrap();
    drop(f);
    assert_eq!(std::os::unix::fs::MetadataExt::ino(&fs::metadata(lab.log()).unwrap()), ino, "test setup: same inode");

    wait_until(15, || lab.stub.count("E") >= 20 || lab.stub.bad() > 0);
    thread::sleep(Duration::from_secs(2));
    assert_eq!(lab.stub.bad(), 0, "a partial line was sent: the collector continued from the old offset");
    assert_eq!(lab.stub.count("E"), 20, "the new file is read from its beginning: {}", lab.collector_log());
    let gaps = lab.stub.gaps();
    let g = gaps.iter().find(|g| g.get("reason").and_then(|r| r.as_str()) == Some("file-replaced"));
    let g = g.unwrap_or_else(|| panic!("no 'not collected' report for the replaced file; got {gaps:?}"));
    assert_eq!(gap_ms(g, "fromMs"), last_a);
}
