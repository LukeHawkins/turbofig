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
    run_js_full(None, &[], home, env_bridge, mcp_port, ws_port, args, stdin).await
}

/// The general form: a working directory and extra env vars as well.
#[allow(clippy::too_many_arguments)]
async fn run_js_full(
    cwd: Option<&Path>,
    extra_env: &[(&str, &str)],
    home: &Path,
    env_bridge: &Path,
    mcp_port: u16,
    ws_port: u16,
    args: &[&str],
    stdin: &str,
) -> (std::process::ExitStatus, String, String) {
    let mut cmd = Command::new(BIN);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd
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

    // The lock file always holds one complete JSON body (never empty).
    let info: Value = serde_json::from_str(
        std::fs::read_to_string(home.join("daemon.lock"))
            .unwrap()
            .trim(),
    )
    .expect("lock info is valid JSON");
    assert_eq!(info["locked"], Value::Bool(true));
    assert!(info["pid"].as_u64().unwrap() > 0);
    assert!(info["requestTimeoutMs"].as_u64().unwrap() > 0);

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
    // The daemon had claimed the job (the plugin got EXECUTE): it may have run.
    assert_eq!(v["code"], "started_unknown", "{stdout}");
    assert_eq!(v["reason"], "daemon_stopped", "{stdout}");
    assert!(v["error"]
        .as_str()
        .unwrap()
        .contains("check before resending"));
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
    assert_eq!(status.code(), Some(1), "{stdout} {stderr}");
    let v: Value = serde_json::from_str(stdout.trim()).expect("one JSON line");
    assert_eq!(v["code"], "bridge_error", "{stdout}");
    assert!(v["error"].as_str().unwrap().contains("symlink"), "{stdout}");

    stop_daemon(&client, mcp_port, &dir).await;
}

#[tokio::test]
async fn a_job_the_dead_daemon_never_claimed_is_taken_back_and_reported_not_started() {
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let (mcp_port, ws_port) = (free_port(), free_port());
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let mut daemon = common::spawn_daemon(home.path(), mcp_port, ws_port, None);
    wait_for_health(&client, mcp_port).await;
    // Freeze the daemon: it keeps the lock but claims nothing.
    let pid = daemon.id().expect("daemon pid") as libc::pid_t;
    unsafe { libc::kill(pid, libc::SIGSTOP) };

    let h = home.path().to_path_buf();
    let job =
        tokio::spawn(
            async move { run_js(&h, mcp_port, ws_port, &[], "{\"op\":\"status\"}").await },
        );
    // The job file is in the inbox and unclaimed. Now kill the daemon.
    common::wait_until(
        || {
            std::fs::read_dir(home.path().join("inbox"))
                .unwrap()
                .flatten()
                .any(|e| e.file_name().to_string_lossy().ends_with(".json"))
        },
        10_000,
        "the job file in the inbox",
    )
    .await;
    daemon.start_kill().unwrap();
    daemon.wait().await.unwrap();

    let (status, stdout, _) = tokio::time::timeout(Duration::from_secs(15), job)
        .await
        .expect("js must stop waiting")
        .unwrap();
    assert_eq!(status.code(), Some(1), "{stdout}");
    let v: Value = serde_json::from_str(stdout.trim()).expect("one JSON line");
    assert_eq!(v["code"], "not_started", "{stdout}");
    assert_eq!(v["reason"], "daemon_stopped", "{stdout}");
    assert!(
        inbox_outbox_entries(home.path()).is_empty(),
        "inbox must be empty"
    );
}

#[tokio::test]
async fn a_relative_dir_is_made_absolute_and_the_default_folder_is_not_exported() {
    use std::os::unix::fs::PermissionsExt;
    let _serial = common::serial_process_test().await;
    let root = tempfile::tempdir().expect("temp root");
    let (mcp_port, ws_port) = (free_port(), free_port());
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    // Relative --dir: the folder is root/rel/bridge, never root/rel/bridge/rel/bridge.
    let (status, stdout, stderr) = run_js_full(
        Some(root.path()),
        &[("TURBOFIG_REQUEST_TIMEOUT_MS", "20000")],
        root.path(),
        root.path(),
        mcp_port,
        ws_port,
        &["--dir", "rel/bridge"],
        "{\"op\":\"status\"}",
    )
    .await;
    assert!(status.success(), "{stdout} {stderr}");
    wait_for_health(&client, mcp_port).await;
    let bridge = root.path().join("rel/bridge");
    let lock = std::fs::read_to_string(bridge.join("daemon.lock")).expect("daemon.lock");
    assert!(lock.contains("\"requestTimeoutMs\":20000"), "{lock}");
    assert!(!bridge.join("rel").exists());
    stop_daemon(&client, mcp_port, &bridge).await;

    // --dir equal to $HOME/.turbofig: the daemon runs in default-folder mode
    // (the folder is set to 0700), which it skips when the variable is set.
    let (mcp2, ws2) = (free_port(), free_port());
    let default = root.path().join(".turbofig");
    let other = root.path().join("other");
    let (status, stdout, stderr) = run_js_env(
        root.path(),
        &other,
        mcp2,
        ws2,
        &["--dir", default.to_str().unwrap()],
        "{\"op\":\"status\"}",
    )
    .await;
    assert!(status.success(), "{stdout} {stderr}");
    wait_for_health(&client, mcp2).await;
    let mode = std::fs::metadata(&default).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
    assert!(!other.exists());
    stop_daemon(&client, mcp2, &default).await;
}

/// A stand-in daemon: serves the inbox of `home` with a fixed reply and
/// takes no lock. Returns the stop flag and the thread.
fn fake_inbox_server(
    home: &Path,
) -> (
    std::sync::Arc<std::sync::atomic::AtomicBool>,
    std::thread::JoinHandle<()>,
) {
    let inbox = home.join("inbox");
    let outbox = home.join("outbox");
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::create_dir_all(&outbox).unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let handle = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                for e in std::fs::read_dir(&inbox).unwrap().flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if let Some(id) = name.strip_suffix(".json") {
                        let _ = std::fs::remove_file(e.path());
                        std::fs::write(
                            outbox.join(format!("{id}.json")),
                            "{\"ok\":true,\"old\":1}",
                        )
                        .unwrap();
                    }
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    };
    (stop, handle)
}

#[tokio::test]
async fn js_works_against_an_older_daemon_that_holds_no_lock() {
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let (mcp_port, ws_port) = (free_port(), free_port());
    // A stand-in for an older daemon: it listens on the port, serves the
    // inbox, and never takes a lock or creates daemon.lock.
    let _listener = std::net::TcpListener::bind(("127.0.0.1", mcp_port)).unwrap();
    let (stop, fake) = fake_inbox_server(home.path());
    let (status, stdout, stderr) =
        run_js(home.path(), mcp_port, ws_port, &[], "{\"op\":\"status\"}").await;
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    fake.join().unwrap();
    assert!(status.success(), "{stdout} {stderr}");
    assert_eq!(stdout.trim(), "{\"ok\":true,\"old\":1}");
    assert!(
        !home.path().join("daemon.lock").exists(),
        "js must not start a daemon next to an older one"
    );
}

#[tokio::test]
async fn js_works_against_a_daemon_on_a_filesystem_without_locks() {
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let (mcp_port, ws_port) = (free_port(), free_port());
    // No listener on the port: only the lock file says a daemon is there.
    // Pid = this test process, which is alive.
    std::fs::write(
        home.path().join("daemon.lock"),
        format!(
            "{{\"pid\":{},\"requestTimeoutMs\":30000,\"locked\":false}}\n",
            std::process::id()
        ),
    )
    .unwrap();
    let (stop, fake) = fake_inbox_server(home.path());
    let (status, stdout, stderr) =
        run_js(home.path(), mcp_port, ws_port, &[], "{\"op\":\"status\"}").await;
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    fake.join().unwrap();
    assert!(status.success(), "{stdout} {stderr}");
    assert_eq!(stdout.trim(), "{\"ok\":true,\"old\":1}");
}

#[tokio::test]
async fn a_lock_file_rules_out_legacy_mode_even_with_a_listener_on_the_port() {
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let (mcp_port, ws_port) = (free_port(), free_port());
    // An unrelated process on the port, a stale inbox, and a stale lock file
    // of a dead daemon (locked:true, nobody holds it): js must not write into
    // this folder as if a daemon served it. It reports a bridge error.
    let _listener = std::net::TcpListener::bind(("127.0.0.1", mcp_port)).unwrap();
    let (stop, fake) = fake_inbox_server(home.path());
    std::fs::write(
        home.path().join("daemon.lock"),
        "{\"pid\":2147483646,\"locked\":true}\n",
    )
    .unwrap();
    let (status, stdout, _) =
        run_js(home.path(), mcp_port, ws_port, &[], "{\"op\":\"status\"}").await;
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    fake.join().unwrap();
    assert_eq!(status.code(), Some(1), "{stdout}");
    let v: Value = serde_json::from_str(stdout.trim()).expect("one JSON line");
    assert_eq!(v["code"], json!("bridge_error"));
    assert!(!stdout.contains("\"old\""), "{stdout}");
}

#[tokio::test]
async fn no_home_and_no_bridge_env_is_a_bridge_error_line_with_exit_1() {
    let _serial = common::serial_process_test().await;
    let mut child = Command::new(BIN)
        .arg("js")
        .env_remove("HOME")
        .env_remove("TURBOFIG_BRIDGE_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut si = child.stdin.take().unwrap();
    si.write_all(b"{\"op\":\"status\"}").await.unwrap();
    drop(si);
    let out = child.wait_with_output().await.unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: Value = serde_json::from_str(stdout.trim()).expect("one JSON line on stdout");
    assert_eq!(v["code"], json!("bridge_error"));
    assert!(v["error"].as_str().unwrap().contains("HOME"), "{stdout}");
}

#[tokio::test]
async fn the_daemon_refuses_a_group_writable_bridge_folder_at_startup() {
    use std::os::unix::fs::PermissionsExt;
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let dir = home.path().join("shared");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o775)).unwrap();
    let (p, w) = (free_port(), free_port());
    let out = Command::new(BIN)
        .arg("serve")
        .env("HOME", home.path())
        .env("TURBOFIG_MCP_PORT", p.to_string())
        .env("TURBOFIG_WS_PORT", w.to_string())
        .env("TURBOFIG_BRIDGE_DIR", &dir)
        .stdin(Stdio::null())
        .output()
        .await
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("other accounts can write"), "{err}");
    assert!(!dir.join("inbox").exists(), "nothing is served or created");
}

#[tokio::test]
async fn stdin_that_never_closes_gives_usage_and_exit_2() {
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let mut child = Command::new(BIN)
        .arg("js")
        .env("HOME", home.path())
        .env("TURBOFIG_BRIDGE_DIR", home.path())
        .env("TURBOFIG_TEST_JS_STDIN_WAIT_MS", "300")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let held = child.stdin.take().unwrap(); // open, silent
    let out = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .expect("js must not block on a silent stdin")
        .unwrap();
    drop(held);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no input on stdin"));
    assert!(out.stdout.is_empty());
}

#[tokio::test]
async fn a_second_lock_call_in_one_process_fails_and_keeps_the_first_lock() {
    let _serial = common::serial_process_test().await;
    let a = tempfile::tempdir().unwrap();
    let (p, w) = (free_port(), free_port());
    let zero = Duration::ZERO;
    assert_eq!(
        turbofig::lock_bridge_dir(a.path(), false, zero).unwrap(),
        turbofig::LockOutcome::Held
    );
    // The second call must fail and must not close the first call's fd.
    assert!(turbofig::lock_bridge_dir(a.path(), false, zero).is_err());
    // Another process still cannot take the lock.
    let out = Command::new(BIN)
        .arg("serve")
        .env("HOME", a.path())
        .env("TURBOFIG_MCP_PORT", p.to_string())
        .env("TURBOFIG_WS_PORT", w.to_string())
        .env("TURBOFIG_BRIDGE_DIR", a.path())
        .output()
        .await
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("already using"));
}
