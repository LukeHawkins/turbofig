//! End-to-end tests for the Phase 3 design loop over both transports.
//!
//! The loop is: create a frame (execute), read the selection (get_selection),
//! then export a screenshot. These tests run the whole loop over the file
//! bridge and again over the MCP HTTP endpoint (the curl transport), against
//! one stateful mock plugin. They prove both transports reach the same shared
//! `run_*` functions and return the same shaped results.
//!
//! The live-file check is manual: run the daemon, import the plugin into a real
//! Figma file, then drive the same three ops. These tests cover the routing and
//! the result shapes without a live Figma.

use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
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

/// Poll for a file to appear, up to `deadline_ms` milliseconds.
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
    tokio::fs::write(inbox.join(format!("{id}.json")), body.to_string())
        .await
        .expect("write job");
    dir.path().join("outbox").join(format!("{id}.json"))
}

/// Bind an ephemeral WS listener, spawn serve_ws, and return its address.
async fn spawn_ws(state: Arc<turbofig::AppState>) -> SocketAddr {
    let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind WS port");
    let ws_addr = ws_listener.local_addr().expect("ws local addr");
    tokio::spawn(async move {
        turbofig::serve_ws(ws_listener, state)
            .await
            .expect("serve_ws error");
    });
    ws_addr
}

/// Connect a stateful mock plugin. It models the design loop:
///   EXECUTE        -> creates a frame, returns `{id:"1:5", type:"FRAME"}`.
///   GET_SELECTION  -> returns that frame as the one selected node.
///   SCREENSHOT     -> returns a base64 PNG (bytes "hello").
async fn spawn_design_plugin(ws_addr: SocketAddr) {
    let (mut plugin_ws, _) = connect_async(format!("ws://127.0.0.1:{}/", ws_addr.port()))
        .await
        .expect("mock plugin connect");
    plugin_ws
        .send(TtMessage::Text(
            serde_json::json!({"type": "FILE_INFO", "fileKey": "F1", "name": "Design File"})
                .to_string(),
        ))
        .await
        .expect("send FILE_INFO");
    tokio::time::sleep(Duration::from_millis(50)).await;

    tokio::spawn(async move {
        while let Some(Ok(msg)) = plugin_ws.next().await {
            if let TtMessage::Text(text) = msg {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    let ty = json.get("type").and_then(|t| t.as_str());
                    let Some(id) = json.get("requestId").and_then(|v| v.as_u64()) else {
                        continue;
                    };
                    let reply = match ty {
                        Some("EXECUTE") => serde_json::json!({
                            "type": "RESULT", "requestId": id, "ok": true,
                            "result": {"id": "1:5", "type": "FRAME"}
                        }),
                        Some("GET_SELECTION") => serde_json::json!({
                            "type": "RESULT", "requestId": id, "ok": true,
                            "selection": [{"id": "1:5", "name": "Frame 1", "type": "FRAME",
                                           "x": 0, "y": 0, "w": 100, "h": 100}]
                        }),
                        Some("SCREENSHOT") => serde_json::json!({
                            "type": "RESULT", "requestId": id, "ok": true,
                            "png": "aGVsbG8=", "w": 100, "h": 100
                        }),
                        _ => continue,
                    };
                    let _ = plugin_ws.send(TtMessage::Text(reply.to_string())).await;
                }
            }
        }
    });
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// The full create -> select -> screenshot loop works over the file bridge.
#[tokio::test]
async fn test_full_loop_over_bridge() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(turbofig::AppState::new());

    let ws_addr = spawn_ws(state.clone()).await;
    spawn_design_plugin(ws_addr).await;
    tokio::spawn({
        let state = state.clone();
        let dir = tmp.path().to_path_buf();
        async move {
            turbofig::serve_bridge(state, dir)
                .await
                .expect("serve_bridge error");
        }
    });

    // 1. Create a frame.
    let out = write_job(
        &tmp,
        "loop_exec",
        serde_json::json!({"op": "execute", "code": "return figma.createFrame().id;"}),
    )
    .await;
    let exec: serde_json::Value =
        serde_json::from_str(&poll_file(&out, 2000).await).expect("valid JSON");
    assert_eq!(exec["ok"], serde_json::json!(true), "execute must succeed");
    assert_eq!(
        exec["result"]["id"],
        serde_json::json!("1:5"),
        "execute must return the new frame id, got: {exec}"
    );

    // 2. Read the selection.
    let out = write_job(&tmp, "loop_sel", serde_json::json!({"op": "get_selection"})).await;
    let sel: serde_json::Value =
        serde_json::from_str(&poll_file(&out, 2000).await).expect("valid JSON");
    assert_eq!(
        sel["ok"],
        serde_json::json!(true),
        "get_selection must succeed"
    );
    assert_eq!(
        sel["selection"][0]["id"],
        serde_json::json!("1:5"),
        "selection must carry the created frame, got: {sel}"
    );

    // 3. Get a screenshot back (file mode writes a PNG to the outbox).
    let out = write_job(&tmp, "loop_shot", serde_json::json!({"op": "screenshot"})).await;
    let shot: serde_json::Value =
        serde_json::from_str(&poll_file(&out, 2000).await).expect("valid JSON");
    assert_eq!(
        shot["ok"],
        serde_json::json!(true),
        "screenshot must succeed"
    );
    let path = shot["path"]
        .as_str()
        .unwrap_or_else(|| panic!("screenshot must carry a path, got: {shot}"));
    let bytes = tokio::fs::read(path).await.expect("read screenshot png");
    assert_eq!(bytes, b"hello", "the PNG file must hold the decoded bytes");
}

/// The same loop works over the MCP HTTP endpoint (the curl transport).
#[tokio::test]
async fn test_full_loop_over_http_mcp() {
    let state = Arc::new(turbofig::AppState::new());

    let ws_addr = spawn_ws(state.clone()).await;
    let http_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind HTTP port");
    let http_addr = http_listener.local_addr().expect("http local addr");
    tokio::spawn({
        let state = state.clone();
        async move {
            turbofig::serve_with_state(http_listener, state)
                .await
                .expect("serve_with_state error");
        }
    });
    spawn_design_plugin(ws_addr).await;

    let base_url = format!("http://{http_addr}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("build reqwest client");

    // Handshake: initialize then notifications/initialized.
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

    // A small helper to call a tool and return the parsed tool payload.
    let call_tool = |name: &'static str, args: serde_json::Value, id: u64| {
        let client = client.clone();
        let base_url = base_url.clone();
        let session_id = session_id.clone();
        async move {
            let res = client
                .post(format!("{base_url}/mcp"))
                .header("Accept", "application/json, text/event-stream")
                .header("Content-Type", "application/json")
                .header("mcp-session-id", &session_id)
                .json(&serde_json::json!({
                    "jsonrpc": "2.0", "id": id, "method": "tools/call",
                    "params": {"name": name, "arguments": args}
                }))
                .send()
                .await
                .expect("tools/call");
            assert!(res.status().is_success(), "tools/call must return HTTP 200");
            let body = res.text().await.expect("read tool body");
            let text = parse_sse_data(&body)["result"]["content"][0]["text"]
                .as_str()
                .expect("tool text is a string")
                .to_owned();
            serde_json::from_str::<serde_json::Value>(&text).expect("tool text is JSON")
        }
    };

    // 1. Create a frame.
    let exec = call_tool(
        "turbofig_execute",
        serde_json::json!({"code": "return figma.createFrame().id;"}),
        2,
    )
    .await;
    assert_eq!(exec["ok"], serde_json::json!(true), "execute must succeed");
    assert_eq!(
        exec["result"]["id"],
        serde_json::json!("1:5"),
        "execute must return the new frame id, got: {exec}"
    );

    // 2. Read the selection.
    let sel = call_tool("turbofig_get_selection", serde_json::json!({}), 3).await;
    assert_eq!(
        sel["ok"],
        serde_json::json!(true),
        "get_selection must succeed"
    );
    assert_eq!(
        sel["selection"][0]["id"],
        serde_json::json!("1:5"),
        "selection must carry the created frame, got: {sel}"
    );

    // 3. Get a screenshot back inline (no outbox dir in this MCP-only test).
    let shot = call_tool(
        "turbofig_screenshot",
        serde_json::json!({"return": "inline"}),
        4,
    )
    .await;
    assert_eq!(
        shot["ok"],
        serde_json::json!(true),
        "screenshot must succeed"
    );
    assert_eq!(
        shot["png"],
        serde_json::json!("aGVsbG8="),
        "inline screenshot must carry the base64 PNG, got: {shot}"
    );
}

// ── fields/depth forwarding tests ────────────────────────────────────────────

/// Spawn a mock plugin that, on GET_SELECTION, echoes the fields and depth it
/// received back inside the result so the test can assert the daemon forwarded them.
async fn spawn_echo_plugin(ws_addr: SocketAddr) {
    let (mut plugin_ws, _) = connect_async(format!("ws://127.0.0.1:{}/", ws_addr.port()))
        .await
        .expect("mock echo plugin connect");
    plugin_ws
        .send(TtMessage::Text(
            serde_json::json!({"type": "FILE_INFO", "fileKey": "Echo", "name": "Echo File"})
                .to_string(),
        ))
        .await
        .expect("send FILE_INFO");
    tokio::time::sleep(Duration::from_millis(50)).await;

    tokio::spawn(async move {
        while let Some(Ok(msg)) = plugin_ws.next().await {
            if let TtMessage::Text(text) = msg {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    let Some(id) = json.get("requestId").and_then(|v| v.as_u64()) else {
                        continue;
                    };
                    if json.get("type").and_then(|t| t.as_str()) == Some("GET_SELECTION") {
                        // Echo back the fields and depth the daemon sent.
                        let received_fields = json
                            .get("fields")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        let received_depth = json
                            .get("depth")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        let reply = serde_json::json!({
                            "type": "RESULT", "requestId": id, "ok": true,
                            "selection": [{
                                "id": "echo",
                                "received_fields": received_fields,
                                "received_depth": received_depth
                            }]
                        });
                        let _ = plugin_ws.send(TtMessage::Text(reply.to_string())).await;
                    }
                }
            }
        }
    });
}

/// GET_SELECTION with no fields/depth: the daemon sends neither key to the plugin.
/// The default path (compact shape) must still succeed.
#[tokio::test]
async fn test_get_selection_default_no_fields_no_depth() {
    let state = Arc::new(turbofig::AppState::new());
    let ws_addr = spawn_ws(state.clone()).await;
    spawn_echo_plugin(ws_addr).await;

    let result = turbofig::run_get_selection(&state, None, None, None, None).await;
    assert_eq!(
        result["ok"],
        serde_json::json!(true),
        "must succeed: {result}"
    );
    // Neither field was sent, so the plugin echoed null for both.
    assert_eq!(
        result["selection"][0]["received_fields"],
        serde_json::json!(null),
        "no fields forwarded when None: {result}"
    );
    assert_eq!(
        result["selection"][0]["received_depth"],
        serde_json::json!(null),
        "no depth forwarded when None: {result}"
    );
}

/// GET_SELECTION with fields and depth: the daemon forwards both to the plugin.
#[tokio::test]
async fn test_get_selection_fields_and_depth_forwarded() {
    let state = Arc::new(turbofig::AppState::new());
    let ws_addr = spawn_ws(state.clone()).await;
    spawn_echo_plugin(ws_addr).await;

    let fields = vec!["opacity".to_owned(), "visible".to_owned()];
    let result = turbofig::run_get_selection(&state, None, None, Some(&fields), Some(2)).await;
    assert_eq!(
        result["ok"],
        serde_json::json!(true),
        "must succeed: {result}"
    );
    assert_eq!(
        result["selection"][0]["received_fields"],
        serde_json::json!(["opacity", "visible"]),
        "fields must be forwarded to the plugin: {result}"
    );
    assert_eq!(
        result["selection"][0]["received_depth"],
        serde_json::json!(2),
        "depth must be forwarded to the plugin: {result}"
    );
}

/// Spawn a mock plugin whose EXECUTE reply carries a large `result` string
/// (well over the 20 000-byte read budget) so the budget warning fires.
/// The small-result variant reuses spawn_design_plugin, which returns a small object.
async fn spawn_large_result_plugin(ws_addr: SocketAddr) {
    let (mut plugin_ws, _) = connect_async(format!("ws://127.0.0.1:{}/", ws_addr.port()))
        .await
        .expect("mock large-result plugin connect");
    plugin_ws
        .send(TtMessage::Text(
            serde_json::json!({"type": "FILE_INFO", "fileKey": "Big", "name": "Big File"})
                .to_string(),
        ))
        .await
        .expect("send FILE_INFO");
    tokio::time::sleep(Duration::from_millis(50)).await;

    tokio::spawn(async move {
        while let Some(Ok(msg)) = plugin_ws.next().await {
            if let TtMessage::Text(text) = msg {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    let Some(id) = json.get("requestId").and_then(|v| v.as_u64()) else {
                        continue;
                    };
                    if json.get("type").and_then(|t| t.as_str()) == Some("EXECUTE") {
                        // Return a result string that is well over 20 000 bytes.
                        let big_string = "x".repeat(25_000);
                        let reply = serde_json::json!({
                            "type": "RESULT", "requestId": id, "ok": true,
                            "result": big_string
                        });
                        let _ = plugin_ws.send(TtMessage::Text(reply.to_string())).await;
                    }
                }
            }
        }
    });
}

/// A large execute result triggers a "warning" field in the response.
#[tokio::test]
async fn test_execute_large_result_has_warning() {
    let state = Arc::new(turbofig::AppState::new());
    let ws_addr = spawn_ws(state.clone()).await;
    spawn_large_result_plugin(ws_addr).await;

    let result = turbofig::run_execute(&state, None, None, "any code").await;
    assert_eq!(
        result["ok"],
        serde_json::json!(true),
        "must succeed: {result}"
    );
    assert!(
        result.get("warning").and_then(|v| v.as_str()).is_some(),
        "large result must carry a warning field, got: {result}"
    );
    let w = result["warning"].as_str().unwrap();
    assert!(
        w.contains("fields") || w.contains("file"),
        "warning must name a remedy: {w}"
    );
}

/// A small execute result (the normal design-loop result) has no "warning" field.
#[tokio::test]
async fn test_execute_small_result_has_no_warning() {
    let state = Arc::new(turbofig::AppState::new());
    let ws_addr = spawn_ws(state.clone()).await;
    // spawn_design_plugin returns {"id": "1:5", "type": "FRAME"} — well under budget.
    spawn_design_plugin(ws_addr).await;

    let result = turbofig::run_execute(&state, None, None, "return figma.createFrame().id;").await;
    assert_eq!(
        result["ok"],
        serde_json::json!(true),
        "must succeed: {result}"
    );
    assert!(
        result.get("warning").is_none(),
        "small result must not carry a warning field, got: {result}"
    );
}

/// Depth clamps to 5 at the daemon before reaching the plugin.
#[tokio::test]
async fn test_get_selection_depth_clamps_at_daemon() {
    let state = Arc::new(turbofig::AppState::new());
    let ws_addr = spawn_ws(state.clone()).await;
    spawn_echo_plugin(ws_addr).await;

    let result = turbofig::run_get_selection(&state, None, None, None, Some(99)).await;
    assert_eq!(
        result["ok"],
        serde_json::json!(true),
        "must succeed: {result}"
    );
    assert_eq!(
        result["selection"][0]["received_depth"],
        serde_json::json!(5),
        "daemon must clamp depth 99 to 5: {result}"
    );
}

// ── get_selection budget warning tests ────────────────────────────────────────

/// Spawn a mock plugin whose GET_SELECTION reply carries a selection payload
/// well over the 20 000-byte read budget so the warning fires.
async fn spawn_large_selection_plugin(ws_addr: SocketAddr) {
    let (mut plugin_ws, _) = connect_async(format!("ws://127.0.0.1:{}/", ws_addr.port()))
        .await
        .expect("mock large-selection plugin connect");
    plugin_ws
        .send(TtMessage::Text(
            serde_json::json!({"type": "FILE_INFO", "fileKey": "BigSel", "name": "Big Selection File"})
                .to_string(),
        ))
        .await
        .expect("send FILE_INFO");
    tokio::time::sleep(Duration::from_millis(50)).await;

    tokio::spawn(async move {
        while let Some(Ok(msg)) = plugin_ws.next().await {
            if let TtMessage::Text(text) = msg {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    let Some(id) = json.get("requestId").and_then(|v| v.as_u64()) else {
                        continue;
                    };
                    if json.get("type").and_then(|t| t.as_str()) == Some("GET_SELECTION") {
                        // Build a selection payload well over 20 000 bytes.
                        // Each item carries a long name string to inflate the JSON.
                        let long_name = "n".repeat(2_000);
                        let items: Vec<serde_json::Value> = (0..15)
                            .map(|i| {
                                serde_json::json!({
                                    "id": format!("1:{i}"),
                                    "name": long_name,
                                    "type": "FRAME",
                                    "x": 0, "y": 0, "w": 100, "h": 100
                                })
                            })
                            .collect();
                        let reply = serde_json::json!({
                            "type": "RESULT", "requestId": id, "ok": true,
                            "selection": items
                        });
                        let _ = plugin_ws.send(TtMessage::Text(reply.to_string())).await;
                    }
                }
            }
        }
    });
}

/// A large get_selection result triggers a "warning" field in the response.
#[tokio::test]
async fn test_get_selection_large_result_has_warning() {
    let state = Arc::new(turbofig::AppState::new());
    let ws_addr = spawn_ws(state.clone()).await;
    spawn_large_selection_plugin(ws_addr).await;

    let result = turbofig::run_get_selection(&state, None, None, None, None).await;
    assert_eq!(
        result["ok"],
        serde_json::json!(true),
        "must succeed: {result}"
    );
    assert!(
        result.get("warning").and_then(|v| v.as_str()).is_some(),
        "large selection must carry a warning field, got: {result}"
    );
    let w = result["warning"].as_str().unwrap();
    assert!(
        w.contains("fields") || w.contains("file"),
        "warning must name a remedy: {w}"
    );
}

/// A small get_selection result (the normal design-loop result) has no "warning" field.
#[tokio::test]
async fn test_get_selection_small_result_has_no_warning() {
    let state = Arc::new(turbofig::AppState::new());
    let ws_addr = spawn_ws(state.clone()).await;
    // spawn_design_plugin returns a single small item - well under the 20 000-byte budget.
    spawn_design_plugin(ws_addr).await;

    let result = turbofig::run_get_selection(&state, None, None, None, None).await;
    assert_eq!(
        result["ok"],
        serde_json::json!(true),
        "must succeed: {result}"
    );
    assert!(
        result.get("warning").is_none(),
        "small selection must not carry a warning field, got: {result}"
    );
}

// ── inline screenshot budget warning tests ─────────────────────────────────────

/// Spawn a mock plugin that replies to SCREENSHOT with a large base64 string.
///
/// The string is valid base64 but not a valid PNG. The downscale pass-through
/// returns the same bytes; re-encoding them gives a base64 string well over the
/// 100 000-byte inline budget so the warning fires.
async fn spawn_large_screenshot_plugin(ws_addr: SocketAddr) {
    let (mut plugin_ws, _) = connect_async(format!("ws://127.0.0.1:{}/", ws_addr.port()))
        .await
        .expect("mock large-screenshot plugin connect");
    plugin_ws
        .send(TtMessage::Text(
            serde_json::json!({"type": "FILE_INFO", "fileKey": "BigShot", "name": "Big Screenshot File"})
                .to_string(),
        ))
        .await
        .expect("send FILE_INFO");
    tokio::time::sleep(Duration::from_millis(50)).await;

    tokio::spawn(async move {
        while let Some(Ok(msg)) = plugin_ws.next().await {
            if let TtMessage::Text(text) = msg {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    let Some(id) = json.get("requestId").and_then(|v| v.as_u64()) else {
                        continue;
                    };
                    if json.get("type").and_then(|t| t.as_str()) == Some("SCREENSHOT") {
                        // 135 000 'A' chars is valid base64. It decodes to ~101 250 bytes
                        // of null data, which is not a valid PNG. The daemon pass-through
                        // re-encodes those bytes back to ~135 000 base64 chars, well over
                        // the 100 000-byte inline budget.
                        let large_b64 = "A".repeat(135_000);
                        let reply = serde_json::json!({
                            "type": "RESULT", "requestId": id, "ok": true,
                            "png": large_b64, "w": 10, "h": 10
                        });
                        let _ = plugin_ws.send(TtMessage::Text(reply.to_string())).await;
                    }
                }
            }
        }
    });
}

/// A large inline screenshot triggers a "warning" field in the response.
#[tokio::test]
async fn test_screenshot_inline_large_has_warning() {
    let state = Arc::new(turbofig::AppState::new());
    let ws_addr = spawn_ws(state.clone()).await;
    spawn_large_screenshot_plugin(ws_addr).await;

    let result =
        turbofig::run_screenshot(&state, None, None, 1.0, None, "inline", None, 1200, false).await;
    assert_eq!(
        result["ok"],
        serde_json::json!(true),
        "must succeed: {result}"
    );
    assert!(
        result.get("warning").and_then(|v| v.as_str()).is_some(),
        "large inline screenshot must carry a warning field, got: {result}"
    );
    let w = result["warning"].as_str().unwrap();
    assert!(
        w.contains("file") || w.contains("subagent"),
        "warning must name a remedy: {w}"
    );
}

/// A small inline screenshot (the normal case) has no "warning" field.
#[tokio::test]
async fn test_screenshot_inline_small_has_no_warning() {
    let state = Arc::new(turbofig::AppState::new());
    let ws_addr = spawn_ws(state.clone()).await;
    // spawn_design_plugin returns "aGVsbG8=" (b"hello" base64) - well under budget.
    spawn_design_plugin(ws_addr).await;

    let result =
        turbofig::run_screenshot(&state, None, None, 1.0, None, "inline", None, 1200, false).await;
    assert_eq!(
        result["ok"],
        serde_json::json!(true),
        "must succeed: {result}"
    );
    assert!(
        result.get("warning").is_none(),
        "small inline screenshot must not carry a warning field, got: {result}"
    );
}
