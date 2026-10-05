//! Integration tests for `turbofig mcp`, the stdio MCP proxy.
//!
//! These spawn the real compiled binary (`CARGO_BIN_EXE_turbofig`): the
//! proxy's own detached-daemon spawn, and the process-group kill tests,
//! only mean anything against real OS processes, not an in-process mock.
//! Every test cleans up the daemon it started via an authenticated
//! `POST /control` `stop`, using the token the daemon wrote to its temp
//! home, so no spawned daemon outlives its test.

use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

const BIN: &str = env!("CARGO_BIN_EXE_turbofig");
const STDIO_READ_TIMEOUT: Duration = Duration::from_secs(10);

// ── process and port helpers ────────────────────────────────────────────────

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind an ephemeral port")
        .local_addr()
        .expect("local addr")
        .port()
}

/// Owns a spawned `turbofig mcp` child. Kills and reaps it on drop, so a
/// failing assertion never leaks a process holding the test's stdio pipes.
struct ProxyChild(Child);

impl Drop for ProxyChild {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}

/// Spawns `turbofig mcp` with piped stdin/stdout, pointed at `mcp_port`/
/// `ws_port`/`home` via the `TURBOFIG_*` env vars. `own_process_group` puts
/// the child in a process group of its own (pgid == its pid), so a test can
/// signal that whole group without also signalling the test runner.
fn spawn_proxy(home: &Path, mcp_port: u16, ws_port: u16, own_process_group: bool) -> ProxyChild {
    let mut cmd = Command::new(BIN);
    cmd.arg("mcp")
        .env("TURBOFIG_MCP_PORT", mcp_port.to_string())
        .env("TURBOFIG_WS_PORT", ws_port.to_string())
        .env("TURBOFIG_BRIDGE_DIR", home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    if own_process_group {
        cmd.process_group(0);
    }
    ProxyChild(cmd.spawn().expect("spawn turbofig mcp"))
}

// ── stdio MCP framing (newline-delimited JSON, per the MCP stdio transport) ─

async fn send_json<W: tokio::io::AsyncWrite + Unpin>(writer: &mut W, msg: &Value) {
    let line = serde_json::to_string(msg).expect("serialize request");
    writer
        .write_all(line.as_bytes())
        .await
        .expect("write request line");
    writer.write_all(b"\n").await.expect("write newline");
    writer.flush().await.expect("flush request");
}

/// Reads lines until one parses as JSON with `"id": expected_id`, or panics
/// after `STDIO_READ_TIMEOUT`. Lines for other ids (there should be none in
/// these single-client tests) are skipped rather than rejected.
async fn read_response_for_id<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
    expected_id: u64,
) -> Value {
    let deadline = Instant::now() + STDIO_READ_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            panic!("timed out waiting for stdio response id {expected_id}");
        }
        let mut line = String::new();
        let read = tokio::time::timeout(remaining, reader.read_line(&mut line))
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for stdio response id {expected_id}"))
            .expect("read a line from the proxy's stdout");
        if read == 0 {
            panic!("proxy stdout closed while waiting for response id {expected_id}");
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if value.get("id").and_then(Value::as_u64) == Some(expected_id) {
            return value;
        }
    }
}

/// Runs the standard MCP stdio handshake (initialize, notifications/
/// initialized) over `proxy`'s stdin/stdout, and returns the stdin writer and
/// a buffered reader over stdout for further requests.
async fn handshake(
    proxy: &mut ProxyChild,
) -> (
    tokio::process::ChildStdin,
    BufReader<tokio::process::ChildStdout>,
) {
    let mut writer = proxy.0.stdin.take().expect("proxy stdin");
    let mut reader = BufReader::new(proxy.0.stdout.take().expect("proxy stdout"));

    send_json(
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
    let init = read_response_for_id(&mut reader, 1).await;
    assert!(
        init.get("result").is_some(),
        "initialize must succeed over stdio: {init}"
    );

    send_json(
        &mut writer,
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;

    (writer, reader)
}

async fn stdio_tools_list(
    writer: &mut tokio::process::ChildStdin,
    reader: &mut BufReader<tokio::process::ChildStdout>,
) -> Vec<Value> {
    send_json(
        writer,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    )
    .await;
    let resp = read_response_for_id(reader, 2).await;
    resp["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list must return an array: {resp}"))
        .clone()
}

async fn stdio_call_tool(
    writer: &mut tokio::process::ChildStdin,
    reader: &mut BufReader<tokio::process::ChildStdout>,
    id: u64,
    name: &str,
    arguments: Value,
) -> Value {
    send_json(
        writer,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }),
    )
    .await;
    read_response_for_id(reader, id).await
}

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
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = turbofig::serve(listener).await;
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
        .header("mcp-session-id", &session_id)
        .json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}}))
        .send()
        .await
        .expect("send notifications/initialized");

    let list = client
        .post(format!("{base}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
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

// ── daemon cleanup ───────────────────────────────────────────────────────────

/// Stops the daemon at `mcp_port` via an authenticated `POST /control`, using
/// the token it wrote to `<home>/token`. Best-effort: a test that never
/// managed to start a daemon has no token file and nothing to stop.
async fn stop_daemon(client: &reqwest::Client, mcp_port: u16, home: &Path) {
    let Ok(token) = tokio::fs::read_to_string(home.join("token")).await else {
        return;
    };
    let _ = client
        .post(format!("http://127.0.0.1:{mcp_port}/control"))
        .bearer_auth(token.trim())
        .json(&json!({"action": "stop"}))
        .send()
        .await;
}

async fn wait_for_health(client: &reqwest::Client, mcp_port: u16) {
    let url = format!("http://127.0.0.1:{mcp_port}/health");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(resp) = client.get(&url).send().await {
            if resp.status().is_success() {
                return;
            }
        }
        if Instant::now() >= deadline {
            panic!("daemon on port {mcp_port} never became healthy");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ── (a) tools/list over stdio matches the HTTP MCP tool list ───────────────

#[tokio::test]
async fn proxy_tools_list_matches_the_http_mcp_tools_list() {
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let mut proxy = spawn_proxy(home.path(), mcp_port, ws_port, false);
    let (mut writer, mut reader) = handshake(&mut proxy).await;
    let stdio_tools = stdio_tools_list(&mut writer, &mut reader).await;

    let http_tools = http_tools_list().await;

    assert_eq!(
        normalize_tools(&stdio_tools),
        normalize_tools(&http_tools),
        "the stdio proxy's tools/list must match the HTTP MCP's tool-for-tool"
    );
    assert_eq!(stdio_tools.len(), 4, "the tool surface is exactly 4 tools");

    stop_daemon(&client, mcp_port, home.path()).await;
}

// ── (b) `turbofig mcp` with no daemon running starts one ───────────────────

#[tokio::test]
async fn proxy_starts_the_daemon_when_none_is_running_and_a_status_call_works() {
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

    let mut proxy = spawn_proxy(home.path(), mcp_port, ws_port, false);
    let (mut writer, mut reader) = handshake(&mut proxy).await;

    let resp = stdio_call_tool(&mut writer, &mut reader, 2, "turbofig_status", json!({})).await;
    let content = resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("turbofig_status must return text content: {resp}"));
    let status: Value = serde_json::from_str(content).expect("status content is JSON");
    assert_eq!(status["ok"], json!(true));

    // The daemon the proxy started is reachable directly too.
    wait_for_health(&client, mcp_port).await;

    stop_daemon(&client, mcp_port, home.path()).await;
}

// ── (c) killing the proxy, or its whole process group, never touches the daemon ─

#[tokio::test]
async fn killing_the_proxy_with_sigkill_leaves_the_daemon_running() {
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let mut proxy = spawn_proxy(home.path(), mcp_port, ws_port, false);
    let (mut writer, mut reader) = handshake(&mut proxy).await;
    let _ = stdio_call_tool(&mut writer, &mut reader, 2, "turbofig_status", json!({})).await;
    wait_for_health(&client, mcp_port).await;

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

    stop_daemon(&client, mcp_port, home.path()).await;
}

#[tokio::test]
async fn sigterm_to_the_proxys_process_group_leaves_the_daemon_running() {
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let mut proxy = spawn_proxy(home.path(), mcp_port, ws_port, true);
    let (mut writer, mut reader) = handshake(&mut proxy).await;
    let _ = stdio_call_tool(&mut writer, &mut reader, 2, "turbofig_status", json!({})).await;
    wait_for_health(&client, mcp_port).await;

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

    stop_daemon(&client, mcp_port, home.path()).await;
}

// ── (d) two proxies started at once share exactly one daemon ───────────────

#[tokio::test]
async fn two_proxies_started_at_once_share_exactly_one_daemon() {
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    // Spawned back to back, with no daemon running yet for either to find:
    // both race to start one, and the TCP bind on mcp_port/ws_port is the
    // only thing that decides which one actually serves.
    let mut proxy_a = spawn_proxy(home.path(), mcp_port, ws_port, false);
    let mut proxy_b = spawn_proxy(home.path(), mcp_port, ws_port, false);

    let (mut writer_a, mut reader_a) = handshake(&mut proxy_a).await;
    let (mut writer_b, mut reader_b) = handshake(&mut proxy_b).await;

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
        let content = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("proxy {label} status call must return text: {resp}"));
        let status: Value = serde_json::from_str(content).expect("status content is JSON");
        assert_eq!(
            status["ok"],
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

    stop_daemon(&client, mcp_port, home.path()).await;
}
