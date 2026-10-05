//! Integration tests for the WebSocket plugin transport.
//!
//! Each test binds an ephemeral port, starts serve_ws with a fresh AppState,
//! and exercises the FILE_INFO registration, socket-close cleanup, and the
//! full turbofig_status round-trip through a mock plugin.
//! No port 18847 is ever hardcoded here.

mod common;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use common::wait_until;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
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
    let (mut ws, _) = connect_async(format!(
        "ws://127.0.0.1:{}/?token={}",
        addr.port(),
        state.token()
    ))
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

    // Wait for the server to process the frame.
    wait_until(
        || {
            state
                .plugin_snapshot()
                .is_some_and(|(fk, _)| !fk.is_empty())
        },
        common::WAIT_DEADLINE_MS,
        "plugin to register after FILE_INFO",
    )
    .await;

    // The plugin must now be registered with the correct key and name.
    assert_eq!(
        state.plugin_snapshot(),
        Some(("abc123".to_owned(), "My Design File".to_owned())),
        "plugin must be registered after FILE_INFO"
    );

    // Drop the client to trigger a socket close.
    drop(ws);

    // Wait for the server to run clear_plugin.
    wait_until(
        || state.plugin_snapshot().is_none(),
        common::WAIT_DEADLINE_MS,
        "plugin to clear after socket close",
    )
    .await;

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

    let (mut ws, _) = connect_async(format!(
        "ws://127.0.0.1:{}/?token={}",
        addr.port(),
        state.token()
    ))
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

    wait_until(
        || {
            state
                .plugin_snapshot()
                .is_some_and(|(fk, _)| !fk.is_empty())
        },
        common::WAIT_DEADLINE_MS,
        "plugin to register after an unknown message type",
    )
    .await;

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
    let (mut plugin_ws, _) = connect_async(format!(
        "ws://127.0.0.1:{}/?token={}",
        ws_addr.port(),
        state.token()
    ))
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

    // Wait for the WS server to process FILE_INFO.
    wait_until(
        || {
            state
                .plugin_snapshot()
                .is_some_and(|(fk, _)| !fk.is_empty())
        },
        common::WAIT_DEADLINE_MS,
        "silent-plugin to register",
    )
    .await;

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
    assert_eq!(
        payload["plugin"]["fileKey"],
        serde_json::json!("silent-plugin"),
        "the timeout shape must name the unresponsive file, got: {payload}"
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
    let (mut plugin_ws, _) = connect_async(format!(
        "ws://127.0.0.1:{}/?token={}",
        ws_addr.port(),
        state.token()
    ))
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

    // Wait for the WS server to process FILE_INFO.
    wait_until(
        || {
            state
                .plugin_snapshot()
                .is_some_and(|(fk, _)| !fk.is_empty())
        },
        common::WAIT_DEADLINE_MS,
        "abc123 to register",
    )
    .await;

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
    let (mut ws1, _) = connect_async(format!(
        "ws://127.0.0.1:{}/?token={}",
        addr.port(),
        state.token()
    ))
    .await
    .expect("first WS connect failed");
    ws1.send(TtMessage::Text(file_info.to_string()))
        .await
        .expect("send FILE_INFO (1)");
    wait_until(
        || {
            state
                .plugin_snapshot()
                .is_some_and(|(fk, _)| !fk.is_empty())
        },
        common::WAIT_DEADLINE_MS,
        "plugin to register on first connect",
    )
    .await;
    assert_eq!(
        state.plugin_snapshot(),
        Some(("F1".to_owned(), "Deck File".to_owned())),
        "plugin must register on first connect"
    );

    // Figma closes: drop the socket. The registry clears.
    drop(ws1);
    wait_until(
        || state.plugin_snapshot().is_none(),
        common::WAIT_DEADLINE_MS,
        "plugin to clear when the socket drops",
    )
    .await;
    assert_eq!(
        state.plugin_snapshot(),
        None,
        "plugin must clear when the socket drops"
    );

    // Figma reopens: a new socket re-registers with no manual steps.
    let (mut ws2, _) = connect_async(format!(
        "ws://127.0.0.1:{}/?token={}",
        addr.port(),
        state.token()
    ))
    .await
    .expect("reconnect WS failed");
    ws2.send(TtMessage::Text(file_info.to_string()))
        .await
        .expect("send FILE_INFO (2)");
    wait_until(
        || {
            state
                .plugin_snapshot()
                .is_some_and(|(fk, _)| !fk.is_empty())
        },
        common::WAIT_DEADLINE_MS,
        "plugin to re-pair after a Figma restart",
    )
    .await;
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
    let (mut ws3, _) = connect_async(format!(
        "ws://127.0.0.1:{}/?token={}",
        addr_b.port(),
        state_b.token()
    ))
    .await
    .expect("connect to restarted daemon failed");
    ws3.send(TtMessage::Text(file_info.to_string()))
        .await
        .expect("send FILE_INFO (3)");
    wait_until(
        || {
            state_b
                .plugin_snapshot()
                .is_some_and(|(fk, _)| !fk.is_empty())
        },
        common::WAIT_DEADLINE_MS,
        "plugin to re-pair after a daemon restart",
    )
    .await;
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
    let (mut plugin_ws, _) = connect_async(format!(
        "ws://127.0.0.1:{}/?token={}",
        ws_addr.port(),
        state.token()
    ))
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

    // Wait for the WS server to process FILE_INFO.
    wait_until(
        || {
            state
                .plugin_snapshot()
                .is_some_and(|(fk, _)| !fk.is_empty())
        },
        common::WAIT_DEADLINE_MS,
        "plugin to register",
    )
    .await;

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
    let (mut plugin_ws, _) = connect_async(format!(
        "ws://127.0.0.1:{}/?token={}",
        ws_addr.port(),
        state.token()
    ))
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

    // Wait for the WS server to process FILE_INFO.
    wait_until(
        || {
            state
                .plugin_snapshot()
                .is_some_and(|(fk, _)| !fk.is_empty())
        },
        common::WAIT_DEADLINE_MS,
        "plugin to register",
    )
    .await;

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

    let (mut plugin_ws, _) = connect_async(format!(
        "ws://127.0.0.1:{}/?token={}",
        ws_addr.port(),
        state.token()
    ))
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

    wait_until(
        || {
            state
                .plugin_snapshot()
                .is_some_and(|(fk, _)| !fk.is_empty())
        },
        common::WAIT_DEADLINE_MS,
        "plugin to register",
    )
    .await;

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
    // w and h are floats: a node's width and height can be fractional.
    assert_eq!(
        payload["w"],
        serde_json::json!(100.0),
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

    let (mut plugin_ws, _) = connect_async(format!(
        "ws://127.0.0.1:{}/?token={}",
        ws_addr.port(),
        state.token()
    ))
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

    wait_until(
        || {
            state
                .plugin_snapshot()
                .is_some_and(|(fk, _)| !fk.is_empty())
        },
        common::WAIT_DEADLINE_MS,
        "plugin to register",
    )
    .await;

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
    let result = turbofig::run_screenshot(
        &state,
        None,
        None,
        1.0,
        None,
        "file",
        Some(tmp.path()),
        1200,
        false,
    )
    .await;

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
    let value = turbofig::run_execute(&state, None, None, "return 1+1;").await;

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

/// run_get_selection returns a clean error when no plugin is connected.
#[tokio::test]
async fn test_run_get_selection_no_plugin_returns_error() {
    let state = Arc::new(turbofig::AppState::new());
    let value = turbofig::run_get_selection(&state, None, None, None, None).await;
    assert_eq!(value["ok"], serde_json::json!(false), "must be ok:false");
    assert_eq!(
        value["error"],
        serde_json::json!("no plugin connected"),
        "error must name the missing plugin, got: {value}"
    );
}

/// run_screenshot returns a clean error when no plugin is connected.
#[tokio::test]
async fn test_run_screenshot_no_plugin_returns_error() {
    let state = Arc::new(turbofig::AppState::new());
    let value =
        turbofig::run_screenshot(&state, None, None, 1.0, None, "file", None, 1200, false).await;
    assert_eq!(value["ok"], serde_json::json!(false), "must be ok:false");
    assert_eq!(
        value["error"],
        serde_json::json!("no plugin connected"),
        "error must name the missing plugin, got: {value}"
    );
}

/// Spawn a WS server and a mock plugin that replies to each SCREENSHOT frame
/// with the given `png` string and a 10x10 size.
async fn spawn_screenshot_plugin(state: Arc<turbofig::AppState>, png: &'static str) {
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

    let (mut plugin_ws, _) = connect_async(format!(
        "ws://127.0.0.1:{}/?token={}",
        ws_addr.port(),
        state.token()
    ))
    .await
    .expect("mock plugin connect");
    plugin_ws
        .send(TtMessage::Text(
            serde_json::json!({"type": "FILE_INFO", "fileKey": "F", "name": "F"}).to_string(),
        ))
        .await
        .expect("send FILE_INFO");
    wait_until(
        || {
            state
                .plugin_snapshot()
                .is_some_and(|(fk, _)| !fk.is_empty())
        },
        common::WAIT_DEADLINE_MS,
        "plugin to register",
    )
    .await;

    tokio::spawn(async move {
        while let Some(Ok(msg)) = plugin_ws.next().await {
            if let TtMessage::Text(text) = msg {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    if json.get("type").and_then(|t| t.as_str()) == Some("SCREENSHOT") {
                        if let Some(id) = json.get("requestId").and_then(|v| v.as_u64()) {
                            let reply = serde_json::json!({
                                "type": "RESULT", "requestId": id, "ok": true,
                                "png": png, "w": 10, "h": 10
                            });
                            let _ = plugin_ws.send(TtMessage::Text(reply.to_string())).await;
                        }
                    }
                }
            }
        }
    });
}

/// run_screenshot reports a clean error when the plugin sends a bad base64 PNG.
#[tokio::test]
async fn test_run_screenshot_invalid_base64_returns_error() {
    let state = Arc::new(turbofig::AppState::new());
    spawn_screenshot_plugin(state.clone(), "!!! not base64 !!!").await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let value = turbofig::run_screenshot(
        &state,
        None,
        None,
        1.0,
        None,
        "file",
        Some(tmp.path()),
        1200,
        false,
    )
    .await;
    assert_eq!(value["ok"], serde_json::json!(false), "must be ok:false");
    assert_eq!(
        value["error"],
        serde_json::json!("invalid base64 png"),
        "error must name the bad base64, got: {value}"
    );
}

/// run_screenshot in file mode reports a clean error when no output dir is set.
#[tokio::test]
async fn test_run_screenshot_file_mode_without_output_dir_returns_error() {
    let state = Arc::new(turbofig::AppState::new());
    spawn_screenshot_plugin(state.clone(), "aGVsbG8=").await;

    let value =
        turbofig::run_screenshot(&state, None, None, 1.0, None, "file", None, 1200, false).await;
    assert_eq!(value["ok"], serde_json::json!(false), "must be ok:false");
    assert_eq!(
        value["error"],
        serde_json::json!("file mode needs an output dir"),
        "error must name the missing output dir, got: {value}"
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
    let (mut plugin_ws, _) = connect_async(format!(
        "ws://127.0.0.1:{}/?token={}",
        ws_addr.port(),
        state.token()
    ))
    .await
    .expect("mock plugin WS connect");
    plugin_ws
        .send(TtMessage::Text(
            serde_json::json!({"type": "FILE_INFO", "fileKey": "F1", "name": "Deck File"})
                .to_string(),
        ))
        .await
        .expect("send FILE_INFO");
    wait_until(
        || {
            state
                .plugin_snapshot()
                .is_some_and(|(fk, _)| !fk.is_empty())
        },
        common::WAIT_DEADLINE_MS,
        "plugin to register",
    )
    .await;

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

    let (mut plugin_ws, _) = connect_async(format!(
        "ws://127.0.0.1:{}/?token={}",
        ws_addr.port(),
        state.token()
    ))
    .await
    .expect("mock plugin WS connect");
    plugin_ws
        .send(TtMessage::Text(
            serde_json::json!({"type": "FILE_INFO", "fileKey": "abc123", "name": "My Design File"})
                .to_string(),
        ))
        .await
        .expect("send FILE_INFO");
    wait_until(
        || {
            state
                .plugin_snapshot()
                .is_some_and(|(fk, _)| !fk.is_empty())
        },
        common::WAIT_DEADLINE_MS,
        "plugin to register",
    )
    .await;

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

/// run_screenshot with a real oversized PNG is downscaled to maxDim correctly.
///
/// Generates a 2000x1000 RgbaImage, encodes it to PNG, base64-encodes it, and
/// feeds it through a mock plugin. Asserts:
///   - default (maxDim=1200, fullRes=false) returns w=1200, h=600 from the decoded image.
///   - fullRes=true returns the original w=2000, h=1000.
#[tokio::test]
async fn test_run_screenshot_real_png_is_downscaled() {
    // Build a 2000x1000 blank image and encode to PNG bytes.
    let img = image::RgbaImage::new(2000, 1000);
    let mut png_bytes: Vec<u8> = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            image::ImageFormat::Png,
        )
        .expect("encode test PNG");
    let png_b64 = B64.encode(&png_bytes);

    // Set up a mock plugin that returns this PNG for every SCREENSHOT request.
    let state = Arc::new(turbofig::AppState::new());
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

    let (mut plugin_ws, _) = connect_async(format!(
        "ws://127.0.0.1:{}/?token={}",
        ws_addr.port(),
        state.token()
    ))
    .await
    .expect("mock plugin connect");
    plugin_ws
        .send(TtMessage::Text(
            serde_json::json!({"type": "FILE_INFO", "fileKey": "F", "name": "F"}).to_string(),
        ))
        .await
        .expect("send FILE_INFO");
    wait_until(
        || {
            state
                .plugin_snapshot()
                .is_some_and(|(fk, _)| !fk.is_empty())
        },
        common::WAIT_DEADLINE_MS,
        "plugin to register",
    )
    .await;

    let b64_for_plugin = png_b64.clone();
    tokio::spawn(async move {
        while let Some(Ok(msg)) = plugin_ws.next().await {
            if let TtMessage::Text(text) = msg {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    if json.get("type").and_then(|t| t.as_str()) == Some("SCREENSHOT") {
                        if let Some(id) = json.get("requestId").and_then(|v| v.as_u64()) {
                            let reply = serde_json::json!({
                                "type": "RESULT", "requestId": id, "ok": true,
                                "png": b64_for_plugin, "w": 2000, "h": 1000
                            });
                            let _ = plugin_ws.send(TtMessage::Text(reply.to_string())).await;
                        }
                    }
                }
            }
        }
    });

    // Default: maxDim=1200, fullRes=false. Longest edge 2000 -> scales to 1200x600.
    let result =
        turbofig::run_screenshot(&state, None, None, 1.0, None, "inline", None, 1200, false).await;
    assert_eq!(
        result["ok"],
        serde_json::json!(true),
        "must succeed with default downscale: {result}"
    );
    assert_eq!(
        result["w"],
        serde_json::json!(1200u64),
        "w must equal maxDim=1200 after downscale: {result}"
    );
    assert_eq!(
        result["h"],
        serde_json::json!(600u64),
        "h must be proportionally halved to 600 after downscale: {result}"
    );

    // fullRes=true: no downscaling; original dims from the decoded image.
    let result_full =
        turbofig::run_screenshot(&state, None, None, 1.0, None, "inline", None, 1200, true).await;
    assert_eq!(
        result_full["ok"],
        serde_json::json!(true),
        "must succeed with fullRes=true: {result_full}"
    );
    assert_eq!(
        result_full["w"],
        serde_json::json!(2000u64),
        "w must be original 2000 for fullRes=true: {result_full}"
    );
    assert_eq!(
        result_full["h"],
        serde_json::json!(1000u64),
        "h must be original 1000 for fullRes=true: {result_full}"
    );
}

/// A WebSocket upgrade request carrying a browser Origin must be rejected
/// before the upgrade completes. A real browser page cannot open this socket
/// and drive the Figma plugin through it.
#[tokio::test]
async fn test_ws_upgrade_with_browser_origin_is_rejected() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral WS port");
    let addr = listener.local_addr().expect("read local addr");
    let state = Arc::new(turbofig::AppState::new());

    tokio::spawn(async move {
        turbofig::serve_ws(listener, state)
            .await
            .expect("serve_ws error in test");
    });

    let mut request = format!("ws://127.0.0.1:{}/", addr.port())
        .into_client_request()
        .expect("build client request");
    request.headers_mut().insert(
        "Origin",
        "https://evil.example".parse().expect("header value"),
    );

    let result = connect_async(request).await;
    assert!(
        result.is_err(),
        "a WS upgrade carrying a browser Origin must be rejected, not upgraded"
    );
}

/// A WebSocket upgrade request with Origin: null (the Figma plugin UI iframe)
/// must be accepted. Guards against the Origin check being too strict.
#[tokio::test]
async fn test_ws_upgrade_with_null_origin_is_accepted() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral WS port");
    let addr = listener.local_addr().expect("read local addr");
    let state = Arc::new(turbofig::AppState::new());
    let token = state.token().to_owned();

    tokio::spawn(async move {
        turbofig::serve_ws(listener, state)
            .await
            .expect("serve_ws error in test");
    });

    let mut request = format!("ws://127.0.0.1:{}/?token={token}", addr.port())
        .into_client_request()
        .expect("build client request");
    request
        .headers_mut()
        .insert("Origin", "null".parse().expect("header value"));

    let result = connect_async(request).await;
    assert!(
        result.is_ok(),
        "a WS upgrade with Origin: null must be accepted, got: {:?}",
        result.err()
    );
}

/// A message larger than the server's 32 MiB cap must not be accepted: the
/// connection drops instead of the daemon buffering an unbounded payload.
#[tokio::test]
async fn test_oversize_ws_message_drops_the_connection() {
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

    let (mut ws, _) = connect_async(format!(
        "ws://127.0.0.1:{}/?token={}",
        addr.port(),
        state.token()
    ))
    .await
    .expect("WS connect failed");

    ws.send(TtMessage::Text(
        serde_json::json!({"type": "FILE_INFO", "fileKey": "big", "name": "Big File"}).to_string(),
    ))
    .await
    .expect("send FILE_INFO");
    wait_until(
        || {
            state
                .plugin_snapshot()
                .is_some_and(|(fk, _)| !fk.is_empty())
        },
        common::WAIT_DEADLINE_MS,
        "big to register",
    )
    .await;
    assert_eq!(
        state.plugin_snapshot(),
        Some(("big".to_owned(), "Big File".to_owned())),
        "plugin must be registered before the oversize send"
    );

    // One text frame well past the 32 MiB server-side cap.
    let huge = "x".repeat(33 * 1024 * 1024);
    // The client may itself error on such a large send, or the server may
    // close the connection after receiving it; either way the registry
    // must not keep a connection that just blew past the size cap.
    let _ = ws.send(TtMessage::Text(huge)).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if state.plugin_snapshot().is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        state.plugin_snapshot(),
        None,
        "an oversize message must drop the connection, not sit buffered forever"
    );
}

/// A reconnect (or a second window) on the same fileKey must not evict the
/// older connection: both stay registered, so a later FILE_INFO from either
/// one (e.g. both re-announcing a file rename) can never knock out the
/// other. `state.list_connections()` is the raw registry view and lists
/// both; routing itself (`connections_named`/`resolve_route`) is what
/// dedupes to the newest, and that is covered in state.rs/routing.rs's own
/// unit tests.
#[tokio::test]
async fn test_reconnect_same_file_key_keeps_both_connections_registered() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral WS port");
    let addr = listener.local_addr().expect("read local addr");
    let state = Arc::new(turbofig::AppState::with_timeout(Duration::from_secs(5)));
    let state_srv = state.clone();

    tokio::spawn(async move {
        turbofig::serve_ws(listener, state_srv)
            .await
            .expect("serve_ws error in test");
    });

    let (mut ws1, _) = connect_async(format!(
        "ws://127.0.0.1:{}/?token={}",
        addr.port(),
        state.token()
    ))
    .await
    .expect("first connect");
    ws1.send(TtMessage::Text(
        serde_json::json!({"type": "FILE_INFO", "fileKey": "dup", "name": "First"}).to_string(),
    ))
    .await
    .expect("send first FILE_INFO");
    wait_until(
        || {
            state
                .list_connections()
                .iter()
                .any(|(_, fk, nm)| fk == "dup" && nm == "First")
        },
        common::WAIT_DEADLINE_MS,
        "first dup connection to register",
    )
    .await;

    let (mut ws2, _) = connect_async(format!(
        "ws://127.0.0.1:{}/?token={}",
        addr.port(),
        state.token()
    ))
    .await
    .expect("second connect");
    ws2.send(TtMessage::Text(
        serde_json::json!({"type": "FILE_INFO", "fileKey": "dup", "name": "Second"}).to_string(),
    ))
    .await
    .expect("send second FILE_INFO");
    wait_until(
        || {
            state
                .list_connections()
                .iter()
                .any(|(_, fk, nm)| fk == "dup" && nm == "Second")
        },
        common::WAIT_DEADLINE_MS,
        "second dup connection to register",
    )
    .await;

    let conns = state.list_connections();
    assert_eq!(
        conns.len(),
        2,
        "both connections sharing a fileKey must stay registered"
    );
    let names: Vec<&str> = conns.iter().map(|(_, _, nm)| nm.as_str()).collect();
    assert!(names.contains(&"First") && names.contains(&"Second"));
    // The routing-facing dedupe (newest conn_id wins) is covered by
    // connections_named's own unit tests in state.rs/routing.rs; it is
    // pub(crate), so not reachable from this integration test.

    // The old socket is still physically open; make sure it does not panic
    // anything when it later closes.
    drop(ws1);
    drop(ws2);
    wait_until(
        || state.list_connections().is_empty(),
        common::WAIT_DEADLINE_MS,
        "both dup connections to deregister after close",
    )
    .await;
}

// ── pairing token tests ──────────────────────────────────────────────────────

/// A WS upgrade with no `token` query parameter at all is rejected with 401.
#[tokio::test]
async fn test_ws_upgrade_with_no_token_is_rejected() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral WS port");
    let addr = listener.local_addr().expect("read local addr");
    let state = Arc::new(turbofig::AppState::new());

    tokio::spawn(async move {
        turbofig::serve_ws(listener, state)
            .await
            .expect("serve_ws error in test");
    });

    let result = connect_async(format!("ws://127.0.0.1:{}/", addr.port())).await;
    assert!(
        result.is_err(),
        "a WS upgrade with no token must be rejected"
    );
}

/// A WS upgrade with a wrong (but correctly sized) token is rejected with 401.
#[tokio::test]
async fn test_ws_upgrade_with_wrong_token_is_rejected() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral WS port");
    let addr = listener.local_addr().expect("read local addr");
    let state = Arc::new(turbofig::AppState::new());
    let wrong = "0".repeat(state.token().len());

    tokio::spawn(async move {
        turbofig::serve_ws(listener, state)
            .await
            .expect("serve_ws error in test");
    });

    let result = connect_async(format!("ws://127.0.0.1:{}/?token={wrong}", addr.port())).await;
    assert!(
        result.is_err(),
        "a WS upgrade with a wrong token must be rejected"
    );
}

/// A WS upgrade with a token of the wrong length is rejected with 401.
#[tokio::test]
async fn test_ws_upgrade_with_wrong_length_token_is_rejected() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral WS port");
    let addr = listener.local_addr().expect("read local addr");
    let state = Arc::new(turbofig::AppState::new());

    tokio::spawn(async move {
        turbofig::serve_ws(listener, state)
            .await
            .expect("serve_ws error in test");
    });

    let result = connect_async(format!("ws://127.0.0.1:{}/?token=short", addr.port())).await;
    assert!(
        result.is_err(),
        "a WS upgrade with a wrong-length token must be rejected"
    );
}

/// A WS upgrade with the correct token succeeds.
#[tokio::test]
async fn test_ws_upgrade_with_correct_token_is_accepted() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral WS port");
    let addr = listener.local_addr().expect("read local addr");
    let state = Arc::new(turbofig::AppState::new());
    let token = state.token().to_owned();

    tokio::spawn(async move {
        turbofig::serve_ws(listener, state)
            .await
            .expect("serve_ws error in test");
    });

    let result = connect_async(format!("ws://127.0.0.1:{}/?token={token}", addr.port())).await;
    assert!(
        result.is_ok(),
        "a WS upgrade with the correct token must be accepted, got: {:?}",
        result.err()
    );
}

/// The pairing token never appears in turbofig_status output, even when a
/// plugin is connected. The daemon has no HTTP /health endpoint; status is
/// the equivalent surface to check.
#[tokio::test]
async fn test_turbofig_status_never_includes_the_token() {
    let state = Arc::new(turbofig::AppState::new());
    let token = state.token().to_owned();

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

    let (mut plugin_ws, _) =
        connect_async(format!("ws://127.0.0.1:{}/?token={token}", ws_addr.port()))
            .await
            .expect("mock plugin WS connect failed");

    let fi =
        serde_json::json!({"type": "FILE_INFO", "fileKey": "abc123", "name": "My Design File"});
    plugin_ws
        .send(TtMessage::Text(fi.to_string()))
        .await
        .expect("send FILE_INFO");
    wait_until(
        || {
            state
                .plugin_snapshot()
                .is_some_and(|(fk, _)| !fk.is_empty())
        },
        common::WAIT_DEADLINE_MS,
        "plugin to register",
    )
    .await;

    let status = turbofig::run_status(&state, None, None).await;
    let status_str = status.to_string();
    assert!(
        !status_str.contains(&token),
        "turbofig_status output must never contain the pairing token, got: {status_str}"
    );
}
