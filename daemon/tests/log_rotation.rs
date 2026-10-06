//! Integration test for the daemon's own startup log rotation
//! (`spawn::rotate_daemon_log_and_reopen_std_streams`), exercised only under
//! `TURBOFIG_SUPERVISED=1`: a real compiled binary, with its stdout/stderr
//! pre-pointed at an oversize `daemon.log`, exactly as launchd's plist
//! `StandardOutPath`/`StandardErrorPath` would. `spawn_detached_daemon`'s own
//! rotation (the parent-side path) already has unit coverage in `spawn.rs`;
//! this is the daemon-side path, which needs a real process because it
//! `dup2`s its own stdout/stderr.

mod common;

use common::{free_port, spawn_supervised_daemon, stop_daemon, wait_for_health};

const LOG_ROTATE_THRESHOLD_BYTES: u64 = 5 * 1024 * 1024;

#[tokio::test]
async fn a_supervised_daemon_rotates_its_oversize_log_and_keeps_logging_to_the_fresh_one() {
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    std::fs::create_dir_all(home.path()).expect("create home");
    let log_path = home.path().join("daemon.log");
    let oversize = vec![b'o'; (LOG_ROTATE_THRESHOLD_BYTES + 1) as usize];
    std::fs::write(&log_path, &oversize).expect("seed an oversize log");

    let mut daemon = spawn_supervised_daemon(home.path(), mcp_port, ws_port);
    wait_for_health(&client, mcp_port).await;

    let rotated_len = std::fs::metadata(home.path().join("daemon.log.1"))
        .expect("the oversize log must have been rotated to daemon.log.1")
        .len();
    assert_eq!(
        rotated_len,
        oversize.len() as u64,
        "the rotated generation must carry the full old content"
    );

    let fresh_log =
        std::fs::read_to_string(&log_path).expect("a fresh daemon.log must exist after rotation");
    assert!(
        fresh_log.len() < oversize.len(),
        "the fresh log must not still be the oversize one"
    );
    assert!(
        fresh_log.contains("turbofig MCP listening on"),
        "the daemon's own startup lines must land in the fresh log, proving stdout was \
         reopened onto it rather than still writing into the renamed-away daemon.log.1: {fresh_log:?}"
    );

    stop_daemon(&client, mcp_port, home.path()).await;
    let _ = daemon.wait().await;
}
