//! Integration tests for the restructured CLI: `start`, `stop`, and a second
//! `serve` refusing to run alongside an existing daemon. Real compiled
//! binary, real processes, test ports, a temp home.

mod common;

use common::{fetch_health, free_port, run_turbofig, spawn_daemon, stop_daemon, wait_for_health};

#[tokio::test]
async fn start_detaches_a_daemon_when_none_is_running_and_prints_version_and_ports() {
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let (status, stdout, stderr) = run_turbofig(&["start"], home.path(), mcp_port, ws_port).await;
    assert!(status.success(), "start must exit 0, stderr: {stderr}");
    assert!(stdout.contains("started"), "stdout: {stdout}");
    assert!(stdout.contains(&mcp_port.to_string()), "stdout: {stdout}");
    assert!(stdout.contains(&ws_port.to_string()), "stdout: {stdout}");

    // By the time `start` printed, the daemon it spawned must already answer.
    wait_for_health(&client, mcp_port).await;

    stop_daemon(&client, mcp_port, home.path()).await;
}

#[tokio::test]
async fn start_when_already_running_prints_already_running_and_exits_zero() {
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let mut daemon = spawn_daemon(home.path(), mcp_port, ws_port, None);
    wait_for_health(&client, mcp_port).await;

    let (status, stdout, stderr) = run_turbofig(&["start"], home.path(), mcp_port, ws_port).await;
    assert!(
        status.success(),
        "start must still exit 0, stderr: {stderr}"
    );
    assert!(stdout.contains("already running"), "stdout: {stdout}");
    assert!(stdout.contains(&mcp_port.to_string()), "stdout: {stdout}");

    stop_daemon(&client, mcp_port, home.path()).await;
    let _ = daemon.wait().await;
}

#[tokio::test]
async fn stop_stops_a_running_daemon_and_health_goes_unreachable() {
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let mut daemon = spawn_daemon(home.path(), mcp_port, ws_port, None);
    wait_for_health(&client, mcp_port).await;

    let (status, stdout, stderr) = run_turbofig(&["stop"], home.path(), mcp_port, ws_port).await;
    assert!(status.success(), "stop must exit 0, stderr: {stderr}");
    assert!(stdout.contains("stopped"), "stdout: {stdout}");

    let exit_status = daemon.wait().await.expect("daemon must exit");
    assert!(exit_status.success());
    assert!(
        fetch_health(&client, mcp_port).await.is_none(),
        "the daemon must be unreachable after stop"
    );
}

#[tokio::test]
async fn stop_when_nothing_is_running_is_a_no_op() {
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();

    let (status, stdout, stderr) = run_turbofig(&["stop"], home.path(), mcp_port, ws_port).await;
    assert!(
        status.success(),
        "stop with nothing running must still exit 0, stderr: {stderr}"
    );
    assert!(stdout.contains("no daemon"), "stdout: {stdout}");
}

#[tokio::test]
async fn a_second_serve_exits_at_once_with_the_already_running_message() {
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let mut first = spawn_daemon(home.path(), mcp_port, ws_port, None);
    wait_for_health(&client, mcp_port).await;

    let (status, _stdout, stderr) = run_turbofig(&["serve"], home.path(), mcp_port, ws_port).await;
    assert_eq!(status.code(), Some(1), "a second serve must exit 1");
    assert!(
        stderr.contains("turbofig is already running"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains(&mcp_port.to_string()), "stderr: {stderr}");

    stop_daemon(&client, mcp_port, home.path()).await;
    let _ = first.wait().await;
}
