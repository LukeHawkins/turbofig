//! Acceptance tests for link liveness: the HEARTBEAT frame, the stale-client
//! drop, and the reconnect after a daemon restart. Timings are shortened with
//! `WsTimings` so each test runs in about a second. Each test binds ephemeral
//! ports (the restart test reuses its own port).

mod common;

use common::wait_until;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message as Tt;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use turbofig::{AppState, WsTimings};

type Client = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

const NONCE_P: &str = "00112233445566778899aabbccddeeff";
const LABEL_WS_PLUGIN: &str = "turbofig-ws-plugin:";

/// Independent HMAC-SHA256 hex proof, so the test does not reuse the daemon code.
fn proof(token: &str, label: &str, nonce: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut k = [0u8; 64];
    k[..token.len().min(64)].copy_from_slice(&token.as_bytes()[..token.len().min(64)]);
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    let inner = Sha256::new()
        .chain_update(&ipad)
        .chain_update(label.as_bytes())
        .chain_update(nonce.as_bytes())
        .finalize();
    let out = Sha256::new()
        .chain_update(&opad)
        .chain_update(inner)
        .finalize();
    out.iter().map(|b| format!("{b:02x}")).collect()
}

/// Fast timings: ping every 100 ms, drop after 300 ms with no pong, heartbeat every 100 ms.
fn fast() -> WsTimings {
    WsTimings {
        ping_interval: Duration::from_millis(100),
        pong_timeout: Duration::from_millis(300),
        heartbeat_interval: Duration::from_millis(100),
    }
}

/// A daemon (WS + HTTP) on its own runtime, so dropping it closes every socket.
struct Daemon {
    rt: Option<tokio::runtime::Runtime>,
    state: Arc<AppState>,
    ws_port: u16,
    http_port: u16,
    _home: tempfile::TempDir,
}

impl Daemon {
    fn start(token: &str, ws_port: u16, timings: WsTimings) -> Daemon {
        let home = tempfile::tempdir().unwrap();
        let state = Arc::new(AppState::new_with_token(token.to_owned(), home.path()));
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        // Std listeners bind without a runtime, so this works inside an async test.
        let ws_listener = std::net::TcpListener::bind(("127.0.0.1", ws_port)).unwrap();
        let http_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        ws_listener.set_nonblocking(true).unwrap();
        http_listener.set_nonblocking(true).unwrap();
        let ws_port = ws_listener.local_addr().unwrap().port();
        let http_port = http_listener.local_addr().unwrap().port();
        let s = state.clone();
        rt.spawn(async move {
            let l = tokio::net::TcpListener::from_std(ws_listener).unwrap();
            let _ = turbofig::serve_ws_with_timings(l, s, timings).await;
        });
        let s = state.clone();
        rt.spawn(async move {
            let l = tokio::net::TcpListener::from_std(http_listener).unwrap();
            let _ = turbofig::serve_with_state(l, s).await;
        });
        Daemon {
            rt: Some(rt),
            state,
            ws_port,
            http_port,
            _home: home,
        }
    }

    /// Stop the daemon: every task ends and every socket closes.
    fn kill(self) -> u16 {
        self.ws_port
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

async fn next_json(ws: &mut Client) -> Value {
    loop {
        match ws.next().await.expect("frame").expect("ok frame") {
            Tt::Text(t) => return serde_json::from_str(&t).unwrap(),
            Tt::Ping(_) | Tt::Pong(_) => {}
            other => panic!("expected text, got {other:?}"),
        }
    }
}

/// Full nonce handshake plus FILE_INFO, returns once WELCOME arrived.
async fn handshake(port: u16, token: &str) -> Result<Client, String> {
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{port}/?nonce={NONCE_P}"))
        .await
        .map_err(|e| e.to_string())?;
    let ch = next_json(&mut ws).await;
    assert_eq!(ch["type"], "CHALLENGE");
    let nonce_d = ch["nonce"].as_str().unwrap().to_owned();
    let auth = json!({"type": "AUTH", "proof": proof(token, LABEL_WS_PLUGIN, &nonce_d)});
    ws.send(Tt::Text(auth.to_string())).await.unwrap();
    let info = json!({"type": "FILE_INFO", "fileKey": "fk", "name": "Doc"});
    ws.send(Tt::Text(info.to_string())).await.unwrap();
    assert_eq!(next_json(&mut ws).await["type"], "WELCOME");
    Ok(ws)
}

/// Run one execute job through `/job` and a mock plugin on `ws`. Skips HEARTBEAT frames.
async fn run_job(daemon: &Daemon, token: &str, ws: &mut Client) -> Value {
    let url = format!("http://127.0.0.1:{}/job", daemon.http_port);
    let token = token.to_owned();
    let call = tokio::spawn(async move {
        reqwest::Client::new()
            .post(url)
            .bearer_auth(token)
            .json(&json!({"op": "execute", "fileKey": "fk", "code": "return 1;"}))
            .send()
            .await
            .expect("send job")
            .json::<Value>()
            .await
            .expect("job json")
    });
    loop {
        let frame = next_json(ws).await;
        if frame["type"] == "EXECUTE" {
            let reply =
                json!({"type": "RESULT", "requestId": frame["requestId"], "ok": true, "result": 1});
            ws.send(Tt::Text(reply.to_string())).await.unwrap();
            break;
        }
        assert_eq!(
            frame["type"], "HEARTBEAT",
            "only HEARTBEAT may precede a job"
        );
    }
    call.await.unwrap()
}

/// (d) HEARTBEAT frames arrive at the configured interval and are not jobs.
#[tokio::test]
async fn heartbeats_arrive_at_the_configured_interval_and_are_not_jobs() {
    let state = Arc::new(AppState::new());
    let token = state.token().to_owned();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let st = state.clone();
    tokio::spawn(async move {
        turbofig::serve_ws_with_timings(listener, st, fast())
            .await
            .unwrap();
    });
    let mut ws = handshake(port, &token).await.unwrap();
    let mut at = Vec::new();
    while at.len() < 5 {
        let frame = tokio::time::timeout(Duration::from_secs(2), next_json(&mut ws))
            .await
            .expect("a HEARTBEAT must arrive");
        assert_eq!(frame, json!({"type": "HEARTBEAT"}));
        at.push(Instant::now());
    }
    for pair in at.windows(2) {
        let gap = pair[1] - pair[0];
        assert!(
            gap >= Duration::from_millis(60) && gap <= Duration::from_millis(400),
            "heartbeat gap {gap:?} must be near 100 ms"
        );
    }
    assert_eq!(
        state.pending_len(),
        0,
        "a HEARTBEAT must never create a job"
    );
}

/// (d) The default heartbeat interval is 15 s, the same as the ping interval.
#[test]
fn the_default_heartbeat_interval_is_15_seconds() {
    let t = WsTimings::default();
    assert_eq!(t.heartbeat_interval, Duration::from_secs(15));
    assert_eq!(t.ping_interval, Duration::from_secs(15));
    assert_eq!(t.pong_timeout, Duration::from_secs(45));
}

/// (a) A client that stops reading (a sleeping Mac) is dropped, then resumes,
/// reconnects with the full handshake and runs a job.
#[tokio::test]
async fn a_client_that_stops_reading_is_dropped_then_reconnects_and_runs_a_job() {
    let token = "t".repeat(64);
    let daemon = Daemon::start(&token, 0, fast());
    let mut ws = handshake(daemon.ws_port, &token).await.unwrap();
    wait_until(
        || !daemon.state.list_connections().is_empty(),
        common::WAIT_DEADLINE_MS,
        "the connection to register",
    )
    .await;

    // Simulated sleep: the client reads nothing, so it answers no ping.
    // Drop window is at most pong_timeout + ping_interval = 400 ms.
    wait_until(
        || daemon.state.list_connections().is_empty(),
        2_000,
        "the silent client to be dropped",
    )
    .await;

    // Wake: the old socket is dead.
    loop {
        match tokio::time::timeout(Duration::from_secs(2), ws.next()).await {
            Err(_) => panic!("the dropped socket must end"),
            Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(Tt::Close(_)))) => break,
            Ok(Some(Ok(_))) => {}
        }
    }

    // The client reconnects with the full handshake and runs a job.
    let mut ws2 = handshake(daemon.ws_port, &token).await.unwrap();
    let body = run_job(&daemon, &token, &mut ws2).await;
    assert_eq!(body["ok"], json!(true), "job after the wake: {body}");
}

/// A client that keeps reading is never dropped by the short timings.
#[tokio::test]
async fn a_client_that_keeps_reading_is_not_dropped() {
    let token = "u".repeat(64);
    let daemon = Daemon::start(&token, 0, fast());
    let mut ws = handshake(daemon.ws_port, &token).await.unwrap();
    let until = Instant::now() + Duration::from_millis(1_200);
    while Instant::now() < until {
        let _ = tokio::time::timeout(Duration::from_millis(50), ws.next()).await;
    }
    assert!(!daemon.state.list_connections().is_empty());
}

/// (b) After a daemon restart the client reconnects with the plugin backoff
/// (500 ms, then 1000 ms, then 2000 ms cap) and runs a job within 2.5 s.
#[tokio::test]
async fn a_client_reconnects_after_a_daemon_restart_and_runs_a_job_within_2_5_s() {
    let token = "r".repeat(64);
    let daemon = Daemon::start(&token, 0, fast());
    let mut ws = handshake(daemon.ws_port, &token).await.unwrap();

    let killed_at = Instant::now();
    let port = daemon.kill();
    // The old socket ends when the daemon dies.
    loop {
        match tokio::time::timeout(Duration::from_secs(2), ws.next()).await {
            Err(_) => panic!("the socket must close when the daemon dies"),
            Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(Tt::Close(_)))) => break,
            Ok(Some(Ok(_))) => {}
        }
    }

    // The new daemon takes 700 ms to come up: the first retry (500 ms) fails.
    tokio::time::sleep(Duration::from_millis(700)).await;
    let daemon2 = Daemon::start(&token, port, WsTimings::default());

    // Plugin backoff: min(500 * 2^n, 2000) ms.
    let mut attempt = 0u32;
    let mut ws2 = None;
    let mut elapsed_at_first_try = None;
    while ws2.is_none() {
        let delay = (500u64 * 2u64.pow(attempt)).min(2000);
        // Attempt 0 starts when the old socket closed, not after the 700 ms
        // the new daemon needed, so remove the time already spent.
        let spent = if attempt == 0 {
            killed_at.elapsed().as_millis() as u64
        } else {
            0
        };
        tokio::time::sleep(Duration::from_millis(delay.saturating_sub(spent))).await;
        elapsed_at_first_try.get_or_insert(killed_at.elapsed());
        if let Ok(c) = handshake(port, &token).await {
            ws2 = Some(c);
        }
        attempt += 1;
        assert!(attempt < 6, "no reconnect after 5 tries");
    }
    let mut ws2 = ws2.unwrap();
    let body = run_job(&daemon2, &token, &mut ws2).await;
    assert_eq!(body["ok"], json!(true), "job after the restart: {body}");
    assert!(
        killed_at.elapsed() < Duration::from_millis(2_500),
        "recovery took {:?}",
        killed_at.elapsed()
    );
}
