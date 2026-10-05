//! Detached daemon spawn.
//!
//! Starts `<turbofig binary> serve` as a background process that outlives
//! the caller: its own session (`setsid`), stdin from `/dev/null`, stdout
//! and stderr appended to `<home>/daemon.log`. Neither a `SIGTERM`/`SIGKILL`
//! to the caller, nor one sent to the caller's whole process group, ever
//! reaches a process in its own session.
//!
//! Shared by `turbofig mcp` (`proxy.rs`) and the bare `turbofig` first-run
//! flow (a later step): both need "make sure a daemon is running" with the
//! exact same detached-start behaviour.

use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long `wait_for_health` polls before giving up.
pub const HEALTH_DEADLINE: Duration = Duration::from_secs(5);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Spawns `<turbofig_binary> serve` fully detached into its own session.
///
/// `home` is the daemon's `TURBOFIG_BRIDGE_DIR`; `<home>/daemon.log` gets
/// the child's stdout and stderr, appended (never truncated, so a repeated
/// start never loses earlier log lines). The child inherits this process's
/// environment unchanged, so any `TURBOFIG_*` override already in the
/// caller's environment reaches the child the same way it reached the
/// caller. The returned child is intentionally never waited on here: once
/// `setsid` detaches it, it is no longer this process's job to reap or
/// supervise.
pub fn spawn_detached_daemon(turbofig_binary: &Path, home: &Path) -> io::Result<()> {
    std::fs::create_dir_all(home)?;
    let log_path = home.join("daemon.log");
    let stdout_log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let stderr_log = stdout_log.try_clone()?;
    let dev_null = std::fs::File::open("/dev/null")?;

    let mut cmd = Command::new(turbofig_binary);
    cmd.arg("serve")
        .stdin(Stdio::from(dev_null))
        .stdout(Stdio::from(stdout_log))
        .stderr(Stdio::from(stderr_log));

    detach_into_own_session(&mut cmd);

    cmd.spawn()?;
    Ok(())
}

/// Arranges for the spawned child to call `setsid` right after `fork`, before
/// `exec`, putting it in a new session and process group of its own. Must be
/// called before `cmd.spawn()`.
#[cfg(unix)]
fn detach_into_own_session(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: setsid(2) is async-signal-safe and takes no arguments, so it
    // is sound to call between fork and exec, where only async-signal-safe
    // calls are allowed. A failure here (already a session leader) is
    // reported as the child's exit status via Command's normal error path;
    // propagating it from this closure just gives a clearer error than a
    // silent, still-attached child.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// No-op on non-Unix platforms: there is no POSIX session model to detach
/// into. The daemon, launchd integration, and this whole crate are macOS-only
/// today; this keeps the crate buildable elsewhere rather than failing to
/// compile.
#[cfg(not(unix))]
fn detach_into_own_session(_cmd: &mut Command) {}

/// Returns `GET /health`'s parsed JSON body, or `None` for any failure
/// (connection refused, timeout, a non-success status, an unparseable
/// body). All of those mean the same thing to a caller of this function:
/// not currently answerable, never worth telling apart.
pub async fn fetch_health(client: &reqwest::Client, mcp_port: u16) -> Option<serde_json::Value> {
    let url = format!("http://127.0.0.1:{mcp_port}/health");
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<serde_json::Value>().await.ok()
}

/// Polls `GET /health` on `127.0.0.1:<mcp_port>` until it answers with a
/// success status, or `HEALTH_DEADLINE` elapses. Returns a clear, user-facing
/// error message on timeout: never a bare `reqwest::Error`.
pub async fn wait_for_health(client: &reqwest::Client, mcp_port: u16) -> Result<(), String> {
    wait_for_health_with_deadline(client, mcp_port, HEALTH_DEADLINE).await
}

/// Polls `GET /health` until it stops answering (a daemon that was running
/// has exited), or `deadline` elapses. Returns true once unreachable, false
/// on timeout. The counterpart to `wait_for_health`: `turbofig stop` and the
/// version-handoff restart (`proxy.rs`) both need to know the *old* daemon
/// is actually gone before starting (or trusting launchd to start) a new one.
pub async fn wait_for_unreachable(
    client: &reqwest::Client,
    mcp_port: u16,
    deadline: Duration,
) -> bool {
    let until = Instant::now() + deadline;
    loop {
        if fetch_health(client, mcp_port).await.is_none() {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
    }
}

/// The testable half of `wait_for_health`: takes an explicit deadline so a
/// test can use a short one instead of the real 5 s.
async fn wait_for_health_with_deadline(
    client: &reqwest::Client,
    mcp_port: u16,
    deadline: Duration,
) -> Result<(), String> {
    let url = format!("http://127.0.0.1:{mcp_port}/health");
    let until = Instant::now() + deadline;
    loop {
        if let Ok(resp) = client.get(&url).send().await {
            if resp.status().is_success() {
                return Ok(());
            }
        }
        if Instant::now() >= until {
            return Err(format!(
                "the daemon on port {mcp_port} did not become healthy within {deadline:?}"
            ));
        }
        tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wait_for_health_times_out_with_a_clear_message_when_nothing_is_listening() {
        let client = reqwest::Client::new();
        // Port 1 is a privileged, essentially never-bound port: nothing
        // should ever answer here in CI or on a dev machine.
        let err = wait_for_health_with_deadline(&client, 1, Duration::from_millis(100))
            .await
            .expect_err("nothing listens on port 1");
        assert!(err.contains("did not become healthy"));
        assert!(err.contains('1'));
    }

    #[tokio::test]
    async fn wait_for_health_succeeds_once_a_health_endpoint_answers() {
        let state = std::sync::Arc::new(crate::state::AppState::with_timeout(
            Duration::from_millis(100),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let port = listener.local_addr().expect("local addr").port();
        tokio::spawn(async move {
            let _ = crate::mcp::serve_with_state(listener, state).await;
        });

        let client = reqwest::Client::new();
        wait_for_health_with_deadline(&client, port, Duration::from_secs(2))
            .await
            .expect("a real /health endpoint must satisfy the wait");
    }

    #[tokio::test]
    async fn fetch_health_is_none_when_nothing_is_listening() {
        let client = reqwest::Client::new();
        assert!(fetch_health(&client, 1).await.is_none());
    }

    #[tokio::test]
    async fn fetch_health_returns_the_body_once_a_health_endpoint_answers() {
        let state = std::sync::Arc::new(crate::state::AppState::with_timeout(
            Duration::from_millis(100),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let port = listener.local_addr().expect("local addr").port();
        tokio::spawn(async move {
            let _ = crate::mcp::serve_with_state(listener, state).await;
        });

        let client = reqwest::Client::new();
        let body = fetch_health(&client, port).await.expect("health body");
        assert!(body["version"].is_string());
    }

    #[tokio::test]
    async fn wait_for_unreachable_returns_true_immediately_when_nothing_is_listening() {
        let client = reqwest::Client::new();
        assert!(wait_for_unreachable(&client, 1, Duration::from_millis(50)).await);
    }

    #[tokio::test]
    async fn wait_for_unreachable_times_out_while_a_daemon_still_answers() {
        let state = std::sync::Arc::new(crate::state::AppState::with_timeout(
            Duration::from_millis(100),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let port = listener.local_addr().expect("local addr").port();
        tokio::spawn(async move {
            let _ = crate::mcp::serve_with_state(listener, state).await;
        });

        assert!(
            !wait_for_unreachable(&reqwest::Client::new(), port, Duration::from_millis(100)).await,
            "a still-healthy daemon must never be reported unreachable"
        );
    }

    #[test]
    fn spawn_detached_daemon_rejects_a_missing_binary_cleanly() {
        let tmp = tempfile::tempdir().expect("temp home");
        let err = spawn_detached_daemon(Path::new("/no/such/turbofig/binary"), tmp.path())
            .expect_err("a missing binary must fail to spawn");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn spawn_detached_daemon_creates_and_appends_to_the_log_file() {
        let tmp = tempfile::tempdir().expect("temp home");
        let log_path = tmp.path().join("daemon.log");
        std::fs::write(&log_path, b"earlier line\n").expect("seed an existing log");

        // /bin/echo exits almost immediately; this exercises the detach
        // wiring (log file creation, stdin/stdout/stderr redirection,
        // setsid) without needing the real turbofig binary or a lingering
        // process.
        spawn_detached_daemon(Path::new("/bin/echo"), tmp.path())
            .expect("spawn a trivial detached process");

        // The child runs asynchronously; give it a moment to exit and flush.
        std::thread::sleep(Duration::from_millis(200));

        let contents = std::fs::read_to_string(&log_path).expect("read log");
        assert!(
            contents.starts_with("earlier line\n"),
            "an existing log must never be truncated: {contents:?}"
        );
    }
}
