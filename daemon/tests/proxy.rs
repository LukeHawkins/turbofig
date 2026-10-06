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
    fetch_health_with_token, free_port, handshake, read_response_for_id, send_json, spawn_daemon,
    spawn_proxy, stdio_call_tool, stdio_tools_list, stop_daemon, tool_call_status, wait_for_health,
    DaemonGuard,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tokio_tungstenite::{connect_async, tungstenite::Message as TtMessage};

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
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    // Built right after the spawn, before any `await` that could panic
    // (`handshake`, `wait_for_health`): the proxy's background bootstrap
    // starts a detached daemon this test never ran itself, so a guard built
    // only after those awaits would leak it on a panic in either one.
    let mut daemon_guard = DaemonGuard::new(home.path(), mcp_port);
    let mut proxy = spawn_proxy(home.path(), mcp_port, ws_port, false, None);
    let (mut writer, mut reader) = handshake(&mut proxy).await;
    // `initialize` and `tools/list` both answer at once, with no daemon
    // needed (see `proxy::run`'s doc comment), so the proxy's background
    // bootstrap may still be in flight here. Wait for it, then fill in the
    // guard's pid for the kill fallback.
    wait_for_health(&client, mcp_port).await;
    daemon_guard.refresh_pid().await;
    let stdio_tools = stdio_tools_list(&mut writer, &mut reader).await;

    let http_tools = http_tools_list().await;

    assert_eq!(
        normalize_tools(&stdio_tools),
        normalize_tools(&http_tools),
        "the stdio proxy's tools/list must match the HTTP MCP's tool-for-tool"
    );
    assert_eq!(stdio_tools.len(), 4, "the tool surface is exactly 4 tools");
}

// ── (a2) stdio initialize reports the turbofig serverInfo, not rmcp's ──────

#[tokio::test]
async fn proxy_initialize_reports_the_turbofig_server_info_over_stdio() {
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    // Built right after the spawn, before any `await` that could panic: see
    // `proxy_tools_list_matches_the_http_mcp_tools_list`'s guard comment.
    let mut daemon_guard = DaemonGuard::new(home.path(), mcp_port);
    let mut proxy = spawn_proxy(home.path(), mcp_port, ws_port, false, None);
    let mut writer = proxy.0.stdin.take().expect("proxy stdin");
    let mut reader = tokio::io::BufReader::new(proxy.0.stdout.take().expect("proxy stdout"));

    common::send_json(
        &mut writer,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "clientInfo": {"name": "test-client", "version": "0.1.0"},
                "capabilities": {}
            }
        }),
    )
    .await;
    let init = common::read_response_for_id(&mut reader, 1).await;
    let server_info = &init["result"]["serverInfo"];
    assert_eq!(server_info["name"], json!("turbofig"));
    assert_eq!(server_info["version"], json!(env!("CARGO_PKG_VERSION")));
    assert_ne!(server_info["name"], json!("rmcp"));
    assert_ne!(server_info["version"], json!("3.1.0"));

    // `initialize` answers at once, with no daemon needed (see `proxy::run`'s
    // doc comment), so the proxy's background bootstrap may still be
    // starting the daemon this test never ran itself.
    wait_for_health(&client, mcp_port).await;
    daemon_guard.refresh_pid().await;
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

    // Built right after the spawn, before any `await` that could panic: see
    // `proxy_tools_list_matches_the_http_mcp_tools_list`'s guard comment.
    let mut daemon_guard = DaemonGuard::new(home.path(), mcp_port);
    let mut proxy = spawn_proxy(home.path(), mcp_port, ws_port, false, None);
    let (mut writer, mut reader) = handshake(&mut proxy).await;

    let resp = stdio_call_tool(&mut writer, &mut reader, 2, "turbofig_status", json!({})).await;
    assert_eq!(tool_call_status(&resp)["ok"], json!(true));

    // The daemon the proxy started is reachable directly too.
    wait_for_health(&client, mcp_port).await;
    daemon_guard.refresh_pid().await;
}

// ── (c) killing the proxy, or its whole process group, never touches the daemon ─

#[tokio::test]
async fn killing_the_proxy_with_sigkill_leaves_the_daemon_running() {
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    // Built right after the spawn, before any `await` that could panic: see
    // `proxy_tools_list_matches_the_http_mcp_tools_list`'s guard comment.
    let mut daemon_guard = DaemonGuard::new(home.path(), mcp_port);
    let mut proxy = spawn_proxy(home.path(), mcp_port, ws_port, false, None);
    let (mut writer, mut reader) = handshake(&mut proxy).await;
    let _ = stdio_call_tool(&mut writer, &mut reader, 2, "turbofig_status", json!({})).await;
    wait_for_health(&client, mcp_port).await;
    daemon_guard.refresh_pid().await;

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

    // Built right after the spawn, before any `await` that could panic: see
    // `proxy_tools_list_matches_the_http_mcp_tools_list`'s guard comment.
    let mut daemon_guard = DaemonGuard::new(home.path(), mcp_port);
    let mut proxy = spawn_proxy(home.path(), mcp_port, ws_port, true, None);
    let (mut writer, mut reader) = handshake(&mut proxy).await;
    let _ = stdio_call_tool(&mut writer, &mut reader, 2, "turbofig_status", json!({})).await;
    wait_for_health(&client, mcp_port).await;
    daemon_guard.refresh_pid().await;

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
    // Built right after the spawns, before any `await` that could panic: see
    // `proxy_tools_list_matches_the_http_mcp_tools_list`'s guard comment.
    let mut daemon_guard = DaemonGuard::new(home.path(), mcp_port);
    let mut proxy_a = spawn_proxy(home.path(), mcp_port, ws_port, false, None);
    let mut proxy_b = spawn_proxy(home.path(), mcp_port, ws_port, false, None);

    let (mut writer_a, mut reader_a) = handshake(&mut proxy_a).await;
    let (mut writer_b, mut reader_b) = handshake(&mut proxy_b).await;
    daemon_guard.refresh_pid().await;

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
        .filter(|l| l.contains("turbofig MCP listening on") && l.contains(&mcp_port.to_string()))
        .count();
    assert_eq!(
        listening_lines, 1,
        "exactly one daemon may ever bind {mcp_port}, got log:\n{log}"
    );
}

// ── (e) the retry-once path: a lost daemon is restarted exactly once ───────

/// A daemon that dies mid-session (a crash, a `kill`) makes the next call's
/// connect attempt fail outright; `run_job` must restart it exactly once and
/// retry, so the call still succeeds.
#[tokio::test]
async fn a_mid_session_daemon_death_is_retried_once_and_the_call_succeeds() {
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let mut daemon = spawn_daemon(home.path(), mcp_port, ws_port, None);
    wait_for_health(&client, mcp_port).await;

    // Built right after the spawn, before any `await` that could panic: the
    // proxy restarts the daemon on this same port further down, so a guard
    // built only at the end would leak that restarted daemon on an earlier
    // panic. See `proxy_tools_list_matches_the_http_mcp_tools_list`'s guard
    // comment.
    let mut daemon_guard = DaemonGuard::new(home.path(), mcp_port);
    let mut proxy = spawn_proxy(home.path(), mcp_port, ws_port, false, None);
    let (mut writer, mut reader) = handshake(&mut proxy).await;

    let resp = stdio_call_tool(&mut writer, &mut reader, 2, "turbofig_status", json!({})).await;
    assert_eq!(
        tool_call_status(&resp)["ok"],
        json!(true),
        "the first call must reach the original daemon: {resp}"
    );

    // Kill the original daemon outright and wait for its port to actually
    // free up, so the next call's connect attempt fails rather than racing
    // a half-closed socket.
    daemon.kill().await.expect("kill the original daemon");
    let _ = daemon.wait().await;
    turbofig::spawn::wait_for_unreachable(&client, mcp_port, Duration::from_secs(5)).await;

    let resp2 = stdio_call_tool(&mut writer, &mut reader, 3, "turbofig_status", json!({})).await;
    assert_eq!(
        tool_call_status(&resp2)["ok"],
        json!(true),
        "the proxy must restart the daemon once and the retried call must succeed: {resp2}"
    );

    daemon_guard.refresh_pid().await;
}

// ── (e2) a token rotation mid-session is retried once with the fresh token ──

/// A token rotation (`turbofig stop`, delete the token, `turbofig start`,
/// per SECURITY.md) while a proxy's agent session stays open must not fail
/// every `/job` call for the rest of that session: the first call after the
/// rotation gets a 401 against the proxy's cached token, so `post_job` must
/// re-read `<home>/token` and retry once.
#[tokio::test]
async fn a_token_rotation_mid_session_is_retried_once_and_the_call_succeeds() {
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let mut old_daemon = spawn_daemon(home.path(), mcp_port, ws_port, None);
    wait_for_health(&client, mcp_port).await;
    let old_token = tokio::fs::read_to_string(home.path().join("token"))
        .await
        .expect("read the original token");

    let mut proxy = spawn_proxy(home.path(), mcp_port, ws_port, false, None);
    let (mut writer, mut reader) = handshake(&mut proxy).await;

    let resp = stdio_call_tool(&mut writer, &mut reader, 2, "turbofig_status", json!({})).await;
    assert_eq!(
        tool_call_status(&resp)["ok"],
        json!(true),
        "the first call must succeed and cache the original token: {resp}"
    );

    // Rotate the token exactly as SECURITY.md describes: stop, delete the
    // token file, start a new daemon, which writes a fresh one.
    stop_daemon(&client, mcp_port, home.path()).await;
    let _ = old_daemon.wait().await;
    tokio::fs::remove_file(home.path().join("token"))
        .await
        .expect("delete the token to force rotation");
    let mut new_daemon = spawn_daemon(home.path(), mcp_port, ws_port, None);
    wait_for_health(&client, mcp_port).await;
    let new_token = tokio::fs::read_to_string(home.path().join("token"))
        .await
        .expect("read the rotated token");
    assert_ne!(
        old_token, new_token,
        "the rotation must actually produce a new token"
    );

    // The proxy still only knows the old token; this call must 401 once
    // internally, re-read the token file, and retry rather than surfacing
    // the 401 to the caller.
    let resp2 = stdio_call_tool(&mut writer, &mut reader, 3, "turbofig_status", json!({})).await;
    assert_eq!(
        tool_call_status(&resp2)["ok"],
        json!(true),
        "the proxy must retry once with the rotated token: {resp2}"
    );

    let _ = new_daemon.kill().await;
    let _ = new_daemon.wait().await;
}

/// Polls the daemon's `/health` `connectedFiles` until `file_key` appears.
/// Shared shape with `version_handoff.rs`'s identical helper: both tests
/// connect a mock plugin directly to a real spawned daemon's WS port (not
/// through an in-process `AppState`), so this is the only way to know the
/// plugin has actually registered.
async fn wait_for_file_connected(
    client: &reqwest::Client,
    mcp_port: u16,
    token: &str,
    file_key: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(health) = fetch_health_with_token(client, mcp_port, token).await {
            if health["connectedFiles"]
                .as_array()
                .map(|files| files.iter().any(|f| f["fileKey"] == json!(file_key)))
                .unwrap_or(false)
            {
                return;
            }
        }
        if Instant::now() >= deadline {
            panic!("plugin for fileKey {file_key} never registered");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A request that has already reached the daemon (and the plugin) before the
/// daemon dies must never be retried: the daemon may have already acted on
/// it, so retrying could run it twice. The error must say so, not just that
/// the call failed.
#[tokio::test]
async fn an_in_flight_request_killed_mid_call_is_never_retried() {
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let mut daemon = spawn_daemon(home.path(), mcp_port, ws_port, None);
    wait_for_health(&client, mcp_port).await;
    let token = tokio::fs::read_to_string(home.path().join("token"))
        .await
        .expect("read token")
        .trim()
        .to_owned();

    // A mock plugin that never replies to EXECUTE: once it forwards the
    // request, the job stays in flight from the daemon's point of view for
    // as long as the daemon itself is alive.
    let (mut plugin_ws, _) = connect_async(format!("ws://127.0.0.1:{ws_port}/?token={token}"))
        .await
        .expect("mock plugin connect");
    plugin_ws
        .send(TtMessage::Text(
            json!({"type": "FILE_INFO", "fileKey": "silent-file", "name": "Silent File"})
                .to_string(),
        ))
        .await
        .expect("send FILE_INFO");
    wait_for_file_connected(&client, mcp_port, &token, "silent-file").await;

    // Fires once the plugin has actually received the EXECUTE request,
    // proving the job reached the daemon (and the plugin) before the daemon
    // is killed below, not merely that the proxy attempted to send it.
    let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
    let plugin_task = tokio::spawn(async move {
        let mut reached_tx = Some(reached_tx);
        while let Some(Ok(msg)) = plugin_ws.next().await {
            let TtMessage::Text(text) = msg else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            if v["type"] == "EXECUTE" {
                if let Some(tx) = reached_tx.take() {
                    let _ = tx.send(());
                }
                // Never reply: the job must stay in flight.
            }
        }
    });

    let mut proxy = spawn_proxy(home.path(), mcp_port, ws_port, false, None);
    let (mut writer, mut reader) = handshake(&mut proxy).await;

    send_json(
        &mut writer,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "turbofig_execute",
                "arguments": {"code": "return 1;", "fileKey": "silent-file"}
            }
        }),
    )
    .await;
    reached_rx
        .await
        .expect("the plugin must receive the EXECUTE request before the daemon dies");

    daemon.kill().await.expect("kill the daemon mid-call");
    let _ = daemon.wait().await;

    let resp = read_response_for_id(&mut reader, 2).await;
    let status = tool_call_status(&resp);
    assert_eq!(status["ok"], json!(false), "the call must fail: {resp}");
    let error = status["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("may already have run"),
        "the error must say the job may already have run, not just that it failed: {error}"
    );

    // Not retried: an in-flight failure must never trigger the proxy's
    // reconnect-and-restart path, so no fresh daemon ever came up.
    assert!(
        turbofig::spawn::fetch_health(&client, mcp_port)
            .await
            .is_none(),
        "an in-flight failure must never cause the proxy to restart the daemon"
    );

    plugin_task.abort();
}

// ── (g) the X-Turbofig-Session header pairs a file over stdio with 2 open ──

/// Connects a mock plugin for `file_key` and replies `{"ok":true}` to every
/// request it receives, echoing the fileKey back so a caller can tell which
/// plugin actually answered.
async fn spawn_replying_plugin(ws_port: u16, token: &str, file_key: &'static str) {
    let (mut plugin_ws, _) = connect_async(format!("ws://127.0.0.1:{ws_port}/?token={token}"))
        .await
        .expect("mock plugin connect");
    plugin_ws
        .send(TtMessage::Text(
            json!({"type": "FILE_INFO", "fileKey": file_key, "name": file_key}).to_string(),
        ))
        .await
        .expect("send FILE_INFO");

    tokio::spawn(async move {
        while let Some(Ok(msg)) = plugin_ws.next().await {
            let TtMessage::Text(text) = msg else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            let Some(id) = v.get("requestId").and_then(Value::as_u64) else {
                continue;
            };
            let reply = json!({
                "type": "RESULT", "requestId": id, "ok": true, "fileKey": file_key
            });
            let _ = plugin_ws.send(TtMessage::Text(reply.to_string())).await;
        }
    });
}

/// With 2 files connected, one `turbofig mcp` stdio session that explicitly
/// targets one of them must have every later call with no `fileKey` keep
/// routing to that same file, over the real stdio transport end to end: the
/// `X-Turbofig-Session` header this proxy sends on every `/job` call is what
/// makes that possible (see `mcp::job_session_id`). Before that header
/// existed, every stdio call routed with no session at all, so a second call
/// with no `fileKey` failed "multiple files connected; specify fileKey" even
/// right after an explicit call had named one.
#[tokio::test]
async fn stdio_proxy_pairs_a_file_across_calls_with_2_files_connected() {
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let _daemon = spawn_daemon(home.path(), mcp_port, ws_port, None);
    wait_for_health(&client, mcp_port).await;
    let token = tokio::fs::read_to_string(home.path().join("token"))
        .await
        .expect("read token")
        .trim()
        .to_owned();

    spawn_replying_plugin(ws_port, &token, "fk1").await;
    spawn_replying_plugin(ws_port, &token, "fk2").await;
    wait_for_file_connected(&client, mcp_port, &token, "fk1").await;
    wait_for_file_connected(&client, mcp_port, &token, "fk2").await;

    let mut proxy = spawn_proxy(home.path(), mcp_port, ws_port, false, None);
    let (mut writer, mut reader) = handshake(&mut proxy).await;

    let resp1 = stdio_call_tool(
        &mut writer,
        &mut reader,
        2,
        "turbofig_status",
        json!({"fileKey": "fk2"}),
    )
    .await;
    assert_eq!(
        tool_call_status(&resp1)["plugin"]["fileKey"],
        json!("fk2"),
        "the explicit fileKey call must resolve to fk2: {resp1}"
    );

    let resp2 = stdio_call_tool(&mut writer, &mut reader, 3, "turbofig_status", json!({})).await;
    assert_eq!(
        tool_call_status(&resp2)["plugin"]["fileKey"],
        json!("fk2"),
        "a later call on the same stdio session with no fileKey must stay paired to fk2, \
         not fail ambiguous: {resp2}"
    );
}
