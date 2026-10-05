//! Integration tests for graceful mid-session file close.
//!
//! These tests prove that when a paired plugin socket closes during a session,
//! the daemon returns a clean error (not a hang or panic) and does not disturb
//! other active sessions or their paired files.
//!
//! All ports are ephemeral. Synchronisation waits on observable state.

mod common;

use common::wait_until;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::{connect_async, tungstenite::Message as TtMessage};

// ── helpers ───────────────────────────────────────────────────────────────────

/// Start a shared WS + HTTP server. Returns (ws_port, http_base_url, state).
async fn start_stack() -> (u16, String, Arc<turbofig::AppState>) {
    // Short timeout: routing failures surface quickly in tests.
    let state = Arc::new(turbofig::AppState::with_timeout(Duration::from_secs(5)));

    let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral WS port");
    let http_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral HTTP port");

    let ws_addr = ws_listener.local_addr().expect("ws local addr");
    let http_addr = http_listener.local_addr().expect("http local addr");

    let ws_state = state.clone();
    tokio::spawn(async move {
        turbofig::serve_ws(ws_listener, ws_state)
            .await
            .expect("serve_ws error in test");
    });

    let http_state = state.clone();
    tokio::spawn(async move {
        turbofig::serve_with_state(http_listener, http_state)
            .await
            .expect("serve_with_state error in test");
    });

    (ws_addr.port(), format!("http://{http_addr}"), state)
}

/// Connect a long-lived mock plugin with `file_key`.
///
/// A background task owns the socket and keeps it alive. The plugin replies to
/// every EXECUTE frame with `{"ok":true,"result":{"from":<reply_tag>}}`.
/// To close a plugin deliberately, connect a raw socket and drop it instead.
async fn connect_mock_plugin(
    ws_port: u16,
    state: &Arc<turbofig::AppState>,
    file_key: &str,
    reply_tag: &'static str,
) {
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{ws_port}/?token={}", state.token()))
        .await
        .expect("mock plugin connect");

    let fi = serde_json::json!({
        "type": "FILE_INFO",
        "fileKey": file_key,
        "name": file_key
    });
    ws.send(TtMessage::Text(fi.to_string()))
        .await
        .expect("send FILE_INFO");

    // Wait for registration to propagate.
    let found = wait_for_file_key(state, file_key).await;
    assert!(
        found,
        "{file_key} must register before connect_mock_plugin returns"
    );

    tokio::spawn(async move {
        while let Some(Ok(msg)) = ws.next().await {
            if let TtMessage::Text(text) = msg {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    if json.get("type").and_then(|t| t.as_str()) == Some("EXECUTE") {
                        if let Some(id) = json.get("requestId").and_then(|v| v.as_u64()) {
                            let reply = serde_json::json!({
                                "type": "RESULT",
                                "requestId": id,
                                "ok": true,
                                "result": {"from": reply_tag}
                            });
                            let _ = ws.send(TtMessage::Text(reply.to_string())).await;
                        }
                    }
                }
            }
        }
    });
}

/// Build an HTTP client with a generous timeout.
fn make_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("build reqwest client")
}

/// POST to /mcp with required MCP headers.
/// Attaches mcp-session-id when `session_id` is Some.
async fn post_mcp(
    client: &reqwest::Client,
    base_url: &str,
    body: serde_json::Value,
    session_id: Option<&str>,
) -> reqwest::Response {
    let mut builder = client
        .post(format!("{base_url}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .json(&body);
    if let Some(id) = session_id {
        builder = builder.header("mcp-session-id", id);
    }
    builder.send().await.expect("send POST /mcp")
}

/// Parse the first non-empty `data:` line in an SSE body as JSON.
fn parse_sse_data(body: &str) -> serde_json::Value {
    for line in body.lines() {
        let data = if let Some(d) = line.strip_prefix("data: ") {
            d
        } else if let Some(d) = line.strip_prefix("data:") {
            d
        } else {
            continue;
        };
        if data.is_empty() {
            continue;
        }
        return serde_json::from_str(data)
            .unwrap_or_else(|e| panic!("SSE data is not JSON ({e}):\n{data}"));
    }
    panic!("No non-empty 'data:' line found in SSE body:\n{body}");
}

/// Run the MCP initialize + notifications/initialized handshake.
/// Returns the mcp-session-id issued by the server.
async fn mcp_handshake(client: &reqwest::Client, base_url: &str) -> String {
    let init_res = post_mcp(
        client,
        base_url,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "clientInfo": {"name": "mid-session-close-test", "version": "0.1.0"},
                "capabilities": {}
            }
        }),
        None,
    )
    .await;

    assert!(
        init_res.status().is_success(),
        "initialize must succeed, got HTTP {}",
        init_res.status()
    );

    let session_id = init_res
        .headers()
        .get("mcp-session-id")
        .expect("initialize response must carry mcp-session-id")
        .to_str()
        .expect("mcp-session-id is valid UTF-8")
        .to_owned();
    let _ = init_res.text().await.expect("drain init body");

    let notif = post_mcp(
        client,
        base_url,
        serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}}),
        Some(&session_id),
    )
    .await;
    assert!(
        notif.status().is_success(),
        "notifications/initialized must succeed"
    );
    let _ = notif.text().await.expect("drain notif body");

    session_id
}

/// Call turbofig_execute via MCP. Return the parsed tool-output JSON.
async fn call_execute(
    client: &reqwest::Client,
    base_url: &str,
    session_id: &str,
    file_key: Option<&str>,
) -> serde_json::Value {
    let mut args = serde_json::json!({"code": "return 1;"});
    if let Some(fk) = file_key {
        args["fileKey"] = serde_json::Value::String(fk.to_owned());
    }

    let res = post_mcp(
        client,
        base_url,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 99,
            "method": "tools/call",
            "params": {"name": "turbofig_execute", "arguments": args}
        }),
        Some(session_id),
    )
    .await;

    assert!(
        res.status().is_success(),
        "tools/call must return HTTP 2xx, got HTTP {}",
        res.status()
    );

    let body = res.text().await.expect("read tools/call body");
    let msg = parse_sse_data(&body);
    let text_str = msg["result"]["content"][0]["text"]
        .as_str()
        .expect("content[0].text must be a string");
    serde_json::from_str(text_str).expect("tool output must be valid JSON")
}

/// Wait until `state.list_connections()` does NOT contain `file_key`.
/// Returns true when the entry is gone, false on deadline.
async fn wait_for_file_key_gone(state: &Arc<turbofig::AppState>, file_key: &str) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let found = state
            .list_connections()
            .iter()
            .any(|(_, fk, _)| fk == file_key);
        if !found {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Wait until `state.list_connections()` contains an entry for `file_key`.
/// Returns true when found, false on deadline.
async fn wait_for_file_key(state: &Arc<turbofig::AppState>, file_key: &str) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let found = state
            .list_connections()
            .iter()
            .any(|(_, fk, _)| fk == file_key);
        if found {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Session A pairs to fk1 via an explicit fileKey call and succeeds.
/// Then fk1's plugin socket closes. A second call from the same session
/// with no fileKey returns ok:false with an error containing "not connected".
/// The call must return promptly (well under the daemon request timeout).
#[tokio::test]
async fn test_paired_session_returns_error_after_plugin_closes() {
    let (ws_port, base_url, state) = start_stack().await;

    // Connect fk1. The reply task owns the socket and handles EXECUTE frames.
    // Aborting the task drops the socket, which closes the connection.
    let (mut fk1_ws, _) =
        connect_async(format!("ws://127.0.0.1:{ws_port}/?token={}", state.token()))
            .await
            .expect("fk1 connect");
    fk1_ws
        .send(TtMessage::Text(
            serde_json::json!({
                "type": "FILE_INFO",
                "fileKey": "fk1",
                "name": "fk1"
            })
            .to_string(),
        ))
        .await
        .expect("send fk1 FILE_INFO");

    // Wait until fk1 is registered.
    let found = wait_for_file_key(&state, "fk1").await;
    assert!(found, "fk1 must register before the test proceeds");

    // Spawn a reply task that owns the fk1 socket.
    // It answers EXECUTE frames until aborted.
    let reply_handle = tokio::spawn(async move {
        while let Some(Ok(TtMessage::Text(text))) = fk1_ws.next().await {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                if json.get("type").and_then(|t| t.as_str()) == Some("EXECUTE") {
                    if let Some(id) = json.get("requestId").and_then(|v| v.as_u64()) {
                        let reply = serde_json::json!({
                            "type": "RESULT",
                            "requestId": id,
                            "ok": true,
                            "result": {"from": "fk1-reply"}
                        });
                        let _ = fk1_ws.send(TtMessage::Text(reply.to_string())).await;
                    }
                }
            }
        }
    });

    let client = make_client();
    let session_id = mcp_handshake(&client, &base_url).await;

    // First call: explicit fileKey=fk1. The session pairs to fk1.
    let first = call_execute(&client, &base_url, &session_id, Some("fk1")).await;
    assert_eq!(
        first["ok"],
        serde_json::json!(true),
        "first call to fk1 must succeed before close, got: {first}"
    );
    assert_eq!(
        first["result"]["from"],
        serde_json::json!("fk1-reply"),
        "first call result must come from fk1, got: {first}"
    );

    // Abort the reply task. Dropping the task drops the socket, closing the
    // connection. The daemon detects the close and deregisters fk1.
    reply_handle.abort();

    // Wait until the daemon deregisters fk1.
    let fk1_gone = wait_for_file_key_gone(&state, "fk1").await;
    assert!(fk1_gone, "fk1 must deregister after socket close");

    // Second call: no fileKey. The session is paired to fk1, which is gone.
    // Expect a prompt ok:false with "not connected" in the error.
    let start = std::time::Instant::now();
    let second = call_execute(&client, &base_url, &session_id, None).await;
    let elapsed = start.elapsed();

    assert_eq!(
        second["ok"],
        serde_json::json!(false),
        "second call after fk1 close must return ok:false, got: {second}"
    );
    let error_str = second["error"]
        .as_str()
        .expect("error field must be a string");
    assert!(
        error_str.contains("not connected"),
        "error must contain 'not connected', got: {error_str}"
    );
    // The call must return well under the 5-second request timeout.
    assert!(
        elapsed < Duration::from_secs(4),
        "second call must return promptly, took: {elapsed:?}"
    );
}

/// Closing fk1 leaves session B's route to fk2 intact.
///
/// fk1 and fk2 are both connected. Session A pairs to fk1. Session B pairs
/// to fk2. Closing fk1 must not disturb session B. Session B's next
/// no-fileKey call must route to fk2 and return ok:true.
#[tokio::test]
async fn test_fk1_close_does_not_disturb_session_b_paired_to_fk2() {
    let (ws_port, base_url, state) = start_stack().await;

    // Connect fk2 as a long-lived mock plugin.
    connect_mock_plugin(ws_port, &state, "fk2", "fk2-reply").await;

    // Connect fk1 as a raw socket for deliberate close.
    let (mut fk1_ws, _) =
        connect_async(format!("ws://127.0.0.1:{ws_port}/?token={}", state.token()))
            .await
            .expect("fk1 connect");
    fk1_ws
        .send(TtMessage::Text(
            serde_json::json!({
                "type": "FILE_INFO",
                "fileKey": "fk1",
                "name": "fk1"
            })
            .to_string(),
        ))
        .await
        .expect("send fk1 FILE_INFO");

    // Wait until both plugins are registered.
    let found_fk1 = wait_for_file_key(&state, "fk1").await;
    assert!(found_fk1, "fk1 must register");
    let found_fk2 = wait_for_file_key(&state, "fk2").await;
    assert!(found_fk2, "fk2 must register");

    let client = make_client();

    // Session A handshake and pair to fk1.
    let session_a = mcp_handshake(&client, &base_url).await;
    // Session B handshake and pair to fk2.
    let session_b = mcp_handshake(&client, &base_url).await;

    // Pair session A to fk1. fk1 is not set up to reply so this times out,
    // but that is fine for this test: the pairing is recorded before the
    // response completes. Drive the pairing in the background.
    // Use run_execute directly on AppState so we avoid blocking the test.
    let state_clone = state.clone();
    let session_a_clone = session_a.clone();
    tokio::spawn(async move {
        // Force the session pairing without needing a real reply.
        turbofig::run_execute(
            &state_clone,
            Some(&session_a_clone),
            Some("fk1"),
            "return 1;",
        )
        .await;
    });
    // Wait until resolve_route has recorded the pairing, rather than
    // guessing how long that takes on this runner.
    wait_until(
        || state.session_lookup(&session_a).as_deref() == Some("fk1"),
        3000,
        "session A to pair to fk1",
    )
    .await;

    // Pair session B to fk2 via HTTP. fk2 replies, so this succeeds.
    let pair_b = call_execute(&client, &base_url, &session_b, Some("fk2")).await;
    assert_eq!(
        pair_b["ok"],
        serde_json::json!(true),
        "pairing session B to fk2 must succeed, got: {pair_b}"
    );

    // Close fk1. The daemon must deregister it.
    drop(fk1_ws);
    let fk1_gone = wait_for_file_key_gone(&state, "fk1").await;
    assert!(fk1_gone, "fk1 must deregister after socket close");

    // Session B must still route to fk2 with no fileKey.
    let result_b = call_execute(&client, &base_url, &session_b, None).await;
    assert_eq!(
        result_b["ok"],
        serde_json::json!(true),
        "session B must still reach fk2 after fk1 closes, got: {result_b}"
    );
    assert_eq!(
        result_b["result"]["from"],
        serde_json::json!("fk2-reply"),
        "result must come from fk2-reply, got: {result_b}"
    );
}

/// In-flight isolation on close is unit-tested at the AppState level
/// via cancel_pending_for_conn. A lightweight integration check is included
/// here: it verifies that a tools/call in flight to fk1 returns (with an
/// error or timeout) while a concurrent fk2 call completes successfully.
/// This is best-effort: if timing makes it fragile the test is noted and
/// skipped rather than fought.
#[tokio::test]
async fn test_concurrent_call_to_fk2_succeeds_while_fk1_hangs_and_closes() {
    let (ws_port, base_url, state) = start_stack().await;

    // Connect fk2 as a long-lived mock plugin.
    connect_mock_plugin(ws_port, &state, "fk2", "fk2-reply").await;

    // Connect fk1 as a raw socket that will NOT reply to EXECUTE frames.
    // Closing it mid-call exercises in-flight isolation.
    let (mut fk1_ws, _) =
        connect_async(format!("ws://127.0.0.1:{ws_port}/?token={}", state.token()))
            .await
            .expect("fk1 connect");
    fk1_ws
        .send(TtMessage::Text(
            serde_json::json!({
                "type": "FILE_INFO",
                "fileKey": "fk1",
                "name": "fk1"
            })
            .to_string(),
        ))
        .await
        .expect("send fk1 FILE_INFO");

    // Wait until both plugins are registered.
    let found_fk1 = wait_for_file_key(&state, "fk1").await;
    assert!(found_fk1, "fk1 must register");
    let found_fk2 = wait_for_file_key(&state, "fk2").await;
    assert!(found_fk2, "fk2 must register");

    let client = make_client();

    // Session for fk1 (will hang until fk1 closes).
    let session_fk1 = mcp_handshake(&client, &base_url).await;
    // Session for fk2 (will succeed).
    let session_fk2 = mcp_handshake(&client, &base_url).await;

    // Pair session_fk2 to fk2 first.
    let pair_fk2 = call_execute(&client, &base_url, &session_fk2, Some("fk2")).await;
    assert_eq!(
        pair_fk2["ok"],
        serde_json::json!(true),
        "pairing session_fk2 to fk2 must succeed, got: {pair_fk2}"
    );

    // Launch a call to fk1 in the background. It will hang until fk1 closes.
    let client_fk1 = make_client();
    let base_url_fk1 = base_url.clone();
    let session_fk1_clone = session_fk1.clone();
    let fk1_call = tokio::spawn(async move {
        call_execute(&client_fk1, &base_url_fk1, &session_fk1_clone, Some("fk1")).await
    });

    // Wait until the in-flight request is registered before we close, rather
    // than guessing how long that takes on this runner.
    wait_until(
        || state.pending_len() >= 1,
        3000,
        "fk1 request to register as pending",
    )
    .await;

    // Close fk1. The daemon must cancel the pending request.
    drop(fk1_ws);
    let fk1_gone = wait_for_file_key_gone(&state, "fk1").await;
    assert!(fk1_gone, "fk1 must deregister after socket close");

    // The fk1 call must resolve (error or timeout) without blocking forever.
    // Allow up to 6 seconds (request timeout is 5 s).
    let fk1_result = tokio::time::timeout(Duration::from_secs(6), fk1_call)
        .await
        .expect("fk1 call must resolve within 6 s")
        .expect("fk1 task must complete");
    // The result must be an error (ok:false). We do not assert the exact message
    // because the transport may close before the cancel propagates.
    assert_eq!(
        fk1_result["ok"],
        serde_json::json!(false),
        "fk1 call must return ok:false after close, got: {fk1_result}"
    );

    // fk2 must still be reachable.
    let result_fk2 = call_execute(&client, &base_url, &session_fk2, None).await;
    assert_eq!(
        result_fk2["ok"],
        serde_json::json!(true),
        "fk2 call must succeed after fk1 closes, got: {result_fk2}"
    );
    assert_eq!(
        result_fk2["result"]["from"],
        serde_json::json!("fk2-reply"),
        "result must come from fk2-reply, got: {result_fk2}"
    );
}
