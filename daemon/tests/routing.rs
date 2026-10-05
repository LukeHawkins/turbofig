//! Integration tests for MCP session pairing and file-key routing.
//!
//! These tests drive the real MCP HTTP endpoint over ephemeral ports. They
//! connect mock plugins over WebSocket with distinct fileKeys, then assert
//! that tools/call routes to the correct plugin based on the explicit fileKey
//! param and session-sticky pairing.

use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::{connect_async, tungstenite::Message as TtMessage};

// ── helpers ───────────────────────────────────────────────────────────────────

/// Start a shared WS + HTTP server pair. Returns (ws_addr_port, http_base_url, state).
async fn start_stack() -> (u16, String, Arc<turbofig::AppState>) {
    let state = Arc::new(turbofig::AppState::with_timeout(Duration::from_secs(5)));

    let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind WS port");
    let http_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind HTTP port");

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

/// Connect a mock plugin with `file_key`. The plugin replies to every EXECUTE
/// frame with `{"ok":true,"result":{"from":<reply_tag>}}`, where `reply_tag`
/// identifies which plugin sent the reply.
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

    // No fixed sleep here. Callers use wait_for_file_key to poll observable state.
    tokio::spawn(async move {
        while let Some(Ok(msg)) = ws.next().await {
            if let TtMessage::Text(text) = msg {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    let ty = json.get("type").and_then(|t| t.as_str());
                    if ty == Some("EXECUTE") {
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

/// Poll `state.list_connections()` until it contains an entry for `file_key`
/// or the 3 s deadline passes. Returns true when found.
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

/// Build a reqwest client with a generous timeout.
fn make_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("build reqwest client")
}

/// POST to /mcp with the required MCP headers.
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
                "clientInfo": {"name": "routing-test", "version": "0.1.0"},
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

/// Call turbofig_execute via MCP and return the parsed tool-output JSON.
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
        "tools/call must succeed, got HTTP {}",
        res.status()
    );

    let body = res.text().await.expect("read tools/call body");
    let msg = parse_sse_data(&body);
    let text_str = msg["result"]["content"][0]["text"]
        .as_str()
        .expect("content[0].text must be a string");
    serde_json::from_str(text_str).expect("tool output must be valid JSON")
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// With one connected plugin and no explicit fileKey, turbofig_execute routes
/// to the sole plugin and returns ok:true.
#[tokio::test]
async fn test_execute_one_plugin_no_file_key_routes_to_it() {
    let (ws_port, base_url, state) = start_stack().await;

    connect_mock_plugin(ws_port, &state, "fk1", "fk1-reply").await;
    // Wait on observable state, not a fixed sleep.
    let found = wait_for_file_key(&state, "fk1").await;
    assert!(found, "fk1 must register before routing");

    let client = make_client();
    let session_id = mcp_handshake(&client, &base_url).await;

    // No fileKey: must auto-route to the sole connected plugin.
    let payload = call_execute(&client, &base_url, &session_id, None).await;

    assert_eq!(
        payload["ok"],
        serde_json::json!(true),
        "execute must succeed with one plugin and no fileKey, got: {payload}"
    );
    assert_eq!(
        payload["result"]["from"],
        serde_json::json!("fk1-reply"),
        "result must come from the sole plugin, got: {payload}"
    );
}

/// With two connected plugins, an explicit fileKey routes to the correct one.
/// The reply tag identifies which plugin handled the request.
#[tokio::test]
async fn test_execute_two_plugins_explicit_file_key_routes_to_correct_plugin() {
    let (ws_port, base_url, state) = start_stack().await;

    connect_mock_plugin(ws_port, &state, "fk1", "fk1-reply").await;
    connect_mock_plugin(ws_port, &state, "fk2", "fk2-reply").await;
    // Wait on observable state before routing.
    let found1 = wait_for_file_key(&state, "fk1").await;
    let found2 = wait_for_file_key(&state, "fk2").await;
    assert!(found1, "fk1 must register before routing");
    assert!(found2, "fk2 must register before routing");

    let client = make_client();
    let session_id = mcp_handshake(&client, &base_url).await;

    // Explicit fileKey=fk2: must route to fk2, not fk1.
    let payload = call_execute(&client, &base_url, &session_id, Some("fk2")).await;

    assert_eq!(
        payload["ok"],
        serde_json::json!(true),
        "execute with explicit fk2 must succeed, got: {payload}"
    );
    assert_eq!(
        payload["result"]["from"],
        serde_json::json!("fk2-reply"),
        "reply must come from fk2, not fk1, got: {payload}"
    );
}

/// A first call with explicit fileKey=fk2 pairs the session. A second call
/// from the SAME mcp-session-id without fileKey still routes to fk2.
#[tokio::test]
async fn test_session_stickiness_after_explicit_file_key() {
    let (ws_port, base_url, state) = start_stack().await;

    connect_mock_plugin(ws_port, &state, "fk1", "fk1-reply").await;
    connect_mock_plugin(ws_port, &state, "fk2", "fk2-reply").await;
    // Wait on observable state before routing.
    let found1 = wait_for_file_key(&state, "fk1").await;
    let found2 = wait_for_file_key(&state, "fk2").await;
    assert!(found1, "fk1 must register before routing");
    assert!(found2, "fk2 must register before routing");

    let client = make_client();
    let session_id = mcp_handshake(&client, &base_url).await;

    // First call: explicit fk2 -> pairs session to fk2.
    let first = call_execute(&client, &base_url, &session_id, Some("fk2")).await;
    assert_eq!(
        first["result"]["from"],
        serde_json::json!("fk2-reply"),
        "first call with explicit fk2 must reach fk2, got: {first}"
    );

    // Second call: no fileKey -> session pairing must still direct to fk2.
    let second = call_execute(&client, &base_url, &session_id, None).await;
    assert_eq!(
        second["ok"],
        serde_json::json!(true),
        "second call must succeed via session pairing, got: {second}"
    );
    assert_eq!(
        second["result"]["from"],
        serde_json::json!("fk2-reply"),
        "second call without fileKey must still reach fk2 via session pairing, got: {second}"
    );
}
