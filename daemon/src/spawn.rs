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

/// Connect timeout for every admin HTTP call (`/health`, `/control`), and
/// also the overall timeout for those calls: a wedged daemon, or an
/// unrelated foreign process that happens to answer on the port, must never
/// hang `turbofig mcp`, `turbofig start`/`stop`/`status`, or the `serve`
/// pre-check forever. 2s is generous for a local loopback call that never
/// does real work beyond JSON (de)serialization.
pub const ADMIN_CLIENT_TIMEOUT: Duration = Duration::from_secs(2);

/// Builds the shared HTTP client every admin call (`GET /health`,
/// `POST /control`) uses: `ADMIN_CLIENT_TIMEOUT` for both the connect phase
/// and the whole request. Never intercepted by a corporate proxy env var
/// (`HTTP_PROXY`/`HTTPS_PROXY`): the daemon is always local (127.0.0.1).
pub fn build_admin_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(ADMIN_CLIENT_TIMEOUT)
        .timeout(ADMIN_CLIENT_TIMEOUT)
        .build()
}

/// Builds the HTTP client `POST /job` calls use: the same
/// `ADMIN_CLIENT_TIMEOUT` connect timeout (a wedged daemon or a foreign
/// process on the port must still fail to *connect* quickly), but
/// deliberately no overall request timeout: a real job (an `execute` op in
/// particular) can legitimately run for up to the daemon's configured
/// `TURBOFIG_REQUEST_TIMEOUT_MS`, far longer than an admin call ever should.
pub fn build_job_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(ADMIN_CLIENT_TIMEOUT)
        .build()
}

/// `daemon.log` is rotated once it grows past this size: the current log is
/// renamed to `daemon.log.1` (replacing any older `.1`), and a fresh,
/// empty `daemon.log` is started. Exactly one rotated generation is kept,
/// never more: this is a size cap, not a full log-retention scheme.
const LOG_ROTATE_THRESHOLD_BYTES: u64 = 5 * 1024 * 1024;

/// Rotates `<home>/daemon.log` to `<home>/daemon.log.1` if it is currently
/// over `LOG_ROTATE_THRESHOLD_BYTES`. A missing log (nothing to rotate yet)
/// is not an error; any other failure to read its size is propagated.
fn rotate_log_if_oversize(home: &Path) -> io::Result<()> {
    let log_path = home.join("daemon.log");
    let metadata = match std::fs::metadata(&log_path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if metadata.len() <= LOG_ROTATE_THRESHOLD_BYTES {
        return Ok(());
    }
    // A rename onto an existing daemon.log.1 replaces it (same filesystem,
    // same directory), so this always keeps exactly one rotated generation.
    std::fs::rename(&log_path, home.join("daemon.log.1"))
}

/// Spawns `<turbofig_binary> serve` fully detached into its own session.
///
/// `home` is the daemon's `TURBOFIG_BRIDGE_DIR`; `<home>/daemon.log` gets
/// the child's stdout and stderr, appended (never truncated, so a repeated
/// start never loses earlier log lines), unless it has grown past
/// `LOG_ROTATE_THRESHOLD_BYTES`, in which case it is first rotated to
/// `daemon.log.1` (see `rotate_log_if_oversize`) and a fresh file is opened.
/// The child inherits this process's environment unchanged, so any
/// `TURBOFIG_*` override already in the caller's environment reaches the
/// child the same way it reached the caller. The returned child is
/// intentionally never waited on here: once `setsid` detaches it, it is no
/// longer this process's job to reap or supervise.
pub fn spawn_detached_daemon(turbofig_binary: &Path, home: &Path) -> io::Result<()> {
    std::fs::create_dir_all(home)?;
    rotate_log_if_oversize(home)?;
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
    fetch_health_with_token(client, mcp_port, None).await
}

/// Same as `fetch_health`, but attaches `Authorization: Bearer <token>` when
/// `token` is `Some`, so the response is `/health`'s full payload (connected
/// files, pid) rather than the reduced one an unauthenticated caller gets.
/// Use this only where a caller actually needs that detail (the bare
/// `turbofig` first-run/status text, `turbofig status`), not for a plain
/// liveness check.
pub async fn fetch_health_with_token(
    client: &reqwest::Client,
    mcp_port: u16,
    token: Option<&str>,
) -> Option<serde_json::Value> {
    let url = format!("http://127.0.0.1:{mcp_port}/health");
    let mut req = client.get(&url);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.ok()?;
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
    async fn admin_client_request_fails_within_its_timeout_against_a_listener_that_never_answers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let port = listener.local_addr().expect("local addr").port();
        tokio::spawn(async move {
            // Accept the connection and hold it open without ever replying:
            // a wedged daemon, or an unrelated foreign process that happens
            // to answer on this port, looks exactly like this to a client.
            if let Ok((_stream, _)) = listener.accept().await {
                std::future::pending::<()>().await
            }
        });

        let client = build_admin_client().expect("build admin client");
        let start = Instant::now();
        let result = fetch_health(&client, port).await;
        let elapsed = start.elapsed();

        assert!(
            result.is_none(),
            "a connection that never answers must never look healthy"
        );
        assert!(
            elapsed < ADMIN_CLIENT_TIMEOUT + Duration::from_secs(3),
            "the request must fail near the client's own {ADMIN_CLIENT_TIMEOUT:?} timeout, \
             not hang forever: took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn job_client_request_never_hangs_beyond_a_bounded_wait_on_an_unroutable_address() {
        let client = build_job_client().expect("build job client");
        // 192.0.2.1 is TEST-NET-1 (RFC 5737): reserved and never routable,
        // so the connect attempt itself must fail, via `connect_timeout`
        // rather than hang. The outer `tokio::time::timeout` bounds this
        // test, not the client: `job_client` deliberately has no overall
        // request timeout (a real `/job` call can run far longer), so a
        // bug that silently added one, or removed `connect_timeout`
        // entirely, must still be caught without this test itself hanging.
        let bounded = tokio::time::timeout(
            ADMIN_CLIENT_TIMEOUT + Duration::from_secs(5),
            client.get("http://192.0.2.1:18846/job").send(),
        )
        .await;
        assert!(
            bounded.is_ok(),
            "connect_timeout must bound the connect attempt; the outer test timeout fired instead"
        );
        assert!(
            bounded.unwrap().is_err(),
            "an unroutable address must fail to connect, not somehow succeed"
        );
    }

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
    async fn fetch_health_with_token_returns_the_full_payload_and_fetch_health_does_not() {
        let state = std::sync::Arc::new(crate::state::AppState::with_timeout(
            Duration::from_millis(100),
        ));
        let token = state.token().to_owned();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let port = listener.local_addr().expect("local addr").port();
        tokio::spawn(async move {
            let _ = crate::mcp::serve_with_state(listener, state).await;
        });

        let client = reqwest::Client::new();
        let reduced = fetch_health(&client, port).await.expect("reduced health");
        assert!(reduced.get("connectedFiles").is_none());
        assert!(reduced.get("pid").is_none());

        let full = fetch_health_with_token(&client, port, Some(&token))
            .await
            .expect("full health");
        assert_eq!(full["connectedFiles"], serde_json::json!([]));
        assert_eq!(full["pid"], serde_json::json!(std::process::id()));
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

    #[test]
    fn rotate_log_if_oversize_is_a_no_op_when_the_log_is_small() {
        let tmp = tempfile::tempdir().expect("temp home");
        let log_path = tmp.path().join("daemon.log");
        std::fs::write(&log_path, b"small\n").expect("seed a small log");

        rotate_log_if_oversize(tmp.path()).expect("rotate check must not fail");

        assert!(log_path.exists(), "the small log must stay in place");
        assert!(!tmp.path().join("daemon.log.1").exists());
    }

    #[test]
    fn rotate_log_if_oversize_is_a_no_op_when_there_is_no_log_yet() {
        let tmp = tempfile::tempdir().expect("temp home");
        rotate_log_if_oversize(tmp.path()).expect("a missing log must not be an error");
        assert!(!tmp.path().join("daemon.log").exists());
    }

    #[test]
    fn rotate_log_if_oversize_rotates_an_oversize_log_to_dot_1() {
        let tmp = tempfile::tempdir().expect("temp home");
        let log_path = tmp.path().join("daemon.log");
        let oversize = vec![b'x'; (LOG_ROTATE_THRESHOLD_BYTES + 1) as usize];
        std::fs::write(&log_path, &oversize).expect("seed an oversize log");

        rotate_log_if_oversize(tmp.path()).expect("rotate must succeed");

        assert!(
            !log_path.exists(),
            "the oversize log must be moved out of the way"
        );
        let rotated = std::fs::read(tmp.path().join("daemon.log.1")).expect("read rotated log");
        assert_eq!(rotated.len(), oversize.len());
    }

    #[test]
    fn rotate_log_if_oversize_replaces_an_older_dot_1_keeping_only_one_generation() {
        let tmp = tempfile::tempdir().expect("temp home");
        let log_path = tmp.path().join("daemon.log");
        let rotated_path = tmp.path().join("daemon.log.1");
        std::fs::write(&rotated_path, b"stale generation").expect("seed a stale .1");
        let oversize = vec![b'y'; (LOG_ROTATE_THRESHOLD_BYTES + 1) as usize];
        std::fs::write(&log_path, &oversize).expect("seed an oversize log");

        rotate_log_if_oversize(tmp.path()).expect("rotate must succeed");

        let rotated = std::fs::read(&rotated_path).expect("read rotated log");
        assert_eq!(
            rotated.len(),
            oversize.len(),
            "the stale .1 must be replaced by the just-rotated log, not kept"
        );
    }

    #[test]
    fn spawn_detached_daemon_rotates_an_oversize_log_before_appending() {
        let tmp = tempfile::tempdir().expect("temp home");
        let log_path = tmp.path().join("daemon.log");
        let oversize = vec![b'z'; (LOG_ROTATE_THRESHOLD_BYTES + 1) as usize];
        std::fs::write(&log_path, &oversize).expect("seed an oversize log");

        spawn_detached_daemon(Path::new("/bin/echo"), tmp.path())
            .expect("spawn a trivial detached process");
        std::thread::sleep(Duration::from_millis(200));

        let rotated =
            std::fs::metadata(tmp.path().join("daemon.log.1")).expect("rotated log must exist");
        assert_eq!(rotated.len(), oversize.len() as u64);
        let fresh = std::fs::metadata(&log_path).expect("fresh log must exist");
        assert!(
            fresh.len() < oversize.len() as u64,
            "the fresh log must not still carry the oversize content"
        );
    }
}
