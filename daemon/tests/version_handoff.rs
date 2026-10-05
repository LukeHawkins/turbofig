//! Integration tests for the version-handoff restart: `turbofig mcp`
//! comparing the running daemon's `/health` version against its own at
//! startup, and restarting the daemon when it is older.
//!
//! Every daemon here is started with `TURBOFIG_TEST_VERSION_OVERRIDE` (an
//! "old daemon" fixture) or a proxy with `TURBOFIG_TEST_OWN_VERSION_OVERRIDE`
//! (an "old proxy" fixture): debug-build-only escape hatches
//! (`mcp::reported_version`, `proxy::own_version`) that let these tests
//! simulate a version mismatch without two real builds. Real processes, test
//! ports, a temp home; every daemon spawned is stopped at the end via an
//! authenticated `POST /control stop`.

mod common;

use common::{
    fetch_health, free_port, handshake, spawn_daemon, spawn_proxy, stdio_call_tool, stop_daemon,
    tool_call_status, wait_for_health,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tokio_tungstenite::{connect_async, tungstenite::Message as TtMessage};

/// Clearly older than any real build of this crate (which is pre-1.0 today,
/// and will never retroactively become 0.0.x again).
const OLD_VERSION: &str = "0.0.1";

// ── (a) an old daemon plus a new proxy gives a restart ──────────────────────

#[tokio::test]
async fn old_daemon_and_new_proxy_triggers_a_restart_and_health_reports_the_new_version() {
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let mut old_daemon = spawn_daemon(home.path(), mcp_port, ws_port, Some(OLD_VERSION));
    wait_for_health(&client, mcp_port).await;
    let before = fetch_health(&client, mcp_port).await.expect("health");
    assert_eq!(before["version"], json!(OLD_VERSION));

    let mut proxy = spawn_proxy(home.path(), mcp_port, ws_port, false, None);
    let (mut writer, mut reader) = handshake(&mut proxy).await;
    let resp = stdio_call_tool(&mut writer, &mut reader, 2, "turbofig_status", json!({})).await;
    assert_eq!(
        tool_call_status(&resp)["ok"],
        json!(true),
        "the proxy must still serve a status call after the handoff: {resp}"
    );

    let status = old_daemon
        .wait()
        .await
        .expect("wait for the old daemon to exit");
    assert!(status.success(), "the old daemon must exit 0: {status:?}");

    let after = fetch_health(&client, mcp_port).await.expect("health");
    assert_eq!(
        after["version"],
        json!(env!("CARGO_PKG_VERSION")),
        "the daemon /health reports after the handoff must be this build's real version"
    );

    stop_daemon(&client, mcp_port, home.path()).await;
}

// ── (b) a new daemon plus an old proxy gives no restart ─────────────────────

#[tokio::test]
async fn new_daemon_and_old_proxy_gives_no_restart_and_the_daemon_pid_stays_the_same() {
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    // The daemon reports its real (current) version, unmodified: from an
    // old proxy's point of view this daemon is newer than itself.
    let mut daemon = spawn_daemon(home.path(), mcp_port, ws_port, None);
    wait_for_health(&client, mcp_port).await;
    let daemon_pid = daemon.id().expect("daemon has a pid");

    // The proxy believes its own version is clearly older than the daemon's.
    let mut proxy = spawn_proxy(home.path(), mcp_port, ws_port, false, Some(OLD_VERSION));
    let (mut writer, mut reader) = handshake(&mut proxy).await;
    let resp = stdio_call_tool(&mut writer, &mut reader, 2, "turbofig_status", json!({})).await;
    assert_eq!(tool_call_status(&resp)["ok"], json!(true));

    // No restart: the exact same daemon process is still running.
    assert_eq!(
        daemon.try_wait().expect("try_wait"),
        None,
        "an old proxy must never cause a newer daemon to exit"
    );
    assert_eq!(
        daemon.id(),
        Some(daemon_pid),
        "the daemon pid must be unchanged"
    );

    let health = fetch_health(&client, mcp_port).await.expect("health");
    assert_eq!(
        health["version"],
        json!(env!("CARGO_PKG_VERSION")),
        "the daemon must still report its own real, newer version"
    );

    stop_daemon(&client, mcp_port, home.path()).await;
    let _ = daemon.wait().await;
}

// ── (c) 2 new proxies plus 1 old daemon give exactly 1 new daemon ──────────

#[tokio::test]
async fn two_new_proxies_and_one_old_daemon_give_exactly_one_new_daemon() {
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let mut old_daemon = spawn_daemon(home.path(), mcp_port, ws_port, Some(OLD_VERSION));
    wait_for_health(&client, mcp_port).await;

    // Started back to back against the same old daemon: both must decide a
    // restart is needed, race the authenticated /control call (only one
    // really triggers it; control.rs's try_begin_draining makes the other a
    // no-op ack), then race the respawn, with the port bind deciding which
    // one actually ends up serving.
    let mut proxy_a = spawn_proxy(home.path(), mcp_port, ws_port, false, None);
    let mut proxy_b = spawn_proxy(home.path(), mcp_port, ws_port, false, None);

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
        assert_eq!(
            tool_call_status(resp)["ok"],
            json!(true),
            "proxy {label} must still work after the handoff: {resp}"
        );
    }

    let status = old_daemon.wait().await.expect("wait old daemon");
    assert!(status.success());

    let health = fetch_health(&client, mcp_port).await.expect("health");
    assert_eq!(health["version"], json!(env!("CARGO_PKG_VERSION")));

    // Exactly one new daemon ever bound the port after the handoff: the
    // shared daemon.log carries the old daemon's one "listening on" line
    // plus exactly one more (the new daemon's), never a third.
    let log = tokio::fs::read_to_string(home.path().join("daemon.log"))
        .await
        .expect("read daemon.log");
    let listening_lines = log
        .lines()
        .filter(|l| l.contains("Turbofig MCP listening on") && l.contains(&mcp_port.to_string()))
        .count();
    assert_eq!(
        listening_lines, 2,
        "the old daemon's start plus exactly one new daemon's start, got log:\n{log}"
    );

    stop_daemon(&client, mcp_port, home.path()).await;
}

// ── (d) an in-flight job finishes before the old daemon exits ──────────────

/// Polls the daemon's `/health` `connectedFiles` until `file_key` appears.
async fn wait_for_file_connected(client: &reqwest::Client, mcp_port: u16, file_key: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(health) = fetch_health(client, mcp_port).await {
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

#[tokio::test]
async fn an_in_flight_job_finishes_before_the_old_daemon_exits_during_a_restart() {
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let mut old_daemon = spawn_daemon(home.path(), mcp_port, ws_port, Some(OLD_VERSION));
    wait_for_health(&client, mcp_port).await;
    let token = tokio::fs::read_to_string(home.path().join("token"))
        .await
        .expect("read token")
        .trim()
        .to_owned();

    // A mock plugin that deliberately sits on an EXECUTE request for a while
    // before replying, so the job stays in flight across the restart's drain.
    const JOB_DELAY: Duration = Duration::from_millis(1500);
    let (mut plugin_ws, _) = connect_async(format!("ws://127.0.0.1:{ws_port}/?token={token}"))
        .await
        .expect("mock plugin connect");
    plugin_ws
        .send(TtMessage::Text(
            json!({"type": "FILE_INFO", "fileKey": "slow-file", "name": "Slow File"}).to_string(),
        ))
        .await
        .expect("send FILE_INFO");
    wait_for_file_connected(&client, mcp_port, "slow-file").await;

    let plugin_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = plugin_ws.next().await {
            let TtMessage::Text(text) = msg else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            if v["type"] == "EXECUTE" {
                if let Some(id) = v.get("requestId").and_then(Value::as_u64) {
                    tokio::time::sleep(JOB_DELAY).await;
                    let reply = json!({"type": "RESULT", "requestId": id, "ok": true, "result": 1});
                    let _ = plugin_ws.send(TtMessage::Text(reply.to_string())).await;
                }
            }
        }
    });

    // Fire the slow job without awaiting it yet.
    let job_client = client.clone();
    let job_url = format!("http://127.0.0.1:{mcp_port}/job");
    let job_task = tokio::spawn(async move {
        job_client
            .post(job_url)
            .json(&json!({"op": "execute", "fileKey": "slow-file", "code": "return 1;"}))
            .send()
            .await
            .expect("send job")
            .json::<Value>()
            .await
            .expect("job response json")
    });

    // Give the job a head start so it is registered as in-flight before the
    // restart begins draining.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let restart_requested_at = Instant::now();

    let mut proxy = spawn_proxy(home.path(), mcp_port, ws_port, false, None);
    let (mut writer, mut reader) = handshake(&mut proxy).await;
    let resp = stdio_call_tool(&mut writer, &mut reader, 2, "turbofig_status", json!({})).await;
    assert_eq!(tool_call_status(&resp)["ok"], json!(true));

    let job_body = job_task.await.expect("job task");
    assert_eq!(
        job_body["ok"],
        json!(true),
        "the in-flight job must still succeed across the restart: {job_body}"
    );

    let status = old_daemon.wait().await.expect("wait old daemon");
    assert!(status.success());
    let elapsed = restart_requested_at.elapsed();
    assert!(
        elapsed >= JOB_DELAY - Duration::from_millis(200),
        "the old daemon must not exit before the slow in-flight job finished \
         (restart to exit took only {elapsed:?}, the job alone takes {JOB_DELAY:?})"
    );

    plugin_task.abort();
    stop_daemon(&client, mcp_port, home.path()).await;
}
