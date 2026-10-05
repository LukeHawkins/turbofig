//! Integration tests for `turbofig mcp`, the stdio MCP proxy.
//!
//! These spawn the real compiled binary (`CARGO_BIN_EXE_turbofig`): the
//! proxy's own detached-daemon spawn, and the process-group kill tests,
//! only mean anything against real OS processes, not an in-process mock.
//! Every test holds a `DaemonGuard` for the daemon its proxy spawned: that
//! daemon is fully detached (`setsid`), with no `Child` handle a test could
//! otherwise reap, so the guard's drop (an authenticated `/control stop`,
//! waited out, with a pid-kill fallback) is what keeps it from outliving
//! the test, even when an assertion panics first.

mod common;

use common::{
    free_port, handshake, spawn_proxy, stdio_call_tool, stdio_tools_list, tool_call_status,
    wait_for_health, DaemonGuard,
};
use serde_json::{json, Value};
use std::time::Duration;

// ── HTTP MCP tools/list, for comparison against the stdio one (test a) ──────

/// Collects `(name, description, inputSchema)` triples from a live MCP
/// transport's `tools/list`, sorted by name so the comparison in test (a) is
/// order-independent.
fn normalize_tools(tools: &[Value]) -> Vec<(String, Value, Value)> {
    let mut out: Vec<(String, Value, Value)> = tools
        .iter()
        .map(|t| {
            (
                t["name"].as_str().unwrap_or_default().to_owned(),
                t["description"].clone(),
                t["inputSchema"].clone(),
            )
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

async fn http_tools_list() -> Vec<Value> {
    let state = std::sync::Arc::new(turbofig::AppState::new());
    let token = state.token().to_owned();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = turbofig::serve_with_state(listener, state).await;
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("build client");
    let base = format!("http://{addr}");

    let init = client
        .post(format!("{base}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {token}"))
        .json(&json!({
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
        .expect("send initialize");
    let session_id = init
        .headers()
        .get("mcp-session-id")
        .expect("mcp-session-id header")
        .to_str()
        .expect("utf8 header")
        .to_owned();
    let _ = init.text().await;

    client
        .post(format!("{base}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {token}"))
        .header("mcp-session-id", &session_id)
        .json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}}))
        .send()
        .await
        .expect("send notifications/initialized");

    let list = client
        .post(format!("{base}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {token}"))
        .header("mcp-session-id", &session_id)
        .json(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}))
        .send()
        .await
        .expect("send tools/list");
    let body = list.text().await.expect("read tools/list body");
    let msg = parse_sse_data(&body);
    msg["result"]["tools"]
        .as_array()
        .expect("result.tools is an array")
        .clone()
}

fn parse_sse_data(body: &str) -> Value {
    for line in body.lines() {
        let data = line
            .strip_prefix("data: ")
            .or_else(|| line.strip_prefix("data:"));
        let Some(data) = data else { continue };
        if data.is_empty() {
            continue;
        }
        return serde_json::from_str(data).expect("SSE data line is valid JSON");
    }
    panic!("no data line found in SSE body:\n{body}");
}

// ── (a) tools/list over stdio matches the HTTP MCP tool list ───────────────

#[tokio::test]
async fn proxy_tools_list_matches_the_http_mcp_tools_list() {
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();

    let mut proxy = spawn_proxy(home.path(), mcp_port, ws_port, false, None);
    let (mut writer, mut reader) = handshake(&mut proxy).await;
    let _daemon_guard = DaemonGuard::for_daemon_on(home.path(), mcp_port).await;
    let stdio_tools = stdio_tools_list(&mut writer, &mut reader).await;

    let http_tools = http_tools_list().await;

    assert_eq!(
        normalize_tools(&stdio_tools),
        normalize_tools(&http_tools),
        "the stdio proxy's tools/list must match the HTTP MCP's tool-for-tool"
    );
    assert_eq!(stdio_tools.len(), 4, "the tool surface is exactly 4 tools");
}

// ── (b) `turbofig mcp` with no daemon running starts one ───────────────────

#[tokio::test]
async fn proxy_starts_the_daemon_when_none_is_running_and_a_status_call_works() {
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    // Nothing is listening on mcp_port/ws_port yet, and no token file exists:
    // the proxy must start the daemon itself before it can serve a tool call.
    assert!(
        !home.path().join("token").exists(),
        "no daemon has run yet, so there must be no token file"
    );

    let mut proxy = spawn_proxy(home.path(), mcp_port, ws_port, false, None);
    let (mut writer, mut reader) = handshake(&mut proxy).await;

    let resp = stdio_call_tool(&mut writer, &mut reader, 2, "turbofig_status", json!({})).await;
    assert_eq!(tool_call_status(&resp)["ok"], json!(true));

    // The daemon the proxy started is reachable directly too.
    wait_for_health(&client, mcp_port).await;
    let _daemon_guard = DaemonGuard::for_daemon_on(home.path(), mcp_port).await;
}

// ── (c) killing the proxy, or its whole process group, never touches the daemon ─

#[tokio::test]
async fn killing_the_proxy_with_sigkill_leaves_the_daemon_running() {
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let mut proxy = spawn_proxy(home.path(), mcp_port, ws_port, false, None);
    let (mut writer, mut reader) = handshake(&mut proxy).await;
    let _ = stdio_call_tool(&mut writer, &mut reader, 2, "turbofig_status", json!({})).await;
    wait_for_health(&client, mcp_port).await;
    let _daemon_guard = DaemonGuard::for_daemon_on(home.path(), mcp_port).await;

    let pid = proxy.0.id().expect("proxy has a pid") as i32;
    drop(writer);
    drop(reader);
    // SAFETY: libc::kill with a valid pid and a standard signal number is a
    // plain syscall wrapper; no preconditions beyond a live pid, which the
    // handshake above already proved.
    let rc = unsafe { libc::kill(pid, libc::SIGKILL) };
    assert_eq!(rc, 0, "SIGKILL to the proxy must succeed");
    let _ = proxy.0.wait().await;

    // The daemon, in its own session, must still answer.
    wait_for_health(&client, mcp_port).await;
}

#[tokio::test]
async fn sigterm_to_the_proxys_process_group_leaves_the_daemon_running() {
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let mut proxy = spawn_proxy(home.path(), mcp_port, ws_port, true, None);
    let (mut writer, mut reader) = handshake(&mut proxy).await;
    let _ = stdio_call_tool(&mut writer, &mut reader, 2, "turbofig_status", json!({})).await;
    wait_for_health(&client, mcp_port).await;
    let _daemon_guard = DaemonGuard::for_daemon_on(home.path(), mcp_port).await;

    // `process_group(0)` made the proxy the leader of its own group
    // (pgid == pid), so signalling -pid reaches only the proxy, never the
    // daemon: `spawn_detached_daemon` put the daemon in a separate session
    // (and therefore a separate process group) via `setsid`.
    let pid = proxy.0.id().expect("proxy has a pid") as i32;
    drop(writer);
    drop(reader);
    // SAFETY: as above; a negative pid here is a documented kill(2) form
    // meaning "every process in this group", not a sign of a bad argument.
    let rc = unsafe { libc::kill(-pid, libc::SIGTERM) };
    assert_eq!(rc, 0, "SIGTERM to the proxy's process group must succeed");
    let _ = proxy.0.wait().await;

    wait_for_health(&client, mcp_port).await;
}

// ── (d) two proxies started at once share exactly one daemon ───────────────

#[tokio::test]
async fn two_proxies_started_at_once_share_exactly_one_daemon() {
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();

    // Spawned back to back, with no daemon running yet for either to find:
    // both race to start one, and the TCP bind on mcp_port/ws_port is the
    // only thing that decides which one actually serves.
    let mut proxy_a = spawn_proxy(home.path(), mcp_port, ws_port, false, None);
    let mut proxy_b = spawn_proxy(home.path(), mcp_port, ws_port, false, None);

    let (mut writer_a, mut reader_a) = handshake(&mut proxy_a).await;
    let (mut writer_b, mut reader_b) = handshake(&mut proxy_b).await;
    let _daemon_guard = DaemonGuard::for_daemon_on(home.path(), mcp_port).await;

    let resp_a = stdio_call_tool(
        &mut writer_a,
        &mut reader_a,
        2,
        "turbofig_status",
        json!({}),
    )
    .await;
    let resp_b = stdio_call_tool(
        &mut writer_b,
        &mut reader_b,
        2,
        "turbofig_status",
        json!({}),
    )
    .await;

    for (label, resp) in [("a", &resp_a), ("b", &resp_b)] {
        assert_eq!(
            tool_call_status(resp)["ok"],
            json!(true),
            "proxy {label} must reach the daemon"
        );
    }

    // Exactly one daemon ever bound the port: the shared daemon.log (both
    // proxies point at the same home) carries exactly one "listening on"
    // line for this port, never two, no matter which proxy's spawn attempt
    // won the race.
    let log = tokio::fs::read_to_string(home.path().join("daemon.log"))
        .await
        .expect("read daemon.log");
    let listening_lines = log
        .lines()
        .filter(|l| l.contains("Turbofig MCP listening on") && l.contains(&mcp_port.to_string()))
        .count();
    assert_eq!(
        listening_lines, 1,
        "exactly one daemon may ever bind {mcp_port}, got log:\n{log}"
    );
}
