//! Integration tests for `turbofig js`: a real daemon on a temp bridge folder
//! and free ports, a real `turbofig js` child per call. No plugin connects, so
//! JS code gets an error result, which is still a result (exit 0).

mod common;

use common::{free_port, stop_daemon, wait_for_health, BIN};
use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

async fn run_js(
    home: &Path,
    mcp_port: u16,
    ws_port: u16,
    args: &[&str],
    stdin: &str,
) -> (std::process::ExitStatus, String, String) {
    run_js_env(home, home, mcp_port, ws_port, args, stdin).await
}

/// Like `run_js`, but `env_bridge` is what `TURBOFIG_BRIDGE_DIR` holds (the
/// `--dir` flag in `args` must win over it).
async fn run_js_env(
    home: &Path,
    env_bridge: &Path,
    mcp_port: u16,
    ws_port: u16,
    args: &[&str],
    stdin: &str,
) -> (std::process::ExitStatus, String, String) {
    let mut child = Command::new(BIN)
        .arg("js")
        .args(args)
        .env("HOME", home)
        .env("TURBOFIG_MCP_PORT", mcp_port.to_string())
        .env("TURBOFIG_WS_PORT", ws_port.to_string())
        .env("TURBOFIG_BRIDGE_DIR", env_bridge)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn turbofig js");
    let mut si = child.stdin.take().unwrap();
    si.write_all(stdin.as_bytes()).await.unwrap();
    drop(si);
    let out = child.wait_with_output().await.unwrap();
    (
        out.status,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[tokio::test]
async fn js_runs_status_and_code_jobs_and_rejects_empty_stdin() {
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    // No daemon yet: `turbofig js` starts it on demand, then answers.
    let (status, stdout, stderr) =
        run_js(home.path(), mcp_port, ws_port, &[], "{\"op\":\"status\"}\n").await;
    assert!(
        status.success(),
        "status job must exit 0: {stdout} {stderr}"
    );
    wait_for_health(&client, mcp_port).await;
    let v: Value = serde_json::from_str(stdout.trim()).expect("result JSON");
    assert_eq!(v["ok"], Value::Bool(true), "{stdout}");
    assert!(stdout.ends_with('\n'));

    // Plain JS with no plugin: an error result, still exit 0.
    let (status, stdout, stderr) = run_js(
        home.path(),
        mcp_port,
        ws_port,
        &["--file", "nofile"],
        "return figma.root.name;\n",
    )
    .await;
    assert!(status.success(), "code job must exit 0: {stdout} {stderr}");
    let v: Value = serde_json::from_str(stdout.trim()).expect("result JSON");
    assert_eq!(v["ok"], Value::Bool(false), "{stdout}");
    assert!(v["code"].is_string(), "{stdout}");

    // The result file is deleted after the read.
    let left: Vec<_> = std::fs::read_dir(home.path().join("outbox"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
        .collect();
    assert!(left.is_empty(), "outbox results must be deleted: {left:?}");

    // Empty stdin: usage on stderr, exit 2, nothing on stdout.
    let (status, stdout, stderr) = run_js(home.path(), mcp_port, ws_port, &[], "  \n").await;
    assert_eq!(status.code(), Some(2));
    assert!(stdout.is_empty());
    assert!(stderr.contains("usage: turbofig js"), "{stderr}");

    // Timing: median of 50 status round trips.
    let mut times = Vec::new();
    for _ in 0..50 {
        let t = Instant::now();
        let (status, _, _) =
            run_js(home.path(), mcp_port, ws_port, &[], "{\"op\":\"status\"}").await;
        assert!(status.success());
        times.push(t.elapsed());
    }
    times.sort();
    let median: Duration = times[25];
    println!("TIMING status round trip median: {} ms", median.as_millis());

    stop_daemon(&client, mcp_port, home.path()).await;
}

fn inbox_outbox_entries(home: &Path) -> Vec<String> {
    ["inbox", "outbox"]
        .iter()
        .flat_map(|d| std::fs::read_dir(home.join(d)).unwrap().flatten())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect()
}

#[tokio::test]
async fn js_size_cap_shot_usage_and_a_bridge_path_with_a_space() {
    let _serial = common::serial_process_test().await;
    let root = tempfile::tempdir().expect("temp root");
    let home = root.path().join("bridge with space");
    let mcp_port = free_port();
    let ws_port = free_port();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let (status, stdout, stderr) =
        run_js(&home, mcp_port, ws_port, &[], "{\"op\":\"status\"}").await;
    assert!(status.success(), "{stdout} {stderr}");
    wait_for_health(&client, mcp_port).await;
    assert!(inbox_outbox_entries(&home).is_empty());

    // Near the cap: the daemon gets the job and answers (no plugin: error result).
    let cap = 30 * 1024 * 1024;
    let near = format!("//{}", "a".repeat(cap - 1024));
    let (status, stdout, stderr) = run_js(&home, mcp_port, ws_port, &[], &near).await;
    assert!(status.success(), "near-cap job: {stderr}");
    let v: Value = serde_json::from_str(stdout.trim()).expect("result JSON");
    assert_eq!(v["ok"], Value::Bool(false), "{stdout}");

    // Over the cap: exit 2, a clear message, nothing in the inbox.
    let over = "a".repeat(cap + 1);
    let (status, stdout, stderr) = run_js(&home, mcp_port, ws_port, &[], &over).await;
    assert_eq!(status.code(), Some(2));
    assert!(stdout.is_empty());
    assert!(stderr.contains("larger than 30 MiB"), "{stderr}");
    assert!(inbox_outbox_entries(&home).is_empty());

    // --shot with a job that is not execute: exit 2, nothing sent.
    let (status, stdout, stderr) =
        run_js(&home, mcp_port, ws_port, &["--shot"], "{\"op\":\"status\"}").await;
    assert_eq!(status.code(), Some(2));
    assert!(stdout.is_empty());
    assert!(stderr.contains("--shot"), "{stderr}");

    // --shot with code and no plugin: the first job fails, so one line only.
    let (status, stdout, _) = run_js(&home, mcp_port, ws_port, &["--shot"], "return '1:2';").await;
    assert!(status.success());
    assert_eq!(stdout.trim().lines().count(), 1, "{stdout}");
    assert!(inbox_outbox_entries(&home).is_empty());

    stop_daemon(&client, mcp_port, &home).await;
}

#[tokio::test]
async fn twenty_parallel_js_calls_all_get_their_own_result() {
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let (status, _, stderr) =
        run_js(home.path(), mcp_port, ws_port, &[], "{\"op\":\"status\"}").await;
    assert!(status.success(), "{stderr}");
    wait_for_health(&client, mcp_port).await;

    let mut tasks = Vec::new();
    for _ in 0..20 {
        let home = home.path().to_path_buf();
        tasks.push(tokio::spawn(async move {
            run_js(&home, mcp_port, ws_port, &[], "{\"op\":\"status\"}").await
        }));
    }
    for t in tasks {
        let (status, stdout, stderr) = t.await.unwrap();
        assert!(status.success(), "{stdout} {stderr}");
        let v: Value = serde_json::from_str(stdout.trim()).expect("result JSON");
        assert_eq!(v["ok"], Value::Bool(true), "{stdout}");
        assert_eq!(stdout.trim().lines().count(), 1);
    }
    assert!(inbox_outbox_entries(home.path()).is_empty());

    stop_daemon(&client, mcp_port, home.path()).await;
}

#[tokio::test]
async fn a_second_daemon_on_one_bridge_folder_exits_but_another_folder_starts() {
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let other = tempfile::tempdir().expect("other home");
    let (p1, w1) = (free_port(), free_port());
    let (p2, w2) = (free_port(), free_port());
    let (p3, w3) = (free_port(), free_port());
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let _first = common::spawn_daemon(home.path(), p1, w1, None);
    wait_for_health(&client, p1).await;

    // Same folder, different ports: refused by the lock.
    let out = Command::new(BIN)
        .arg("serve")
        .env("HOME", home.path())
        .env("TURBOFIG_MCP_PORT", p2.to_string())
        .env("TURBOFIG_WS_PORT", w2.to_string())
        .env("TURBOFIG_BRIDGE_DIR", home.path())
        .output()
        .await
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("another turbofig daemon is already using"),
        "{stderr}"
    );
    assert!(client
        .get(format!("http://127.0.0.1:{p2}/health"))
        .send()
        .await
        .is_err());

    // Different folder: starts fine.
    let _third = common::spawn_daemon(other.path(), p3, w3, None);
    wait_for_health(&client, p3).await;

    stop_daemon(&client, p3, other.path()).await;
    stop_daemon(&client, p1, home.path()).await;
}

#[tokio::test]
async fn js_fails_fast_with_daemon_stopped_when_the_daemon_dies_mid_job() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let (mcp_port, ws_port) = (free_port(), free_port());
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let mut daemon = common::spawn_daemon(home.path(), mcp_port, ws_port, None);
    wait_for_health(&client, mcp_port).await;

    // A mock plugin connects and never answers, so an execute job hangs
    // until the daemon's 30 s deadline.
    let token = std::fs::read_to_string(home.path().join("token")).unwrap();
    let mut plugin = common::connect_plugin(ws_port, token.trim()).await;
    plugin
        .send(Message::Text(
            json!({"type": "FILE_INFO", "fileKey": "abc", "name": "F"}).to_string(),
        ))
        .await
        .unwrap();
    loop {
        let health = common::fetch_health_with_token(&client, mcp_port, token.trim()).await;
        if health.is_some_and(|h| h.to_string().contains("\"abc\"")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let h = home.path().to_path_buf();
    let job = tokio::spawn(async move {
        let t = Instant::now();
        let r = run_js(&h, mcp_port, ws_port, &[], "return 1;").await;
        (r, t.elapsed())
    });
    // The EXECUTE frame reached the plugin: the job is in flight. Kill the daemon now.
    loop {
        match plugin.next().await {
            Some(Ok(Message::Text(t))) if t.contains("\"EXECUTE\"") => break,
            Some(Ok(_)) => continue,
            other => panic!("plugin socket ended before a job arrived: {other:?}"),
        }
    }
    daemon.start_kill().unwrap();
    daemon.wait().await.unwrap();

    let ((status, stdout, _), elapsed) = tokio::time::timeout(Duration::from_secs(15), job)
        .await
        .expect("js must not wait out the 35 s timeout")
        .unwrap();
    assert_eq!(status.code(), Some(1), "{stdout}");
    let v: Value = serde_json::from_str(stdout.trim()).expect("one JSON line");
    assert_eq!(v["code"], "daemon_stopped", "{stdout}");
    assert!(v["error"].as_str().unwrap().contains("status, then retry"));
    assert!(v["outbox"].as_str().unwrap().contains("/outbox/"));
    assert!(elapsed < Duration::from_secs(8), "took {elapsed:?}");
}

#[tokio::test]
async fn dir_flag_wins_over_the_env_folder_and_works_with_a_space() {
    let _serial = common::serial_process_test().await;
    let root = tempfile::tempdir().expect("temp root");
    let dir = root.path().join("my bridge");
    let env_dir = root.path().join("env bridge");
    let (mcp_port, ws_port) = (free_port(), free_port());
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    // No daemon holds the lock yet: js starts one in `--dir`.
    let (status, stdout, stderr) = run_js_env(
        root.path(),
        &env_dir,
        mcp_port,
        ws_port,
        &["--dir", dir.to_str().unwrap()],
        "{\"op\":\"status\"}",
    )
    .await;
    assert!(status.success(), "{stdout} {stderr}");
    wait_for_health(&client, mcp_port).await;
    assert!(dir.join("daemon.lock").exists());
    assert!(!env_dir.exists(), "the env folder must stay untouched");
    let v: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["ok"], Value::Bool(true));

    // A symlinked --dir is refused, with no daemon started.
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&dir, &link).unwrap();
    let (status, stdout, stderr) = run_js_env(
        root.path(),
        &env_dir,
        mcp_port,
        ws_port,
        &["--dir", link.to_str().unwrap()],
        "{\"op\":\"status\"}",
    )
    .await;
    assert_eq!(status.code(), Some(1), "{stdout}");
    assert!(stderr.contains("symlink"), "{stderr}");

    stop_daemon(&client, mcp_port, &dir).await;
}
