//! Forward records and chain anchors to the customer's SIEM.
//!
//! What goes out is the processed record (who through which way, which object, the risk
//! rules, the chain hash), not the raw log line. Each target keeps its own cursor in the
//! database and only moves it after a successful send, so a target that is down catches up
//! later and nothing is skipped. Records are held back a short while so the Rancher and
//! downstream halves of an action have time to pair; a half that pairs later than that is
//! sent on its own, with its audit ID so the SIEM can match them.
//!
//! Targets (any combination), from the environment:
//! - AL_FORWARD_SYSLOG = tls://host:6514 | tcp://host:514 | udp://host:514 (RFC 5424, JSON message)
//! - AL_FORWARD_SPLUNK_HEC = https://host:8088/services/collector/event, AL_FORWARD_SPLUNK_TOKEN_FILE
//! - AL_FORWARD_WEBHOOK = https://..., optional AL_FORWARD_WEBHOOK_TOKEN_FILE (sent as a bearer token)
//! - AL_FORWARD_CA_FILE: a private CA for the TLS targets
//! - AL_FORWARD_DELAY_SECONDS: hold-back before sending (default 30)

use crate::store::{Anchor, ForwardRecord, Store};
use anyhow::Context;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tracing::{info, warn};

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Syslog { proto: String, host: String, port: u16 },
    SplunkHec { url: String, token: String },
    Webhook { url: String, token: Option<String> },
}

impl Target {
    pub fn name(&self) -> &'static str {
        match self {
            Target::Syslog { .. } => "syslog",
            Target::SplunkHec { .. } => "splunk-hec",
            Target::Webhook { .. } => "webhook",
        }
    }
}

fn read_secret(var: &str) -> anyhow::Result<Option<String>> {
    match std::env::var(var).ok().filter(|v| !v.is_empty()) {
        Some(path) => Ok(Some(std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?.trim().to_string())),
        None => Ok(None),
    }
}

pub fn parse_syslog(spec: &str) -> anyhow::Result<Target> {
    let (proto, rest) = spec.split_once("://").context("expected tls://, tcp:// or udp://")?;
    anyhow::ensure!(matches!(proto, "tls" | "tcp" | "udp"), "unsupported syslog protocol {proto}");
    let (host, port) = rest.trim_end_matches('/').rsplit_once(':').context("expected host:port")?;
    Ok(Target::Syslog { proto: proto.into(), host: host.trim_matches(['[', ']']).into(), port: port.parse()? })
}

pub fn targets_from_env() -> anyhow::Result<Vec<Target>> {
    let mut out = Vec::new();
    if let Some(s) = std::env::var("AL_FORWARD_SYSLOG").ok().filter(|v| !v.is_empty()) {
        out.push(parse_syslog(&s)?);
    }
    if let Some(url) = std::env::var("AL_FORWARD_SPLUNK_HEC").ok().filter(|v| !v.is_empty()) {
        let token = read_secret("AL_FORWARD_SPLUNK_TOKEN_FILE")?.context("AL_FORWARD_SPLUNK_TOKEN_FILE is required with AL_FORWARD_SPLUNK_HEC")?;
        out.push(Target::SplunkHec { url, token });
    }
    if let Some(url) = std::env::var("AL_FORWARD_WEBHOOK").ok().filter(|v| !v.is_empty()) {
        out.push(Target::Webhook { url, token: read_secret("AL_FORWARD_WEBHOOK_TOKEN_FILE")? });
    }
    Ok(out)
}

fn record_json(r: &ForwardRecord) -> Value {
    serde_json::to_value(r).unwrap_or(Value::Null)
}

fn anchor_json(a: &Anchor) -> Value {
    json!({"type": "anchor", "cluster": a.cluster, "eventId": a.event_id, "hash": a.hash,
           "at": chrono::DateTime::from_timestamp_millis(a.at_ms).map(|t| t.to_rfc3339())})
}

/// RFC 5424: <PRI>1 TIMESTAMP HOST APP PROCID MSGID SD MSG. Facility local0; warning for
/// medium/high risk records, notice otherwise.
pub fn syslog_line(host: &str, msgid: &str, ts: &str, warn: bool, msg: &Value) -> String {
    let pri = 16 * 8 + if warn { 4 } else { 5 };
    let host: String = host.chars().filter(|c| c.is_ascii_graphic()).take(255).collect();
    format!("<{pri}>1 {ts} {} audit-loupe - {msgid} - {msg}", if host.is_empty() { "-" } else { &host })
}

pub struct Forwarder {
    store: Store,
    targets: Vec<Target>,
    http: reqwest::Client,
    tls: Arc<rustls::ClientConfig>,
    delay: Duration,
}

impl Forwarder {
    pub fn new(store: Store, targets: Vec<Target>) -> anyhow::Result<Self> {
        let mut http = reqwest::Client::builder().timeout(Duration::from_secs(30));
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        if let Ok(ca) = std::env::var("AL_FORWARD_CA_FILE") {
            let pem = std::fs::read(&ca).with_context(|| format!("reading {ca}"))?;
            http = http.add_root_certificate(reqwest::Certificate::from_pem(&pem)?);
            use rustls_pki_types::pem::PemObject;
            for cert in rustls_pki_types::CertificateDer::pem_slice_iter(&pem) {
                roots.add(cert?)?;
            }
        }
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()?
            .with_root_certificates(roots)
            .with_no_client_auth();
        let delay = Duration::from_secs(std::env::var("AL_FORWARD_DELAY_SECONDS").ok().and_then(|v| v.parse().ok()).unwrap_or(30));
        Ok(Forwarder { store, targets, http: http.build()?, tls: Arc::new(tls), delay })
    }

    async fn send(&self, t: &Target, recs: &[ForwardRecord], anchors: &[Anchor]) -> anyhow::Result<()> {
        match t {
            Target::Syslog { proto, host, port } => {
                let lines: Vec<String> = recs
                    .iter()
                    .map(|r| syslog_line(&r.cluster, "event", &r.ts, r.risky, &record_json(r)))
                    .chain(anchors.iter().map(|a| {
                        let ts = chrono::DateTime::from_timestamp_millis(a.at_ms).map(|t| t.to_rfc3339()).unwrap_or_default();
                        syslog_line(&a.cluster, "anchor", &ts, false, &anchor_json(a))
                    }))
                    .collect();
                let addr = format!("{host}:{port}");
                if proto == "udp" {
                    let sock = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
                    sock.connect(&addr).await?;
                    for l in &lines {
                        sock.send(l.as_bytes()).await?;
                    }
                    return Ok(());
                }
                // Octet-counting framing (RFC 5425 / 6587): "<length> <message>".
                let mut framed = Vec::new();
                for l in &lines {
                    framed.extend_from_slice(format!("{} {l}", l.len()).as_bytes());
                }
                let tcp = tokio::time::timeout(Duration::from_secs(10), tokio::net::TcpStream::connect(&addr)).await??;
                if proto == "tls" {
                    let name = rustls_pki_types::ServerName::try_from(host.clone())?;
                    let mut s = tokio_rustls::TlsConnector::from(self.tls.clone()).connect(name, tcp).await?;
                    s.write_all(&framed).await?;
                    s.shutdown().await?;
                } else {
                    let mut s = tcp;
                    s.write_all(&framed).await?;
                    s.shutdown().await?;
                }
                Ok(())
            }
            Target::SplunkHec { url, token } => {
                let mut body = String::new();
                for r in recs {
                    let secs = chrono::DateTime::parse_from_rfc3339(&r.ts).map(|t| t.timestamp_millis() as f64 / 1000.0).unwrap_or(0.0);
                    body.push_str(&json!({"time": secs, "host": r.cluster, "source": "audit-loupe", "sourcetype": "audit-loupe:event", "event": record_json(r)}).to_string());
                    body.push('\n');
                }
                for a in anchors {
                    body.push_str(&json!({"time": a.at_ms as f64 / 1000.0, "host": a.cluster, "source": "audit-loupe", "sourcetype": "audit-loupe:anchor", "event": anchor_json(a)}).to_string());
                    body.push('\n');
                }
                let res = self.http.post(url).header("authorization", format!("Splunk {token}")).body(body).send().await?;
                anyhow::ensure!(res.status().is_success(), "HEC answered {}", res.status());
                Ok(())
            }
            Target::Webhook { url, token } => {
                let body = json!({"events": recs, "anchors": anchors.iter().map(anchor_json).collect::<Vec<_>>()});
                let mut req = self.http.post(url).header("content-type", "application/json").body(body.to_string());
                if let Some(t) = token {
                    req = req.bearer_auth(t);
                }
                let res = req.send().await?;
                anyhow::ensure!(res.status().is_success(), "webhook answered {}", res.status());
                Ok(())
            }
        }
    }

    /// One round for every target: send what is new, move the cursor on success.
    pub async fn round(&self) {
        for t in &self.targets {
            let name = t.name();
            let store = self.store.clone();
            let before = chrono::Utc::now().timestamp_millis() - self.delay.as_millis() as i64;
            let work = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
                let (ev, an) = store.forward_cursor(name)?;
                Ok((store.forward_records(ev, before, 500)?, store.anchors(an, 100)?))
            })
            .await;
            let (recs, anchors) = match work {
                Ok(Ok(x)) => x,
                Ok(Err(e)) => {
                    warn!(target = name, error = %e, "reading records to forward failed");
                    continue;
                }
                Err(e) => {
                    warn!(target = name, error = %e, "forward task failed");
                    continue;
                }
            };
            if recs.is_empty() && anchors.is_empty() {
                continue;
            }
            let result = self.send(t, &recs, &anchors).await;
            let store = self.store.clone();
            let (last_ev, last_an) = (recs.last().map(|r| r.id), anchors.last().map(|a| a.id));
            let err = result.as_ref().err().map(|e| e.to_string());
            if let Some(e) = &err {
                warn!(target = name, error = %e, "forwarding failed; will retry");
            }
            let _ = tokio::task::spawn_blocking(move || store.forward_done(name, if err.is_none() { last_ev } else { None }, if err.is_none() { last_an } else { None }, err)).await;
        }
    }

    pub fn spawn(self) {
        let names: Vec<_> = self.targets.iter().map(Target::name).collect();
        info!(targets = ?names, "forwarding to SIEM");
        tokio::spawn(async move {
            loop {
                self.round().await;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::{parse_body, to_record};
    use tokio::io::AsyncReadExt;

    #[test]
    fn syslog_specs_and_lines() {
        assert_eq!(parse_syslog("tls://siem.example.com:6514").unwrap(), Target::Syslog { proto: "tls".into(), host: "siem.example.com".into(), port: 6514 });
        assert!(parse_syslog("http://x:1").is_err());
        let l = syslog_line("prod rke2", "event", "2026-10-14T02:00:00+00:00", true, &json!({"a":1}));
        assert_eq!(l, r#"<132>1 2026-10-14T02:00:00+00:00 prodrke2 audit-loupe - event - {"a":1}"#);
    }

    fn store_with_records() -> Store {
        let s = Store::open(":memory:").unwrap();
        let ev = r#"{"auditID":"x1","stage":"ResponseComplete","verb":"delete","user":{"username":"system:admin"},
          "objectRef":{"resource":"namespaces","name":"legacy"},"responseStatus":{"code":200},"stageTimestamp":"2026-10-14T15:00:00Z"}"#;
        let r = to_record(&parse_body(ev.replace('\n', " ").as_bytes()).unwrap()[0]).unwrap();
        s.insert_event("c1", &r).unwrap();
        s.take_anchors(chrono::Utc::now()).unwrap();
        s
    }

    #[tokio::test]
    async fn tcp_syslog_gets_framed_records_and_anchors_and_cursor_moves() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let got = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = String::new();
            sock.read_to_string(&mut buf).await.unwrap();
            buf
        });
        let s = store_with_records();
        // A zero hold-back so the record is due at once.
        unsafe { std::env::set_var("AL_FORWARD_DELAY_SECONDS", "0") };
        let f = Forwarder::new(s.clone(), vec![Target::Syslog { proto: "tcp".into(), host: "127.0.0.1".into(), port }]).unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
        f.round().await;
        let text = got.await.unwrap();
        let first = text.split_once(' ').unwrap();
        assert!(first.0.parse::<usize>().is_ok(), "octet-counting frame: {text}");
        assert!(text.contains(r#""rules":["namespace_deleted","bypassed_rancher","off_hours"]"#), "{text}");
        assert!(text.contains("<132>1 "), "warning severity for a high-risk record");
        assert!(text.contains(" anchor - "), "anchor sent too");
        let st = s.forward_status().unwrap();
        assert_eq!((st[0].0.as_str(), st[0].3), ("syslog", 0), "cursor moved, nothing pending");
    }
}
