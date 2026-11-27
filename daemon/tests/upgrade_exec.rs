//! Integration tests for the in-place upgrade restart (`upgrade.rs`) with the
//! real compiled daemon: temp HOME, temp bridge folder, temp Applications and
//! LaunchAgents folders, free ports.
//!
//! The "new build" is a shell script that stands in for the Homebrew binary:
//! its bytes differ from the daemon binary, so the watcher sees an upgrade,
//! and it then runs the real daemon binary. That exercises the whole path
//! with no second compiled build: the watcher, the drain, `exec` with the
//! folder lock fd left open, the new image adopting that fd, the port
//! rebind, and the loop guard (the script leads to the same build the daemon
//! already ran as, so the new image must not restart again).

mod common;

use common::{wait_until, BIN};
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Kills and reaps the child on drop, so a failing assertion never leaks a
/// daemon.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        common::kill_and_reap_bounded(&mut self.0);
    }
}

struct Env {
    tmp: tempfile::TempDir,
    bridge: PathBuf,
    mcp: u16,
    ws: u16,
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("temp dir");
        let bridge = tmp.path().join("bridge");
        std::fs::create_dir_all(&bridge).expect("bridge dir");
        std::fs::create_dir_all(tmp.path().join("apps")).expect("apps dir");
        std::fs::create_dir_all(tmp.path().join("agents")).expect("agents dir");
        let (mcp, ws) = common::free_port_pair();
        Self {
            tmp,
            bridge,
            mcp,
            ws,
        }
    }

    fn log(&self) -> PathBuf {
        self.tmp.path().join("daemon.log")
    }

    /// `turbofig serve` on this env's ports, with its output in `daemon.log`.
    fn command(&self, mcp: u16, ws: u16) -> Command {
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log())
            .expect("open log");
        let mut cmd = Command::new(BIN);
        cmd.arg("serve")
            .env("HOME", self.tmp.path())
            .env("TURBOFIG_BRIDGE_DIR", &self.bridge)
            .env("TURBOFIG_APPLICATIONS_DIR", self.tmp.path().join("apps"))
            .env("TURBOFIG_LAUNCH_AGENTS_DIR", self.tmp.path().join("agents"))
            .env("TURBOFIG_MCP_PORT", mcp.to_string())
            .env("TURBOFIG_WS_PORT", ws.to_string())
            .env_remove("TURBOFIG_SUPERVISED")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().expect("clone log")))
            .stderr(Stdio::from(log));
        cmd
    }

    fn lock_info(&self) -> Option<Value> {
        let text = std::fs::read_to_string(self.bridge.join("daemon.lock")).ok()?;
        serde_json::from_str(text.trim()).ok()
    }

    fn log_text(&self) -> String {
        std::fs::read_to_string(self.log()).unwrap_or_default()
    }

    /// True when some process holds the daemon lock (`F_GETLK`; this process
    /// is not the holder, and its own fd closes again at once).
    fn lock_held(&self) -> bool {
        let Ok(file) = std::fs::File::open(self.bridge.join("daemon.lock")) else {
            return false;
        };
        use std::os::unix::io::AsRawFd;
        // SAFETY: plain data, valid fd.
        unsafe {
            let mut fl: libc::flock = std::mem::zeroed();
            fl.l_type = libc::F_RDLCK as libc::c_short;
            fl.l_whence = libc::SEEK_SET as libc::c_short;
            libc::fcntl(file.as_raw_fd(), libc::F_GETLK, &mut fl) == 0
                && fl.l_type != libc::F_UNLCK as libc::c_short
        }
    }
}

async fn healthy(client: &reqwest::Client, port: u16) -> bool {
    common::fetch_health(client, port).await.is_some()
}

async fn wait_healthy(client: &reqwest::Client, port: u16, secs: u64) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !healthy(client, port).await {
        assert!(Instant::now() < deadline, "no health on port {port}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_unreachable(client: &reqwest::Client, port: u16, secs: u64) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while healthy(client, port).await {
        assert!(Instant::now() < deadline, "port {port} never went quiet");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A second `serve` on the same bridge folder but other ports: it must exit 1
/// with the "already using" message, and never serve.
fn assert_second_daemon_refused(env: &Env) {
    let (mcp, ws) = common::free_port_pair();
    let out = env
        .command(mcp, ws)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("run a second daemon");
    assert_eq!(out.status.code(), Some(1), "second daemon must be refused");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("another turbofig daemon is already using"),
        "{stderr}"
    );
}

fn write_new_build_script(path: &Path) {
    std::fs::write(
        path,
        "#!/bin/sh\n\
         # Stands in for a freshly installed build: answers --version at once,\n\
         # and pauses before it runs the real daemon, so a test can look at the\n\
         # window between the old image and the new one.\n\
         if [ \"$1\" = \"--version\" ]; then exec \"$TF_REAL\" --version; fi\n\
         sleep \"$TF_PAUSE\"\n\
         exec \"$TF_REAL\" \"$@\"\n",
    )
    .expect("write script");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

#[tokio::test]
async fn an_upgrade_restarts_in_place_and_the_folder_lock_never_drops() {
    let _serial = common::serial_process_test().await;
    let env = Env::new();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let script = env.tmp.path().join("new-turbofig");
    write_new_build_script(&script);

    let mut cmd = env.command(env.mcp, env.ws);
    cmd.env("TURBOFIG_TEST_UPGRADE_WATCH", &script)
        .env("TURBOFIG_TEST_UPGRADE_CHECK_MS", "100")
        .env("TF_REAL", BIN)
        .env("TF_PAUSE", "2");
    let daemon = Daemon(cmd.spawn().expect("spawn the daemon"));
    let pid = u64::from(daemon.0.id());

    // The watcher sees a build that differs from the running one.
    wait_until(
        || env.log_text().contains("detected an upgrade"),
        15_000,
        "the watcher to detect the new build",
    )
    .await;

    // The old image `exec`s: the ports close. The script then pauses 2 s
    // before it runs the real daemon, with the lock fd still open.
    wait_unreachable(&client, env.mcp, 10).await;
    assert!(
        env.lock_held(),
        "the folder lock must stay held during the restart"
    );
    assert_eq!(
        env.lock_info().and_then(|v| v["pid"].as_u64()),
        Some(pid),
        "the pid is unchanged by an in-place restart"
    );
    // DURING the restart a second daemon is still refused.
    assert_second_daemon_refused(&env);
    assert!(
        env.lock_held(),
        "a refused second daemon must not drop the lock"
    );

    // The new image adopts the inherited fd, rebinds both ports and serves.
    wait_healthy(&client, env.mcp, 15).await;
    // The build id lands in the lock info once the new image's startup hash
    // is done, which can be after /health answers.
    let want = turbofig::upgrade::build_id_for_file(Path::new(BIN)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while env
        .lock_info()
        .and_then(|v| v["buildId"].as_str().map(str::to_owned))
        .as_deref()
        != Some(want.as_str())
    {
        assert!(
            Instant::now() < deadline,
            "no build id after the restart: {:?}",
            env.lock_info()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let info = env.lock_info().expect("lock info after the restart");
    assert_eq!(info["pid"].as_u64(), Some(pid), "same process: {info}");
    assert_eq!(info["version"], env!("CARGO_PKG_VERSION"), "{info}");
    assert_eq!(
        info["buildId"].as_str(),
        Some(
            turbofig::upgrade::build_id_for_file(Path::new(BIN))
                .unwrap()
                .as_str()
        ),
        "{info}"
    );
    assert_eq!(info["locked"], true, "{info}");
    assert!(env.lock_held());
    // AFTER the restart a second daemon is still refused.
    assert_second_daemon_refused(&env);
    assert!(
        env.lock_held(),
        "the adopted lock must survive a refused daemon"
    );

    // The script led to the build that was already running: the new image
    // must not restart again (the loop guard), however long it runs.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let log = env.log_text();
    assert_eq!(log.matches("detected an upgrade").count(), 1, "{log}");
    assert_eq!(
        log.matches("restarted into the same build").count(),
        1,
        "{log}"
    );
    wait_healthy(&client, env.mcp, 15).await;
    assert_eq!(env.lock_info().and_then(|v| v["pid"].as_u64()), Some(pid));
    let mut daemon = daemon;
    assert!(
        daemon.0.try_wait().expect("try_wait").is_none(),
        "the daemon process never exited"
    );
}

#[tokio::test]
async fn a_new_build_that_cannot_start_is_not_restarted_into() {
    let _serial = common::serial_process_test().await;
    let env = Env::new();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    // A "binary" that fails `--version`: the daemon must keep running.
    let broken = env.tmp.path().join("broken-turbofig");
    std::fs::write(&broken, "#!/bin/sh\nexit 3\n").unwrap();
    std::fs::set_permissions(&broken, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut cmd = env.command(env.mcp, env.ws);
    cmd.env("TURBOFIG_TEST_UPGRADE_WATCH", &broken)
        .env("TURBOFIG_TEST_UPGRADE_CHECK_MS", "100");
    let mut daemon = Daemon(cmd.spawn().expect("spawn the daemon"));
    wait_healthy(&client, env.mcp, 15).await;
    wait_until(
        || env.log_text().contains("did not start with --version"),
        15_000,
        "the preflight to refuse the broken build",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    wait_healthy(&client, env.mcp, 15).await;
    assert!(!env.log_text().contains("detected an upgrade"));
    assert!(daemon.0.try_wait().unwrap().is_none());
}

#[tokio::test]
async fn the_lock_info_carries_the_version_and_the_build_id() {
    let _serial = common::serial_process_test().await;
    let env = Env::new();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let _daemon = Daemon(env.command(env.mcp, env.ws).spawn().expect("spawn"));
    wait_healthy(&client, env.mcp, 15).await;
    wait_until(
        || env.lock_info().is_some_and(|v| v["buildId"].is_string()),
        15_000,
        "the lock info to carry a build id",
    )
    .await;
    let info = env.lock_info().unwrap();
    assert_eq!(info["version"], env!("CARGO_PKG_VERSION"));
    assert!(info["buildId"].as_str().is_some_and(|b| b.contains('-')));
    assert_eq!(info["locked"], true);
    // An old reader that knows only pid, requestTimeoutMs and locked still works.
    assert!(info["pid"].is_u64() && info["requestTimeoutMs"].is_u64());
    assert_second_daemon_refused(&env);
}

#[tokio::test]
async fn a_daemon_that_fails_to_bind_never_takes_the_folder_lock() {
    let _serial = common::serial_process_test().await;
    let env = Env::new();
    // Something else already holds the WS port; the MCP port is free.
    let squatter = std::net::TcpListener::bind("127.0.0.1:0").expect("squat a port");
    let busy_ws = squatter.local_addr().unwrap().port();
    let status = env
        .command(env.mcp, busy_ws)
        .status()
        .expect("run the daemon");
    assert_eq!(status.code(), Some(1), "a daemon that cannot bind exits 1");
    assert!(
        !env.bridge.join("daemon.lock").exists(),
        "the lock file must not even exist: the lock is taken after both binds"
    );
    assert!(!env.lock_held());
    drop(squatter);
}

/// A "new build" that passes the `--version` preflight and then makes its own
/// file non-executable, so the later `exec` of it fails.
fn write_unexecable_build_script(path: &Path) {
    std::fs::write(
        path,
        "#!/bin/sh\n\
         if [ \"$1\" = \"--version\" ]; then chmod -x \"$0\"; echo turbofig; exit 0; fi\n\
         exit 3\n",
    )
    .expect("write script");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

fn token_of(env: &Env) -> String {
    std::fs::read_to_string(env.bridge.join("token"))
        .expect("token")
        .trim()
        .to_owned()
}

#[tokio::test]
async fn a_failed_exec_is_retried_a_few_times_then_the_build_is_skipped_and_the_daemon_serves() {
    let _serial = common::serial_process_test().await;
    let env = Env::new();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let script = env.tmp.path().join("unexecable-turbofig");
    write_unexecable_build_script(&script);

    let mut cmd = env.command(env.mcp, env.ws);
    cmd.env("TURBOFIG_TEST_UPGRADE_WATCH", &script)
        .env("TURBOFIG_TEST_UPGRADE_CHECK_MS", "100");
    let mut daemon = Daemon(cmd.spawn().expect("spawn the daemon"));
    wait_healthy(&client, env.mcp, 15).await;
    wait_until(
        || env.log_text().contains("could not start"),
        15_000,
        "the exec to fail",
    )
    .await;
    // The failing signature is remembered: the watcher gives up on it and
    // never drains again for that file.
    wait_until(
        || env.log_text().contains("giving up on build"),
        15_000,
        "the watcher to give up on the build",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    let log = env.log_text();
    assert_eq!(log.matches("giving up on build").count(), 1, "{log}");
    assert_eq!(log.matches("detected an upgrade").count(), 1, "{log}");
    // The daemon serves on, and the lock is still held.
    wait_healthy(&client, env.mcp, 15).await;
    assert!(env.lock_held());
    assert!(daemon.0.try_wait().unwrap().is_none());
}

#[tokio::test]
async fn a_control_restart_that_joined_the_drain_is_honored_after_a_failed_exec() {
    let _serial = common::serial_process_test().await;
    let env = Env::new();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let script = env.tmp.path().join("unexecable-turbofig");
    write_unexecable_build_script(&script);

    let mut cmd = env.command(env.mcp, env.ws);
    cmd.env("TURBOFIG_TEST_UPGRADE_WATCH", &script)
        .env("TURBOFIG_TEST_UPGRADE_CHECK_MS", "100")
        // A wide window between the drain and the `exec`.
        .env("TURBOFIG_TEST_UPGRADE_EXIT_GRACE_MS", "2500");
    let mut daemon = Daemon(cmd.spawn().expect("spawn the daemon"));
    wait_healthy(&client, env.mcp, 15).await;
    wait_until(
        || env.log_text().contains("detected an upgrade"),
        15_000,
        "the drain to start",
    )
    .await;
    // The upgrade drain is running: this restart joins it and gets a 202.
    let resp = client
        .post(format!("http://127.0.0.1:{}/control", env.mcp))
        .bearer_auth(token_of(&env))
        .json(&serde_json::json!({"action": "restart"}))
        .send()
        .await
        .expect("send /control");
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);

    // The `exec` fails. The daemon must not serve on: the restart it
    // promised ends the process (exit 0, unsupervised: the caller starts the
    // next daemon).
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(s) = daemon.0.try_wait().expect("try_wait") {
            break s;
        }
        assert!(
            Instant::now() < deadline,
            "the daemon never honored the restart: {}",
            env.log_text()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(status.code(), Some(0), "{}", env.log_text());
    assert!(
        env.log_text().contains("honoring the /control restart"),
        "{}",
        env.log_text()
    );
}

#[tokio::test]
async fn an_in_place_restart_survives_a_passing_listener_on_the_port() {
    let _serial = common::serial_process_test().await;
    let env = Env::new();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    // Something unrelated holds the MCP port for about a second, as a
    // passing process can right after the old image closed it.
    // It answers like a web server that is not turbofig, so a startup check
    // would see an unverified listener and exit 1.
    let intruder = std::net::TcpListener::bind(("127.0.0.1", env.mcp)).expect("hold the port");
    intruder.set_nonblocking(true).expect("nonblocking");
    let stop_intruder = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let intruder_thread = {
        let stop = stop_intruder.clone();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                if let Ok((mut conn, _)) = intruder.accept() {
                    let mut buf = [0u8; 1024];
                    let _ = conn.set_read_timeout(Some(Duration::from_millis(200)));
                    let _ = conn.read(&mut buf);
                    let _ = conn.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi",
                    );
                } else {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        })
    };

    let mut cmd = env.command(env.mcp, env.ws);
    // What the old image sets for the new one (`upgrade::exec_in_place`).
    cmd.env("TURBOFIG_REEXEC_FROM", "0123abcd-1");
    let mut daemon = Daemon(cmd.spawn().expect("spawn the daemon"));
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert!(
        daemon.0.try_wait().unwrap().is_none(),
        "the daemon exited at the startup check: {}",
        env.log_text()
    );
    stop_intruder.store(true, std::sync::atomic::Ordering::SeqCst);
    intruder_thread.join().expect("intruder thread");
    wait_healthy(&client, env.mcp, 15).await;
    assert!(env.lock_held());
}

/// A restart request that arrives while the upgrade watcher already drains
/// ends in an `exec`, so the folder lock is never released. `turbofig js`
/// must see the new build in the lock info and go on, not wait out the
/// 75 s it gives a daemon that exits.
#[tokio::test]
async fn js_does_not_wait_out_a_restart_that_joined_an_upgrade_drain() {
    let _serial = common::serial_process_test().await;
    let env = Env::new();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    // The new build runs the real daemon without the old-version override,
    // so its lock info says it is current.
    let script = env.tmp.path().join("new-turbofig");
    std::fs::write(
        &script,
        "#!/bin/sh\n\
         if [ \"$1\" = \"--version\" ]; then exec \"$TF_REAL\" --version; fi\n\
         unset TURBOFIG_TEST_VERSION_OVERRIDE\n\
         exec \"$TF_REAL\" \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut cmd = env.command(env.mcp, env.ws);
    cmd.env("TURBOFIG_TEST_VERSION_OVERRIDE", "0.0.1")
        .env("TURBOFIG_TEST_UPGRADE_WATCH", &script)
        .env("TURBOFIG_TEST_UPGRADE_CHECK_MS", "100")
        .env("TURBOFIG_TEST_UPGRADE_EXIT_GRACE_MS", "6000")
        .env("TF_REAL", BIN);
    let daemon = Daemon(cmd.spawn().expect("spawn the daemon"));
    let pid = u64::from(daemon.0.id());
    wait_healthy(&client, env.mcp, 15).await;
    wait_until(
        || env.log_text().contains("detected an upgrade"),
        15_000,
        "the upgrade drain to start",
    )
    .await;

    let started = Instant::now();
    let mut js = Command::new(BIN);
    js.arg("js")
        .env("HOME", env.tmp.path())
        .env("TURBOFIG_BRIDGE_DIR", &env.bridge)
        .env("TURBOFIG_MCP_PORT", env.mcp.to_string())
        .env("TURBOFIG_WS_PORT", env.ws.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = js.spawn().expect("spawn js");
    {
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"{\"op\":\"status\"}\n")
            .unwrap();
    }
    let out = tokio::task::spawn_blocking(move || child.wait_with_output())
        .await
        .unwrap()
        .expect("js output");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout} {}", env.log_text());
    assert!(
        started.elapsed() < Duration::from_secs(40),
        "js waited {:?} for a daemon that restarted in place",
        started.elapsed()
    );
    let v: Value = serde_json::from_str(stdout.trim()).expect("result JSON");
    assert_eq!(v["ok"], Value::Bool(true), "{stdout}");
    // Same process, now a current build.
    let info = env.lock_info().expect("lock info");
    assert_eq!(info["pid"].as_u64(), Some(pid), "{info}");
    assert_eq!(info["version"], env!("CARGO_PKG_VERSION"), "{info}");
}
