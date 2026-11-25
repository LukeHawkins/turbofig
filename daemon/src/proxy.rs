//! `turbofig mcp`: a stdio MCP server that forwards every tool call onto the
//! running daemon's `POST /job` endpoint.
//!
//! This is for a native MCP client (`claude mcp add turbofig -- turbofig
//! mcp`) that spawns its own child process and speaks MCP over that child's
//! stdin/stdout, rather than talking streamable-HTTP to the daemon's `/mcp`
//! port directly. The daemon itself still owns all plugin state (connected
//! plugins, the pairing token, the screenshot directory); this proxy holds
//! only a random per-process session id (see `ProxyHandler::session_id`),
//! sent with every `/job` call so the daemon's fileKey pairing behaves the
//! same as an HTTP MCP session's. A lost daemon (crash, `brew upgrade`, a
//! manual `kill`) is started again once, transparently, from inside a tool
//! call.
//!
//! `ProxyHandler` shares its tool names and parameter types with
//! `mcp::TurbofigHandler` (the exact same `FileTargetParams`,
//! `ExecuteParams`, `SelectionParams`, `ScreenshotParams` structs, so the
//! JSON Schemas are identical by construction) and its `Job`-to-
//! `CallToolResult` mapping with the bridge (`bridge::job::Job`) and
//! `mcp::call_tool_result`, so all three transports (HTTP MCP, stdio MCP,
//! the filesystem bridge) resolve the same contract from one set of types.
//!
//! The `#[tool(description = "...")]` text itself cannot be factored into a
//! shared `const`: `rmcp-macros` requires a string literal there, not a
//! path expression (it parses the attribute with `darling`, which rejects
//! anything but a literal for this field). The four descriptions below are
//! therefore the one piece of this file that is a deliberate, matching copy
//! of `mcp.rs`'s; `proxy_tools_list_matches_the_http_mcp_tools_list` in
//! `daemon/tests/proxy.rs` compares a live `tools/list` from each transport
//! byte-for-byte, so any future drift between the two fails a test instead
//! of silently diverging.

use crate::bridge::job::Job;
use crate::mcp::{
    call_tool_result, ExecuteParams, FileTargetParams, ScreenshotParams, SelectionParams,
};
use rmcp::{
    handler::server::wrapper::Parameters, model::*, tool, tool_handler, tool_router,
    transport::io::stdio, ErrorData as McpError, ServerHandler, ServiceExt,
};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Longest the proxy waits, after an authenticated `/control restart`, for
/// `/health` to stop answering before giving up and proceeding with whatever
/// daemon is still there. The daemon's own drain wait is up to 60 s
/// (`control::CONTROL_DRAIN_MAX_WAIT`); this must comfortably outlast that.
const UNREACHABLE_DEADLINE: Duration = Duration::from_secs(65);

/// Longest a tool call waits for `do_bootstrap` (the health check, the
/// daemon start, and the version handoff) to finish, on top of whatever
/// deadlines that work already carries internally (`spawn::HEALTH_DEADLINE`,
/// `UNREACHABLE_DEADLINE`). This is a backstop, not the expected wait: it
/// only matters if a future change to that work drops one of its own
/// deadlines, so a tool call still fails cleanly instead of hanging forever.
/// It must outlast the slowest healthy path: about 114 s (2 s health check,
/// 2 s control request, `UNREACHABLE_DEADLINE` 65 s, the 30 s wait for a
/// launchd relaunch, then `spawn::HEALTH_DEADLINE` 15 s).
const TOOL_CALL_BOOTSTRAP_DEADLINE: Duration = Duration::from_secs(150);

/// Longest `restart_for_upgrade` waits for launchd's own relaunch of a
/// supervised daemon to answer `/health`, before giving up on it and
/// spawning a new daemon itself. See `must_spawn_after_restart`'s doc
/// comment for why this wait exists at all.
const SUPERVISED_RELAUNCH_WAIT: Duration = Duration::from_secs(30);

/// Starts the stdio MCP proxy: serves the four tools over stdio at once, so
/// `initialize` and `tools/list` (both answered from static data, needing
/// neither the daemon nor the token) never wait on anything. Ensuring a
/// daemon is reachable on `mcp_port` (starting one via
/// `spawn::spawn_detached_daemon` if `GET /health` fails) and handing off to
/// a newer binary if the running daemon is older than this one both happen
/// in a background task instead (`do_bootstrap`): the version handoff's own
/// drain wait is up to 60 s (`control::CONTROL_DRAIN_MAX_WAIT`), and running
/// it before serving stdio at all used to leave `initialize` unanswered for
/// up to that long, which can trip an MCP client's own startup timeout. The
/// first tool call that actually needs the daemon awaits the same
/// background work (`ProxyHandler::bootstrapped`), bounded by
/// `TOOL_CALL_BOOTSTRAP_DEADLINE`.
///
/// `turbofig_binary` and `home` are only used if the daemon needs to be
/// (re)started, either at startup or mid-session after a lost connection.
pub async fn run(mcp_port: u16, home: PathBuf, turbofig_binary: PathBuf) -> Result<(), String> {
    let handler = ProxyHandler {
        mcp_port,
        turbofig_binary,
        home,
        session_id: crate::token::random_token_hex(),
        bootstrap: std::sync::Arc::new(tokio::sync::OnceCell::new()),
    };

    // Kick the bootstrap off right away in the background, so a tool call
    // that arrives before the client even finishes `initialize` does not
    // have to wait for it to start, only to finish. `ensure_bootstrapped`
    // uses the same `OnceCell` a tool call awaits, so this and a concurrent
    // first tool call can never run the work twice.
    let background = handler.clone();
    tokio::spawn(async move {
        if let Err(e) = background.ensure_bootstrapped().await {
            eprintln!("turbofig mcp: startup failed: {e}");
        }
    });

    let service = handler
        .serve(stdio())
        .await
        .map_err(|e| format!("turbofig mcp: failed to start the stdio transport: {e}"))?;
    service
        .waiting()
        .await
        .map_err(|e| format!("turbofig mcp: the stdio transport ended unexpectedly: {e}"))?;
    Ok(())
}

/// Why `post_job` failed: the daemon check, or the HTTP call.
#[derive(Debug)]
enum PostError {
    Verify(crate::daemon_client::VerifyError),
    Http(reqwest::Error),
}

impl std::fmt::Display for PostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PostError::Verify(e) => e.fmt(f),
            PostError::Http(e) => e.fmt(f),
        }
    }
}

/// Everything a tool call needs that can only be known once a daemon is
/// confirmed reachable: the two HTTP clients (see `run`'s old doc comment,
/// now on `do_bootstrap`, for why there are two) and the pairing token.
struct Bootstrapped {
    /// Bounds only the connect phase of `/job` calls the same way; no
    /// overall timeout, since a real job can legitimately run far longer.
    job_client: reqwest::Client,
    /// The pairing token read from `<home>/token`. Sent as `Authorization:
    /// Bearer <token>` on every `/job` call: the daemon requires it since
    /// another local macOS account can also reach 127.0.0.1 (see
    /// `mcp::require_bearer_token`). Behind a lock, not a plain `String`: a
    /// token rotation (`turbofig stop`, delete the token, `turbofig start`)
    /// while this proxy's session stays open means the token read at
    /// bootstrap can go stale, so `post_job` re-reads and updates this on a
    /// 401 instead of failing every call for the rest of the session.
    token: tokio::sync::RwLock<String>,
}

/// Ensures a daemon is reachable on `mcp_port` (starting one via
/// `spawn::spawn_detached_daemon` if `GET /health` fails), hands off to a
/// newer binary if the running daemon is older than this one, and reads the
/// pairing token. Run from a background task at startup and, via the same
/// `OnceCell`, awaited by the first tool call that needs it (see `run` and
/// `ProxyHandler::bootstrapped`): either way this body runs exactly once.
async fn do_bootstrap(
    mcp_port: u16,
    home: PathBuf,
    turbofig_binary: PathBuf,
) -> Result<Bootstrapped, String> {
    // Two clients, deliberately: `health_client` bounds `/health` and
    // `/control` calls to `ADMIN_CLIENT_TIMEOUT` total, so a wedged daemon or
    // a foreign process on this port can never hang the proxy forever.
    // `job_client` only bounds the *connect* phase the same way; a real
    // `/job` call (an `execute` op especially) can legitimately run for far
    // longer than that, up to the daemon's own `TURBOFIG_REQUEST_TIMEOUT_MS`.
    let health_client = crate::spawn::build_admin_client()
        .map_err(|e| format!("turbofig mcp: could not build the HTTP client: {e}"))?;
    let job_client = crate::spawn::build_job_client()
        .map_err(|e| format!("turbofig mcp: could not build the HTTP client: {e}"))?;

    // Only a daemon that proves it holds the pairing token is trusted. The
    // token goes to nothing else (see `daemon_client`).
    // An older same-user daemon (no proof support) is allowed here only to be
    // handed off: it gets a stop/restart request and nothing else. No job and
    // no full `/health` goes to it, and the proxy waits for a verified daemon.
    let (health, listener) =
        match crate::daemon_client::verify_allowing_legacy_in_home(mcp_port, &home).await {
            Ok(found) => found,
            Err(crate::daemon_client::VerifyError::Unreachable) => {
                eprintln!("turbofig mcp: no daemon reachable on port {mcp_port}; starting one");
                crate::spawn::spawn_detached_daemon(&turbofig_binary, &home)
                    .map_err(|e| format!("turbofig mcp: could not start the daemon: {e}"))?;
                let health = crate::daemon_client::wait_for_verified_daemon(
                    mcp_port,
                    &home,
                    crate::spawn::HEALTH_DEADLINE,
                )
                .await
                .map_err(|e| format!("turbofig mcp: {e}"))?;
                (health, crate::daemon_client::Listener::Verified)
            }
            Err(e) => return Err(format!("turbofig mcp: {e}")),
        };
    let legacy = listener == crate::daemon_client::Listener::LegacySameUser;

    // The daemon is verified, so its token file is on disk and the daemon
    // proved it holds that token.
    let token = crate::token::read_token_file(&home).await.ok_or_else(|| {
        "turbofig mcp: could not read the pairing token to authenticate /job calls".to_owned()
    })?;

    let daemon_version = health["version"].as_str().unwrap_or_default().to_owned();
    // `supervised` is only on the authenticated payload (see
    // `mcp::health_handler`'s doc comment). `verified_full_health` verifies
    // again before it sends the token. A failure here (the daemon went away
    // in between) just means "assume unsupervised", the same conservative
    // default `restart_for_upgrade` already falls back to.
    // An older daemon is unproven: it gets no full `/health`, so assume it is
    // unsupervised.
    let daemon_supervised = if legacy {
        false
    } else {
        crate::daemon_client::verified_full_health(mcp_port, &token)
            .await
            .ok()
            .and_then(|h| h.get("supervised").and_then(serde_json::Value::as_bool))
            .unwrap_or(false)
    };
    if legacy_needs_handoff(legacy) {
        // An unproven same-user daemon is always handed off, whatever its
        // version: only a verified daemon may run a job.
        hand_off_legacy_daemon(mcp_port, &home).await?;
    } else {
        handle_version_handoff(
            &health_client,
            mcp_port,
            &home,
            &turbofig_binary,
            &daemon_version,
            daemon_supervised,
        )
        .await?;
    }
    if legacy {
        // No job runs until a VERIFIED daemon answers. The handoff above
        // already waits for one when it restarted the old daemon.
        crate::daemon_client::wait_for_verified_daemon(
            mcp_port,
            &home,
            crate::spawn::HEALTH_DEADLINE,
        )
        .await
        .map_err(|e| format!("turbofig mcp: {e}"))?;
    }

    Ok(Bootstrapped {
        job_client,
        token: tokio::sync::RwLock::new(token),
    })
}

/// This proxy's own version for the handoff comparison.
///
/// Always `CARGO_PKG_VERSION`, with one **debug-build-only** escape hatch:
/// `TURBOFIG_TEST_OWN_VERSION_OVERRIDE` lets a test simulate "an old proxy
/// talking to a newer daemon" without a second real build. See
/// `mcp::reported_version` for the matching daemon-side override and why
/// `#[cfg(debug_assertions)]` is the right gate for both.
fn own_version() -> String {
    #[cfg(debug_assertions)]
    if let Ok(v) = std::env::var("TURBOFIG_TEST_OWN_VERSION_OVERRIDE") {
        return v;
    }
    env!("CARGO_PKG_VERSION").to_owned()
}

/// Parses both versions as semver and compares them. `None` when either
/// fails to parse: an unparseable version must never be treated as older or
/// newer, only as "cannot tell, do nothing".
fn compare_versions(a: &str, b: &str) -> Option<Ordering> {
    let va = semver::Version::parse(a).ok()?;
    let vb = semver::Version::parse(b).ok()?;
    Some(va.cmp(&vb))
}

/// The stdio MCP handler. Holds no plugin state of its own: every tool call
/// is forwarded to the daemon's `POST /job` and the daemon's answer is
/// returned as-is. Besides its own `session_id` (see below), everything
/// else it needs (the HTTP clients, the pairing token) only exists once
/// `bootstrap` has resolved: see `do_bootstrap` and `bootstrapped`.
#[derive(Clone)]
struct ProxyHandler {
    mcp_port: u16,
    /// The running `turbofig` binary, re-invoked with `serve` if the daemon
    /// needs to be (re)started.
    turbofig_binary: PathBuf,
    /// The daemon's `TURBOFIG_BRIDGE_DIR`, passed to a restart the same way
    /// it reached this process.
    home: PathBuf,
    /// A random id generated once per proxy process (not persisted, not the
    /// pairing token), sent as the `X-Turbofig-Session` header on every
    /// `/job` call. The daemon's `job_handler` reads it (`mcp::
    /// job_session_id`) and routes with it exactly like an HTTP MCP
    /// session's `mcp-session-id`: with no explicit `fileKey`, the first
    /// call this process makes against a given file pairs that file to this
    /// session, and every later call with no `fileKey` re-routes to it,
    /// which is what lets two files stay open and addressable without every
    /// stdio call naming a `fileKey` explicitly.
    session_id: String,
    /// Resolves to `do_bootstrap`'s result, computed at most once no matter
    /// how many callers (the background task in `run`, and every tool call
    /// via `bootstrapped`) race to need it. `Arc`-wrapped so every clone of
    /// this handler (`rmcp` clones it per call) shares the one in-progress
    /// or completed bootstrap, rather than each starting its own.
    bootstrap: std::sync::Arc<tokio::sync::OnceCell<Bootstrapped>>,
}

impl ProxyHandler {
    /// Awaits `do_bootstrap`, running it on first call and simply waiting
    /// for it on every later or concurrent one (`OnceCell` guarantees the
    /// body runs exactly once). Not bounded by any deadline itself: `run`'s
    /// background task calls this directly, with nothing to time out
    /// against; `bootstrapped` is the bounded wrapper a tool call uses.
    async fn ensure_bootstrapped(&self) -> Result<&Bootstrapped, String> {
        let mcp_port = self.mcp_port;
        let home = self.home.clone();
        let turbofig_binary = self.turbofig_binary.clone();
        self.bootstrap
            .get_or_try_init(|| do_bootstrap(mcp_port, home, turbofig_binary))
            .await
    }

    /// The bounded wrapper a tool call uses: waits for `ensure_bootstrapped`
    /// up to `TOOL_CALL_BOOTSTRAP_DEADLINE`, on top of whatever deadlines
    /// that work already carries internally. `initialize` and `tools/list`
    /// never call this at all (see `run`'s doc comment); only a tool call
    /// that actually needs the daemon does.
    async fn bootstrapped(&self) -> Result<&Bootstrapped, String> {
        match tokio::time::timeout(TOOL_CALL_BOOTSTRAP_DEADLINE, self.ensure_bootstrapped()).await
        {
            Ok(result) => result,
            Err(_) => Err(format!(
                "turbofig mcp: still starting up after {TOOL_CALL_BOOTSTRAP_DEADLINE:?}; try again shortly"
            )),
        }
    }

    /// Posts `job` to the daemon and returns its result JSON, restarting the
    /// daemon exactly once if the request never reached it.
    ///
    /// A connect error (`reqwest::Error::is_connect`) means the request was
    /// never sent at all: no daemon was listening to accept the TCP
    /// connection, so retrying after a restart is safe, the job provably
    /// never ran. Any other failure (a timeout, a connection reset after the
    /// request was already sent, a body-read error) leaves the daemon's side
    /// unknown: the job may have already run, so this never retries it; it
    /// only reports the uncertainty back to the caller.
    async fn run_job(&self, job: Job) -> serde_json::Value {
        let bootstrapped = match self.bootstrapped().await {
            Ok(b) => b,
            Err(e) => return serde_json::json!({"ok": false, "error": e}),
        };
        match self.post_job(bootstrapped, &job).await {
            Ok(value) => value,
            Err(PostError::Verify(crate::daemon_client::VerifyError::Unreachable)) => {
                eprintln!("turbofig mcp: lost the daemon connection; starting it again once");
                self.restart_and_retry(bootstrapped, &job).await
            }
            Err(PostError::Verify(e)) => serde_json::json!({"ok": false, "error": e.to_string()}),
            Err(PostError::Http(e)) if e.is_connect() => {
                eprintln!("turbofig mcp: lost the daemon connection; starting it again once");
                self.restart_and_retry(bootstrapped, &job).await
            }
            Err(PostError::Http(e)) => serde_json::json!({
                "ok": false,
                "error": format!(
                    "could not get the daemon's response; the job may already have run: {e}"
                )
            }),
        }
    }

    /// Verifies the daemon through the job client, right before a send. The
    /// check reuses the pooled keep-alive connection that the send then
    /// uses, so the token cannot reach a process that took the port after
    /// the daemon died. A rotated token file is picked up on a failed check.
    async fn verify_before_send(
        &self,
        bootstrapped: &Bootstrapped,
    ) -> Result<(), crate::daemon_client::VerifyError> {
        let url = crate::daemon_client::base_url(self.mcp_port);
        let token = bootstrapped.token.read().await.clone();
        let first =
            crate::daemon_client::verify_with_client(&bootstrapped.job_client, &url, &token).await;
        let Err(crate::daemon_client::VerifyError::Unverified { .. }) = first else {
            return first.map(|_| ());
        };
        match crate::token::read_token_file(&self.home).await {
            Some(fresh) if fresh != token => {
                crate::daemon_client::verify_with_client(&bootstrapped.job_client, &url, &fresh)
                    .await?;
                *bootstrapped.token.write().await = fresh;
                Ok(())
            }
            _ => first.map(|_| ()),
        }
    }

    /// Starts the daemon again (verified), then runs the job once more.
    async fn restart_and_retry(&self, bootstrapped: &Bootstrapped, job: &Job) -> serde_json::Value {
        if let Err(restart_err) = self.restart_daemon().await {
            return serde_json::json!({
                "ok": false,
                "error": format!(
                    "the daemon was unreachable and could not be restarted: {restart_err}"
                )
            });
        }
        match self.post_job(bootstrapped, job).await {
            Ok(value) => value,
            Err(e2) => serde_json::json!({
                "ok": false,
                "error": format!("the daemon restarted but this job still failed: {e2}")
            }),
        }
    }

    /// One `POST /job` call, retried once on a 401. Returns the raw
    /// `reqwest::Error` so `run_job` can tell a connect failure from any
    /// other kind (a 401 never surfaces as an `Err` here: it is either
    /// resolved by the retry or passed through as the daemon's own
    /// `{"ok":false,...}` body, exactly like any other job failure).
    ///
    /// The token read at bootstrap can go stale (a rotation: `turbofig
    /// stop`, delete the token, `turbofig start`, while this proxy's agent
    /// session stays open), which would otherwise fail every `/job` call for
    /// the rest of the session. On a 401, re-read `<home>/token` and retry
    /// once with whatever it now holds; a second 401 (a real auth problem,
    /// not a stale cache) is returned to the caller as-is.
    async fn post_job(
        &self,
        bootstrapped: &Bootstrapped,
        job: &Job,
    ) -> Result<serde_json::Value, PostError> {
        self.verify_before_send(bootstrapped)
            .await
            .map_err(PostError::Verify)?;
        let token = bootstrapped.token.read().await.clone();
        let (status, value) = self
            .send_job(bootstrapped, &token, job)
            .await
            .map_err(PostError::Http)?;
        if status != reqwest::StatusCode::UNAUTHORIZED {
            return Ok(value);
        }
        let Some(fresh_token) = crate::token::read_token_file(&self.home).await else {
            return Ok(value);
        };
        // The fresh token goes only to a daemon that proves it holds it.
        if let Err(e) = crate::daemon_client::verify_with_client(
            &bootstrapped.job_client,
            &crate::daemon_client::base_url(self.mcp_port),
            &fresh_token,
        )
        .await
        {
            return Ok(serde_json::json!({"ok": false, "error": e.to_string()}));
        }
        *bootstrapped.token.write().await = fresh_token.clone();
        let (_status, value) = self
            .send_job(bootstrapped, &fresh_token, job)
            .await
            .map_err(PostError::Http)?;
        Ok(value)
    }

    /// One raw `POST /job` call with the given `token`, no retry. Returns
    /// the response status alongside the parsed body so `post_job` can
    /// decide whether to retry on a 401 without a second round trip just to
    /// re-check the status.
    async fn send_job(
        &self,
        bootstrapped: &Bootstrapped,
        token: &str,
        job: &Job,
    ) -> Result<(reqwest::StatusCode, serde_json::Value), reqwest::Error> {
        let url = format!("http://127.0.0.1:{}/job", self.mcp_port);
        let resp = bootstrapped
            .job_client
            .post(url)
            .bearer_auth(token)
            .header("X-Turbofig-Session", &self.session_id)
            .json(job)
            .send()
            .await?;
        let status = resp.status();
        // A hostile listener cannot exhaust memory: the read stops at the cap.
        let value = match crate::daemon_client::read_json_capped(
            resp,
            crate::daemon_client::RESULT_BODY_CAP,
        )
        .await
        {
            Some(v) => v,
            None => serde_json::json!({
                "ok": false,
                "error": "the daemon's response was too large or not valid JSON"
            }),
        };
        Ok((status, value))
    }

    /// Starts the daemon again and waits for it to become healthy.
    async fn restart_daemon(&self) -> Result<(), String> {
        crate::spawn::spawn_detached_daemon(&self.turbofig_binary, &self.home)
            .map_err(|e| format!("could not start the daemon: {e}"))?;
        crate::daemon_client::wait_for_verified_daemon(
            self.mcp_port,
            &self.home,
            crate::spawn::HEALTH_DEADLINE,
        )
        .await?;
        Ok(())
    }
}

/// True when the daemon must be handed off regardless of its version.
fn legacy_needs_handoff(legacy: bool) -> bool {
    legacy
}

/// True when the daemon version is older than this binary's own. An
/// unparseable version is never older: "cannot tell" does nothing.
pub fn daemon_version_is_older(daemon_version: &str) -> bool {
    compare_versions(daemon_version, &own_version()) == Some(Ordering::Less)
}

/// Restarts an older same-user daemon (no proof support) through the
/// allowed `send_control` path, then waits for a VERIFIED daemon. Shared by
/// the proxy, the menu bar and `turbofig start`. No `/job` and no full
/// `/health` goes to the old daemon.
pub async fn hand_off_legacy_daemon(mcp_port: u16, home: &Path) -> Result<(), String> {
    hand_off_older_daemon(mcp_port, home).await
}

/// Restarts a daemon that is older than this binary: one with no proof
/// support (legacy) or a verified one with a lower version. The caller has
/// already decided that it is older.
///
/// When the headless launchd service owns the daemon, the old daemon exits
/// with the restart code and launchd starts the new one, so this waits for
/// that instead of racing it with its own spawn (the launchd `serve` would
/// then exit "already running" and the service would stay stopped).
pub async fn hand_off_older_daemon(mcp_port: u16, home: &Path) -> Result<(), String> {
    eprintln!("turbofig: an older turbofig daemon is running; restarting it");
    // The Homebrew binary, not an app bundle's copy: see `upgrade::daemon_start_binary`.
    let binary = crate::upgrade::daemon_start_binary()
        .map_err(|e| format!("failed to determine the running binary's path: {e}"))?;
    let client = crate::spawn::build_admin_client()
        .map_err(|e| format!("could not build the HTTP client: {e}"))?;
    restart_for_upgrade(
        &client,
        mcp_port,
        home,
        &binary,
        crate::upgrade::headless_service_owns_daemon(),
    )
    .await
}

/// Compares `daemon_version` against this proxy's own version, once, at
/// startup, and restarts the daemon if it is older. Never restarts a daemon
/// that is newer or the same version: an agent session started before a
/// `brew upgrade` still runs an old proxy, and it must neither downgrade the
/// daemon nor fight a newer proxy for control of it.
async fn handle_version_handoff(
    health_client: &reqwest::Client,
    mcp_port: u16,
    home: &Path,
    turbofig_binary: &Path,
    daemon_version: &str,
    daemon_supervised: bool,
) -> Result<(), String> {
    let mine = own_version();
    match compare_versions(daemon_version, &mine) {
        Some(Ordering::Less) => {
            eprintln!(
                "turbofig mcp: the daemon ({daemon_version}) is older than this proxy ({mine}); restarting it"
            );
            restart_for_upgrade(
                health_client,
                mcp_port,
                home,
                turbofig_binary,
                daemon_supervised,
            )
            .await
        }
        Some(Ordering::Greater) => {
            eprintln!(
                "turbofig mcp: the daemon ({daemon_version}) is newer than this proxy ({mine}); leaving it running"
            );
            Ok(())
        }
        Some(Ordering::Equal) | None => Ok(()),
    }
}

/// Asks the daemon to restart (drain then exit) via the authenticated
/// `/control` path, waits for it to actually go away, then makes sure a new
/// one is running.
///
/// Tolerant of every kind of race on purpose:
/// - A concurrent proxy may have already triggered the same restart;
///   `/control` replies the exact same `202 {"draining":true}` either way,
///   so this treats it exactly like a normal restart ack (still waits,
///   still spawns).
/// - The daemon may not go away at all (the restart request failed to reach
///   it, or drained past `UNREACHABLE_DEADLINE`): this logs a warning and
///   falls through to use whatever is still running, rather than blindly
///   spawning a second daemon to fight the first over the port.
/// - When the old daemon was supervised (`daemon_was_supervised`, from its
///   own `/health`), this does not race launchd's relaunch with its own
///   spawn: it waits up to `SUPERVISED_RELAUNCH_WAIT` for `/health` to
///   answer again on its own first, and only spawns a daemon itself as a
///   fallback if nothing does. See `must_spawn_after_restart`'s doc comment
///   for why racing that relaunch was a problem worth avoiding.
async fn restart_for_upgrade(
    health_client: &reqwest::Client,
    mcp_port: u16,
    home: &Path,
    turbofig_binary: &Path,
    daemon_was_supervised: bool,
) -> Result<(), String> {
    let Some(token) = crate::token::read_token_file(home).await else {
        eprintln!(
            "turbofig mcp: could not read the pairing token to request a restart; leaving the old daemon running"
        );
        return Ok(());
    };

    // What the lock info says now (no network): a restart that ends as an
    // in-place `exec` never frees the port or the lock for good, so it is
    // recognized by a new build id or a version that is no longer older.
    let before = crate::secure_fs::read_lock_info(home);
    let asked_build_id = before.as_ref().and_then(|i| i.build_id.clone());
    let wait = crate::upgrade::restart_wait(
        UNREACHABLE_DEADLINE,
        before.as_ref().and_then(|i| i.request_timeout),
    );

    // `send_control` verifies the daemon before it sends the token.
    match crate::daemon_client::send_control(mcp_port, &token, "restart").await {
        Ok(status) if status.is_success() => {}
        Ok(status) => {
            eprintln!(
                "turbofig mcp: the daemon refused the restart request ({status}); leaving it running"
            );
            return Ok(());
        }
        Err(crate::daemon_client::ClientError::Verify(e)) => {
            eprintln!("turbofig mcp: {e}; the restart request was not sent");
            return Ok(());
        }
        Err(e) => {
            eprintln!(
                "turbofig mcp: could not reach the daemon to request a restart ({e}); leaving it running"
            );
            return Ok(());
        }
    }

    let own = own_version();
    match wait_for_old_daemon_to_leave(
        wait,
        RESTART_POLL_INTERVAL,
        || async {
            crate::spawn::wait_for_unreachable(health_client, mcp_port, Duration::ZERO).await
        },
        || {
            crate::upgrade::restart_took_effect(
                asked_build_id.as_deref(),
                crate::secure_fs::read_lock_info(home).as_ref(),
                &own,
            )
        },
    )
    .await
    {
        RestartWait::Gone => {}
        RestartWait::RestartedInPlace => {
            // The upgrade watcher was already draining and `exec`ed the new
            // build: a daemon runs already, so nothing is spawned and no
            // relaunch is waited for.
            return crate::daemon_client::wait_for_verified_daemon(
                mcp_port,
                home,
                crate::spawn::HEALTH_DEADLINE,
            )
            .await
            .map(|_| ());
        }
        RestartWait::TimedOut => {
            eprintln!(
                "turbofig mcp: the daemon was still answering {wait:?} after the restart request; leaving it running"
            );
            return Ok(());
        }
    }

    // The old daemon is gone. If it was supervised, ask launchd to start the
    // service (`kickstart`, no `-k`: a running service is left alone) and give
    // it a chance to answer on its own first, rather than racing it with our
    // own spawn (see `must_spawn_after_restart`'s doc comment for why that
    // race was a problem). A kickstart that fails means the service is not
    // loaded: launchd will not relaunch anything, so spawn at once instead of
    // waiting 30 s for nothing.
    let relies = relies_on_launchd(
        daemon_was_supervised,
        crate::upgrade::headless_plist_exists(),
        crate::upgrade::kickstart_headless_service,
    );
    let launchd_relaunch_answered = relies
        && crate::daemon_client::wait_for_verified_daemon(mcp_port, home, SUPERVISED_RELAUNCH_WAIT)
            .await
            .is_ok();

    if must_spawn_after_restart(relies, launchd_relaunch_answered) {
        if relies {
            eprintln!(
                "turbofig mcp: launchd did not relaunch the daemon within {SUPERVISED_RELAUNCH_WAIT:?}; starting one ourselves"
            );
        } else if daemon_was_supervised {
            eprintln!(
                "turbofig mcp: the launchd service is not loaded; starting the daemon ourselves"
            );
        }
        if let Err(e) = crate::spawn::spawn_detached_daemon(turbofig_binary, home) {
            eprintln!(
                "turbofig mcp: could not start a new daemon after the restart ({e}); hoping another starter wins"
            );
        }
    }

    crate::daemon_client::wait_for_verified_daemon(mcp_port, home, crate::spawn::HEALTH_DEADLINE)
        .await
        .map(|_| ())
}

/// True when the caller may wait for launchd to start the next daemon.
/// - Not supervised: false, the caller spawns.
/// - Supervised and the headless plist is not in this LaunchAgents folder: true,
///   the old behavior (launchd relaunches on its own, nothing here can nudge
///   it). `kickstart` is never run then.
/// - Supervised and the plist exists: `kickstart` (`launchctl kickstart`, no
///   `-k`) decides. If launchd accepts it, the service is loaded and starts
///   the next daemon: wait. If it fails, the service is not loaded (the old
///   daemon was started by hand, or an exit 0 left the service stopped): no
///   relaunch is coming, so the caller spawns at once instead of waiting
///   `SUPERVISED_RELAUNCH_WAIT` with no daemon.
fn relies_on_launchd(
    daemon_was_supervised: bool,
    plist_exists: bool,
    kickstart: impl FnOnce() -> bool,
) -> bool {
    daemon_was_supervised && (!plist_exists || kickstart())
}

/// How often `wait_for_old_daemon_to_leave` looks.
const RESTART_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How the old daemon left after a restart request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestartWait {
    /// `/health` stopped answering: the process exited.
    Gone,
    /// The daemon restarted in place (`exec`, lock kept): `/health` may never
    /// look down, but the lock info shows another build.
    RestartedInPlace,
    /// Still the same daemon when `limit` ended.
    TimedOut,
}

/// Waits for the old daemon to go away or to restart in place, for at most
/// `limit`. `gone` is one `/health` probe (true when nothing answers).
/// `replaced` reads the lock info (`upgrade::restart_took_effect`). Checking
/// `replaced` is what keeps a request that joined an upgrade drain from
/// waiting out the whole limit: the drain ends in an `exec`, not an exit.
async fn wait_for_old_daemon_to_leave<F, Fut>(
    limit: Duration,
    poll: Duration,
    mut gone: F,
    mut replaced: impl FnMut() -> bool,
) -> RestartWait
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let until = std::time::Instant::now() + limit;
    loop {
        if gone().await {
            return RestartWait::Gone;
        }
        if replaced() {
            return RestartWait::RestartedInPlace;
        }
        if std::time::Instant::now() >= until {
            return RestartWait::TimedOut;
        }
        tokio::time::sleep(poll).await;
    }
}

/// True when `restart_for_upgrade` must spawn a new daemon itself, after the
/// old one has gone away.
///
/// Before this existed, the proxy always spawned its own daemon the moment
/// the old one went away, racing launchd's own relaunch under
/// `TURBOFIG_SUPERVISED=1`. If this proxy's spawn won that race, `serve`'s
/// own "already running" pre-check (`already_running_health`, `main.rs`)
/// makes launchd's own `serve` invocation exit 0 at once, so launchd leaves
/// the daemon stopped (`KeepAlive: {SuccessfulExit: false}`) rather than
/// supervising it, and a later crash would leave it down. Waiting for the
/// relaunch first avoids that outright, and only falls back to spawning when nothing
/// answers within `SUPERVISED_RELAUNCH_WAIT`: an unsupervised daemon still
/// needs this caller to spawn it, the same as always.
fn must_spawn_after_restart(daemon_was_supervised: bool, launchd_relaunch_answered: bool) -> bool {
    !daemon_was_supervised || !launchd_relaunch_answered
}

#[tool_router]
impl ProxyHandler {
    #[tool(description = "Return daemon status")]
    async fn turbofig_status(
        &self,
        Parameters(params): Parameters<FileTargetParams>,
    ) -> Result<CallToolResult, McpError> {
        let value = self.run_job(Job::Status(params)).await;
        Ok(call_tool_result(value))
    }

    #[tool(
        description = "Run async JS in the Figma plugin; `figma` and `tf.*` are in scope. Use return. Return ids, not nodes."
    )]
    async fn turbofig_execute(
        &self,
        Parameters(params): Parameters<ExecuteParams>,
    ) -> Result<CallToolResult, McpError> {
        let value = self.run_job(Job::Execute(params)).await;
        Ok(call_tool_result(value))
    }

    #[tool(description = "Return the current Figma selection")]
    async fn turbofig_get_selection(
        &self,
        Parameters(params): Parameters<SelectionParams>,
    ) -> Result<CallToolResult, McpError> {
        let value = self.run_job(Job::GetSelection(params)).await;
        Ok(call_tool_result(value))
    }

    #[tool(description = "PNG screenshot of a node. inline returns the image itself.")]
    async fn turbofig_screenshot(
        &self,
        Parameters(params): Parameters<ScreenshotParams>,
    ) -> Result<CallToolResult, McpError> {
        let value = self.run_job(Job::Screenshot(params)).await;
        Ok(call_tool_result(value))
    }
}

#[tool_handler]
impl ServerHandler for ProxyHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(crate::mcp::turbofig_server_info())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_legacy_listener_is_always_handed_off() {
        // Equal, higher and unknown versions are all handed off when legacy.
        assert!(legacy_needs_handoff(true));
        assert!(!legacy_needs_handoff(false));
    }

    #[test]
    fn a_plist_without_a_loaded_service_spawns_at_once_instead_of_waiting_for_launchd() {
        // Supervised, plist exists, kickstart failed: no service is loaded,
        // so no relaunch is coming. Spawn now, do not wait 30 s.
        let relies = relies_on_launchd(true, true, || false);
        assert!(!relies);
        assert!(must_spawn_after_restart(relies, false));
        // A loaded service: wait for it, spawn only if it never answers.
        let relies = relies_on_launchd(true, true, || true);
        assert!(relies);
        assert!(!must_spawn_after_restart(relies, true));
        assert!(must_spawn_after_restart(relies, false));
        // An unsupervised daemon: the caller always spawns, and launchd is
        // never asked.
        let relies = relies_on_launchd(false, true, || panic!("must not kickstart"));
        assert!(must_spawn_after_restart(relies, false));
    }

    #[test]
    fn without_the_headless_plist_launchctl_is_never_run() {
        // The daemon said it is supervised but this LaunchAgents folder has no
        // headless plist (a temp folder, an app-only setup): wait for the
        // relaunch as before and never touch launchctl.
        assert!(relies_on_launchd(true, false, || panic!(
            "must not kickstart"
        )));
    }

    #[tokio::test]
    async fn the_wait_stops_at_once_when_the_lock_info_shows_an_in_place_restart() {
        // The old daemon never goes down for good (an upgrade `exec`): the
        // wait must not run its 65 s limit.
        let started = std::time::Instant::now();
        let mut polls = 0;
        let outcome = wait_for_old_daemon_to_leave(
            Duration::from_secs(65),
            Duration::from_millis(5),
            || async { false },
            || {
                polls += 1;
                polls >= 3
            },
        )
        .await;
        assert_eq!(outcome, RestartWait::RestartedInPlace);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn the_wait_reports_gone_and_times_out_when_nothing_changes() {
        let gone = wait_for_old_daemon_to_leave(
            Duration::from_secs(5),
            Duration::from_millis(5),
            || async { true },
            || false,
        )
        .await;
        assert_eq!(gone, RestartWait::Gone);
        let timed_out = wait_for_old_daemon_to_leave(
            Duration::from_millis(30),
            Duration::from_millis(5),
            || async { false },
            || false,
        )
        .await;
        assert_eq!(timed_out, RestartWait::TimedOut);
    }

    #[test]
    fn compare_versions_orders_older_and_newer_correctly() {
        assert_eq!(compare_versions("0.1.0", "0.2.0"), Some(Ordering::Less));
        assert_eq!(compare_versions("0.2.0", "0.1.0"), Some(Ordering::Greater));
        assert_eq!(compare_versions("1.2.3", "1.2.3"), Some(Ordering::Equal));
    }

    #[test]
    fn daemon_version_is_older_only_for_a_lower_parseable_version() {
        // Debug builds honour `TURBOFIG_TEST_OWN_VERSION_OVERRIDE`; no test
        // here sets it, so `own_version()` is the crate version.
        assert!(daemon_version_is_older("0.0.1"));
        assert!(!daemon_version_is_older(env!("CARGO_PKG_VERSION")));
        assert!(!daemon_version_is_older("999.0.0"));
        assert!(!daemon_version_is_older("not-a-version"));
    }

    #[test]
    fn compare_versions_is_none_for_an_unparseable_version() {
        assert_eq!(compare_versions("not-a-version", "0.1.0"), None);
        assert_eq!(compare_versions("0.1.0", "not-a-version"), None);
        assert_eq!(compare_versions("not-a-version", "also-not"), None);
    }

    /// The stdio proxy must report its own name and version, not `rmcp`'s
    /// (see `mcp::turbofig_server_info`'s doc comment for why
    /// `Implementation::from_build_env()` alone gets this wrong).
    #[test]
    fn proxy_handler_reports_turbofig_name_and_its_own_version() {
        let info = crate::mcp::turbofig_server_info();
        assert_eq!(info.name, "turbofig");
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert_ne!(info.name, "rmcp");
        assert_ne!(info.version, "3.1.0");
    }

    #[test]
    fn must_spawn_after_restart_spawns_for_an_unsupervised_daemon_regardless_of_relaunch() {
        assert!(
            must_spawn_after_restart(false, false),
            "unsupervised and nothing answered: must spawn"
        );
        assert!(
            must_spawn_after_restart(false, true),
            "unsupervised, even if something happened to answer: this caller always spawns, \
             nothing else is watching to"
        );
    }

    #[test]
    fn must_spawn_after_restart_waits_for_a_supervised_relaunch_before_spawning() {
        assert!(
            !must_spawn_after_restart(true, true),
            "supervised and launchd already relaunched it: must not race it with our own spawn"
        );
        assert!(
            must_spawn_after_restart(true, false),
            "supervised but nothing answered within the wait: must fall back to spawning"
        );
    }
}

#[cfg(test)]
mod client_trust_tests {
    use super::*;
    use crate::daemon_client::test_mock::{mock, Mode, TOKEN};
    use crate::mcp::FileTargetParams;

    fn handler(port: u16, home: &Path) -> ProxyHandler {
        ProxyHandler {
            mcp_port: port,
            turbofig_binary: PathBuf::from("/nonexistent/turbofig"),
            home: home.to_path_buf(),
            session_id: "test-session".to_owned(),
            bootstrap: std::sync::Arc::new(tokio::sync::OnceCell::new()),
        }
    }

    fn status_job() -> Job {
        Job::Status(FileTargetParams { file_key: None })
    }

    /// A listener that fails the proof gets no token, and the job error
    /// names the port.
    #[tokio::test]
    async fn an_unverified_listener_never_receives_the_token() {
        for mode in [Mode::NoProof, Mode::WrongProof] {
            let (port, seen) = mock(mode).await;
            let home = tempfile::tempdir().expect("tempdir");
            crate::daemon_client::test_mock::write_token(home.path());
            let value = handler(port, home.path()).run_job(status_job()).await;
            assert_eq!(value["ok"], false);
            let err = value["error"].as_str().expect("error text");
            assert!(err.contains(&format!("127.0.0.1:{port}")), "{err}");
            assert!(err.contains("not your turbofig daemon"), "{err}");
            let g = seen.lock().unwrap();
            assert!(g.requests >= 1);
            assert!(g.auth.is_empty(), "token leaked: {:?}", g.auth);
        }
    }

    /// A verified daemon gets the token, and the job result comes back.
    #[tokio::test]
    async fn a_verified_daemon_receives_the_token_and_runs_the_job() {
        let (port, seen) = mock(Mode::Sign).await;
        let home = tempfile::tempdir().expect("tempdir");
        crate::daemon_client::test_mock::write_token(home.path());
        let value = handler(port, home.path()).run_job(status_job()).await;
        assert_eq!(value["ok"], true, "{value}");
        let g = seen.lock().unwrap();
        assert!(!g.auth.is_empty());
        assert!(g.auth.iter().all(|a| a == &format!("Bearer {TOKEN}")));
    }

    /// The daemon is replaced by a listener that cannot prove itself between
    /// two jobs. The second job must never send the token to it.
    #[tokio::test]
    async fn a_listener_that_replaces_the_daemon_between_jobs_gets_no_token() {
        use crate::daemon_client::test_mock::mock_switch;
        let (port, seen, mode) = mock_switch(Mode::Sign).await;
        let home = tempfile::tempdir().expect("tempdir");
        crate::daemon_client::test_mock::write_token(home.path());
        let h = handler(port, home.path());
        let first = h.run_job(status_job()).await;
        assert_eq!(first["ok"], true, "{first}");
        let auth_after_first = seen.lock().unwrap().auth.len();
        assert!(auth_after_first > 0);

        *mode.lock().unwrap() = Mode::NoProof;
        let second = h.run_job(status_job()).await;
        assert_eq!(second["ok"], false);
        assert!(second["error"]
            .as_str()
            .unwrap()
            .contains("not your turbofig daemon"));
        assert_eq!(
            seen.lock().unwrap().auth.len(),
            auth_after_first,
            "the second job sent Authorization to an unverified listener"
        );
    }
}
