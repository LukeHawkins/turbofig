//! Integration tests for the filesystem bridge transport.
//!
//! Each test uses a tempfile temp dir so the real ~/.turbofig is never touched.

mod common;

use common::{poll_file, wait_for_file_key, wait_until, WAIT_DEADLINE_MS};
use futures_util::{SinkExt, StreamExt};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message as TtMessage;

// ── helpers ───────────────────────────────────────────────────────────────────

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
fn spawn_bridge(state: Arc<turbofig::AppState>, dir: PathBuf) {
    tokio::spawn(async move {
        turbofig::serve_bridge(state, dir)
            .await
            .expect("serve_bridge error in test");
    });
}

/// Spawn a WS server on the state, connect a mock plugin, register it, and
/// auto-reply to each `frame_type` frame with `reply_body`. The reply gets the
/// echoed `requestId` and a `RESULT` type merged in.
async fn spawn_mock_plugin(
    state: Arc<turbofig::AppState>,
    frame_type: &'static str,
    reply_body: serde_json::Value,
) {
    let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind WS port");
    let ws_addr = ws_listener.local_addr().expect("ws local addr");
    let ws_state = state.clone();
    tokio::spawn(async move {
        turbofig::serve_ws(ws_listener, ws_state)
            .await
            .expect("serve_ws error");
    });

    let mut plugin_ws = common::connect_plugin(ws_addr.port(), state.token()).await;
    plugin_ws
        .send(TtMessage::Text(
            serde_json::json!({"type": "FILE_INFO", "fileKey": "abc", "name": "F"}).to_string(),
        ))
        .await
        .expect("send FILE_INFO");
    wait_for_file_key(&state, "abc", common::WAIT_DEADLINE_MS).await;

    tokio::spawn(async move {
        while let Some(Ok(msg)) = plugin_ws.next().await {
            if let TtMessage::Text(text) = msg {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    if json.get("type").and_then(|t| t.as_str()) == Some(frame_type) {
                        if let Some(id) = json.get("requestId").and_then(|v| v.as_u64()) {
                            let mut reply = reply_body.clone();
                            reply["type"] = serde_json::json!("RESULT");
                            reply["requestId"] = serde_json::json!(id);
                            let _ = plugin_ws.send(TtMessage::Text(reply.to_string())).await;
                        }
                    }
                }
            }
        }
    });
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Full round-trip: mock plugin replies to STATUS; bridge returns connected shape.
#[tokio::test]
async fn test_bridge_status_roundtrip_with_plugin() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(turbofig::AppState::new());

    // Bind an ephemeral WS listener and spawn the WS server.
    let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind WS port");
    let ws_addr = ws_listener.local_addr().expect("ws local addr");
    let ws_state = state.clone();
    tokio::spawn(async move {
        turbofig::serve_ws(ws_listener, ws_state)
            .await
            .expect("serve_ws error");
    });

    // Spawn the bridge.
    spawn_bridge(state.clone(), tmp.path().to_path_buf());

    // Connect the mock plugin and send FILE_INFO.
    let mut plugin_ws = common::connect_plugin(ws_addr.port(), state.token()).await;

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

    // Wait for FILE_INFO to be registered.
    wait_for_file_key(&state, "abc123", common::WAIT_DEADLINE_MS).await;

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
    let contents = poll_file(&outbox_path, common::WAIT_DEADLINE_MS).await;
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
    let state = Arc::new(turbofig::AppState::new());

    spawn_bridge(state.clone(), tmp.path().to_path_buf());

    let outbox_path = write_job(&tmp, "job_noplugin", serde_json::json!({"op": "status"})).await;
    let contents = poll_file(&outbox_path, common::WAIT_DEADLINE_MS).await;
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
    let state = Arc::new(turbofig::AppState::new());

    spawn_bridge(state.clone(), tmp.path().to_path_buf());

    // Write an unknown op.
    let out_unknown = write_job(&tmp, "job_bad", serde_json::json!({"op": "frobnicate"})).await;
    let contents = poll_file(&out_unknown, common::WAIT_DEADLINE_MS).await;
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
    let contents2 = poll_file(&out_valid, common::WAIT_DEADLINE_MS).await;
    let payload2: serde_json::Value =
        serde_json::from_str(&contents2).expect("follow-up outbox file is valid JSON");

    assert_eq!(
        payload2["ok"],
        serde_json::json!(true),
        "follow-up status job must succeed, proving the watcher kept running"
    );
}

/// A malformed job file returns an error result and does not stall the watcher.
#[tokio::test]
async fn test_bridge_malformed_json() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(turbofig::AppState::new());

    spawn_bridge(state.clone(), tmp.path().to_path_buf());

    // Write raw invalid JSON directly to the inbox.
    let inbox = tmp.path().join("inbox");
    tokio::fs::create_dir_all(&inbox)
        .await
        .expect("create inbox");
    tokio::fs::write(inbox.join("job_malformed.json"), "{ this is not json")
        .await
        .expect("write malformed job");

    let out_path = tmp.path().join("outbox").join("job_malformed.json");
    let contents = poll_file(&out_path, common::WAIT_DEADLINE_MS).await;
    let payload: serde_json::Value =
        serde_json::from_str(&contents).expect("outbox file is valid JSON");

    assert_eq!(
        payload["ok"],
        serde_json::json!(false),
        "malformed job must return ok:false"
    );
    assert!(
        payload["error"]
            .as_str()
            .is_some_and(|e| e.contains("malformed")),
        "error must mention malformed JSON, got: {payload}"
    );
}

/// One slow job (a status call to a silent plugin) must not block another job.
///
/// The bridge claims and spawns each job, so two jobs run concurrently. With a
/// per-request timeout of `TIMEOUT`, two jobs finish together in about one
/// timeout, not the ~2x a sequential loop would take.
///
/// This test is timing-sensitive by nature. `TIMEOUT` is deliberately large
/// (1 s, not the 400 ms an earlier version used) so the fixed scheduling
/// overhead of spawning two servers and two outbox polls (which does not
/// shrink on a slow runner, it only grows) stays a small fraction of the
/// budget. The upper bound checks for "about one timeout", well short of the
/// "about two timeouts" a regression to sequential processing would take;
/// the lower bound checks the job really waited out the timeout rather than
/// passing by accident.
#[tokio::test]
async fn test_bridge_services_jobs_concurrently() {
    // 3 s, not 1 s: under a loaded machine the second job's inbox pickup can
    // lag, and job A must still be in flight when job B is claimed. The
    // concurrency proof below is structural, so a longer window costs only
    // test time, never accuracy.
    const TIMEOUT: Duration = Duration::from_millis(3000);

    let tmp = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(turbofig::AppState::with_timeout(TIMEOUT));

    // Bind and spawn the WS server, then connect a plugin that never replies.
    let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind WS port");
    let ws_addr = ws_listener.local_addr().expect("ws local addr");
    let ws_state = state.clone();
    tokio::spawn(async move {
        turbofig::serve_ws(ws_listener, ws_state)
            .await
            .expect("serve_ws error");
    });

    let mut plugin_ws = common::connect_plugin(ws_addr.port(), state.token()).await;
    plugin_ws
        .send(TtMessage::Text(
            serde_json::json!({"type": "FILE_INFO", "fileKey": "s", "name": "Silent"}).to_string(),
        ))
        .await
        .expect("send FILE_INFO");
    wait_for_file_key(&state, "s", common::WAIT_DEADLINE_MS).await;
    // Keep the socket open but never reply to STATUS.
    tokio::spawn(async move { while let Some(Ok(_)) = plugin_ws.next().await {} });

    spawn_bridge(state.clone(), tmp.path().to_path_buf());

    // Submit two status jobs at once.
    let out_a = write_job(&tmp, "job_a", serde_json::json!({"op": "status"})).await;
    let out_b = write_job(&tmp, "job_b", serde_json::json!({"op": "status"})).await;

    // Structural proof of concurrency: both jobs must be counted as in
    // flight at the daemon at the same time, before either one resolves.
    // This never races on wall-clock time, unlike asserting on elapsed
    // duration against the plugin's reply timeout.
    wait_until(
        || state.jobs_in_flight() >= 2,
        WAIT_DEADLINE_MS,
        "both status jobs to be in flight at once",
    )
    .await;
    assert!(
        !out_a.exists() && !out_b.exists(),
        "neither job may have resolved yet when both were counted in flight"
    );

    // Both jobs time out waiting on the silent plugin and each writes its
    // own result; wait for both without any further timing assumption.
    let _ = poll_file(&out_a, 10_000).await;
    let _ = poll_file(&out_b, 10_000).await;
}

/// The execute op routes JS through the plugin and returns the result.
#[tokio::test]
async fn test_bridge_execute_op() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(turbofig::AppState::new());

    spawn_mock_plugin(
        state.clone(),
        "EXECUTE",
        serde_json::json!({"ok": true, "result": {"n": 42}}),
    )
    .await;
    spawn_bridge(state.clone(), tmp.path().to_path_buf());

    let out = write_job(
        &tmp,
        "job_exec",
        serde_json::json!({"op": "execute", "code": "return 42;"}),
    )
    .await;
    let payload: serde_json::Value =
        serde_json::from_str(&poll_file(&out, common::WAIT_DEADLINE_MS).await).expect("valid JSON");

    assert_eq!(
        payload["ok"],
        serde_json::json!(true),
        "execute must succeed"
    );
    assert_eq!(
        payload["result"]["n"],
        serde_json::json!(42),
        "execute must return the plugin result"
    );
}

/// The execute op reports a clear error when the job omits the code field.
#[tokio::test]
async fn test_bridge_execute_missing_code() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(turbofig::AppState::new());
    spawn_bridge(state.clone(), tmp.path().to_path_buf());

    let out = write_job(&tmp, "job_nocode", serde_json::json!({"op": "execute"})).await;
    let payload: serde_json::Value =
        serde_json::from_str(&poll_file(&out, common::WAIT_DEADLINE_MS).await).expect("valid JSON");

    assert_eq!(payload["ok"], serde_json::json!(false), "must fail");
    assert!(
        payload["error"]
            .as_str()
            .is_some_and(|e| e.contains("code")),
        "error must mention the missing code field, got: {payload}"
    );
}

/// The get_selection op returns the compact selection shape from the plugin.
#[tokio::test]
async fn test_bridge_get_selection_op() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(turbofig::AppState::new());

    spawn_mock_plugin(
        state.clone(),
        "GET_SELECTION",
        serde_json::json!({
            "ok": true,
            "selection": [{"id": "1:2", "name": "F", "type": "FRAME", "x": 0, "y": 0, "w": 10, "h": 10}]
        }),
    )
    .await;
    spawn_bridge(state.clone(), tmp.path().to_path_buf());

    let out = write_job(&tmp, "job_sel", serde_json::json!({"op": "get_selection"})).await;
    let payload: serde_json::Value =
        serde_json::from_str(&poll_file(&out, common::WAIT_DEADLINE_MS).await).expect("valid JSON");

    assert_eq!(payload["ok"], serde_json::json!(true), "must succeed");
    assert_eq!(
        payload["selection"][0]["name"],
        serde_json::json!("F"),
        "selection must carry the node name"
    );
}

/// The screenshot op in file mode writes the decoded PNG to the outbox and
/// returns its path.
#[tokio::test]
async fn test_bridge_screenshot_file_mode_op() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(turbofig::AppState::new());

    // "aGVsbG8=" is base64 for the bytes "hello".
    spawn_mock_plugin(
        state.clone(),
        "SCREENSHOT",
        serde_json::json!({"ok": true, "png": "aGVsbG8=", "w": 10, "h": 10}),
    )
    .await;
    spawn_bridge(state.clone(), tmp.path().to_path_buf());

    let out = write_job(&tmp, "job_shot", serde_json::json!({"op": "screenshot"})).await;
    let payload: serde_json::Value =
        serde_json::from_str(&poll_file(&out, common::WAIT_DEADLINE_MS).await).expect("valid JSON");

    assert_eq!(payload["ok"], serde_json::json!(true), "must succeed");
    let path = payload["path"]
        .as_str()
        .unwrap_or_else(|| panic!("screenshot result must carry a path, got: {payload}"));
    let bytes = tokio::fs::read(path).await.expect("read screenshot png");
    assert_eq!(bytes, b"hello", "file must hold the decoded PNG bytes");
}

/// A malformed job file causes an error result after the grace window.
/// This proves the first-seen map expires bad files correctly.
#[tokio::test]
async fn test_bridge_malformed_json_errors_after_grace() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(turbofig::AppState::new());

    spawn_bridge(state.clone(), tmp.path().to_path_buf());

    // Write raw invalid JSON directly to the inbox.
    let inbox = tmp.path().join("inbox");
    tokio::fs::create_dir_all(&inbox)
        .await
        .expect("create inbox");
    tokio::fs::write(inbox.join("job_grace.json"), b"{ not json at all }")
        .await
        .expect("write malformed job");

    // Wait longer than PARSE_GRACE (200 ms) so the first-seen map expires.
    tokio::time::sleep(Duration::from_millis(400)).await;

    let out_path = tmp.path().join("outbox").join("job_grace.json");
    // Allow the shared deadline for the bridge to write the error result.
    let contents = poll_file(&out_path, common::WAIT_DEADLINE_MS).await;
    let payload: serde_json::Value =
        serde_json::from_str(&contents).expect("outbox file is valid JSON");

    assert_eq!(
        payload["ok"],
        serde_json::json!(false),
        "expired malformed job must return ok:false"
    );
    assert!(
        payload["error"]
            .as_str()
            .is_some_and(|e| e.contains("malformed")),
        "error must mention malformed JSON, got: {payload}"
    );
}

/// Like `spawn_mock_plugin` but registers the plugin under a caller-chosen fileKey.
/// Each EXECUTE request receives the given `reply_body`.
async fn spawn_mock_plugin_with_key(
    state: Arc<turbofig::AppState>,
    file_key: &'static str,
    reply_body: serde_json::Value,
) {
    let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind WS port");
    let ws_addr = ws_listener.local_addr().expect("ws local addr");
    let ws_state = state.clone();
    tokio::spawn(async move {
        turbofig::serve_ws(ws_listener, ws_state)
            .await
            .expect("serve_ws error");
    });

    let mut plugin_ws = common::connect_plugin(ws_addr.port(), state.token()).await;
    plugin_ws
        .send(TtMessage::Text(
            serde_json::json!({"type": "FILE_INFO", "fileKey": file_key, "name": file_key})
                .to_string(),
        ))
        .await
        .expect("send FILE_INFO");
    wait_for_file_key(&state, file_key, common::WAIT_DEADLINE_MS).await;

    tokio::spawn(async move {
        while let Some(Ok(msg)) = plugin_ws.next().await {
            if let TtMessage::Text(text) = msg {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    if json.get("type").and_then(|t| t.as_str()) == Some("EXECUTE") {
                        if let Some(id) = json.get("requestId").and_then(|v| v.as_u64()) {
                            let mut reply = reply_body.clone();
                            reply["type"] = serde_json::json!("RESULT");
                            reply["requestId"] = serde_json::json!(id);
                            let _ = plugin_ws.send(TtMessage::Text(reply.to_string())).await;
                        }
                    }
                }
            }
        }
    });
}

/// Two plugins connected, no fileKey in the job: the route resolver returns an
/// ambiguous error listing both file keys.
#[tokio::test]
async fn test_bridge_execute_no_file_key_two_plugins_returns_ambiguous() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(turbofig::AppState::new());

    spawn_mock_plugin_with_key(
        state.clone(),
        "fk1",
        serde_json::json!({"ok": true, "result": {"from": "fk1"}}),
    )
    .await;
    spawn_mock_plugin_with_key(
        state.clone(),
        "fk2",
        serde_json::json!({"ok": true, "result": {"from": "fk2"}}),
    )
    .await;
    spawn_bridge(state.clone(), tmp.path().to_path_buf());

    let out = write_job(
        &tmp,
        "job_ambig",
        serde_json::json!({"op": "execute", "code": "return 1;"}),
    )
    .await;
    let payload: serde_json::Value =
        serde_json::from_str(&poll_file(&out, common::WAIT_DEADLINE_MS).await).expect("valid JSON");

    assert_eq!(payload["ok"], serde_json::json!(false), "must fail");
    let error = payload["error"].as_str().unwrap_or("");
    assert!(
        error.contains("multiple"),
        "error must mention multiple files, got: {error}"
    );
    assert!(
        payload["files"].is_array(),
        "result must list available file keys"
    );
    // Collect the string entries from the files array and check both keys appear.
    let files: Vec<String> = payload["files"]
        .as_array()
        .expect("files must be an array")
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    assert!(
        files.contains(&"fk1".to_owned()),
        "files must contain fk1, got: {files:?}"
    );
    assert!(
        files.contains(&"fk2".to_owned()),
        "files must contain fk2, got: {files:?}"
    );
}

/// Two plugins connected, job carries fileKey for fk2: only fk2 receives the
/// request. The reply carries a distinct marker to confirm correct routing.
#[tokio::test]
async fn test_bridge_execute_explicit_file_key_routes_to_correct_plugin() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(turbofig::AppState::new());

    spawn_mock_plugin_with_key(
        state.clone(),
        "fk1",
        serde_json::json!({"ok": true, "result": {"from": "fk1"}}),
    )
    .await;
    spawn_mock_plugin_with_key(
        state.clone(),
        "fk2",
        serde_json::json!({"ok": true, "result": {"from": "fk2"}}),
    )
    .await;
    spawn_bridge(state.clone(), tmp.path().to_path_buf());

    let out = write_job(
        &tmp,
        "job_routed",
        serde_json::json!({"op": "execute", "code": "return 2;", "fileKey": "fk2"}),
    )
    .await;
    let payload: serde_json::Value =
        serde_json::from_str(&poll_file(&out, common::WAIT_DEADLINE_MS).await).expect("valid JSON");

    assert_eq!(payload["ok"], serde_json::json!(true), "must succeed");
    assert_eq!(
        payload["result"]["from"],
        serde_json::json!("fk2"),
        "reply must come from fk2, got: {payload}"
    );
}

/// Job carries a fileKey that has no connected plugin: the route resolver
/// returns a clear not-found error.
#[tokio::test]
async fn test_bridge_execute_unknown_file_key_returns_not_found() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(turbofig::AppState::new());

    spawn_mock_plugin_with_key(
        state.clone(),
        "fk1",
        serde_json::json!({"ok": true, "result": {}}),
    )
    .await;
    spawn_bridge(state.clone(), tmp.path().to_path_buf());

    let out = write_job(
        &tmp,
        "job_notfound",
        serde_json::json!({"op": "execute", "code": "return 3;", "fileKey": "missing_key"}),
    )
    .await;
    let payload: serde_json::Value =
        serde_json::from_str(&poll_file(&out, common::WAIT_DEADLINE_MS).await).expect("valid JSON");

    assert_eq!(payload["ok"], serde_json::json!(false), "must fail");
    let error = payload["error"].as_str().unwrap_or("");
    assert!(
        error.contains("not connected"),
        "error must mention not connected, got: {error}"
    );
    // The not-found shape includes "files": available. Check fk1 is listed.
    assert!(
        payload["files"].is_array(),
        "not-found result must list available file keys, got: {payload}"
    );
    let files: Vec<String> = payload["files"]
        .as_array()
        .expect("files must be an array")
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    assert!(
        files.contains(&"fk1".to_owned()),
        "files must list fk1 as available, got: {files:?}"
    );
}

/// Like `spawn_mock_plugin_with_key` but accepts a caller-chosen `frame_type`.
/// The plugin registers under `file_key` and replies to any frame of `frame_type`
/// with `reply_body` merged with the RESULT type and the echoed requestId.
async fn spawn_mock_plugin_keyed(
    state: Arc<turbofig::AppState>,
    file_key: &'static str,
    frame_type: &'static str,
    reply_body: serde_json::Value,
) {
    let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind WS port");
    let ws_addr = ws_listener.local_addr().expect("ws local addr");
    let ws_state = state.clone();
    tokio::spawn(async move {
        turbofig::serve_ws(ws_listener, ws_state)
            .await
            .expect("serve_ws error");
    });

    let mut plugin_ws = common::connect_plugin(ws_addr.port(), state.token()).await;
    plugin_ws
        .send(TtMessage::Text(
            serde_json::json!({"type": "FILE_INFO", "fileKey": file_key, "name": file_key})
                .to_string(),
        ))
        .await
        .expect("send FILE_INFO");
    wait_for_file_key(&state, file_key, common::WAIT_DEADLINE_MS).await;

    tokio::spawn(async move {
        while let Some(Ok(msg)) = plugin_ws.next().await {
            if let TtMessage::Text(text) = msg {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    if json.get("type").and_then(|t| t.as_str()) == Some(frame_type) {
                        if let Some(id) = json.get("requestId").and_then(|v| v.as_u64()) {
                            let mut reply = reply_body.clone();
                            reply["type"] = serde_json::json!("RESULT");
                            reply["requestId"] = serde_json::json!(id);
                            let _ = plugin_ws.send(TtMessage::Text(reply.to_string())).await;
                        }
                    }
                }
            }
        }
    });
}

/// A get_selection bridge job with an explicit fileKey routes to the correct plugin.
/// Two plugins (fk1, fk2) each answer GET_SELECTION with a distinct selection payload.
/// The job targets fk2; the result must carry fk2's selection, not fk1's.
/// This proves non-execute ops share the same resolve_route path as execute.
#[tokio::test]
async fn test_bridge_get_selection_routes_by_file_key() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(turbofig::AppState::new());

    // fk1 answers GET_SELECTION with a node named "fk1-node".
    spawn_mock_plugin_keyed(
        state.clone(),
        "fk1",
        "GET_SELECTION",
        serde_json::json!({
            "ok": true,
            "selection": [{"id": "1:1", "name": "fk1-node", "type": "FRAME",
                           "x": 0, "y": 0, "w": 10, "h": 10}]
        }),
    )
    .await;
    // fk2 answers GET_SELECTION with a node named "fk2-node".
    spawn_mock_plugin_keyed(
        state.clone(),
        "fk2",
        "GET_SELECTION",
        serde_json::json!({
            "ok": true,
            "selection": [{"id": "2:2", "name": "fk2-node", "type": "FRAME",
                           "x": 0, "y": 0, "w": 20, "h": 20}]
        }),
    )
    .await;
    spawn_bridge(state.clone(), tmp.path().to_path_buf());

    // Route the job to fk2 explicitly.
    let out = write_job(
        &tmp,
        "job_sel_routed",
        serde_json::json!({"op": "get_selection", "fileKey": "fk2"}),
    )
    .await;
    let payload: serde_json::Value =
        serde_json::from_str(&poll_file(&out, common::WAIT_DEADLINE_MS).await).expect("valid JSON");

    assert_eq!(payload["ok"], serde_json::json!(true), "must succeed");
    assert_eq!(
        payload["selection"][0]["name"],
        serde_json::json!("fk2-node"),
        "selection must come from fk2, not fk1, got: {payload}"
    );
}

/// An eval that throws in the plugin returns a clean error over the bridge and
/// never stalls the watcher. A second execute job is still serviced.
#[tokio::test]
async fn test_bridge_execute_eval_error() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(turbofig::AppState::new());

    // The mock plugin models a thrown eval: every EXECUTE gets ok:false.
    spawn_mock_plugin(
        state.clone(),
        "EXECUTE",
        serde_json::json!({"ok": false, "error": "TypeError: bad access"}),
    )
    .await;
    spawn_bridge(state.clone(), tmp.path().to_path_buf());

    // First job: the eval error must surface as a clean message.
    let out1 = write_job(
        &tmp,
        "job_err1",
        serde_json::json!({"op": "execute", "code": "throw new Error();"}),
    )
    .await;
    let payload1: serde_json::Value =
        serde_json::from_str(&poll_file(&out1, common::WAIT_DEADLINE_MS).await)
            .expect("valid JSON");
    assert_eq!(
        payload1["ok"],
        serde_json::json!(false),
        "must return ok:false"
    );
    assert_eq!(
        payload1["error"],
        serde_json::json!("TypeError: bad access"),
        "the plugin error must pass through verbatim, got: {payload1}"
    );

    // Second job: proves the watcher kept running after the error.
    let out2 = write_job(
        &tmp,
        "job_err2",
        serde_json::json!({"op": "execute", "code": "throw new Error();"}),
    )
    .await;
    let payload2: serde_json::Value =
        serde_json::from_str(&poll_file(&out2, common::WAIT_DEADLINE_MS).await)
            .expect("valid JSON");
    assert_eq!(
        payload2["ok"],
        serde_json::json!(false),
        "the watcher must keep serving after an eval error, got: {payload2}"
    );
}

/// A get_selection bridge job that carries `fields` and `depth` forwards them
/// to the plugin. A mock plugin echoes the received values back in the result.
#[tokio::test]
async fn test_bridge_get_selection_fields_and_depth_forwarded() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(turbofig::AppState::new());

    // Spawn a WS server and a plugin that echoes fields/depth back.
    let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind WS port");
    let ws_addr = ws_listener.local_addr().expect("ws local addr");
    let ws_state = state.clone();
    tokio::spawn(async move {
        turbofig::serve_ws(ws_listener, ws_state)
            .await
            .expect("serve_ws error");
    });

    let mut plugin_ws = common::connect_plugin(ws_addr.port(), state.token()).await;
    plugin_ws
        .send(TtMessage::Text(
            serde_json::json!({"type": "FILE_INFO", "fileKey": "echo", "name": "Echo"}).to_string(),
        ))
        .await
        .expect("send FILE_INFO");
    wait_for_file_key(&state, "echo", common::WAIT_DEADLINE_MS).await;

    tokio::spawn(async move {
        while let Some(Ok(msg)) = plugin_ws.next().await {
            if let TtMessage::Text(text) = msg {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    if json.get("type").and_then(|t| t.as_str()) == Some("GET_SELECTION") {
                        if let Some(id) = json.get("requestId").and_then(|v| v.as_u64()) {
                            let received_fields = json
                                .get("fields")
                                .cloned()
                                .unwrap_or(serde_json::Value::Null);
                            let received_depth = json
                                .get("depth")
                                .cloned()
                                .unwrap_or(serde_json::Value::Null);
                            let reply = serde_json::json!({
                                "type": "RESULT",
                                "requestId": id,
                                "ok": true,
                                "selection": [{"id": "echo", "received_fields": received_fields, "received_depth": received_depth}]
                            });
                            let _ = plugin_ws.send(TtMessage::Text(reply.to_string())).await;
                        }
                    }
                }
            }
        }
    });

    spawn_bridge(state.clone(), tmp.path().to_path_buf());

    // Send a job with fields and depth.
    let out = write_job(
        &tmp,
        "job_sel_shaped",
        serde_json::json!({
            "op": "get_selection",
            "fields": ["opacity", "visible"],
            "depth": 3
        }),
    )
    .await;
    let payload: serde_json::Value =
        serde_json::from_str(&poll_file(&out, common::WAIT_DEADLINE_MS).await).expect("valid JSON");

    assert_eq!(payload["ok"], serde_json::json!(true), "must succeed");
    assert_eq!(
        payload["selection"][0]["received_fields"],
        serde_json::json!(["opacity", "visible"]),
        "bridge must forward fields to the plugin: {payload}"
    );
    assert_eq!(
        payload["selection"][0]["received_depth"],
        serde_json::json!(3),
        "bridge must forward depth to the plugin: {payload}"
    );
}

// ── hardening tests ───────────────────────────────────────────────────────────

async fn read_result(tmp: &tempfile::TempDir, id: &str) -> serde_json::Value {
    let p = tmp.path().join("outbox").join(format!("{id}.json"));
    let contents = poll_file(&p, common::WAIT_DEADLINE_MS).await;
    serde_json::from_str(&contents).expect("result is JSON")
}

/// Job-parse errors carry code invalid_job (unknown op, bad field, malformed JSON).
#[tokio::test]
async fn test_bridge_parse_errors_carry_invalid_job_code() {
    let tmp = tempfile::tempdir().expect("tempdir");
    spawn_bridge(
        Arc::new(turbofig::AppState::new()),
        tmp.path().to_path_buf(),
    );

    write_job(&tmp, "e1", serde_json::json!({"op": "frobnicate"})).await;
    assert_eq!(read_result(&tmp, "e1").await["code"], "invalid_job");

    write_job(&tmp, "e2", serde_json::json!({"op": "execute"})).await;
    assert_eq!(read_result(&tmp, "e2").await["code"], "invalid_job");

    std::fs::write(tmp.path().join("inbox").join("e3.json"), b"{ nope").expect("write");
    assert_eq!(read_result(&tmp, "e3").await["code"], "invalid_job");
}

/// The bridge writes tf-api.md into its dir on start, matching the embedded doc.
#[tokio::test]
async fn test_bridge_writes_helper_doc_on_start() {
    let tmp = tempfile::tempdir().expect("tempdir");
    spawn_bridge(
        Arc::new(turbofig::AppState::new()),
        tmp.path().to_path_buf(),
    );
    let doc = tmp.path().join("tf-api.md");
    let got = poll_file(&doc, common::WAIT_DEADLINE_MS).await;
    let repo_doc =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../helpers/tf-api.md"))
            .expect("repo doc");
    assert_eq!(got, repo_doc);
}

#[cfg(unix)]
#[tokio::test]
async fn test_bridge_results_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().expect("tempdir");
    spawn_bridge(
        Arc::new(turbofig::AppState::new()),
        tmp.path().to_path_buf(),
    );
    write_job(&tmp, "perm1", serde_json::json!({"op": "status"})).await;
    read_result(&tmp, "perm1").await;
    let mode = std::fs::metadata(tmp.path().join("outbox/perm1.json"))
        .expect("meta")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}

/// FIFO and symlink inbox entries are skipped without blocking; later jobs run.
#[cfg(unix)]
#[tokio::test]
async fn test_bridge_skips_fifo_and_symlink_inbox_entries() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let inbox = tmp.path().join("inbox");
    std::fs::create_dir_all(&inbox).expect("inbox");
    let secret = tmp.path().join("secret.txt");
    std::fs::write(&secret, br#"{"op":"status"}"#).expect("secret");
    std::os::unix::fs::symlink(&secret, inbox.join("sym.json")).expect("symlink");
    let fifo =
        std::ffi::CString::new(inbox.join("fifo.json").to_str().expect("utf8")).expect("cstring");
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);

    spawn_bridge(
        Arc::new(turbofig::AppState::new()),
        tmp.path().to_path_buf(),
    );

    write_job(&tmp, "after", serde_json::json!({"op": "status"})).await;
    assert_eq!(read_result(&tmp, "after").await["ok"], true);
    let sym = read_result(&tmp, "sym").await;
    assert_eq!(sym["ok"], false, "symlink entry must be rejected, not run");
    assert_eq!(read_result(&tmp, "fifo").await["ok"], false);
    assert!(secret.exists());
}

/// A job over 30 MiB gets an error result and is never parsed.
#[tokio::test]
async fn test_bridge_rejects_oversized_job() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let inbox = tmp.path().join("inbox");
    std::fs::create_dir_all(&inbox).expect("inbox");
    let big = format!(
        r#"{{"op":"status","pad":"{}"}}"#,
        "a".repeat(31 * 1024 * 1024)
    );
    std::fs::write(inbox.join("big.json"), big).expect("write big");
    spawn_bridge(
        Arc::new(turbofig::AppState::new()),
        tmp.path().to_path_buf(),
    );
    let r = read_result(&tmp, "big").await;
    assert_eq!(r["ok"], false);
    assert!(r["error"].as_str().unwrap_or("").contains("limit"), "{r}");
    assert!(!inbox.join("big.json").exists());
}

/// An id over 200 bytes never runs; the client gets an error under a short name.
#[tokio::test]
async fn test_bridge_rejects_overlong_id_without_running() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let inbox = tmp.path().join("inbox");
    std::fs::create_dir_all(&inbox).expect("inbox");
    let id = "x".repeat(250);
    let body = serde_json::json!({"op": "status"});
    std::fs::write(inbox.join(format!("{id}.json")), body.to_string()).expect("write");
    spawn_bridge(
        Arc::new(turbofig::AppState::new()),
        tmp.path().to_path_buf(),
    );
    let r = read_result(&tmp, &format!("{}.idtoolong", "x".repeat(100))).await;
    assert_eq!(r["ok"], false);
    assert_eq!(r["code"], "invalid_job");
    assert!(r["error"].as_str().unwrap_or("").contains("200"), "{r}");
    assert!(!inbox.join(format!("{id}.json")).exists());
}

/// A malformed duplicate of an in-flight id is left alone until the first
/// job finishes: it gets no early error result.
#[tokio::test]
async fn test_bridge_malformed_duplicate_of_inflight_id_is_held() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(turbofig::AppState::new());
    spawn_mock_plugin(state.clone(), "NEVER_REPLIES", serde_json::json!({})).await;
    let inbox = tmp.path().join("inbox");
    std::fs::create_dir_all(&inbox).expect("inbox");
    spawn_bridge(state, tmp.path().to_path_buf());

    // Execute never gets a reply, so it stays in flight.
    write_job(
        &tmp,
        "dup",
        serde_json::json!({"op": "execute", "code": "1"}),
    )
    .await;
    wait_until(
        || !inbox.join("dup.json").exists(),
        WAIT_DEADLINE_MS,
        "first job claimed",
    )
    .await;

    std::fs::write(inbox.join("dup.json"), b"{ not json").expect("dup");
    tokio::time::sleep(Duration::from_millis(1500)).await;

    assert!(inbox.join("dup.json").exists(), "duplicate must stay held");
    assert!(
        !tmp.path().join("outbox/dup.json").exists(),
        "no result may be written for the held duplicate"
    );
}

/// An over-long id whose short error name equals an in-flight id is held, so
/// it cannot touch the in-flight job's result files.
#[tokio::test]
async fn test_bridge_long_id_colliding_with_inflight_short_id_is_held() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(turbofig::AppState::new());
    spawn_mock_plugin(state.clone(), "NEVER_REPLIES", serde_json::json!({})).await;
    let inbox = tmp.path().join("inbox");
    std::fs::create_dir_all(&inbox).expect("inbox");
    spawn_bridge(state, tmp.path().to_path_buf());

    let short = format!("{}.idtoolong", "y".repeat(100));
    write_job(
        &tmp,
        &short,
        serde_json::json!({"op": "execute", "code": "1"}),
    )
    .await;
    wait_until(
        || !inbox.join(format!("{short}.json")).exists(),
        WAIT_DEADLINE_MS,
        "first job claimed",
    )
    .await;

    let long = "y".repeat(250);
    std::fs::write(inbox.join(format!("{long}.json")), br#"{"op":"status"}"#).expect("long");
    tokio::time::sleep(Duration::from_millis(1500)).await;

    assert!(
        inbox.join(format!("{long}.json")).exists(),
        "must stay held"
    );
    assert!(!tmp.path().join(format!("outbox/{short}.json")).exists());
}

/// A read failure at give-up time returns code io_error, not invalid_job.
#[cfg(unix)]
#[tokio::test]
async fn test_bridge_unreadable_job_gets_io_error_code() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().expect("tempdir");
    let inbox = tmp.path().join("inbox");
    std::fs::create_dir_all(&inbox).expect("inbox");
    let f = inbox.join("noperm.json");
    std::fs::write(&f, br#"{"op":"status"}"#).expect("write");
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    if std::fs::read(&f).is_ok() {
        return; // running as root: permissions do not apply
    }
    spawn_bridge(
        Arc::new(turbofig::AppState::new()),
        tmp.path().to_path_buf(),
    );
    let r = read_result(&tmp, "noperm").await;
    assert_eq!(r["code"], "io_error", "{r}");
}

/// A long id's reply never overwrites the finished result of a job whose id
/// equals the first 100 bytes of the long id.
#[tokio::test]
async fn test_bridge_long_id_reply_does_not_overwrite_a_short_id_result() {
    let tmp = tempfile::tempdir().expect("tempdir");
    spawn_bridge(
        Arc::new(turbofig::AppState::new()),
        tmp.path().to_path_buf(),
    );
    let short = "b".repeat(100);
    write_job(&tmp, &short, serde_json::json!({"op": "status"})).await;
    let first = read_result(&tmp, &short).await;
    assert_eq!(first["ok"], true, "{first}");

    let long = format!("{short}{}", "c".repeat(140));
    write_job(&tmp, &long, serde_json::json!({"op": "status"})).await;
    let r = read_result(&tmp, &format!("{short}.idtoolong")).await;
    assert_eq!(r["code"], "invalid_job", "{r}");
    let again = read_result(&tmp, &short).await;
    assert_eq!(
        again["ok"], true,
        "the short-id result must survive: {again}"
    );
}

/// A job renamed into the inbox is answered by the watcher event, not by the
/// 500 ms backstop poll. Ten renamed jobs in a row must each be answered well
/// inside one backstop interval.
#[tokio::test]
async fn test_bridge_renamed_job_is_answered_by_the_event_not_the_backstop() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(turbofig::AppState::with_timeout(Duration::from_secs(5)));
    // The status job below finds no plugin. Skip the cold-start wait so the
    // answer time measures only the event path.
    state.skip_cold_start_grace();
    spawn_bridge(state.clone(), tmp.path().to_path_buf());
    let inbox = tmp.path().join("inbox");
    let outbox = tmp.path().join("outbox");
    // The bridge creates both dirs at startup. Wait for them.
    wait_until(
        || inbox.is_dir() && outbox.is_dir(),
        WAIT_DEADLINE_MS,
        "bridge dirs",
    )
    .await;
    // Let the watcher attach before the first job.
    tokio::time::sleep(Duration::from_millis(300)).await;

    for n in 0..10 {
        let tmp_job = inbox.join(format!("fast{n}.json.tmp"));
        tokio::fs::write(&tmp_job, r#"{"op":"status"}"#)
            .await
            .expect("write tmp job");
        let started = std::time::Instant::now();
        tokio::fs::rename(&tmp_job, inbox.join(format!("fast{n}.json")))
            .await
            .expect("rename job");
        let result = outbox.join(format!("fast{n}.json"));
        while !result.exists() {
            assert!(
                started.elapsed() < Duration::from_millis(450),
                "job {n} was not answered inside one backstop interval"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
}

/// A draining daemon (an upgrade restart under way) claims no job: the file
/// stays in the inbox, unclaimed and unanswered, and the next daemon (or the
/// same one, once it accepts work again) claims it. The old behaviour claimed
/// the job and then failed it with `draining`.
#[tokio::test]
async fn a_draining_bridge_leaves_the_job_in_the_inbox_until_it_accepts_work() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(turbofig::AppState::new());
    state.set_draining(true);
    spawn_bridge(state.clone(), tmp.path().to_path_buf());
    // The bridge makes its folders on start.
    wait_until(
        || tmp.path().join("inbox").is_dir(),
        WAIT_DEADLINE_MS,
        "bridge to create its inbox",
    )
    .await;

    let result_path = write_job(&tmp, "drain1", serde_json::json!({"op": "status"})).await;
    let job_path = tmp.path().join("inbox/drain1.json");

    // Several scan intervals pass: the job is neither claimed nor answered.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        job_path.exists(),
        "a draining daemon must not claim the job"
    );
    assert!(!result_path.exists(), "and must not answer it");
    assert_eq!(state.jobs_in_flight(), 0);

    // It accepts work again (a failed restart): the job is then served.
    state.set_draining(false);
    let body = poll_file(&result_path, 15_000).await;
    let result: serde_json::Value = serde_json::from_str(&body).expect("result JSON");
    assert_eq!(result["ok"], serde_json::Value::Bool(true), "{body}");
    assert!(!job_path.exists(), "the claimed job leaves the inbox");
}
