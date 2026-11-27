//! Integration tests for the restructured CLI: `start`, `stop`, and a second
//! `serve` refusing to run alongside an existing daemon. Real compiled
//! binary, real processes, test ports, a temp home.

mod common;

use common::{fetch_health, run_turbofig, spawn_daemon, stop_daemon, wait_for_health};

#[tokio::test]
async fn start_detaches_a_daemon_when_none_is_running_and_prints_version_and_ports() {
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let (mcp_port, ws_port) = common::free_port_pair();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let _guard = common::DaemonGuard::new(home.path(), mcp_port);

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
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let (mcp_port, ws_port) = common::free_port_pair();
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
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let (mcp_port, ws_port) = common::free_port_pair();
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

/// Only the CLI `turbofig stop` leaves `stopped-by-user`. The pre-stop of
/// `autostart on` (app variant), app Quit and `uninstall` stop the daemon
/// through `/control stop`, which leaves no marker: a hand-opened app then
/// still starts the daemon.
#[tokio::test]
async fn only_the_stop_command_leaves_the_marker_and_a_control_stop_does_not() {
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let marker = home.path().join("stopped-by-user");
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    // The pre-stop path: `/control stop` only.
    let (mcp_port, ws_port) = common::free_port_pair();
    let mut daemon = spawn_daemon(home.path(), mcp_port, ws_port, None);
    wait_for_health(&client, mcp_port).await;
    stop_daemon(&client, mcp_port, home.path()).await;
    let _ = daemon.wait().await;
    assert!(!marker.exists(), "a /control stop must leave no marker");

    // `turbofig stop`: the marker, owner-only.
    let (mcp_port, ws_port) = common::free_port_pair();
    let mut daemon = spawn_daemon(home.path(), mcp_port, ws_port, None);
    wait_for_health(&client, mcp_port).await;
    let (status, _out, stderr) = run_turbofig(&["stop"], home.path(), mcp_port, ws_port).await;
    assert!(status.success(), "stderr: {stderr}");
    let _ = daemon.wait().await;
    assert!(marker.exists(), "turbofig stop must leave the marker");
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&marker).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // Nothing runs and the user asks again: the marker stays.
    let (status, _out, stderr) = run_turbofig(&["stop"], home.path(), mcp_port, ws_port).await;
    assert!(status.success(), "stderr: {stderr}");
    assert!(marker.exists());
}

/// `turbofig stop` on a missing home (a fresh Mac, or after `uninstall
/// --purge`) is silent: no warning and no folder created.
#[tokio::test]
async fn stop_on_a_missing_home_prints_no_marker_warning() {
    let _serial = common::serial_process_test().await;
    let parent = tempfile::tempdir().expect("temp parent");
    let home = parent.path().join("no-such-home");
    let (mcp_port, ws_port) = common::free_port_pair();
    let (status, stdout, stderr) = run_turbofig(&["stop"], &home, mcp_port, ws_port).await;
    assert!(status.success(), "stderr: {stderr}");
    assert!(stdout.contains("no daemon"), "stdout: {stdout}");
    assert!(!stderr.contains("stop marker"), "stderr: {stderr}");
    assert!(!home.exists(), "stop must not create the home");
}

/// `turbofig stop` with a running daemon whose token file was removed (or
/// no longer matches) must never say "no daemon appears to be running": the
/// daemon is reachable on /health, so that would be a lie. It must print the
/// token-trouble message and exit 1 instead, and the daemon must still be
/// running afterward (stop could not authenticate the request at all).
#[tokio::test]
async fn stop_with_a_missing_token_file_reports_trouble_not_nothing_running() {
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let (mcp_port, ws_port) = common::free_port_pair();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let mut daemon = spawn_daemon(home.path(), mcp_port, ws_port, None);
    wait_for_health(&client, mcp_port).await;

    // A deleted token is rewritten by the daemon within a second, so this
    // test swaps in a different (changed) token to keep the trouble case.
    let token_path = home.path().join("token");
    tokio::fs::remove_file(&token_path)
        .await
        .expect("remove the token file");
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&token_path)
            .expect("write a different token");
        f.write_all("0".repeat(64).as_bytes()).expect("write");
    }

    let (status, stdout, stderr) = run_turbofig(&["stop"], home.path(), mcp_port, ws_port).await;
    assert_eq!(status.code(), Some(1), "stop must exit 1, stdout: {stdout}");
    assert!(
        stderr.contains("token file is missing or changed"),
        "stderr: {stderr}"
    );
    assert!(
        fetch_health(&client, mcp_port).await.is_some(),
        "the daemon must still be running: stop never authenticated"
    );

    let _ = daemon.kill().await;
}

#[tokio::test]
async fn stop_when_nothing_is_running_is_a_no_op() {
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let (mcp_port, ws_port) = common::free_port_pair();

    let (status, stdout, stderr) = run_turbofig(&["stop"], home.path(), mcp_port, ws_port).await;
    assert!(
        status.success(),
        "stop with nothing running must still exit 0, stderr: {stderr}"
    );
    assert!(stdout.contains("no daemon"), "stdout: {stdout}");
}

#[tokio::test]
async fn a_second_serve_exits_at_once_with_the_already_running_message() {
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let (mcp_port, ws_port) = common::free_port_pair();
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
