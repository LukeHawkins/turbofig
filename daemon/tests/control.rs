//! Integration test for the authenticated `POST /control` path.
//!
//! A successful call drains and then calls `std::process::exit(0)`. Calling
//! the handler in-process would kill the whole test binary, so this spawns
//! the real compiled daemon as a child process on ephemeral test ports and a
//! temp home, so the exit only ever ends the child. The unauthorized paths
//! (missing/wrong token) never reach the exit branch, so those are covered
//! in-process in `daemon/src/mcp.rs`'s own test module instead.

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
    // `serve` must be explicit: a bare `turbofig` is the first-run helper,
    // which starts an untracked detached daemon instead of this child.
    let child = Command::new(BIN)
        .arg("serve")
        .env("TURBOFIG_MCP_PORT", mcp_port.to_string())
        .env("TURBOFIG_WS_PORT", ws_port.to_string())
        .env("TURBOFIG_BRIDGE_DIR", home)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the turbofig daemon binary");
    DaemonChild(child)
}

/// Runs one `/control` call (`stop` or `restart`) against a freshly spawned
/// daemon with the right token, and asserts it drains (no in-flight jobs, so
/// this is instant) and exits 0 shortly after responding.
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

    let resp = client
        .post(format!("http://127.0.0.1:{mcp_port}/control"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"action": action}))
        .send()
        .await
        .expect("send POST /control");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.expect("parse /control response");
    assert_eq!(body["ok"], serde_json::json!(true));
    assert_eq!(body["action"], serde_json::json!(action));

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
    assert_control_drains_and_exits("stop").await;
}

#[tokio::test]
async fn control_restart_with_the_right_token_drains_and_exits_zero() {
    assert_control_drains_and_exits("restart").await;
}
