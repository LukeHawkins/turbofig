//! Integration tests for two-session, two-file isolation.
//!
//! These tests prove that two independent MCP sessions, each paired to a
//! distinct Figma file, are fully isolated under concurrent load. No call
//! from session A may receive a payload from file fk2, and no call from
//! session B may receive a payload from file fk1.
//!
//! All ports are ephemeral. Synchronisation waits on observable state.
//!
//! Note on SSE body reading: the MCP server keeps the SSE response stream
//! open after sending a tool-call result. Calling `.text().await` blocks
//! until the stream closes, which prevents concurrent calls from completing.
//! `call_execute` therefore reads body chunks until it has parsed one
//! complete `data:` line, then drops the stream.

use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::{connect_async, tungstenite::Message as TtMessage};

// ── helpers ───────────────────────────────────────────────────────────────────

/// Start a shared WS + HTTP server pair. Returns (ws_port, http_base_url, state).
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
/// A background task owns the socket. The plugin replies to every EXECUTE
/// frame with `{"ok":true,"result":{"from":<reply_tag>}}`. The reply loop
/// serves many EXECUTE frames in sequence without limit.
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

    // Wait for registration to propagate before returning.
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
            "{file_key} must register before connect_mock_plugin returns"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

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
        .timeout(Duration::from_secs(15))
        .build()
        .expect("build reqwest client")
}

/// POST to /mcp with required MCP headers.
/// Attaches mcp-session-id when `session_id` is Some.
async fn post_mcp(
    client: &reqwest::Client,
    token: &str,
    base_url: &str,
    body: serde_json::Value,
    session_id: Option<&str>,
) -> reqwest::Response {
    let mut builder = client
        .post(format!("{base_url}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {token}"))
        .json(&body);
    if let Some(id) = session_id {
        builder = builder.header("mcp-session-id", id);
    }
    builder.send().await.expect("send POST /mcp")
}

/// Read the SSE response body one chunk at a time.
///
/// Stop and return as soon as one non-empty `data:` line is complete. The
/// remaining body is dropped. This prevents blocking on SSE streams that
/// the server keeps open after delivering the first event.
async fn read_first_sse_data(mut res: reqwest::Response) -> serde_json::Value {
    let mut buf = String::new();

    while let Some(chunk) = res.chunk().await.expect("read SSE chunk") {
        buf.push_str(&String::from_utf8_lossy(&chunk));

        // Scan the buffer for a complete `data:` line followed by a newline.
        // Split on newlines so partial lines are not matched prematurely.
        let newline_pos = buf.rfind('\n').map(|p| p + 1).unwrap_or(0);
        let complete = &buf[..newline_pos];
        for line in complete.lines() {
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
    }
    panic!("SSE stream ended with no non-empty 'data:' line in:\n{buf}");
}

/// Run the MCP initialize + notifications/initialized handshake.
/// Returns the mcp-session-id issued by the server.
async fn mcp_handshake(client: &reqwest::Client, token: &str, base_url: &str) -> String {
    let init_res = post_mcp(
        client,
        token,
        base_url,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "clientInfo": {"name": "isolation-test", "version": "0.1.0"},
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
    // Drain the init body to free the connection.
    let _ = init_res.text().await.expect("drain init body");

    let notif = post_mcp(
        client,
        token,
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

/// Call turbofig_execute via MCP and return the parsed tool-output JSON.
///
/// Reads only until the first SSE data frame arrives, then drops the stream.
/// This works correctly for concurrent calls: callers do not block waiting
/// for the server to close its SSE keep-alive stream.
async fn call_execute(
    client: &reqwest::Client,
    token: &str,
    base_url: &str,
    session_id: &str,
    file_key: Option<&str>,
    rpc_id: u64,
) -> serde_json::Value {
    let mut args = serde_json::json!({"code": "return 1;"});
    if let Some(fk) = file_key {
        args["fileKey"] = serde_json::Value::String(fk.to_owned());
    }

    let res = post_mcp(
        client,
        token,
        base_url,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
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

    let msg = read_first_sse_data(res).await;
    let text_str = msg["result"]["content"][0]["text"]
        .as_str()
        .expect("content[0].text must be a string");
    serde_json::from_str(text_str).expect("tool output must be valid JSON")
}

/// Wait until `state.list_connections()` has at least `n` entries or deadline.
/// Returns the count observed when the condition was last checked.
async fn wait_for_connections(state: &Arc<turbofig::AppState>, n: usize) -> usize {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let count = state.list_connections().len();
        if count >= n {
            return count;
        }
        if tokio::time::Instant::now() >= deadline {
            return count;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Two sessions drive two files concurrently with full isolation.
///
/// Setup:
///   - fk1 plugin replies {"from":"fk1-reply"}.
///   - fk2 plugin replies {"from":"fk2-reply"}.
///   - Session A pairs to fk1 via an explicit fileKey call over the MCP
///     HTTP endpoint (initialize -> tools/call with fileKey="fk1").
///   - Session B pairs to fk2 via the same path.
///
/// Isolation assertion:
///   - 5 concurrent no-fileKey calls from session A must ALL return fk1-reply.
///   - 5 concurrent no-fileKey calls from session B must ALL return fk2-reply.
///   - Not one reply may cross over.
///   - Both plugins stay registered throughout (list_connections length 2).
#[tokio::test]
async fn test_two_sessions_two_files_concurrent_and_isolated() {
    let (ws_port, base_url, state) = start_stack().await;

    // Connect both mock plugins. Each plugin echoes a distinct tag.
    connect_mock_plugin(ws_port, &state, "fk1", "fk1-reply").await;
    connect_mock_plugin(ws_port, &state, "fk2", "fk2-reply").await;

    // Wait until both plugins are registered before proceeding.
    let count = wait_for_connections(&state, 2).await;
    assert_eq!(
        count, 2,
        "both plugins must register before the test, got count={count}"
    );

    let client = make_client();
    let token = state.token().to_owned();

    // Handshake two independent MCP sessions.
    let session_a = mcp_handshake(&client, &token, &base_url).await;
    let session_b = mcp_handshake(&client, &token, &base_url).await;

    // Pair session A to fk1 via an explicit fileKey call.
    // Use rpc_id=1 for pairing calls (sequential, no conflict).
    let pair_a = call_execute(&client, &token, &base_url, &session_a, Some("fk1"), 1).await;
    assert_eq!(
        pair_a["ok"],
        serde_json::json!(true),
        "pairing session A to fk1 must succeed, got: {pair_a}"
    );
    assert_eq!(
        pair_a["result"]["from"],
        serde_json::json!("fk1-reply"),
        "pairing call for session A must reach fk1, got: {pair_a}"
    );

    // Pair session B to fk2 via an explicit fileKey call.
    let pair_b = call_execute(&client, &token, &base_url, &session_b, Some("fk2"), 1).await;
    assert_eq!(
        pair_b["ok"],
        serde_json::json!(true),
        "pairing session B to fk2 must succeed, got: {pair_b}"
    );
    assert_eq!(
        pair_b["result"]["from"],
        serde_json::json!("fk2-reply"),
        "pairing call for session B must reach fk2, got: {pair_b}"
    );

    // Both plugins must still be registered after pairing.
    let mid_count = state.list_connections().len();
    assert_eq!(
        mid_count, 2,
        "both plugins must remain registered after pairing, got count={mid_count}"
    );

    // Spawn 5 concurrent no-fileKey calls for each session (10 total).
    // Each call uses a distinct rpc_id to avoid JSON-RPC id conflicts.
    // All tasks run simultaneously to stress the isolation guarantee.
    const N: usize = 5;

    let mut handles_a: Vec<tokio::task::JoinHandle<serde_json::Value>> = Vec::with_capacity(N);
    let mut handles_b: Vec<tokio::task::JoinHandle<serde_json::Value>> = Vec::with_capacity(N);

    for i in 0..N {
        let c = client.clone();
        let tok = token.clone();
        let url = base_url.clone();
        let sid = session_a.clone();
        // Use rpc_id starting at 100 for session A calls to avoid conflicts.
        let rpc_id = 100 + i as u64;
        handles_a.push(tokio::spawn(async move {
            call_execute(&c, &tok, &url, &sid, None, rpc_id).await
        }));
    }

    for i in 0..N {
        let c = client.clone();
        let tok = token.clone();
        let url = base_url.clone();
        let sid = session_b.clone();
        // Use rpc_id starting at 200 for session B calls to avoid conflicts.
        let rpc_id = 200 + i as u64;
        handles_b.push(tokio::spawn(async move {
            call_execute(&c, &tok, &url, &sid, None, rpc_id).await
        }));
    }

    // Collect and assert all session A results. Every reply must come from fk1.
    for (i, handle) in handles_a.into_iter().enumerate() {
        let result = handle.await.expect("session A task must complete");
        assert_eq!(
            result["ok"],
            serde_json::json!(true),
            "session A call {i} must succeed, got: {result}"
        );
        assert_eq!(
            result["result"]["from"],
            serde_json::json!("fk1-reply"),
            "session A call {i} must come from fk1-reply (no cross-talk), got: {result}"
        );
    }

    // Collect and assert all session B results. Every reply must come from fk2.
    for (i, handle) in handles_b.into_iter().enumerate() {
        let result = handle.await.expect("session B task must complete");
        assert_eq!(
            result["ok"],
            serde_json::json!(true),
            "session B call {i} must succeed, got: {result}"
        );
        assert_eq!(
            result["result"]["from"],
            serde_json::json!("fk2-reply"),
            "session B call {i} must come from fk2-reply (no cross-talk), got: {result}"
        );
    }

    // Both plugins must still be registered after all concurrent calls.
    let final_count = state.list_connections().len();
    assert_eq!(
        final_count, 2,
        "both plugins must remain registered after concurrent calls, got count={final_count}"
    );
}
