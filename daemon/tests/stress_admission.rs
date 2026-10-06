//! Reproduces the production incident: 6 concurrent agent workers sending
//! execute jobs over one plugin connection, each retrying on every timeout.
//!
//! Before admission control (`state::try_admit`), every incoming job was
//! forwarded straight to the plugin regardless of how many were already
//! in flight. Under 6x concurrent load against a plugin that can only run
//! one job at a time (the real Figma main thread is single-threaded), the
//! daemon-side queue of in-flight requests grew without bound: every caller
//! waited the full round trip, many hit the daemon's own request timeout,
//! and every one of them retried, compounding the load (a thundering herd).
//! Nothing told a caller "the lane is full, back off"; nothing told a caller
//! whether a timed-out job had actually started, so a retry always risked a
//! duplicate mutation.
//!
//! This test drives 6 concurrent clients x 20 jobs (with retries) against a
//! fake plugin that runs jobs strictly serially with a fixed per-job delay,
//! against a daemon configured with a small admission cap. It asserts:
//!   1. The plugin connection survives the whole run (no disconnect).
//!   2. `busy` responses appear (admission control rejects over-capacity
//!      jobs immediately, rather than letting them queue into a timeout).
//!   3. No pending entry ever leaks (the pending map is empty once every
//!      client has finished).
//!   4. No timeout, disconnect, or idempotency-risk response is ever seen:
//!      every response is either a clean success or `busy`.

use futures_util::{SinkExt, StreamExt};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::{connect_async, tungstenite::Message as TtMessage};

/// How long the fake plugin takes to "run" each job before replying. Models
/// a real Figma job (font load, node creation) that takes a noticeable
/// fraction of a second, not an instant echo.
const FAKE_JOB_DELAY: Duration = Duration::from_millis(30);
/// Concurrent callers, matching the incident's 6 agent workers.
const WORKERS: usize = 6;
/// Jobs per worker, matching the incident's 20-jobs-per-worker load.
const JOBS_PER_WORKER: usize = 20;
/// The daemon's admission cap for this test: small enough that 6 concurrent
/// callers reliably collide with it.
const MAX_INFLIGHT: usize = 3;
/// Ceiling on busy-retry attempts for one job, so a regression that makes
/// admission control never free up a slot fails the test instead of hanging
/// it forever.
const MAX_BUSY_RETRIES: usize = 200;

/// Start a WS server with a fresh, admission-limited AppState.
/// Returns (ws_port, state).
async fn start_ws_stack() -> (u16, Arc<turbofig::AppState>) {
    let state = Arc::new(turbofig::AppState::with_timeout_and_max_inflight(
        Duration::from_secs(5),
        MAX_INFLIGHT,
    ));

    let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral WS port");
    let ws_port = ws_listener.local_addr().expect("ws local addr").port();

    let ws_state = state.clone();
    tokio::spawn(async move {
        turbofig::serve_ws(ws_listener, ws_state)
            .await
            .expect("serve_ws error in test");
    });

    (ws_port, state)
}

/// Connects a fake plugin that processes EXECUTE jobs strictly one at a
/// time: for each frame it reads off the socket, it sends STARTED, waits
/// `FAKE_JOB_DELAY`, then sends a RESULT, before ever reading the next
/// frame. This models the real plugin's single-threaded FIFO queue well
/// enough to reproduce queue starvation under concurrent load.
async fn connect_serial_fake_plugin(ws_port: u16, state: &Arc<turbofig::AppState>, file_key: &str) {
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{ws_port}/?token={}", state.token()))
        .await
        .expect("fake plugin connect");

    let fi = serde_json::json!({"type": "FILE_INFO", "fileKey": file_key, "name": file_key});
    ws.send(TtMessage::Text(fi.to_string()))
        .await
        .expect("send FILE_INFO");

    // Wait for registration to land before returning, so callers never race
    // run_execute against a not-yet-registered connection.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if state
            .list_connections()
            .iter()
            .any(|(_, fk, _)| fk == file_key)
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "fake plugin must register before the test proceeds"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    tokio::spawn(async move {
        while let Some(Ok(TtMessage::Text(text))) = ws.next().await {
            let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            if json.get("type").and_then(|t| t.as_str()) != Some("EXECUTE") {
                continue;
            }
            let Some(id) = json.get("requestId").and_then(|v| v.as_u64()) else {
                continue;
            };
            let started = serde_json::json!({"type": "STARTED", "requestId": id});
            let _ = ws.send(TtMessage::Text(started.to_string())).await;

            tokio::time::sleep(FAKE_JOB_DELAY).await;

            let reply = serde_json::json!({"type": "RESULT", "requestId": id, "ok": true, "result": "ping"});
            let _ = ws.send(TtMessage::Text(reply.to_string())).await;
        }
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn six_concurrent_workers_retrying_on_busy_never_time_out_or_disconnect() {
    let (ws_port, state) = start_ws_stack().await;
    connect_serial_fake_plugin(ws_port, &state, "stress-fk").await;

    let busy_count = Arc::new(AtomicUsize::new(0));
    let success_count = Arc::new(AtomicUsize::new(0));
    let bad_outcome: Arc<std::sync::Mutex<Vec<serde_json::Value>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));

    let mut handles = Vec::new();
    for worker in 0..WORKERS {
        let state = state.clone();
        let busy_count = busy_count.clone();
        let success_count = success_count.clone();
        let bad_outcome = bad_outcome.clone();
        handles.push(tokio::spawn(async move {
            for job in 0..JOBS_PER_WORKER {
                let mut attempts = 0;
                loop {
                    attempts += 1;
                    assert!(
                        attempts <= MAX_BUSY_RETRIES,
                        "worker {worker} job {job} never got past 'busy': admission control is not releasing slots"
                    );
                    let result = turbofig::run_execute(
                        &state,
                        None,
                        Some("stress-fk"),
                        "return 'ping';",
                    )
                    .await;

                    if result.get("ok").and_then(|v| v.as_bool()) == Some(true) {
                        success_count.fetch_add(1, Ordering::SeqCst);
                        break;
                    }

                    let code = result.get("code").and_then(|v| v.as_str()).unwrap_or("");
                    if code == "busy" {
                        busy_count.fetch_add(1, Ordering::SeqCst);
                        let retry_after = result
                            .get("retryAfterMs")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(50);
                        tokio::time::sleep(Duration::from_millis(retry_after)).await;
                        continue;
                    }

                    // Any other ok:false outcome (timeout, not_started,
                    // started_unknown, plugin_disconnected, script_error) is
                    // exactly what this fix must prevent under this load.
                    bad_outcome.lock().unwrap().push(result);
                    break;
                }
            }
        }));
    }

    for h in handles {
        h.await.expect("worker task must not panic");
    }

    let bad = bad_outcome.lock().unwrap();
    assert!(
        bad.is_empty(),
        "no job may time out, disconnect, or come back ok:false for any reason \
         other than busy under this load; saw: {bad:?}"
    );
    drop(bad);

    assert_eq!(
        success_count.load(Ordering::SeqCst),
        WORKERS * JOBS_PER_WORKER,
        "every job must eventually succeed"
    );
    assert!(
        busy_count.load(Ordering::SeqCst) > 0,
        "admission control must have rejected at least one job as busy under \
         6x concurrent load against a cap of {MAX_INFLIGHT}"
    );

    // The connection must have survived the whole run: still registered, and
    // no pending entry left dangling for it.
    assert!(
        state
            .list_connections()
            .iter()
            .any(|(_, fk, _)| fk == "stress-fk"),
        "the plugin connection must still be registered: it must never have been dropped"
    );
    assert_eq!(
        state.pending_len(),
        0,
        "no pending request may leak once every worker has finished"
    );
}
