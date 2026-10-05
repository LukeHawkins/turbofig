//! Shared helpers for the daemon integration tests.
//!
//! `wait_until` replaces a fixed sleep with a condition poll: a fast machine
//! returns as soon as the condition is true, and a slow CI runner still gets
//! up to `deadline_ms` before the test panics. This removes the flake class
//! where a fixed sleep (tuned on a fast dev machine) is too short on a
//! several-times-slower runner.
//!
//! This module is compiled fresh into every test binary that declares
//! `mod common;`, and no single binary uses every helper here, so dead code
//! is expected and allowed rather than a real warning.
#![allow(dead_code)]

use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

/// Generous default deadline for a condition wait. A healthy condition
/// resolves in a few milliseconds; this only bounds a genuine hang.
pub const WAIT_DEADLINE_MS: u64 = 10_000;

/// Poll interval while a condition is still false.
const POLL_INTERVAL_MS: u64 = 10;

/// Poll `condition` until it returns true, or panic with `msg` once
/// `deadline_ms` elapses.
pub async fn wait_until<F>(mut condition: F, deadline_ms: u64, msg: &str)
where
    F: FnMut() -> bool,
{
    let deadline = Instant::now() + Duration::from_millis(deadline_ms);
    loop {
        if condition() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("Timed out waiting for: {msg}");
        }
        tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
}

/// Poll for a file to appear and return its contents, up to `deadline_ms`.
/// Panics on timeout. Shared by every test that reads a bridge outbox result.
pub async fn poll_file(path: &Path, deadline_ms: u64) -> String {
    let deadline = Instant::now() + Duration::from_millis(deadline_ms);
    loop {
        match tokio::fs::read_to_string(path).await {
            Ok(contents) => return contents,
            Err(_) => {
                if Instant::now() >= deadline {
                    panic!("Timed out waiting for {}", path.display());
                }
                tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
            }
        }
    }
}

/// Wait until `state` shows a connection registered under `file_key`.
/// Replaces the fixed post-FILE_INFO sleep every mock-plugin helper used.
pub async fn wait_for_file_key(state: &Arc<turbofig::AppState>, file_key: &str, deadline_ms: u64) {
    let key = file_key.to_owned();
    wait_until(
        || state.list_connections().iter().any(|(_, fk, _)| fk == &key),
        deadline_ms,
        &format!("plugin to register fileKey {file_key}"),
    )
    .await;
}

/// Wait until `state` shows at least `n` connections with a non-empty file
/// key registered. Use this where a test does not care which key, only that
/// enough plugins have announced themselves.
pub async fn wait_for_connection_count(
    state: &Arc<turbofig::AppState>,
    n: usize,
    deadline_ms: u64,
) {
    wait_until(
        || {
            state
                .list_connections()
                .iter()
                .filter(|(_, fk, _)| !fk.is_empty())
                .count()
                >= n
        },
        deadline_ms,
        &format!("{n} plugin(s) to register"),
    )
    .await;
}

/// Wait until `state` shows no connections at all. Use this after closing a
/// mock plugin socket, to replace a fixed "allow the close to land" sleep.
pub async fn wait_for_no_connections(state: &Arc<turbofig::AppState>, deadline_ms: u64) {
    wait_until(
        || state.list_connections().is_empty(),
        deadline_ms,
        "all connections to close",
    )
    .await;
}

// ── real-process helpers (turbofig mcp / turbofig serve as child processes) ─
//
// Shared by `proxy.rs` and `version_handoff.rs`: both spawn the real compiled
// binary and drive it either over stdio (the proxy) or plain HTTP (a daemon
// started directly for a test fixture).

/// The compiled `turbofig` binary under test.
pub const BIN: &str = env!("CARGO_BIN_EXE_turbofig");

/// Runs tests that spawn real turbofig processes one at a time within a test
/// binary. `free_port` picks a port and then releases it, so 2 parallel tests
/// can get the same port, and a proxy can then start a daemon that no test
/// tracks (this leaked detached daemons). Hold the guard for the whole test.
/// Cargo already runs the test binaries one after another.
pub async fn serial_process_test() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}
const STDIO_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Guards every real spawn of the `turbofig` binary in this test suite.
///
/// A bare spawn (no subcommand: `args` empty) runs the first-run helper,
/// which shells out to the real `open -a Figma` and `pbcopy` unless both
/// `TURBOFIG_TEST_FAKE_CLIPBOARD` and `TURBOFIG_TEST_FAKE_OPENER` are set
/// (`main.rs`'s `clipboard_for_run`/`opener_for_run`): a test that forgot
/// this once brought Figma to the front and overwrote the developer's
/// clipboard on every test run, and left its detached daemon untracked (see
/// git history). Call this immediately before every real spawn, passing the
/// exact `args` and the env pairs the spawn sets, so a future test can never
/// reintroduce that mistake silently: an explicit subcommand (`serve`,
/// `mcp`, `start`, `stop`, `status`, ...) is always safe and needs no env
/// check; a bare spawn panics here unless both fakes are present.
pub fn assert_safe_turbofig_spawn(args: &[&str], envs: &[(&str, &str)]) {
    if !args.is_empty() {
        return;
    }
    let has_fake_clipboard = envs
        .iter()
        .any(|(k, _)| *k == "TURBOFIG_TEST_FAKE_CLIPBOARD");
    let has_fake_opener = envs.iter().any(|(k, _)| *k == "TURBOFIG_TEST_FAKE_OPENER");
    assert!(
        has_fake_clipboard && has_fake_opener,
        "a bare `turbofig` spawn (no subcommand) must set TURBOFIG_TEST_FAKE_CLIPBOARD and \
         TURBOFIG_TEST_FAKE_OPENER, or it opens the real Figma app and overwrites the real \
         clipboard. Pass an explicit subcommand (serve/mcp/start/stop/status) instead, unless \
         this really is a first-run test."
    );
}

/// Binds an ephemeral TCP port and returns it, free for a child process to
/// bind next.
pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind an ephemeral port")
        .local_addr()
        .expect("local addr")
        .port()
}

/// Owns a spawned `turbofig mcp` child. Kills and reaps it on drop, so a
/// failing assertion never leaks a process holding the test's stdio pipes.
pub struct ProxyChild(pub Child);

impl Drop for ProxyChild {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}

/// Spawns `turbofig mcp` with piped stdin/stdout, pointed at `mcp_port`/
/// `ws_port`/`home` via the `TURBOFIG_*` env vars.
///
/// `own_process_group` puts the child in a process group of its own (pgid ==
/// its pid), so a test can signal that whole group without also signalling
/// the test runner.
///
/// `own_version_override`, when set, is passed as
/// `TURBOFIG_TEST_OWN_VERSION_OVERRIDE`: the debug-build-only escape hatch
/// `proxy::own_version` reads, so a test can simulate "an old proxy talking
/// to a newer daemon" without a second real build.
pub fn spawn_proxy(
    home: &Path,
    mcp_port: u16,
    ws_port: u16,
    own_process_group: bool,
    own_version_override: Option<&str>,
) -> ProxyChild {
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
    if let Some(v) = own_version_override {
        cmd.env("TURBOFIG_TEST_OWN_VERSION_OVERRIDE", v);
    }
    ProxyChild(cmd.spawn().expect("spawn turbofig mcp"))
}

/// Spawns `turbofig serve` directly (not detached: the test owns this
/// `Child` and is responsible for reaping it), pointed at `mcp_port`/
/// `ws_port`/`home`.
///
/// `version_override`, when set, is passed as
/// `TURBOFIG_TEST_VERSION_OVERRIDE`: the debug-build-only escape hatch
/// `mcp::reported_version` reads, so a test can stand up a daemon that
/// reports an arbitrary "old" version without a second real build.
pub fn spawn_daemon(
    home: &Path,
    mcp_port: u16,
    ws_port: u16,
    version_override: Option<&str>,
) -> Child {
    // Append to <home>/daemon.log, the same destination spawn_detached_daemon
    // uses for a real detached start: a test asserting on the shared log's
    // "listening on" lines (the race-safety tests) needs this fixture
    // daemon's own start recorded there too, not silently discarded.
    std::fs::create_dir_all(home).expect("create home dir");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.join("daemon.log"))
        .expect("open daemon.log");
    let log_err = log.try_clone().expect("clone log handle");

    let mut cmd = Command::new(BIN);
    cmd.arg("serve")
        .env("TURBOFIG_MCP_PORT", mcp_port.to_string())
        .env("TURBOFIG_WS_PORT", ws_port.to_string())
        .env("TURBOFIG_BRIDGE_DIR", home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(log_err))
        .kill_on_drop(true);
    if let Some(v) = version_override {
        cmd.env("TURBOFIG_TEST_VERSION_OVERRIDE", v);
    }
    cmd.spawn().expect("spawn turbofig serve")
}

/// Runs the `turbofig` binary with `args` to completion, pointed at
/// `mcp_port`/`ws_port`/`home` via the `TURBOFIG_*` env vars, and returns its
/// exit status plus captured stdout and stderr. For a one-shot CLI command
/// (`start`, `stop`, `status`), not `serve` or `mcp`, which never exit on
/// their own.
pub async fn run_turbofig(
    args: &[&str],
    home: &Path,
    mcp_port: u16,
    ws_port: u16,
) -> (std::process::ExitStatus, String, String) {
    assert_safe_turbofig_spawn(args, &[]);
    let output = Command::new(BIN)
        .args(args)
        .env("TURBOFIG_MCP_PORT", mcp_port.to_string())
        .env("TURBOFIG_WS_PORT", ws_port.to_string())
        .env("TURBOFIG_BRIDGE_DIR", home)
        .output()
        .await
        .expect("run turbofig");
    (
        output.status,
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

// ── stdio MCP framing (newline-delimited JSON, per the MCP stdio transport) ─

pub async fn send_json<W: tokio::io::AsyncWrite + Unpin>(writer: &mut W, msg: &Value) {
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
pub async fn read_response_for_id<R: tokio::io::AsyncRead + Unpin>(
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
pub async fn handshake(
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

pub async fn stdio_tools_list(
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

pub async fn stdio_call_tool(
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

/// Extracts the `ok:...` JSON body a tool call's text content carries, from
/// a `tools/call` response envelope.
pub fn tool_call_status(resp: &Value) -> Value {
    let content = resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tool call must return text content: {resp}"));
    serde_json::from_str(content).expect("tool content is JSON")
}

// ── daemon health and cleanup ────────────────────────────────────────────────

/// Returns `GET /health`'s parsed JSON body, or `None` for any failure
/// (connection refused, timeout, a non-success status, an unparseable body).
pub async fn fetch_health(client: &reqwest::Client, mcp_port: u16) -> Option<Value> {
    turbofig::spawn::fetch_health(client, mcp_port).await
}

/// Same as `fetch_health`, authenticated: returns `/health`'s full payload
/// (connected files, pid) rather than the reduced one.
pub async fn fetch_health_with_token(
    client: &reqwest::Client,
    mcp_port: u16,
    token: &str,
) -> Option<Value> {
    turbofig::spawn::fetch_health_with_token(client, mcp_port, Some(token)).await
}

/// Polls `GET /health` until it answers with a success status, or panics
/// after 5 s.
pub async fn wait_for_health(client: &reqwest::Client, mcp_port: u16) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if fetch_health(client, mcp_port).await.is_some() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("daemon on port {mcp_port} never became healthy");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Stops the daemon at `mcp_port` via an authenticated `POST /control`, using
/// the token it wrote to `<home>/token`. Best-effort: a test that never
/// managed to start a daemon has no token file and nothing to stop.
pub async fn stop_daemon(client: &reqwest::Client, mcp_port: u16, home: &Path) {
    let Ok(token) = tokio::fs::read_to_string(home.join("token")).await else {
        return;
    };
    let _ = client
        .post(format!("http://127.0.0.1:{mcp_port}/control"))
        .bearer_auth(token.trim())
        .json(&json!({"action": "stop"}))
        .send()
        .await;
    // Wait for it to actually go away, not just for the request to land:
    // /control replies 202 at once and drains in the background (see
    // control.rs), so a caller that returns the instant this POST completes
    // can race a still-draining daemon.
    turbofig::spawn::wait_for_unreachable(client, mcp_port, Duration::from_secs(10)).await;
}

/// How long `DaemonGuard`'s drop waits for `/health` to go unreachable after
/// an authenticated `/control stop`, before falling back to killing the
/// recorded pid directly.
const DAEMON_GUARD_STOP_DEADLINE: Duration = Duration::from_secs(10);

/// RAII guard for a daemon a test's own `turbofig mcp` proxy spawned.
///
/// `spawn_daemon` returns a `tokio::process::Child` with `kill_on_drop(true)`
/// already set, which cleans itself up (even on a panic unwind) with no
/// further help. A daemon `spawn_detached_daemon` starts, though (every
/// daemon `turbofig mcp` spawns itself: at startup when none was running,
/// or after a version-handoff restart), is fully detached (`setsid`) and
/// held by no `Child` at all. Before this guard, cleanup for that daemon
/// was a fire-and-forget `stop_daemon` call at the very end of the test
/// function: a failed assertion anywhere before that line, or a test that
/// forgot the call, left the daemon running forever. 18 such daemons had
/// accumulated on one Mac before this fix.
///
/// Construct this as soon as the daemon is known reachable (right after a
/// `wait_for_health`, or after a handshake that itself implies one); hold it
/// for the rest of the test. On drop: sends an authenticated `/control
/// stop`, waits up to `DAEMON_GUARD_STOP_DEADLINE` for `/health` to go
/// unreachable, then falls back to killing the recorded pid directly if it
/// is somehow still running. Runs during a panic unwind too (`Drop` always
/// does, short of the whole process aborting), so a failing assertion can
/// never leak the daemon again.
pub struct DaemonGuard {
    mcp_port: u16,
    home: std::path::PathBuf,
    /// This daemon's own pid, read from `/health`'s authenticated payload
    /// (`"pid"`) at construction time, if the token file was readable and
    /// the daemon answered. `None` when it could not be determined (the
    /// daemon was already gone, or had no token yet): the drop then relies
    /// entirely on `/control stop`.
    pid: Option<u32>,
}

impl DaemonGuard {
    /// Builds a guard for the daemon on `mcp_port`, reading its pid from the
    /// authenticated `/health` payload if `<home>/token` is readable and the
    /// daemon answers. Never fails: a daemon that already went away, or
    /// whose pid could not be read for any other reason, still gets a (now
    /// inert) guard, so a caller never has to handle a `Result` just to stay
    /// safe.
    pub async fn for_daemon_on(home: &Path, mcp_port: u16) -> Self {
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("build http client");
        let pid = match tokio::fs::read_to_string(home.join("token")).await {
            Ok(token) => fetch_health_with_token(&client, mcp_port, token.trim())
                .await
                .and_then(|h| h.get("pid").and_then(Value::as_u64))
                .map(|p| p as u32),
            Err(_) => None,
        };
        Self {
            mcp_port,
            home: home.to_path_buf(),
            pid,
        }
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let mcp_port = self.mcp_port;
        let home = self.home.clone();
        let pid = self.pid;

        // `Drop::drop` is synchronous, and a test's own `#[tokio::test]`
        // runtime cannot be entered recursively from inside its own drop
        // glue (`block_on` panics "Cannot start a runtime from within a
        // runtime"). A fresh OS thread with its own throwaway
        // current-thread runtime sidesteps that; `thread::spawn` and
        // `JoinHandle::join` both still run during a panic unwind.
        let joined = std::thread::spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            rt.block_on(async {
                let Ok(client) = reqwest::Client::builder().no_proxy().build() else {
                    return;
                };
                if let Ok(token) = tokio::fs::read_to_string(home.join("token")).await {
                    let _ = client
                        .post(format!("http://127.0.0.1:{mcp_port}/control"))
                        .bearer_auth(token.trim())
                        .json(&json!({"action": "stop"}))
                        .send()
                        .await;
                }
                turbofig::spawn::wait_for_unreachable(
                    &client,
                    mcp_port,
                    DAEMON_GUARD_STOP_DEADLINE,
                )
                .await;
            });
        })
        .join();
        if joined.is_err() {
            eprintln!("DaemonGuard: the cleanup thread panicked");
        }

        // Fallback: kill the recorded pid regardless of whether /control
        // stop appeared to succeed above, so a daemon that somehow survived
        // (a stale or rotated token, a bug in /control) never outlives the
        // test either. A pid that already exited is a harmless no-op: ESRCH
        // is not checked or reported.
        #[cfg(unix)]
        if let Some(pid) = pid {
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assert_safe_turbofig_spawn_allows_any_explicit_subcommand() {
        assert_safe_turbofig_spawn(&["serve"], &[]);
        assert_safe_turbofig_spawn(&["mcp"], &[]);
        assert_safe_turbofig_spawn(&["start"], &[]);
        assert_safe_turbofig_spawn(&["stop"], &[]);
        assert_safe_turbofig_spawn(&["status"], &[]);
    }

    #[test]
    #[should_panic(expected = "must set TURBOFIG_TEST_FAKE_CLIPBOARD")]
    fn assert_safe_turbofig_spawn_rejects_a_bare_spawn_with_no_fakes() {
        assert_safe_turbofig_spawn(&[], &[]);
    }

    #[test]
    #[should_panic(expected = "must set TURBOFIG_TEST_FAKE_CLIPBOARD")]
    fn assert_safe_turbofig_spawn_rejects_a_bare_spawn_missing_the_opener_fake() {
        assert_safe_turbofig_spawn(&[], &[("TURBOFIG_TEST_FAKE_CLIPBOARD", "/tmp/x")]);
    }

    #[test]
    fn assert_safe_turbofig_spawn_allows_a_bare_spawn_with_both_fakes() {
        assert_safe_turbofig_spawn(
            &[],
            &[
                ("TURBOFIG_TEST_FAKE_CLIPBOARD", "/tmp/x"),
                ("TURBOFIG_TEST_FAKE_OPENER", "success"),
            ],
        );
    }
}
