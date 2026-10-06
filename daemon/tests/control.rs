//! Integration test for the authenticated `POST /control` path.
//!
//! A successful call drains and then calls `std::process::exit(0)`. Calling
//! the handler in-process would kill the whole test binary, so this spawns
//! the real compiled daemon as a child process on ephemeral test ports and a
//! temp home, so the exit only ever ends the child. The unauthorized paths
//! (missing/wrong token) never reach the exit branch, so those are covered
//! in-process in `daemon/src/mcp.rs`'s own test module instead.

mod common;

use common::assert_safe_turbofig_spawn;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_turbofig");

/// Kills and reaps the child on drop, so a failing assertion never leaks a
/// daemon process bound to the test's ephemeral ports.
struct DaemonChild(Child);

impl Drop for DaemonChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind an ephemeral port")
        .local_addr()
        .expect("local addr")
        .port()
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

async fn spawn_daemon(home: &std::path::Path, mcp_port: u16, ws_port: u16) -> DaemonChild {
    spawn_daemon_with_env(home, mcp_port, ws_port, &[]).await
}

async fn spawn_daemon_with_env(
    home: &std::path::Path,
    mcp_port: u16,
    ws_port: u16,
    extra_env: &[(&str, &str)],
) -> DaemonChild {
    // `serve` must be explicit: a bare `turbofig` is the first-run helper,
    // which starts an untracked detached daemon instead of this child.
    assert_safe_turbofig_spawn(&["serve"], &[]);
    let mut cmd = Command::new(BIN);
    cmd.arg("serve")
        .env("TURBOFIG_MCP_PORT", mcp_port.to_string())
        .env("TURBOFIG_WS_PORT", ws_port.to_string())
        .env("TURBOFIG_BRIDGE_DIR", home)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    let child = cmd.spawn().expect("spawn the turbofig daemon binary");
    DaemonChild(child)
}

/// Runs one `/control` call (`stop` or `restart`) against a freshly spawned
/// daemon with the right token, and asserts it replies 202 at once (well
/// under the drain deadline, proving the response does not wait for the
/// drain), then drains (no in-flight jobs, so that part is instant too) and
/// exits 0 shortly after responding.
async fn assert_control_drains_and_exits(action: &str) {
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let mut daemon = spawn_daemon(home.path(), mcp_port, ws_port).await;

    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("build http client");
    wait_for_health(&client, mcp_port).await;

    let token = tokio::fs::read_to_string(home.path().join("token"))
        .await
        .expect("read the pairing token the daemon wrote on startup")
        .trim()
        .to_owned();
    assert!(
        !token.is_empty(),
        "the pairing token file must not be empty"
    );

    let request_started = Instant::now();
    let resp = client
        .post(format!("http://127.0.0.1:{mcp_port}/control"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"action": action}))
        .send()
        .await
        .expect("send POST /control");
    let response_elapsed = request_started.elapsed();
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let body: serde_json::Value = resp.json().await.expect("parse /control response");
    assert_eq!(body["ok"], serde_json::json!(true));
    assert_eq!(body["action"], serde_json::json!(action));
    assert_eq!(body["draining"], serde_json::json!(true));
    assert!(
        response_elapsed < Duration::from_secs(2),
        "the response must arrive at once, not after the (up to 60s) drain: took {response_elapsed:?}"
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = daemon.0.try_wait().expect("try_wait") {
            break status;
        }
        if Instant::now() >= deadline {
            panic!("daemon did not exit after an authenticated /control {action}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(
        status.success(),
        "daemon must exit 0 after /control {action}, got {status:?}"
    );
}

#[tokio::test]
async fn control_stop_with_the_right_token_drains_and_exits_zero() {
    let _serial = common::serial_process_test().await;
    assert_control_drains_and_exits("stop").await;
}

#[tokio::test]
async fn control_restart_with_the_right_token_drains_and_exits_zero() {
    let _serial = common::serial_process_test().await;
    assert_control_drains_and_exits("restart").await;
}

/// A supervised restart alone exits `SUPERVISED_RESTART_EXIT_CODE` (75), so
/// launchd restarts the daemon. A stop that lands while that restart is
/// still draining must override it: the daemon must exit 0 instead, so
/// launchd's `KeepAlive: {SuccessfulExit: false}` leaves it stopped, matching
/// `turbofig stop`'s own 202 reply. See `AppState::request_stop`.
#[tokio::test]
async fn control_stop_during_a_supervised_restart_drain_overrides_the_exit_code() {
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let mut daemon = spawn_daemon_with_env(
        home.path(),
        mcp_port,
        ws_port,
        &[
            ("TURBOFIG_SUPERVISED", "1"),
            ("TURBOFIG_TEST_CONTROL_EXIT_GRACE_MS", "3000"),
        ],
    )
    .await;

    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("build http client");
    wait_for_health(&client, mcp_port).await;

    let token = tokio::fs::read_to_string(home.path().join("token"))
        .await
        .expect("read the pairing token the daemon wrote on startup")
        .trim()
        .to_owned();

    let restart_resp = client
        .post(format!("http://127.0.0.1:{mcp_port}/control"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"action": "restart"}))
        .send()
        .await
        .expect("send POST /control restart");
    assert_eq!(restart_resp.status(), reqwest::StatusCode::ACCEPTED);

    // Sent right after the restart response: the restart's drain (no jobs
    // in flight) completes almost at once, but the background task still
    // sleeps its exit grace before it exits. The test raises that grace to
    // 3 s (debug-only override), so this stop lands in time even on a slow
    // CI runner.
    let stop_resp = client
        .post(format!("http://127.0.0.1:{mcp_port}/control"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"action": "stop"}))
        .send()
        .await
        .expect("send POST /control stop");
    assert_eq!(stop_resp.status(), reqwest::StatusCode::ACCEPTED);

    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = daemon.0.try_wait().expect("try_wait") {
            break status;
        }
        if Instant::now() >= deadline {
            panic!("daemon did not exit after restart followed by stop");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(
        status.success(),
        "a stop during a restart's drain must still exit 0, got {status:?}"
    );
}
