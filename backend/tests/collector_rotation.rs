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
    /// What GET /ingest/v1/known answers: None = a node never seen.
    known: Option<i64>,
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
    /// Reports of periods not collected (start marks left out).
    fn gaps(&self) -> Vec<serde_json::Value> {
        self.seen.lock().unwrap().gaps.iter().filter(|g| g["reason"] != "collection-start").cloned().collect()
    }
    fn starts(&self) -> Vec<serde_json::Value> {
        self.seen.lock().unwrap().gaps.iter().filter(|g| g["reason"] == "collection-start").cloned().collect()
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
    let request = line.clone();
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
        if request.starts_with("GET /ingest/v1/known") {
            let reply = serde_json::json!({"known": s.known.is_some(), "lastMs": s.known}).to_string();
            write!(conn, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}", reply.len())?;
            return conn.flush();
        }
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
    /// AL_ON_FIRST_START; the older tests read the existing file, as before.
    first_start: &'static str,
    env: Vec<(&'static str, String)>,
    names: Mutex<HashMap<u32, String>>,
}

impl Lab {
    fn new() -> Lab {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("logs")).unwrap();
        fs::create_dir_all(dir.path().join("state")).unwrap();
        fs::write(dir.path().join("token"), "collector-test-token").unwrap();
        Lab { dir, stub: Stub::start(), child: None, first_start: "readExisting", env: vec![], names: Default::default() }
    }
    fn log(&self) -> PathBuf {
        self.dir.path().join("logs/audit.log")
    }
    /// Rotated file number `n`, named like kube-apiserver does with the UTC time it was
    /// rotated (the time of the first call for `n`).
    fn rotated(&self, n: u32) -> PathBuf {
        let mut names = self.names.lock().unwrap();
        let name = names.entry(n).or_insert_with(|| {
            // Two rotations in the same millisecond would get one name; keep them apart.
            thread::sleep(Duration::from_millis(2));
            chrono::Utc::now().format("audit-%Y-%m-%dT%H-%M-%S%.3f.log").to_string()
        });
        self.dir.path().join("logs").join(name)
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
            .env("AL_ON_FIRST_START", self.first_start)
            .envs(self.env.iter().map(|(k, v)| (*k, v.as_str())))
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
    // It had everything up to when it last caught up (just after A), not only up to A.
    let from = gap_ms(g, "fromMs");
    assert!(from >= last_a && from < started, "the gap starts where the collector last had everything ({from} vs A {last_a}, start {started})");
    let to = gap_ms(g, "toMs");
    assert!(to >= started - 1000 && to <= started + 5000, "the gap ends when the collector started again ({to} vs {started})");
}

/// Defect 2, with the file kept open: the file being read is rotated out and removed before
/// it was read to the end. The open handle still reads it, so nothing is lost and no gap is
/// reported; the new file follows.
#[test]
fn removed_file_is_still_read_through_the_open_handle() {
    let mut lab = Lab::new();
    append(&lab.log(), "A", 10, 40);
    lab.start();
    assert!(wait_until(15, || lab.stub.count("A") == 10), "A not collected: {}", lab.collector_log());

    // The server stops taking batches, so B is read but not yet acknowledged.
    lab.stub.accept(false);
    append(&lab.log(), "B", 10, 40);
    thread::sleep(Duration::from_millis(1500));
    fs::rename(lab.log(), lab.rotated(1)).unwrap();
    append(&lab.log(), "C", 10, 40);
    fs::remove_file(lab.rotated(1)).unwrap();
    lab.stub.accept(true);

    assert!(wait_until(20, || lab.stub.count("B") == 10 && lab.stub.count("C") == 10), "B and C: {}", lab.collector_log());
    thread::sleep(Duration::from_secs(2));
    assert!(lab.stub.gaps().is_empty(), "nothing was lost: {:?}", lab.stub.gaps());
}

/// Defect 2, what is still lost: while the collector is behind on A, B is rotated out and
/// removed before it was ever opened. That is reported at once, from when A was rotated to
/// the first record of the next file left.
#[test]
fn files_removed_before_they_were_read_are_reported() {
    let mut lab = Lab::new();
    append(&lab.log(), "A", 10, 40);
    lab.start();
    assert!(wait_until(15, || lab.stub.count("A") == 10), "A not collected: {}", lab.collector_log());
    lab.stub.accept(false);
    append(&lab.log(), "A2", 10, 40);
    thread::sleep(Duration::from_millis(2500)); // seen in the directory as audit.log
    fs::rename(lab.log(), lab.rotated(1)).unwrap();
    thread::sleep(Duration::from_millis(2500)); // seen again under its rotated name
    append(&lab.log(), "B", 10, 40);
    thread::sleep(Duration::from_millis(6000));
    fs::rename(lab.log(), lab.rotated(2)).unwrap();
    let c_first = append(&lab.log(), "C", 10, 40);
    fs::remove_file(lab.rotated(1)).unwrap();
    fs::remove_file(lab.rotated(2)).unwrap();
    lab.stub.accept(true);

    assert!(wait_until(25, || lab.stub.count("C") == 10), "C: {}", lab.collector_log());
    assert_eq!(lab.stub.count("A2"), 10, "the open file is still read to its end");
    assert_eq!(lab.stub.count("B"), 0);
    let gaps = lab.stub.gaps();
    let g = gaps.iter().find(|g| g["reason"] == "file-removed").unwrap_or_else(|| panic!("B's loss not reported: {gaps:?}; {}", lab.collector_log()));
    assert!(gap_ms(g, "toMs") >= c_first - 1000 && gap_ms(g, "toMs") <= c_first, "{g}");
    assert!(gap_ms(g, "fromMs") < gap_ms(g, "toMs") - 5000, "{g}");
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

    assert!(lag_ok, "lagBytes while behind should be {total} (rest of the rotated files and audit.log), last heartbeat said {lag_seen:?}; {}", lab.collector_log());
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
    // From when the collector last had everything: after A, before the file was rewritten.
    assert!(gap_ms(g, "fromMs") >= last_a && gap_ms(g, "fromMs") <= now_ms(), "{g}");
}

/// Decision 1 (default): a first install starts at the end of audit.log and says so with a
/// start mark; what was already in the file is not read.
#[test]
fn first_start_reads_from_now_and_marks_the_start() {
    let mut lab = Lab::new();
    lab.first_start = "fromNow";
    append(&lab.log(), "OLD", 10, 40);
    let started = now_ms();
    lab.start();
    assert!(wait_until(15, || !lab.stub.starts().is_empty()), "no start mark: {}", lab.collector_log());
    append(&lab.log(), "NEW", 5, 40);
    assert!(wait_until(15, || lab.stub.count("NEW") == 5), "NEW not collected: {}", lab.collector_log());
    assert_eq!(lab.stub.count("OLD"), 0, "lines from before the install were read");
    let s = &lab.stub.starts()[0];
    assert_eq!(s["detail"], "fromNow");
    assert!(gap_ms(s, "fromMs") >= started, "{s}");
    assert!(lab.stub.gaps().is_empty(), "a first start is not a gap: {:?}", lab.stub.gaps());
}

/// Decision 1, readExisting: the current audit.log is read from its start, and the start mark
/// says from when the records were read at install rather than as they were written.
#[test]
fn first_start_can_read_the_existing_file() {
    let mut lab = Lab::new();
    lab.first_start = "readExisting";
    let t0 = now_ms() - 3_600_000;
    fs::write(lab.log(), format!("{}\n{}\n", event("OLD-0", t0, 10), event("OLD-1", t0 + 1, 10))).unwrap();
    lab.start();
    assert!(wait_until(15, || lab.stub.count("OLD") == 2), "{}", lab.collector_log());
    let s = &lab.stub.starts()[0];
    assert_eq!(s["detail"], "readExisting");
    assert_eq!(gap_ms(s, "fromMs"), t0, "{s}");
}

/// Decision 1: the state directory is gone but the server knows this node: that is a restart,
/// not an install. Read from now on; report the time since the last check-in as not collected.
#[test]
fn lost_state_is_a_restart_when_the_server_knows_the_node() {
    let mut lab = Lab::new();
    let last_beat = now_ms() - 600_000;
    lab.stub.seen.lock().unwrap().known = Some(last_beat);
    append(&lab.log(), "OLD", 10, 40);
    lab.start();
    assert!(wait_until(15, || !lab.stub.gaps().is_empty()), "{}", lab.collector_log());
    append(&lab.log(), "NEW", 3, 40);
    assert!(wait_until(15, || lab.stub.count("NEW") == 3), "{}", lab.collector_log());
    assert_eq!(lab.stub.count("OLD"), 0);
    let g = &lab.stub.gaps()[0];
    assert_eq!(g["reason"], "collector-state-lost");
    assert_eq!(gap_ms(g, "fromMs"), last_beat, "{g}");
    assert!(lab.stub.starts().is_empty(), "not a first start");
}

/// Review 1: a rotation between listing the directory and checking audit.log made the file
/// being read look replaced; the rest of the rotated file was skipped. The listing is slowed
/// down here to widen that window. With the file kept open, nothing is lost and no gap reported.
#[test]
fn rotation_during_a_slow_listing_loses_nothing() {
    let mut lab = Lab::new();
    lab.env.push(("AL_TEST_LIST_DELAY_MS", "150".into()));
    append(&lab.log(), "S", 1, 40);
    lab.start();
    assert!(wait_until(15, || lab.stub.count("S") == 1), "{}", lab.collector_log());
    let mut total = 0;
    for r in 1..=6u32 {
        append(&lab.log(), &format!("R{r}"), 300, 200);
        total += 300;
        thread::sleep(Duration::from_millis(120));
        fs::rename(lab.log(), lab.rotated(r)).unwrap();
        thread::sleep(Duration::from_millis(5)); // distinct rotation times
    }
    append(&lab.log(), "LIVE", 10, 40);
    total += 10;
    let got = || (1..=6).map(|r| lab.stub.count(&format!("R{r}"))).sum::<usize>() + lab.stub.count("LIVE");
    assert!(wait_until(40, || got() == total), "got {} of {total}: {}", got(), lab.collector_log());
    assert!(lab.stub.gaps().is_empty(), "nothing was lost: {:?}", lab.stub.gaps());
}

/// Review 3: records stopped at an old time but the collector kept up until it was stopped;
/// the downtime starts when it last had everything, not at the last record.
#[test]
fn quiet_cluster_restart_does_not_overstate_the_downtime() {
    let mut lab = Lab::new();
    let old = now_ms() - 4 * 3_600_000;
    fs::write(lab.log(), format!("{}\n", event("Q-0", old, 10))).unwrap();
    lab.start();
    assert!(wait_until(15, || lab.stub.count("Q") == 1), "{}", lab.collector_log());
    thread::sleep(Duration::from_secs(4)); // caught up, nothing new
    let stopped = now_ms();
    lab.kill();
    lab.start();
    assert!(wait_until(15, || !lab.stub.gaps().is_empty()), "{}", lab.collector_log());
    let g = &lab.stub.gaps()[0];
    assert_eq!(g["reason"], "collector-restart");
    assert!(gap_ms(g, "fromMs") > stopped - 35_000, "downtime starts at the last catch-up, not 4 h ago: {g}");
}

/// Replay with real kube-apiserver lines (a copy; the original is never touched):
/// AL_TEST_RAW=<copy of an audit log> cargo test --test collector_rotation -- --ignored real
/// A rotation while running loses nothing; a rotation while stopped is not read afterwards and
/// is reported; every count must match the lines written.
#[test]
#[ignore]
fn real_audit_lines_replay_with_rotations() {
    let raw = fs::read_to_string(std::env::var("AL_TEST_RAW").expect("AL_TEST_RAW")).unwrap();
    let lines: Vec<&str> = raw.lines().filter(|l| !l.is_empty()).collect();
    let n = lines.len() / 6;
    let parts: Vec<&[&str]> = (0..6).map(|i| &lines[i * n..(i + 1) * n]).collect();
    let write = |p: &Path, part: &[&str]| {
        let mut f = OpenOptions::new().create(true).append(true).open(p).unwrap();
        f.write_all((part.join("\n") + "\n").as_bytes()).unwrap();
    };
    let ids = |part: &[&str]| -> std::collections::HashSet<String> {
        part.iter().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()?.get("auditID")?.as_str().map(str::to_string)).collect()
    };
    let got = |lab: &Lab| -> std::collections::HashSet<String> { lab.stub.seen.lock().unwrap().ids.iter().cloned().collect() };
    let mut lab = Lab::new();
    write(&lab.log(), parts[0]);
    lab.start();
    let want0 = ids(parts[0]);
    assert!(wait_until(60, || want0.is_subset(&got(&lab))), "part 0: {}", lab.collector_log());
    // Running: more lines, a rotation, more lines.
    write(&lab.log(), parts[1]);
    fs::rename(lab.log(), lab.rotated(1)).unwrap();
    write(&lab.log(), parts[2]);
    let want: std::collections::HashSet<String> = [0, 1, 2].iter().flat_map(|i| ids(parts[*i])).collect();
    assert!(wait_until(60, || want.is_subset(&got(&lab))), "while running: {} of {}", want.intersection(&got(&lab)).count(), want.len());
    assert!(lab.stub.gaps().is_empty(), "{:?}", lab.stub.gaps());
    let sent = lab.stub.seen.lock().unwrap().ids.len();
    let lines_written = parts[0].len() + parts[1].len() + parts[2].len();
    println!("running rotation: lines written {lines_written}, lines sent {sent}, distinct audit IDs {} of {}", got(&lab).len(), want.len());
    assert_eq!(sent, lines_written, "every line sent exactly once");
    // Stopped: lines, a rotation, lines; then started again and more lines.
    lab.kill();
    write(&lab.log(), parts[3]);
    fs::rename(lab.log(), lab.rotated(2)).unwrap();
    write(&lab.log(), parts[4]);
    lab.start();
    assert!(wait_until(15, || !lab.stub.gaps().is_empty()), "{}", lab.collector_log());
    write(&lab.log(), parts[5]);
    let want5 = ids(parts[5]);
    assert!(wait_until(60, || want5.is_subset(&got(&lab))), "after restart: {}", lab.collector_log());
    let have = got(&lab);
    let read_while_down = ids(parts[3]).union(&ids(parts[4])).filter(|i| have.contains(*i) && !want.contains(*i)).count();
    let sent = lab.stub.seen.lock().unwrap().ids.len();
    println!("rotation while stopped: lines sent in all {sent} (= {} + {}), read from the downtime {read_while_down}", lines_written, parts[5].len());
    assert_eq!(read_while_down, 0, "lines written while stopped are not read afterwards");
    assert_eq!(sent, lines_written + parts[5].len());
    assert_eq!(lab.stub.gaps()[0]["reason"], "collector-restart");
}
