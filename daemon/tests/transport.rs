//! Integration tests for the MCP streamable-HTTP transport.
//!
//! Each test binds an ephemeral port, starts the real daemon router, and
//! exercises the wire contract with a plain HTTP client.  No port 18846 is
//! ever hardcoded here.

use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

// ── helpers ──────────────────────────────────────────────────────────────────

/// Bind an ephemeral port, spawn the server, and return the base URL.
async fn start_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("read local addr");
    tokio::spawn(async move {
        turbofig::serve(listener)
            .await
            .expect("server error in test");
    });
    format!("http://{addr}")
}

/// Build a reqwest client with a generous timeout so the test never hangs.
fn make_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("build reqwest client")
}

/// POST to /mcp with the required MCP headers.
/// Attach an mcp-session-id header when `session_id` is Some.
async fn post_mcp(
    client: &reqwest::Client,
    base_url: &str,
    body: Value,
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

/// Find the first non-empty `data:` line in an SSE body and parse it as JSON.
///
/// The SSE stream may include empty priming lines (`data: ` with no content)
/// before the actual event.  This function skips those and returns the first
/// line that carries JSON content.
///
/// Panics when no non-empty data line is found.
fn parse_sse_data(body: &str) -> Value {
    for line in body.lines() {
        // SSE data lines may include or omit the space after the colon.
        let data = if let Some(d) = line.strip_prefix("data: ") {
            d
        } else if let Some(d) = line.strip_prefix("data:") {
            d
        } else {
            continue;
        };
        if data.is_empty() {
            // Skip priming events: the server sends empty data lines to
            // prime the SSE stream before the first real event.
            continue;
        }
        return serde_json::from_str(data)
            .unwrap_or_else(|e| panic!("SSE data line is not valid JSON ({e}):\n{data}"));
    }
    panic!("No non-empty 'data:' line found in SSE body:\n{body}");
}

// ── integration test ──────────────────────────────────────────────────────────

/// Full MCP handshake: initialize → notifications/initialized → tools/call.
///
/// Also verifies statefulness: a tools/call without a session ID is rejected.
#[tokio::test]
async fn test_mcp_handshake_and_turbofig_status() {
    let base_url = start_server().await;
    let client = make_client();

    // ── step 1: initialize ────────────────────────────────────────────────────
    // The server creates a session and returns its ID in the mcp-session-id
    // response header.
    let init_res = post_mcp(
        &client,
        &base_url,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "clientInfo": {"name": "test-client", "version": "0.1.0"},
                "capabilities": {}
            }
        }),
        None,
    )
    .await;

    assert!(
        init_res.status().is_success(),
        "initialize should succeed, got HTTP {}",
        init_res.status()
    );

    let session_id = init_res
        .headers()
        .get("mcp-session-id")
        .expect("initialize response must carry mcp-session-id header")
        .to_str()
        .expect("mcp-session-id header is valid UTF-8")
        .to_owned();
    assert!(!session_id.is_empty(), "mcp-session-id must not be empty");

    // Parse the init body and assert the server advertises the tools capability.
    let init_body = init_res.text().await.expect("read init body");
    let init_msg = parse_sse_data(&init_body);
    assert!(
        !init_msg["result"]["capabilities"]["tools"].is_null(),
        "initialize result must advertise the tools capability, got: {init_msg}"
    );

    // ── step 2: notifications/initialized ────────────────────────────────────
    // The server acknowledges the handshake.  A 200 or 202 is both acceptable.
    let notif_res = post_mcp(
        &client,
        &base_url,
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }),
        Some(&session_id),
    )
    .await;

    assert!(
        notif_res.status().is_success(),
        "notifications/initialized should succeed, got HTTP {}",
        notif_res.status()
    );
    let _notif_body = notif_res.text().await.expect("read notif body");

    // ── step 3: tools/call turbofig_status ───────────────────────────────────
    // The response is an SSE stream.  The first data: line is the JSON-RPC
    // response object whose result.content[0] holds the tool output.
    let tools_res = post_mcp(
        &client,
        &base_url,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "turbofig_status",
                "arguments": {}
            }
        }),
        Some(&session_id),
    )
    .await;

    assert!(
        tools_res.status().is_success(),
        "tools/call should succeed, got HTTP {}",
        tools_res.status()
    );

    let tools_body = tools_res.text().await.expect("read tools/call body");
    assert!(
        tools_body.lines().any(|l| l.starts_with("data:")),
        "tools/call response must be SSE (contain at least one 'data:' line)"
    );

    let msg = parse_sse_data(&tools_body);
    let content = &msg["result"]["content"];
    assert!(
        content.is_array(),
        "result.content must be an array, got: {content}"
    );
    let first = &content[0];
    assert_eq!(
        first["type"], "text",
        "content[0].type must be 'text', got: {first}"
    );
    let text_str = first["text"]
        .as_str()
        .expect("content[0].text must be a string");
    let text: Value = serde_json::from_str(text_str).expect("content[0].text must be valid JSON");
    // "ok":true is the daemon-liveness contract. The response also carries a
    // "plugin" object, but we only assert the liveness field here so this test
    // remains valid with or without a live plugin.
    assert_eq!(
        text["ok"],
        json!(true),
        "turbofig_status must have ok:true, got: {text}"
    );
    assert_eq!(
        text["plugin"]["connected"],
        json!(false),
        "turbofig_status must report plugin.connected:false when no plugin is attached, got: {text}"
    );

    // ── step 4: statefulness: no session ID must be rejected ──────────────────
    // With legacy_session_mode: true, every non-initialize request must carry
    // a valid mcp-session-id.  A missing header causes the server to treat the
    // request as a new session attempt; only initialize is accepted in that
    // path, so tools/call returns a non-success HTTP status.
    let no_session_res = post_mcp(
        &client,
        &base_url,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "turbofig_status",
                "arguments": {}
            }
        }),
        None,
    )
    .await;

    assert!(
        !no_session_res.status().is_success(),
        "tools/call without mcp-session-id must be rejected, got HTTP {}",
        no_session_res.status()
    );
}

/// tools/list must return exactly one tool named turbofig_status.
///
/// This guards the locked 4-tool surface defined in CLAUDE.md.
/// The surface currently has one implemented tool; the count must not grow
/// without a deliberate PLAN.md update.
#[tokio::test]
async fn test_tools_list_has_exactly_turbofig_status() {
    let base_url = start_server().await;
    let client = make_client();

    // Step 1: initialize to get a session ID.
    let init_res = post_mcp(
        &client,
        &base_url,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "clientInfo": {"name": "test-client", "version": "0.1.0"},
                "capabilities": {}
            }
        }),
        None,
    )
    .await;

    assert!(
        init_res.status().is_success(),
        "initialize should succeed, got HTTP {}",
        init_res.status()
    );

    let session_id = init_res
        .headers()
        .get("mcp-session-id")
        .expect("initialize response must carry mcp-session-id header")
        .to_str()
        .expect("mcp-session-id header is valid UTF-8")
        .to_owned();

    let init_body = init_res.text().await.expect("read init body");
    let _ = parse_sse_data(&init_body); // drain SSE

    // Step 2: notifications/initialized.
    let notif_res = post_mcp(
        &client,
        &base_url,
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }),
        Some(&session_id),
    )
    .await;
    assert!(
        notif_res.status().is_success(),
        "notifications/initialized should succeed, got HTTP {}",
        notif_res.status()
    );
    let _ = notif_res.text().await.expect("drain notif body");

    // Step 3: tools/list — must return exactly one tool named turbofig_status.
    let list_res = post_mcp(
        &client,
        &base_url,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }),
        Some(&session_id),
    )
    .await;

    assert!(
        list_res.status().is_success(),
        "tools/list should succeed, got HTTP {}",
        list_res.status()
    );

    let list_body = list_res.text().await.expect("read tools/list body");
    let msg = parse_sse_data(&list_body);
    let tools = &msg["result"]["tools"];
    assert!(
        tools.is_array(),
        "result.tools must be an array, got: {msg}"
    );
    let tools_arr = tools.as_array().unwrap();
    assert_eq!(
        tools_arr.len(),
        1,
        "tool surface must be exactly 1 tool, got {}: {:?}",
        tools_arr.len(),
        tools_arr
    );
    assert_eq!(
        tools_arr[0]["name"], "turbofig_status",
        "the only tool must be turbofig_status, got: {}",
        tools_arr[0]["name"]
    );
}

/// A bogus session ID must also be rejected with a non-success status.
///
/// Per the MCP spec, the server returns HTTP 404 for unknown sessions.
#[tokio::test]
async fn test_bogus_session_id_is_rejected() {
    let base_url = start_server().await;
    let client = make_client();

    let res = post_mcp(
        &client,
        &base_url,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "turbofig_status",
                "arguments": {}
            }
        }),
        Some("bogus-session-id-that-does-not-exist"),
    )
    .await;

    assert!(
        !res.status().is_success(),
        "tools/call with bogus session ID must be rejected, got HTTP {}",
        res.status()
    );
    assert_eq!(
        res.status(),
        reqwest::StatusCode::NOT_FOUND,
        "server must return 404 for unknown session IDs per MCP spec"
    );
}

/// A client that disconnects mid-request must not stop the daemon.
///
/// The daemon is an independent service. A client teardown or a session end
/// must never take it down. This is the direct fix for the old supergateway
/// SIGTERM-mid-job crash: the daemon owns its own lifecycle, decoupled from
/// any client connection.
#[tokio::test]
async fn test_client_disconnect_does_not_stop_daemon() {
    let base_url = start_server().await;
    let addr = base_url
        .strip_prefix("http://")
        .expect("base_url has http prefix")
        .to_owned();

    // Simulate an abrupt client disconnect mid-request: open a TCP connection,
    // write a partial HTTP request, then drop it without finishing or reading.
    {
        let mut stream = tokio::net::TcpStream::connect(&addr)
            .await
            .expect("connect raw TCP");
        stream
            .write_all(b"POST /mcp HTTP/1.1\r\nHost: localhost\r\n")
            .await
            .expect("write partial request");
        // Drop the stream here without completing the request.
    }

    // The daemon must still serve. A fresh client completes a full handshake
    // and a tools/call that returns ok:true.
    let client = make_client();
    let init_res = post_mcp(
        &client,
        &base_url,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "clientInfo": {"name": "test-client", "version": "0.1.0"},
                "capabilities": {}
            }
        }),
        None,
    )
    .await;
    assert!(
        init_res.status().is_success(),
        "daemon must still serve after a client disconnect, got HTTP {}",
        init_res.status()
    );

    let session_id = init_res
        .headers()
        .get("mcp-session-id")
        .expect("initialize response must carry mcp-session-id header")
        .to_str()
        .expect("mcp-session-id header is valid UTF-8")
        .to_owned();
    let _ = init_res.text().await.expect("drain init body");

    let notif_res = post_mcp(
        &client,
        &base_url,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}}),
        Some(&session_id),
    )
    .await;
    assert!(
        notif_res.status().is_success(),
        "initialized should succeed"
    );
    let _ = notif_res.text().await.expect("drain notif body");

    let tools_res = post_mcp(
        &client,
        &base_url,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "turbofig_status", "arguments": {}}
        }),
        Some(&session_id),
    )
    .await;
    assert!(
        tools_res.status().is_success(),
        "tools/call must succeed after a prior client disconnect, got HTTP {}",
        tools_res.status()
    );
    let body = tools_res.text().await.expect("read tools body");
    let msg = parse_sse_data(&body);
    let text: Value = serde_json::from_str(
        msg["result"]["content"][0]["text"]
            .as_str()
            .expect("tool text is a string"),
    )
    .expect("tool text is JSON");
    assert_eq!(
        text["ok"],
        json!(true),
        "daemon must be alive after the disconnect, got: {text}"
    );
}

/// A session ID from a prior daemon instance must get a clear reinitialize
/// signal after a restart, not a silent failure.
///
/// The session registry lives in memory. A daemon restart loses it. A client
/// that reuses an old, well-formed `mcp-session-id` must receive HTTP 404, the
/// MCP reinitialize signal: the client must start a fresh session with
/// `initialize`. This differs from `test_bogus_session_id_is_rejected` because
/// the session ID here was legitimately issued by a real daemon instance.
#[tokio::test]
async fn test_stale_session_after_restart_signals_reinit() {
    // Instance A: initialize and capture a real, valid session ID.
    let base_a = start_server().await;
    let client = make_client();
    let init_res = post_mcp(
        &client,
        &base_a,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "clientInfo": {"name": "test-client", "version": "0.1.0"},
                "capabilities": {}
            }
        }),
        None,
    )
    .await;
    assert!(
        init_res.status().is_success(),
        "initialize on A should succeed"
    );
    let stale_session = init_res
        .headers()
        .get("mcp-session-id")
        .expect("initialize response must carry mcp-session-id header")
        .to_str()
        .expect("mcp-session-id header is valid UTF-8")
        .to_owned();
    let _ = init_res.text().await.expect("drain init body");

    // Instance B: a fresh daemon on a new port. This models a restart: the new
    // instance has an empty session registry and never saw the stale session.
    let base_b = start_server().await;
    assert_ne!(base_a, base_b, "the two instances must be distinct");

    // Present the stale session to instance B. It must reply 404 (reinit signal).
    let res = post_mcp(
        &client,
        &base_b,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "turbofig_status", "arguments": {}}
        }),
        Some(&stale_session),
    )
    .await;

    assert_eq!(
        res.status(),
        reqwest::StatusCode::NOT_FOUND,
        "a stale session after restart must return 404, the reinitialize signal"
    );
}
