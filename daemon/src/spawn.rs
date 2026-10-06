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
///
/// Two callers can race this (two proxies, or a proxy and this process's own
/// startup, both deciding the daemon needs (re)starting at once): if this
/// caller's `metadata` read sees the log over threshold but a concurrent
/// caller already renamed it away by the time this one calls `rename`, that
/// rename fails with `NotFound`. That is not a real failure, just a lost
/// race to do the exact same rotation: the log has already been rotated by
/// the winner, which is all this call was ever trying to accomplish.
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
    rename_log_tolerating_concurrent_rotation(&log_path, &home.join("daemon.log.1"))
}

/// Renames `log_path` to `rotated_path`, treating `NotFound` as success: the
/// expected cause is a concurrent caller already having won the race to
/// rotate this exact log (see `rotate_log_if_oversize`'s doc comment), so
/// there is nothing left for this caller to do, not a real failure. A
/// `rename` also reports `NotFound` for a missing destination directory,
/// which this cannot tell apart from a lost race; `rotate_daemon_log_best_effort`
/// only ever rotates within `home`, which must already exist by the time
/// this runs, so that case is not expected in practice.
fn rename_log_tolerating_concurrent_rotation(
    log_path: &Path,
    rotated_path: &Path,
) -> io::Result<()> {
    match std::fs::rename(log_path, rotated_path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Rotates `<home>/daemon.log` if it is oversize (see `rotate_log_if_oversize`),
/// never failing the caller: any error (other than the already-handled
/// concurrent-rotation race) is logged as a warning and otherwise ignored.
/// A log that could not be rotated is not a reason to refuse to start the
/// daemon; the log just grows a little further until the next chance.
pub fn rotate_daemon_log_best_effort(home: &Path) {
    if let Err(e) = rotate_log_if_oversize(home) {
        eprintln!(
            "turbofig: could not rotate {}: {e}",
            home.join("daemon.log").display()
        );
    }
}

/// Rotates `<home>/daemon.log` the same way `spawn_detached_daemon` does
/// (`rotate_daemon_log_best_effort`), then, only when this process is itself
/// the one launchd is supervising (`supervisor::is_supervised`), reopens
/// stdout and stderr onto a fresh `<home>/daemon.log`.
///
/// `spawn_detached_daemon`'s rotation only ever runs in the *parent* that
/// starts a new daemon process, so the long-running launchd-supervised
/// daemon's own log (the plist's `StandardOutPath`/`StandardErrorPath`) is
/// never rotated across its lifetime: nothing else ever (re)spawns it to
/// trigger that parent-side check. Call this once, early in `serve`'s
/// startup, so a launchd-managed daemon gets the same chance to rotate an
/// oversize log as an ad-hoc detached one does, every time launchd starts
/// (or restarts) it.
///
/// Under launchd, a rename does not affect an already-open file descriptor:
/// the old `daemon.log` (now `daemon.log.1`) stays backing this process's
/// stdout/stderr until they are explicitly reopened, so every `println!`/
/// `eprintln!` after a rotation would otherwise keep landing in the rotated
/// file forever, not the fresh one. `dup2` onto a newly opened handle on the
/// same path fixes that. Outside supervision (a dev `cargo run`, a bare
/// `turbofig serve`), stdout/stderr are a real terminal or whatever the
/// caller piped them to, so this never touches them: redirecting a
/// developer's terminal output to a file on their behalf would be a
/// surprising, unrelated side effect.
pub fn rotate_daemon_log_and_reopen_std_streams(home: &Path) {
    rotate_daemon_log_best_effort(home);
    if !crate::supervisor::is_supervised() {
        return;
    }
    reopen_std_streams_onto(&home.join("daemon.log"));
}

/// Opens `path` (create, append) and `dup2`s both stdout (fd 1) and stderr
/// (fd 2) onto it, so every later write to either goes to this fresh file
/// handle instead of whatever they were pointing at before. Best-effort: a
/// failure to open or dup2 is logged (to whatever stderr still is) and
/// otherwise ignored, never fails startup.
#[cfg(unix)]
fn reopen_std_streams_onto(path: &Path) {
    use std::os::fd::AsRawFd;
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "turbofig: could not reopen {} after rotation: {e}",
                path.display()
            );
            return;
        }
    };
    let fd = file.as_raw_fd();
    // SAFETY: `fd` is a valid, open file descriptor owned by `file` for the
    // duration of this call; `dup2` with valid fd arguments has no other
    // preconditions. `file` is intentionally leaked (not closed) after this:
    // fd 1/2 now also reference its underlying open file description, so
    // closing `file`'s own fd here would not affect them, but keeping it
    // alive for the process lifetime (by leaking) avoids relying on that.
    unsafe {
        let _ = libc::dup2(fd, libc::STDOUT_FILENO);
        let _ = libc::dup2(fd, libc::STDERR_FILENO);
    }
    std::mem::forget(file);
}

/// No-op on non-Unix platforms: there is no POSIX fd model to `dup2` into.
/// The daemon, launchd integration, and this whole crate are macOS-only
/// today; this keeps the crate buildable elsewhere rather than failing to
/// compile.
#[cfg(not(unix))]
fn reopen_std_streams_onto(_path: &Path) {}

/// Spawns `<turbofig_binary> serve` fully detached into its own session.
///
/// `home` is the daemon's `TURBOFIG_BRIDGE_DIR`; `<home>/daemon.log` gets
/// the child's stdout and stderr, appended (never truncated, so a repeated
/// start never loses earlier log lines), unless it has grown past
/// `LOG_ROTATE_THRESHOLD_BYTES`, in which case it is first rotated to
/// `daemon.log.1` (see `rotate_log_if_oversize`) and a fresh file is opened.
/// The child's working directory is `home`, not whatever directory this
/// process happens to be running from: a detached daemon that inherited the
/// caller's cwd would otherwise keep that directory busy (e.g. blocking an
/// unmount or an `rm -rf` of a dev checkout) for as long as it runs. The
/// child inherits this process's environment unchanged, so any `TURBOFIG_*`
/// override already in the caller's environment reaches the child the same
/// way it reached the caller.
///
/// `setsid` detaches the child into its own session, but it is still this
/// process's OS child: if the daemon exits (a `/control restart`, a crash)
/// while this process keeps running, an un-reaped child becomes a zombie
/// until this process itself exits. A detached background thread calls
/// `Child::wait` so that reap happens promptly instead, without this
/// function (or its caller) blocking on it.
pub fn spawn_detached_daemon(turbofig_binary: &Path, home: &Path) -> io::Result<()> {
    std::fs::create_dir_all(home)?;
    rotate_daemon_log_best_effort(home);
    let log_path = home.join("daemon.log");
    let stdout_log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let stderr_log = stdout_log.try_clone()?;
    let dev_null = std::fs::File::open("/dev/null")?;

    let mut cmd = Command::new(turbofig_binary);
    cmd.arg("serve")
        .current_dir(home)
        .stdin(Stdio::from(dev_null))
        .stdout(Stdio::from(stdout_log))
        .stderr(Stdio::from(stderr_log));

    detach_into_own_session(&mut cmd);

    let mut child = cmd.spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
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
    fn spawn_detached_daemon_runs_the_child_with_home_as_its_working_directory() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("temp home");

        // A fake binary that prints its own cwd: /bin/pwd itself rejects the
        // extra "serve" argument this function always appends ("usage: pwd
        // [-L | -P]"), so a tiny script that ignores its arguments stands in
        // for it instead.
        let script_path = tmp.path().join("fake-turbofig-pwd.sh");
        std::fs::write(&script_path, "#!/bin/sh\npwd\n").expect("write fake binary script");
        let mut perms = std::fs::metadata(&script_path)
            .expect("script metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).expect("make script executable");

        spawn_detached_daemon(&script_path, tmp.path()).expect("spawn a trivial detached process");

        let log_path = tmp.path().join("daemon.log");
        let deadline = Instant::now() + Duration::from_secs(2);
        let contents = loop {
            let contents = std::fs::read_to_string(&log_path).unwrap_or_default();
            if !contents.trim().is_empty() {
                break contents;
            }
            assert!(
                Instant::now() < deadline,
                "the fake binary never wrote its cwd to the log"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        let expected = std::fs::canonicalize(tmp.path())
            .expect("canonicalize home")
            .display()
            .to_string();
        assert_eq!(
            contents.trim(),
            expected,
            "the child's cwd must be the turbofig home, not the caller's"
        );
    }

    /// A child left un-reaped after `setsid` detaches it still shares this
    /// process as its OS parent, so it becomes a zombie once it exits until
    /// something calls `wait` on it. This proves the background reap thread
    /// actually does that: a short-lived fake binary records its own pid,
    /// and once it exits, `ps` must stop reporting it at all (reaped), never
    /// report it in zombie state ("Z").
    #[test]
    fn spawn_detached_daemon_reaps_the_child_instead_of_leaving_a_zombie() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("temp home");
        let pidfile = tmp.path().join("child-pid");
        let script_path = tmp.path().join("fake-turbofig.sh");
        std::fs::write(
            &script_path,
            format!("#!/bin/sh\necho $$ > {}\n", pidfile.display()),
        )
        .expect("write fake binary script");
        let mut perms = std::fs::metadata(&script_path)
            .expect("script metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).expect("make script executable");

        spawn_detached_daemon(&script_path, tmp.path()).expect("spawn the fake daemon");

        let deadline = Instant::now() + Duration::from_secs(2);
        let pid: i32 = loop {
            if let Ok(s) = std::fs::read_to_string(&pidfile) {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    break trimmed.parse().expect("recorded pid must parse");
                }
            }
            assert!(
                Instant::now() < deadline,
                "the fake daemon never recorded its own pid"
            );
            std::thread::sleep(Duration::from_millis(10));
        };

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let output = std::process::Command::new("ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()
                .expect("run ps");
            let stat = String::from_utf8_lossy(&output.stdout);
            let stat = stat.trim();
            if stat.is_empty() {
                break; // no longer in the process table at all: reaped.
            }
            assert!(
                !stat.starts_with('Z'),
                "the child must never be left as a zombie: pid {pid} stat {stat:?}"
            );
            assert!(
                Instant::now() < deadline,
                "the child was never reaped within the deadline (last stat {stat:?})"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
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
    fn rename_log_tolerating_concurrent_rotation_treats_a_missing_source_as_success() {
        // Simulates a concurrent caller having already won the rotation race
        // between this caller's own `metadata` check and its `rename`: the
        // source is gone by the time this call runs, the same `NotFound`
        // kind a losing `rename` call would get.
        let tmp = tempfile::tempdir().expect("tempdir");
        let already_gone = tmp.path().join("daemon.log");
        let rotated = tmp.path().join("daemon.log.1");

        rename_log_tolerating_concurrent_rotation(&already_gone, &rotated)
            .expect("a concurrent winner already rotating this log must not be an error");
        assert!(
            !rotated.exists(),
            "losing the race must not create a destination out of nothing"
        );
    }

    #[test]
    fn rotate_daemon_log_best_effort_never_panics_on_an_unrotatable_log() {
        // No log and no home directory at all: rotate_log_if_oversize's own
        // `metadata` call fails with NotFound, which is already a no-op, so
        // this just exercises that the best-effort wrapper never panics
        // regardless of the underlying result.
        let tmp = tempfile::tempdir().expect("tempdir");
        rotate_daemon_log_best_effort(&tmp.path().join("does-not-exist"));
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
