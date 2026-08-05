//! Integration tests for the WebSocket plugin transport.
//!
//! Each test binds an ephemeral port, starts serve_ws with a fresh AppState,
//! and exercises the FILE_INFO registration, socket-close cleanup, and the
//! full turbofig_status round-trip through a mock plugin.
//! No port 18847 is ever hardcoded here.

use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::{connect_async, tungstenite::Message as TtMessage};

// ── helpers ───────────────────────────────────────────────────────────────────

/// Find the first non-empty `data:` line in an SSE body and parse it as JSON.
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
            .unwrap_or_else(|e| panic!("SSE data line is not valid JSON ({e}):\n{data}"));
    }
    panic!("No non-empty 'data:' line found in SSE body:\n{body}");
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// FILE_INFO registers the plugin; dropping the client clears it.
#[tokio::test]
async fn test_ws_file_info_registers_plugin() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral WS port");
    let addr = listener.local_addr().expect("read local addr");
    let state = Arc::new(turbofig::AppState::new());
    let state_srv = state.clone();

    tokio::spawn(async move {
        turbofig::serve_ws(listener, state_srv)
            .await
            .expect("serve_ws error in test");
    });

    // Connect a tungstenite client.
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{}/", addr.port()))
        .await
        .expect("WS connect failed");

    // Send a FILE_INFO frame.
    let msg = serde_json::json!({
        "type": "FILE_INFO",
        "fileKey": "abc123",
        "name": "My Design File"
    });
    ws.send(TtMessage::Text(msg.to_string()))
        .await
        .expect("send FILE_INFO");

    // Allow the server a short time to process the frame.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // The plugin must now be registered with the correct key and name.
    assert_eq!(
        state.plugin_snapshot(),
        Some(("abc123".to_owned(), "My Design File".to_owned())),
        "plugin must be registered after FILE_INFO"
    );

    // Drop the client to trigger a socket close.
    drop(ws);

    // Allow the server a short time to run clear_plugin.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // The plugin must now be cleared.
    assert_eq!(
        state.plugin_snapshot(),
        None,
        "plugin must be cleared after socket close"
    );
}

/// An unknown message type must not panic the server; subsequent FILE_INFO still works.
#[tokio::test]
async fn test_ws_unknown_message_type_is_ignored() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral WS port");
    let addr = listener.local_addr().expect("read local addr");
    let state = Arc::new(turbofig::AppState::new());
    let state_srv = state.clone();

    tokio::spawn(async move {
        turbofig::serve_ws(listener, state_srv)
            .await
            .expect("serve_ws error in test");
    });

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{}/", addr.port()))
        .await
        .expect("WS connect failed");

    // Send an unknown type. The server must not panic.
    let unknown = serde_json::json!({"type": "MYSTERY_TYPE", "data": 42});
    ws.send(TtMessage::Text(unknown.to_string()))
        .await
        .expect("send unknown type");

    // Send FILE_INFO afterwards. The connection must still be live.
    let fi = serde_json::json!({
        "type": "FILE_INFO",
        "fileKey": "xyz789",
        "name": "Other File"
    });
    ws.send(TtMessage::Text(fi.to_string()))
        .await
        .expect("send FILE_INFO after unknown type");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert_eq!(
        state.plugin_snapshot(),
        Some(("xyz789".to_owned(), "Other File".to_owned())),
        "plugin must register after an unknown message type was received"
    );
}

/// turbofig_status returns plugin.responsive:false when the plugin never replies.
/// The call must complete (not hang) in well under a few seconds.
#[tokio::test]
async fn test_turbofig_status_times_out_when_plugin_silent() {
    // Short timeout so the test stays fast and non-flaky.
    let state = Arc::new(turbofig::AppState::with_timeout(Duration::from_millis(80)));

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

    // Connect the mock plugin and register, but never reply to STATUS.
    let (mut plugin_ws, _) = connect_async(format!("ws://127.0.0.1:{}/", ws_addr.port()))
        .await
        .expect("mock plugin WS connect");

    let fi = serde_json::json!({
        "type": "FILE_INFO",
        "fileKey": "silent-plugin",
        "name": "Silent Plugin"
    });
    plugin_ws
        .send(TtMessage::Text(fi.to_string()))
        .await
        .expect("send FILE_INFO");

    // Allow the WS server to process FILE_INFO.
    tokio::time::sleep(Duration::from_millis(30)).await;

    // Spawn a task that keeps the connection open but discards all incoming frames.
    tokio::spawn(async move { while let Some(Ok(_)) = plugin_ws.next().await {} });

    // Drive the MCP handshake over HTTP.
    let base_url = format!("http://{http_addr}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("build reqwest client");

    let init_res = client
        .post(format!("{base_url}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "clientInfo": {"name": "test-client", "version": "0.1.0"},
                "capabilities": {}
            }
        }))
        .send()
        .await
        .expect("initialize");

    let session_id = init_res
        .headers()
        .get("mcp-session-id")
        .expect("mcp-session-id header")
        .to_str()
        .expect("valid utf8")
        .to_owned();
    let _ = init_res.text().await.expect("drain init body");

    let _ = client
        .post(format!("{base_url}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }))
        .send()
        .await
        .expect("notifications/initialized")
        .text()
        .await
        .expect("drain notif");

    // tools/call turbofig_status. Must return before the test's own timeout (5 s).
    let tools_res = client
        .post(format!("{base_url}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "turbofig_status", "arguments": {}}
        }))
        .send()
        .await
        .expect("tools/call (must not hang)");

    let tools_body = tools_res.text().await.expect("read tools/call body");
    let msg = parse_sse_data(&tools_body);
    let text_str = msg["result"]["content"][0]["text"]
        .as_str()
        .expect("content[0].text is a string");
    let payload: serde_json::Value =
        serde_json::from_str(text_str).expect("content[0].text is valid JSON");

    assert_eq!(
        payload["ok"],
        serde_json::json!(true),
        "ok must be true even on timeout, got: {payload}"
    );
    assert_eq!(
        payload["plugin"]["connected"],
        serde_json::json!(true),
        "plugin.connected must be true (plugin was registered), got: {payload}"
    );
    assert_eq!(
        payload["plugin"]["responsive"],
        serde_json::json!(false),
        "plugin.responsive must be false when the plugin did not reply, got: {payload}"
    );
}

/// turbofig_status routes a STATUS request to the mock plugin and returns
/// the RESULT as `{"ok":true,"plugin":{"connected":true,"fileKey":...,"name":...}}`.
#[tokio::test]
async fn test_turbofig_status_routes_through_plugin() {
    let state = Arc::new(turbofig::AppState::new());

    // Bind ephemeral listeners for both servers.
    let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind WS port");
    let http_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind HTTP port");
    let ws_addr = ws_listener.local_addr().expect("ws local addr");
    let http_addr = http_listener.local_addr().expect("http local addr");

    // Spawn the WS server.
    let ws_state = state.clone();
    tokio::spawn(async move {
        turbofig::serve_ws(ws_listener, ws_state)
            .await
            .expect("serve_ws error in test");
    });

    // Spawn the MCP HTTP server.
    let http_state = state.clone();
    tokio::spawn(async move {
        turbofig::serve_with_state(http_listener, http_state)
            .await
            .expect("serve_with_state error in test");
    });

    // Connect the mock plugin over WebSocket.
    let (mut plugin_ws, _) = connect_async(format!("ws://127.0.0.1:{}/", ws_addr.port()))
        .await
        .expect("mock plugin WS connect failed");

    // Register the plugin.
    let fi = serde_json::json!({
        "type": "FILE_INFO",
        "fileKey": "abc123",
        "name": "My Design File"
    });
    plugin_ws
        .send(TtMessage::Text(fi.to_string()))
        .await
        .expect("send FILE_INFO");

    // Allow the WS server to process FILE_INFO.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Spawn the mock plugin responder: reply to every STATUS frame with RESULT.
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

    // Drive the MCP handshake over HTTP.
    let base_url = format!("http://{http_addr}");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("build reqwest client");

    // Step 1: initialize.
    let init_res = client
        .post(format!("{base_url}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "clientInfo": {"name": "test-client", "version": "0.1.0"},
                "capabilities": {}
            }
        }))
        .send()
        .await
        .expect("initialize request");

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

    // Step 2: notifications/initialized.
    let notif_res = client
        .post(format!("{base_url}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }))
        .send()
        .await
        .expect("notifications/initialized request");
    let _ = notif_res.text().await.expect("drain notif body");

    // Step 3: tools/call turbofig_status.
    let tools_res = client
        .post(format!("{base_url}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "turbofig_status", "arguments": {}}
        }))
        .send()
        .await
        .expect("tools/call request");

    assert!(
        tools_res.status().is_success(),
        "tools/call must succeed, got HTTP {}",
        tools_res.status()
    );

    let tools_body = tools_res.text().await.expect("read tools/call body");
    let msg = parse_sse_data(&tools_body);

    let text_str = msg["result"]["content"][0]["text"]
        .as_str()
        .expect("content[0].text must be a string");
    let payload: serde_json::Value =
        serde_json::from_str(text_str).expect("content[0].text must be valid JSON");

    assert_eq!(
        payload["ok"],
        serde_json::json!(true),
        "ok must be true when plugin is connected, got: {payload}"
    );
    assert_eq!(
        payload["plugin"]["connected"],
        serde_json::json!(true),
        "plugin.connected must be true, got: {payload}"
    );
    assert_eq!(
        payload["plugin"]["fileKey"],
        serde_json::json!("abc123"),
        "plugin.fileKey must match, got: {payload}"
    );
    assert_eq!(
        payload["plugin"]["name"],
        serde_json::json!("My Design File"),
        "plugin.name must match, got: {payload}"
    );
}

/// The plugin re-pairs with zero manual steps after a Figma restart and after
/// a daemon restart.
///
/// The plugin reconnects on its own with infinite backoff and sends FILE_INFO
/// on every connect (see the plugin UI client and code.ts). This test models
/// both restart cases on the daemon side:
///   Part A: the socket drops (Figma closed) and a new socket re-registers.
///   Part B: a fresh AppState on a new listener (daemon restarted) registers
///           the reconnecting plugin.
/// In both cases the only client action is reconnect + FILE_INFO, which the
/// real plugin performs automatically.
#[tokio::test]
async fn test_plugin_repairs_after_figma_and_daemon_restart() {
    let file_info = serde_json::json!({
        "type": "FILE_INFO",
        "fileKey": "F1",
        "name": "Deck File"
    });

    // ── Part A: Figma restart (socket drops, new socket re-registers) ──────────
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral WS port");
    let addr = listener.local_addr().expect("read local addr");
    let state = Arc::new(turbofig::AppState::new());
    let state_srv = state.clone();
    tokio::spawn(async move {
        turbofig::serve_ws(listener, state_srv)
            .await
            .expect("serve_ws error in test");
    });

    // First connect: the plugin registers.
    let (mut ws1, _) = connect_async(format!("ws://127.0.0.1:{}/", addr.port()))
        .await
        .expect("first WS connect failed");
    ws1.send(TtMessage::Text(file_info.to_string()))
        .await
        .expect("send FILE_INFO (1)");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        state.plugin_snapshot(),
        Some(("F1".to_owned(), "Deck File".to_owned())),
        "plugin must register on first connect"
    );

    // Figma closes: drop the socket. The registry clears.
    drop(ws1);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        state.plugin_snapshot(),
        None,
        "plugin must clear when the socket drops"
    );

    // Figma reopens: a new socket re-registers with no manual steps.
    let (mut ws2, _) = connect_async(format!("ws://127.0.0.1:{}/", addr.port()))
        .await
        .expect("reconnect WS failed");
    ws2.send(TtMessage::Text(file_info.to_string()))
        .await
        .expect("send FILE_INFO (2)");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        state.plugin_snapshot(),
        Some(("F1".to_owned(), "Deck File".to_owned())),
        "plugin must re-pair after a Figma restart"
    );

    // ── Part B: daemon restart (fresh state on a new listener) ─────────────────
    let listener_b = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral WS port B");
    let addr_b = listener_b.local_addr().expect("read local addr B");
    let state_b = Arc::new(turbofig::AppState::new());
    let state_b_srv = state_b.clone();
    tokio::spawn(async move {
        turbofig::serve_ws(listener_b, state_b_srv)
            .await
            .expect("serve_ws B error in test");
    });

    // The plugin reconnects to the restarted daemon and re-registers.
    let (mut ws3, _) = connect_async(format!("ws://127.0.0.1:{}/", addr_b.port()))
        .await
        .expect("connect to restarted daemon failed");
    ws3.send(TtMessage::Text(file_info.to_string()))
        .await
        .expect("send FILE_INFO (3)");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        state_b.plugin_snapshot(),
        Some(("F1".to_owned(), "Deck File".to_owned())),
        "plugin must re-pair after a daemon restart"
    );
}

/// turbofig_execute routes an EXECUTE request to the mock plugin and returns
/// the RESULT as `{"ok":true,"result":{"created":"frame-1"}}`.
#[tokio::test]
async fn test_turbofig_execute_routes_through_plugin() {
    let state = Arc::new(turbofig::AppState::new());

    // Bind ephemeral listeners for both servers.
    let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind WS port");
    let http_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind HTTP port");
    let ws_addr = ws_listener.local_addr().expect("ws local addr");
    let http_addr = http_listener.local_addr().expect("http local addr");

    // Spawn the WS server.
    let ws_state = state.clone();
    tokio::spawn(async move {
        turbofig::serve_ws(ws_listener, ws_state)
            .await
            .expect("serve_ws error in test");
    });

    // Spawn the MCP HTTP server.
    let http_state = state.clone();
    tokio::spawn(async move {
        turbofig::serve_with_state(http_listener, http_state)
            .await
            .expect("serve_with_state error in test");
    });

    // Connect the mock plugin over WebSocket.
    let (mut plugin_ws, _) = connect_async(format!("ws://127.0.0.1:{}/", ws_addr.port()))
        .await
        .expect("mock plugin WS connect failed");

    // Register the plugin.
    let fi = serde_json::json!({
        "type": "FILE_INFO",
        "fileKey": "abc123",
        "name": "My Design File"
    });
    plugin_ws
        .send(TtMessage::Text(fi.to_string()))
        .await
        .expect("send FILE_INFO");

    // Allow the WS server to process FILE_INFO.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Spawn the mock plugin responder: reply to every EXECUTE frame with RESULT.
    tokio::spawn(async move {
        while let Some(Ok(msg)) = plugin_ws.next().await {
            if let TtMessage::Text(text) = msg {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    if json.get("type").and_then(|t| t.as_str()) == Some("EXECUTE") {
                        if let Some(id) = json.get("requestId").and_then(|v| v.as_u64()) {
                            let reply = serde_json::json!({
                                "type": "RESULT",
                                "requestId": id,
                                "ok": true,
                                "result": {"created": "frame-1"}
                            });
                            let _ = plugin_ws.send(TtMessage::Text(reply.to_string())).await;
                        }
                    }
                }
            }
        }
    });

    // Drive the MCP handshake over HTTP.
    let base_url = format!("http://{http_addr}");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("build reqwest client");

    // Step 1: initialize.
    let init_res = client
        .post(format!("{base_url}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "clientInfo": {"name": "test-client", "version": "0.1.0"},
                "capabilities": {}
            }
        }))
        .send()
        .await
        .expect("initialize request");

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

    // Step 2: notifications/initialized.
    let notif_res = client
        .post(format!("{base_url}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }))
        .send()
        .await
        .expect("notifications/initialized request");
    let _ = notif_res.text().await.expect("drain notif body");

    // Step 3: tools/call turbofig_execute with JS code.
    let tools_res = client
        .post(format!("{base_url}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "turbofig_execute", "arguments": {"code": "return 1+1;"}}
        }))
        .send()
        .await
        .expect("tools/call request");

    assert!(
        tools_res.status().is_success(),
        "tools/call must succeed, got HTTP {}",
        tools_res.status()
    );

    let tools_body = tools_res.text().await.expect("read tools/call body");
    let msg = parse_sse_data(&tools_body);

    let text_str = msg["result"]["content"][0]["text"]
        .as_str()
        .expect("content[0].text must be a string");
    let payload: serde_json::Value =
        serde_json::from_str(text_str).expect("content[0].text must be valid JSON");

    assert_eq!(
        payload["ok"],
        serde_json::json!(true),
        "ok must be true when plugin executes successfully, got: {payload}"
    );
    assert_eq!(
        payload["result"]["created"],
        serde_json::json!("frame-1"),
        "result.created must match the plugin reply, got: {payload}"
    );
}

/// turbofig_get_selection routes a GET_SELECTION request to the mock plugin
/// and returns the RESULT as `{"ok":true,"selection":[...]}`.
#[tokio::test]
async fn test_turbofig_get_selection_routes_through_plugin() {
    let state = Arc::new(turbofig::AppState::new());

    // Bind ephemeral listeners for both servers.
    let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind WS port");
    let http_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind HTTP port");
    let ws_addr = ws_listener.local_addr().expect("ws local addr");
    let http_addr = http_listener.local_addr().expect("http local addr");

    // Spawn the WS server.
    let ws_state = state.clone();
    tokio::spawn(async move {
        turbofig::serve_ws(ws_listener, ws_state)
            .await
            .expect("serve_ws error in test");
    });

    // Spawn the MCP HTTP server.
    let http_state = state.clone();
    tokio::spawn(async move {
        turbofig::serve_with_state(http_listener, http_state)
            .await
            .expect("serve_with_state error in test");
    });

    // Connect the mock plugin over WebSocket.
    let (mut plugin_ws, _) = connect_async(format!("ws://127.0.0.1:{}/", ws_addr.port()))
        .await
        .expect("mock plugin WS connect failed");

    // Register the plugin.
    let fi = serde_json::json!({
        "type": "FILE_INFO",
        "fileKey": "abc123",
        "name": "My Design File"
    });
    plugin_ws
        .send(TtMessage::Text(fi.to_string()))
        .await
        .expect("send FILE_INFO");

    // Allow the WS server to process FILE_INFO.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Spawn the mock plugin responder: reply to every GET_SELECTION frame with RESULT.
    tokio::spawn(async move {
        while let Some(Ok(msg)) = plugin_ws.next().await {
            if let TtMessage::Text(text) = msg {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    if json.get("type").and_then(|t| t.as_str()) == Some("GET_SELECTION") {
                        if let Some(id) = json.get("requestId").and_then(|v| v.as_u64()) {
                            let reply = serde_json::json!({
                                "type": "RESULT",
                                "requestId": id,
                                "ok": true,
                                "selection": [
                                    {"id": "1:2", "name": "Frame 1", "type": "FRAME",
                                     "x": 0, "y": 0, "w": 100, "h": 50}
                                ]
                            });
                            let _ = plugin_ws.send(TtMessage::Text(reply.to_string())).await;
                        }
                    }
                }
            }
        }
    });

    // Drive the MCP handshake over HTTP.
    let base_url = format!("http://{http_addr}");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("build reqwest client");

    // Step 1: initialize.
    let init_res = client
        .post(format!("{base_url}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "clientInfo": {"name": "test-client", "version": "0.1.0"},
                "capabilities": {}
            }
        }))
        .send()
        .await
        .expect("initialize request");

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

    // Step 2: notifications/initialized.
    let notif_res = client
        .post(format!("{base_url}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }))
        .send()
        .await
        .expect("notifications/initialized request");
    let _ = notif_res.text().await.expect("drain notif body");

    // Step 3: tools/call turbofig_get_selection.
    let tools_res = client
        .post(format!("{base_url}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "turbofig_get_selection", "arguments": {}}
        }))
        .send()
        .await
        .expect("tools/call request");

    assert!(
        tools_res.status().is_success(),
        "tools/call must succeed, got HTTP {}",
        tools_res.status()
    );

    let tools_body = tools_res.text().await.expect("read tools/call body");
    let msg = parse_sse_data(&tools_body);

    let text_str = msg["result"]["content"][0]["text"]
        .as_str()
        .expect("content[0].text must be a string");
    let payload: serde_json::Value =
        serde_json::from_str(text_str).expect("content[0].text must be valid JSON");

    assert_eq!(
        payload["ok"],
        serde_json::json!(true),
        "ok must be true when plugin returns selection, got: {payload}"
    );
    assert_eq!(
        payload["selection"][0]["name"],
        serde_json::json!("Frame 1"),
        "selection[0].name must match the plugin reply, got: {payload}"
    );
}

/// turbofig_screenshot routes a SCREENSHOT request to the mock plugin and returns
/// the RESULT as `{"ok":true,"w":100,"h":50,"png":"aGVsbG8="}` in inline mode.
#[tokio::test]
async fn test_turbofig_screenshot_inline_routes_through_plugin() {
    let state = Arc::new(turbofig::AppState::new());

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

    let (mut plugin_ws, _) = connect_async(format!("ws://127.0.0.1:{}/", ws_addr.port()))
        .await
        .expect("mock plugin WS connect failed");

    let fi = serde_json::json!({
        "type": "FILE_INFO",
        "fileKey": "abc123",
        "name": "My Design File"
    });
    plugin_ws
        .send(TtMessage::Text(fi.to_string()))
        .await
        .expect("send FILE_INFO");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Spawn the mock plugin: reply to every SCREENSHOT frame with a fixed RESULT.
    tokio::spawn(async move {
        while let Some(Ok(msg)) = plugin_ws.next().await {
            if let TtMessage::Text(text) = msg {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    if json.get("type").and_then(|t| t.as_str()) == Some("SCREENSHOT") {
                        if let Some(id) = json.get("requestId").and_then(|v| v.as_u64()) {
                            let reply = serde_json::json!({
                                "type": "RESULT",
                                "requestId": id,
                                "ok": true,
                                "png": "aGVsbG8=",
                                "w": 100,
                                "h": 50
                            });
                            let _ = plugin_ws.send(TtMessage::Text(reply.to_string())).await;
                        }
                    }
                }
            }
        }
    });

    let base_url = format!("http://{http_addr}");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("build reqwest client");

    let init_res = client
        .post(format!("{base_url}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "clientInfo": {"name": "test-client", "version": "0.1.0"},
                "capabilities": {}
            }
        }))
        .send()
        .await
        .expect("initialize request");

    assert!(init_res.status().is_success());

    let session_id = init_res
        .headers()
        .get("mcp-session-id")
        .expect("mcp-session-id header")
        .to_str()
        .expect("valid utf8")
        .to_owned();
    let _ = init_res.text().await.expect("drain init body");

    let notif_res = client
        .post(format!("{base_url}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }))
        .send()
        .await
        .expect("notifications/initialized request");
    let _ = notif_res.text().await.expect("drain notif body");

    let tools_res = client
        .post(format!("{base_url}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "turbofig_screenshot", "arguments": {"return": "inline"}}
        }))
        .send()
        .await
        .expect("tools/call request");

    assert!(tools_res.status().is_success());

    let tools_body = tools_res.text().await.expect("read tools/call body");
    let msg = parse_sse_data(&tools_body);

    let text_str = msg["result"]["content"][0]["text"]
        .as_str()
        .expect("content[0].text must be a string");
    let payload: serde_json::Value =
        serde_json::from_str(text_str).expect("content[0].text must be valid JSON");

    assert_eq!(
        payload["ok"],
        serde_json::json!(true),
        "ok must be true in inline mode, got: {payload}"
    );
    assert_eq!(
        payload["png"],
        serde_json::json!("aGVsbG8="),
        "png must echo the plugin reply, got: {payload}"
    );
    assert_eq!(
        payload["w"],
        serde_json::json!(100),
        "w must be 100, got: {payload}"
    );
}

/// run_screenshot in file mode connects to a mock plugin, receives a RESULT, decodes
/// the base64 PNG, writes it to a temp dir, and returns ok:true with a path field.
/// The file at the returned path must contain the decoded bytes.
#[tokio::test]
async fn test_run_screenshot_file_mode_writes_png() {
    let state = Arc::new(turbofig::AppState::new());

    let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind WS port");
    let ws_addr = ws_listener.local_addr().expect("ws local addr");

    let ws_state = state.clone();
    tokio::spawn(async move {
        turbofig::serve_ws(ws_listener, ws_state)
            .await
            .expect("serve_ws error in test");
    });

    let (mut plugin_ws, _) = connect_async(format!("ws://127.0.0.1:{}/", ws_addr.port()))
        .await
        .expect("mock plugin WS connect");

    let fi = serde_json::json!({
        "type": "FILE_INFO",
        "fileKey": "abc123",
        "name": "My Design File"
    });
    plugin_ws
        .send(TtMessage::Text(fi.to_string()))
        .await
        .expect("send FILE_INFO");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Spawn the mock plugin: reply to SCREENSHOT frames with a fixed base64 payload.
    // "aGVsbG8=" is the base64 encoding of b"hello".
    tokio::spawn(async move {
        while let Some(Ok(msg)) = plugin_ws.next().await {
            if let TtMessage::Text(text) = msg {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    if json.get("type").and_then(|t| t.as_str()) == Some("SCREENSHOT") {
                        if let Some(id) = json.get("requestId").and_then(|v| v.as_u64()) {
                            let reply = serde_json::json!({
                                "type": "RESULT",
                                "requestId": id,
                                "ok": true,
                                "png": "aGVsbG8=",
                                "w": 100,
                                "h": 50
                            });
                            let _ = plugin_ws.send(TtMessage::Text(reply.to_string())).await;
                        }
                    }
                }
            }
        }
    });

    let tmp = tempfile::tempdir().expect("create tempdir");
    let result = turbofig::run_screenshot(&state, 1.0, None, "file", Some(tmp.path())).await;

    assert_eq!(
        result["ok"],
        serde_json::json!(true),
        "ok must be true in file mode, got: {result}"
    );

    let path_str = result["path"]
        .as_str()
        .expect("result must have a path string field");
    let path = std::path::Path::new(path_str);
    assert!(path.exists(), "PNG file must exist at {path_str}");

    let contents = std::fs::read(path).expect("read PNG file");
    assert_eq!(
        contents, b"hello",
        "file contents must be the decoded base64 bytes"
    );
}

/// run_execute returns `{"ok":false,"error":"no plugin connected"}` when no
/// plugin is registered. Tested directly without HTTP overhead.
#[tokio::test]
async fn test_turbofig_execute_no_plugin_returns_error() {
    let state = Arc::new(turbofig::AppState::new());
    let value = turbofig::run_execute(&state, "return 1+1;").await;

    assert_eq!(
        value["ok"],
        serde_json::json!(false),
        "ok must be false when no plugin is connected, got: {value}"
    );
    assert_eq!(
        value["error"],
        serde_json::json!("no plugin connected"),
        "error must be 'no plugin connected', got: {value}"
    );
}

/// A plugin that disconnects mid-request must not hang the caller.
///
/// The default request timeout is 30 s. When the plugin drops the socket while
/// a routed status call waits, the daemon drains the pending map, so the call
/// returns `plugin.connected:false` at once rather than after the timeout.
#[tokio::test]
async fn test_disconnect_mid_request_does_not_hang_caller() {
    // Default (30 s) timeout: a fast return proves the drain path, not a timeout.
    let state = Arc::new(turbofig::AppState::new());

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

    // Connect the mock plugin and register.
    let (mut plugin_ws, _) = connect_async(format!("ws://127.0.0.1:{}/", ws_addr.port()))
        .await
        .expect("mock plugin WS connect");
    plugin_ws
        .send(TtMessage::Text(
            serde_json::json!({"type": "FILE_INFO", "fileKey": "F1", "name": "Deck File"})
                .to_string(),
        ))
        .await
        .expect("send FILE_INFO");
    tokio::time::sleep(Duration::from_millis(50)).await;

    // The plugin waits for one STATUS frame, then drops the socket (no reply).
    tokio::spawn(async move {
        while let Some(Ok(msg)) = plugin_ws.next().await {
            if let TtMessage::Text(text) = msg {
                if text.contains("STATUS") {
                    break; // drop plugin_ws here: simulate an abrupt disconnect.
                }
            }
        }
    });

    // Drive the handshake and the routed status call, measuring wall time.
    let base_url = format!("http://{http_addr}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("build reqwest client");

    let init_res = client
        .post(format!("{base_url}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "clientInfo": {"name": "test-client", "version": "0.1.0"},
                "capabilities": {}
            }
        }))
        .send()
        .await
        .expect("initialize");
    let session_id = init_res
        .headers()
        .get("mcp-session-id")
        .expect("mcp-session-id header")
        .to_str()
        .expect("valid utf8")
        .to_owned();
    let _ = init_res.text().await.expect("drain init body");

    let _ = client
        .post(format!("{base_url}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}}))
        .send()
        .await
        .expect("notifications/initialized")
        .text()
        .await
        .expect("drain notif");

    let start = std::time::Instant::now();
    let tools_res = client
        .post(format!("{base_url}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "turbofig_status", "arguments": {}}
        }))
        .send()
        .await
        .expect("tools/call (must not hang)");
    let elapsed = start.elapsed();

    // The call must return well before the 30 s request timeout.
    assert!(
        elapsed < Duration::from_secs(3),
        "call must return promptly on disconnect, took {elapsed:?}"
    );

    let body = tools_res.text().await.expect("read tools body");
    let msg = parse_sse_data(&body);
    let payload: serde_json::Value = serde_json::from_str(
        msg["result"]["content"][0]["text"]
            .as_str()
            .expect("tool text is a string"),
    )
    .expect("tool text is JSON");
    assert_eq!(
        payload["plugin"]["connected"],
        serde_json::json!(false),
        "a mid-request disconnect must return plugin.connected:false, got: {payload}"
    );
}

/// An eval that throws in the plugin returns a clean error to the tool caller
/// and never crashes the daemon. A follow-up status call still succeeds.
#[tokio::test]
async fn test_turbofig_execute_eval_error_returns_clean_message() {
    let state = Arc::new(turbofig::AppState::new());

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

    let (mut plugin_ws, _) = connect_async(format!("ws://127.0.0.1:{}/", ws_addr.port()))
        .await
        .expect("mock plugin WS connect");
    plugin_ws
        .send(TtMessage::Text(
            serde_json::json!({"type": "FILE_INFO", "fileKey": "abc123", "name": "My Design File"})
                .to_string(),
        ))
        .await
        .expect("send FILE_INFO");
    tokio::time::sleep(Duration::from_millis(50)).await;

    // The mock plugin models a thrown eval: EXECUTE gets ok:false with an error.
    // STATUS still replies normally so the follow-up liveness check passes.
    tokio::spawn(async move {
        while let Some(Ok(msg)) = plugin_ws.next().await {
            if let TtMessage::Text(text) = msg {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    let ty = json.get("type").and_then(|t| t.as_str());
                    let id = json.get("requestId").and_then(|v| v.as_u64());
                    if let Some(id) = id {
                        let reply = match ty {
                            Some("EXECUTE") => serde_json::json!({
                                "type": "RESULT", "requestId": id, "ok": false,
                                "error": "ReferenceError: foo is not defined"
                            }),
                            Some("STATUS") => serde_json::json!({
                                "type": "RESULT", "requestId": id, "ok": true,
                                "fileKey": "abc123", "name": "My Design File"
                            }),
                            _ => continue,
                        };
                        let _ = plugin_ws.send(TtMessage::Text(reply.to_string())).await;
                    }
                }
            }
        }
    });

    let base_url = format!("http://{http_addr}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("build reqwest client");

    let init_res = client
        .post(format!("{base_url}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "clientInfo": {"name": "test-client", "version": "0.1.0"},
                "capabilities": {}
            }
        }))
        .send()
        .await
        .expect("initialize");
    let session_id = init_res
        .headers()
        .get("mcp-session-id")
        .expect("mcp-session-id header")
        .to_str()
        .expect("valid utf8")
        .to_owned();
    let _ = init_res.text().await.expect("drain init body");

    let _ = client
        .post(format!("{base_url}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}}))
        .send()
        .await
        .expect("notifications/initialized")
        .text()
        .await
        .expect("drain notif");

    // Call execute. The eval throws in the plugin. The tool must return a clean
    // error over HTTP 200, not crash.
    let exec_res = client
        .post(format!("{base_url}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "turbofig_execute", "arguments": {"code": "return foo;"}}
        }))
        .send()
        .await
        .expect("tools/call execute");
    assert!(
        exec_res.status().is_success(),
        "tool must return HTTP 200 even on eval error, got HTTP {}",
        exec_res.status()
    );
    let exec_payload: serde_json::Value = serde_json::from_str(
        parse_sse_data(&exec_res.text().await.expect("exec body"))["result"]["content"][0]["text"]
            .as_str()
            .expect("exec text is a string"),
    )
    .expect("exec text is JSON");
    assert_eq!(
        exec_payload["ok"],
        serde_json::json!(false),
        "eval error must return ok:false, got: {exec_payload}"
    );
    assert_eq!(
        exec_payload["error"],
        serde_json::json!("ReferenceError: foo is not defined"),
        "the plugin error message must pass through verbatim, got: {exec_payload}"
    );

    // The daemon must still serve. A follow-up status call succeeds.
    let status_res = client
        .post(format!("{base_url}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "turbofig_status", "arguments": {}}
        }))
        .send()
        .await
        .expect("tools/call status after error");
    let status_payload: serde_json::Value = serde_json::from_str(
        parse_sse_data(&status_res.text().await.expect("status body"))["result"]["content"][0]
            ["text"]
            .as_str()
            .expect("status text is a string"),
    )
    .expect("status text is JSON");
    assert_eq!(
        status_payload["ok"],
        serde_json::json!(true),
        "daemon must stay alive and answer status after an eval error, got: {status_payload}"
    );
}
