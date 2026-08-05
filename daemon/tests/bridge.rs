//! Integration tests for the filesystem bridge transport.
//!
//! Each test uses a tempfile temp dir so the real ~/.turbofig is never touched.

use futures_util::{SinkExt, StreamExt};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_tungstenite::{connect_async, tungstenite::Message as TtMessage};

// ── helpers ───────────────────────────────────────────────────────────────────

/// Poll for a file to appear, up to `deadline_ms` milliseconds.
/// Returns the file contents when found, or panics on timeout.
async fn poll_file(path: &PathBuf, deadline_ms: u64) -> String {
    let deadline = Instant::now() + Duration::from_millis(deadline_ms);
    loop {
        if Instant::now() >= deadline {
            panic!("Timed out waiting for {}", path.display());
        }
        match tokio::fs::read_to_string(path).await {
            Ok(contents) => return contents,
            Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
}

/// Write a job file to inbox and return the expected outbox path.
async fn write_job(dir: &tempfile::TempDir, id: &str, body: serde_json::Value) -> PathBuf {
    let inbox = dir.path().join("inbox");
    tokio::fs::create_dir_all(&inbox)
        .await
        .expect("create inbox");
    let job_path = inbox.join(format!("{id}.json"));
    tokio::fs::write(&job_path, body.to_string())
        .await
        .expect("write job");
    dir.path().join("outbox").join(format!("{id}.json"))
}

/// Spawn the bridge server in the background.
fn spawn_bridge(state: Arc<turbofig_mcp::AppState>, dir: PathBuf) {
    tokio::spawn(async move {
        turbofig_mcp::serve_bridge(state, dir)
            .await
            .expect("serve_bridge error in test");
    });
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Full round-trip: mock plugin replies to STATUS; bridge returns connected shape.
#[tokio::test]
async fn test_bridge_status_roundtrip_with_plugin() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(turbofig_mcp::AppState::new());

    // Bind an ephemeral WS listener and spawn the WS server.
    let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind WS port");
    let ws_addr = ws_listener.local_addr().expect("ws local addr");
    let ws_state = state.clone();
    tokio::spawn(async move {
        turbofig_mcp::serve_ws(ws_listener, ws_state)
            .await
            .expect("serve_ws error");
    });

    // Spawn the bridge.
    spawn_bridge(state.clone(), tmp.path().to_path_buf());

    // Connect the mock plugin and send FILE_INFO.
    let (mut plugin_ws, _) = connect_async(format!("ws://127.0.0.1:{}/", ws_addr.port()))
        .await
        .expect("mock plugin connect");

    plugin_ws
        .send(TtMessage::Text(
            serde_json::json!({
                "type": "FILE_INFO",
                "fileKey": "abc123",
                "name": "My Design File"
            })
            .to_string(),
        ))
        .await
        .expect("send FILE_INFO");

    // Allow FILE_INFO to be registered.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Spawn a task that auto-replies to STATUS frames.
    tokio::spawn(async move {
        while let Some(Ok(msg)) = plugin_ws.next().await {
            if let TtMessage::Text(text) = msg {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    if json.get("type").and_then(|t| t.as_str()) == Some("STATUS") {
                        if let Some(id) = json.get("requestId").and_then(|v| v.as_u64()) {
                            let reply = serde_json::json!({
                                "type": "RESULT",
                                "requestId": id,
                                "ok": true,
                                "fileKey": "abc123",
                                "name": "My Design File"
                            });
                            let _ = plugin_ws.send(TtMessage::Text(reply.to_string())).await;
                        }
                    }
                }
            }
        }
    });

    // Write the status job.
    let outbox_path = write_job(&tmp, "job1", serde_json::json!({"op": "status"})).await;

    // Poll for the result (up to 2 s).
    let contents = poll_file(&outbox_path, 2000).await;
    let payload: serde_json::Value =
        serde_json::from_str(&contents).expect("outbox file is valid JSON");

    assert_eq!(payload["ok"], serde_json::json!(true), "ok must be true");
    assert_eq!(
        payload["plugin"]["connected"],
        serde_json::json!(true),
        "plugin.connected must be true"
    );
    assert_eq!(
        payload["plugin"]["fileKey"],
        serde_json::json!("abc123"),
        "plugin.fileKey must match"
    );

    // The inbox file must have been removed.
    let inbox_path = tmp.path().join("inbox").join("job1.json");
    assert!(
        !inbox_path.exists(),
        "inbox file must be removed after processing"
    );
}

/// No plugin attached: bridge returns plugin.connected:false immediately.
#[tokio::test]
async fn test_bridge_status_no_plugin() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(turbofig_mcp::AppState::new());

    spawn_bridge(state.clone(), tmp.path().to_path_buf());

    let outbox_path = write_job(&tmp, "job_noplugin", serde_json::json!({"op": "status"})).await;
    let contents = poll_file(&outbox_path, 2000).await;
    let payload: serde_json::Value =
        serde_json::from_str(&contents).expect("outbox file is valid JSON");

    assert_eq!(payload["ok"], serde_json::json!(true), "ok must be true");
    assert_eq!(
        payload["plugin"]["connected"],
        serde_json::json!(false),
        "plugin.connected must be false when no plugin is attached"
    );
}

/// Unknown op returns ok:false with an error message; the watcher keeps running
/// and a follow-up valid job is still serviced.
#[tokio::test]
async fn test_bridge_unknown_op() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(turbofig_mcp::AppState::new());

    spawn_bridge(state.clone(), tmp.path().to_path_buf());

    // Write an unknown op.
    let out_unknown = write_job(&tmp, "job_bad", serde_json::json!({"op": "frobnicate"})).await;
    let contents = poll_file(&out_unknown, 2000).await;
    let payload: serde_json::Value =
        serde_json::from_str(&contents).expect("outbox file is valid JSON");

    assert_eq!(
        payload["ok"],
        serde_json::json!(false),
        "unknown op must return ok:false"
    );
    assert!(
        payload["error"].as_str().is_some(),
        "unknown op must include an error string"
    );

    // Write a valid job to prove the watcher is still running.
    let out_valid = write_job(&tmp, "job_after", serde_json::json!({"op": "status"})).await;
    let contents2 = poll_file(&out_valid, 2000).await;
    let payload2: serde_json::Value =
        serde_json::from_str(&contents2).expect("follow-up outbox file is valid JSON");

    assert_eq!(
        payload2["ok"],
        serde_json::json!(true),
        "follow-up status job must succeed, proving the watcher kept running"
    );
}
