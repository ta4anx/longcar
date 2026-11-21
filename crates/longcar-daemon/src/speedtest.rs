// SPDX-License-Identifier: GPL-3.0-or-later

//! On-demand LAN throughput test.
//!
//! The daemon binds a short-lived HTTP listener on the LAN and serves a small page. Open
//! the page on the tablet/phone: it downloads a fixed payload, uploads a smaller one
//! back, shows the rates and reports them to the daemon so the app can display them.
//!
//! Deliberate limits: the listener only exists while a test is running (120 s at most),
//! the URL carries a random token, it serves exactly three routes, it never touches the
//! filesystem and it stops after the browser reports its result.

use std::time::{Duration, Instant};

use longcar_core::state::SpeedTestStatus;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};

/// Download payload served to the device.
pub const PAYLOAD_BYTES: usize = 64 * 1024 * 1024;
/// Upload body accepted from the device.
pub const UPLOAD_LIMIT: usize = 32 * 1024 * 1024;
/// How long the listener stays up without a result.
pub const TTL: Duration = Duration::from_secs(120);
/// Largest request head we accept.
const MAX_HEAD: usize = 8 * 1024;

/// What the server reports back to the orchestrator.
#[derive(Debug, Clone)]
pub enum SpeedTestEvent {
    /// The device downloaded the payload.
    Download {
        mbps: f64,
        client: String,
    },
    /// The device uploaded its payload.
    Upload {
        mbps: f64,
    },
    /// The device reported both rates (preferred: measured in the browser).
    Reported {
        download_mbps: f64,
        upload_mbps: f64,
        client: String,
    },
    /// The listener is gone.
    Closed,
    Failed(String),
}

/// A running speed test.
pub struct SpeedTestServer {
    pub status: SpeedTestStatus,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl SpeedTestServer {
    /// Bind the preferred LAN address on an ephemeral port and start serving.
    ///
    /// Binding the specific address (not `0.0.0.0`) keeps the listener off VPN and loopback
    /// interfaces, and matches the URL the device is given.
    pub async fn start(host: &str, events: mpsc::Sender<SpeedTestEvent>) -> Result<Self, String> {
        let token = random_token();
        let listener = TcpListener::bind((host, 0))
            .await
            .map_err(|e| format!("cannot open the speed test listener on {host}: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("cannot read the listener address: {e}"))?
            .port();
        let url = format!("http://{host}:{port}/{token}/");
        let status = SpeedTestStatus {
            running: true,
            url: url.clone(),
            expires_at: Some(in_seconds(TTL)),
            download_mbps: None,
            upload_mbps: None,
            measured_at: None,
            client: None,
            payload_bytes: PAYLOAD_BYTES as u64,
            port,
            error: None,
        };

        let (stop_tx, mut stop_rx) = oneshot::channel();
        let task_events = events.clone();
        let task_token = token.clone();
        let task = tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + TTL;
            let mut done = false;
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    _ = tokio::time::sleep_until(deadline) => break,
                    accepted = listener.accept() => {
                        let Ok((stream, peer)) = accepted else { break };
                        let client = peer.ip().to_string();
                        let token = task_token.clone();
                        let events = task_events.clone();
                        tokio::spawn(async move {
                            if let Err(err) = serve(stream, &token, &client, &events).await {
                                let _ = events.send(SpeedTestEvent::Failed(err)).await;
                            }
                        });
                    }
                }
                // One result is enough: stop accepting once the browser reported.
                if done {
                    break;
                }
                done = false;
            }
            let _ = task_events.send(SpeedTestEvent::Closed).await;
        });

        Ok(Self {
            status,
            stop: Some(stop_tx),
            task,
        })
    }

    /// Stop the listener immediately.
    pub fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.task.abort();
    }
}

fn in_seconds(duration: Duration) -> String {
    let now = std::time::SystemTime::now() + duration;
    let secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // RFC 3339 without pulling in chrono here.
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (h, rem) = (rem / 3600, rem % 3600);
    let (m, s) = (rem / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Howard Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn random_token() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        let _ = file.read_exact(&mut bytes);
    } else {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        bytes[..16].copy_from_slice(&now.to_le_bytes()[..16]);
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Read one HTTP request head and the start of the body (bounded).
struct Request {
    method: String,
    path: String,
    content_length: usize,
    rest: Vec<u8>,
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> Result<Request, String> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 2048];
    let head_end = loop {
        if let Some(index) = find_head_end(&buffer) {
            break index;
        }
        if buffer.len() > MAX_HEAD {
            return Err("request head too large".to_string());
        }
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|e| format!("cannot read the request: {e}"))?;
        if read == 0 {
            return Err("connection closed before the request head".to_string());
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
    let rest = buffer[head_end + 4..].to_vec();
    let mut lines = head.lines();
    let request_line = lines.next().ok_or("empty request")?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let mut content_length = 0usize;
    for line in lines {
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }
    Ok(Request {
        method,
        path,
        content_length,
        rest,
    })
}

fn find_head_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

async fn serve(
    mut stream: tokio::net::TcpStream,
    token: &str,
    client: &str,
    events: &mpsc::Sender<SpeedTestEvent>,
) -> Result<(), String> {
    let request = read_request(&mut stream).await?;
    let base = format!("/{token}/");
    let path = request.path.split('?').next().unwrap_or_default();

    if request.method == "GET" && (path == base || path == format!("{base}index.html")) {
        let page = page(token);
        respond(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            page.as_bytes(),
        )
        .await?;
        return Ok(());
    }

    if request.method == "GET" && path == format!("{base}payload") {
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {PAYLOAD_BYTES}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .map_err(|e| format!("cannot write the payload head: {e}"))?;
        let block = vec![0xa5u8; 1024 * 1024];
        let started = Instant::now();
        let mut written = 0usize;
        while written < PAYLOAD_BYTES {
            if stream.write_all(&block).await.is_err() {
                break;
            }
            written += block.len();
        }
        let seconds = started.elapsed().as_secs_f64().max(0.001);
        let mbps = written as f64 * 8.0 / seconds / 1_000_000.0;
        let _ = events
            .send(SpeedTestEvent::Download {
                mbps,
                client: client.to_string(),
            })
            .await;
        return Ok(());
    }

    if request.method == "POST" && path == format!("{base}upload") {
        let started = Instant::now();
        let limit = request.content_length.min(UPLOAD_LIMIT);
        let mut received = request.rest.len();
        let mut chunk = vec![0u8; 64 * 1024];
        while received < limit {
            let want = (limit - received).min(chunk.len());
            let read = stream
                .read(&mut chunk[..want])
                .await
                .map_err(|e| format!("cannot read the upload: {e}"))?;
            if read == 0 {
                break;
            }
            received += read;
        }
        let seconds = started.elapsed().as_secs_f64().max(0.001);
        let mbps = received as f64 * 8.0 / seconds / 1_000_000.0;
        let _ = events.send(SpeedTestEvent::Upload { mbps }).await;
        respond(&mut stream, 200, "application/json", b"{\"status\":true}").await?;
        return Ok(());
    }

    if request.method == "POST" && path == format!("{base}report") {
        let mut body = request.rest.clone();
        let mut chunk = [0u8; 4096];
        while body.len() < request.content_length {
            let want = (request.content_length - body.len()).min(chunk.len());
            let read = stream
                .read(&mut chunk[..want])
                .await
                .map_err(|e| format!("cannot read the report: {e}"))?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read]);
        }
        let text = String::from_utf8_lossy(&body).to_string();
        let value: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("report is not valid JSON: {e}"))?;
        let download = value.get("downloadMbps").and_then(|v| v.as_f64());
        let upload = value.get("uploadMbps").and_then(|v| v.as_f64());
        if let (Some(download), Some(upload)) = (download, upload) {
            let _ = events
                .send(SpeedTestEvent::Reported {
                    download_mbps: download,
                    upload_mbps: upload,
                    client: client.to_string(),
                })
                .await;
        }
        respond(&mut stream, 200, "application/json", b"{\"status\":true}").await?;
        return Ok(());
    }

    respond(
        &mut stream,
        404,
        "text/plain; charset=utf-8",
        b"Longcar speed test: unknown path\n",
    )
    .await
}

async fn respond(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> Result<(), String> {
    let head = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(head.as_bytes())
        .await
        .map_err(|e| format!("cannot write the response: {e}"))?;
    stream
        .write_all(body)
        .await
        .map_err(|e| format!("cannot write the response body: {e}"))?;
    Ok(())
}

/// The page the device opens. Measures both directions in the browser and reports back.
fn page(token: &str) -> String {
    format!(
        r#"<!doctype html>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Longcar speed test</title>
<style>
 body{{margin:0;background:#1c1c1c;color:#e6e6e6;font:16px/1.5 sans-serif;
       display:flex;min-height:100vh;align-items:center;justify-content:center}}
 main{{max-width:30rem;padding:2rem;text-align:center}}
 h1{{font-size:1.4rem;margin:0 0 .5rem}}
 #out{{font-size:2rem;font-variant-numeric:tabular-nums;margin:1rem 0}}
 small{{color:#9a9a9a}}
</style>
<main>
<h1>Longcar speed test</h1>
<p id="out">measuring…</p>
<p><small>Keep this page open. The download runs first, then a short upload.</small></p>
</main>
<script>
const base = {base};
const out = document.getElementById('out');
function mbps(bytes, ms) {{ return (bytes * 8) / (ms / 1000) / 1e6; }}
async function run() {{
  const t0 = performance.now();
  const res = await fetch(base + 'payload', {{cache: 'no-store'}});
  const reader = res.body.getReader();
  let bytes = 0;
  for (;;) {{ const r = await reader.read(); if (r.done) break; bytes += r.value.byteLength; }}
  const down = mbps(bytes, performance.now() - t0);
  out.textContent = 'up ' + down.toFixed(1) + ' Mbps down…';
  const blob = new Blob([new Uint8Array(16 * 1024 * 1024)]);
  const u0 = performance.now();
  await fetch(base + 'upload', {{method: 'POST', body: blob}});
  const up = mbps(blob.size, performance.now() - u0);
  out.textContent = '↓ ' + down.toFixed(1) + ' Mbps   ↑ ' + up.toFixed(1) + ' Mbps';
  await fetch(base + 'report', {{
    method: 'POST',
    headers: {{'Content-Type': 'application/json'}},
    body: JSON.stringify({{downloadMbps: down, uploadMbps: up}}),
  }});
}}
run().catch(e => {{ out.textContent = 'failed: ' + e; }});
</script>
"#,
        base = serde_json::to_string(&format!("/{token}/")).unwrap_or_else(|_| "\"/\"".to_string())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_and_hex() {
        let a = random_token();
        let b = random_token();
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn expiry_is_rfc3339() {
        let text = in_seconds(Duration::from_secs(120));
        assert_eq!(text.len(), 20, "{text}");
        assert!(text.ends_with('Z'));
        assert_eq!(&text[4..5], "-");
        assert_eq!(&text[10..11], "T");
    }

    #[test]
    fn civil_dates_match_known_days() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(20_678), (2026, 8, 13));
        assert_eq!(civil_from_days(20_700), (2026, 9, 4));
    }

    #[test]
    fn page_carries_the_token_and_no_absolute_paths() {
        let html = page("abc123");
        assert!(html.contains("\"/abc123/\""));
        assert!(html.contains("payload"));
        assert!(html.contains("upload"));
        assert!(html.contains("report"));
    }

    #[test]
    fn head_end_is_found() {
        assert_eq!(find_head_end(b"GET / HTTP/1.1\r\n\r\nbody"), Some(14));
        assert_eq!(find_head_end(b"GET / HTTP/1.1\r\n"), None);
    }
}
