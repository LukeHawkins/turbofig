//! `turbofig mcp`: a stdio MCP server that forwards every tool call onto the
//! running daemon's `POST /job` endpoint.
//!
//! This is for a native MCP client (`claude mcp add turbofig -- turbofig
//! mcp`) that spawns its own child process and speaks MCP over that child's
//! stdin/stdout, rather than talking streamable-HTTP to the daemon's `/mcp`
//! port directly. The daemon itself still owns all state (connected
//! plugins, the pairing token, the screenshot directory); this proxy holds
//! none of it; a lost daemon (crash, `brew upgrade`, a manual `kill`) is
//! started again once, transparently, from inside a tool call.
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
use std::path::PathBuf;
use std::time::Duration;

/// Longest the proxy waits, after an authenticated `/control restart`, for
/// `/health` to stop answering before giving up and proceeding with whatever
/// daemon is still there. The daemon's own drain wait is up to 60 s
/// (`control::CONTROL_DRAIN_MAX_WAIT`); this must comfortably outlast that.
const UNREACHABLE_DEADLINE: Duration = Duration::from_secs(65);

/// Starts the stdio MCP proxy: ensures a daemon is reachable on `mcp_port`
/// (starting one via `spawn::spawn_detached_daemon` if `GET /health` fails),
/// hands off to a newer binary if the running daemon is older than this one,
/// then serves the four tools over stdio until the client disconnects.
///
/// `turbofig_binary` and `home` are only used if the daemon needs to be
/// (re)started, either at startup or mid-session after a lost connection.
pub async fn run(mcp_port: u16, home: PathBuf, turbofig_binary: PathBuf) -> Result<(), String> {
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

    let health = match crate::spawn::fetch_health(&health_client, mcp_port).await {
        Some(h) => h,
        None => {
            eprintln!("turbofig mcp: no daemon reachable on port {mcp_port}; starting one");
            crate::spawn::spawn_detached_daemon(&turbofig_binary, &home)
                .map_err(|e| format!("turbofig mcp: could not start the daemon: {e}"))?;
            crate::spawn::wait_for_health(&health_client, mcp_port)
                .await
                .map_err(|e| format!("turbofig mcp: {e}"))?;
            crate::spawn::fetch_health(&health_client, mcp_port)
                .await
                .ok_or_else(|| {
                    "turbofig mcp: the daemon answered /health once but not again".to_owned()
                })?
        }
    };

    // The daemon's own startup always writes the token before its listeners
    // bind (token.rs's ensure_token runs first in run_daemon), so by the time
    // /health answered above, the token file is already there to read.
    let token = crate::token::read_token_file(&home).await.ok_or_else(|| {
        "turbofig mcp: could not read the pairing token to authenticate /job calls".to_owned()
    })?;

    let handler = ProxyHandler {
        health_client,
        job_client,
        mcp_port,
        turbofig_binary,
        home,
        token,
    };

    let daemon_version = health["version"].as_str().unwrap_or_default();
    handler.handle_version_handoff(daemon_version).await?;

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

/// The stdio MCP handler. Holds no plugin or session state of its own: every
/// tool call is forwarded to the daemon's `POST /job` and the daemon's
/// answer is returned as-is.
#[derive(Clone)]
struct ProxyHandler {
    /// Bounds `/health` and `/control` calls to `ADMIN_CLIENT_TIMEOUT` total.
    health_client: reqwest::Client,
    /// Bounds only the connect phase of `/job` calls the same way; no
    /// overall timeout, since a real job can legitimately run far longer.
    /// See `run`'s doc comment for why these are two separate clients.
    job_client: reqwest::Client,
    mcp_port: u16,
    /// The running `turbofig` binary, re-invoked with `serve` if the daemon
    /// needs to be (re)started.
    turbofig_binary: PathBuf,
    /// The daemon's `TURBOFIG_BRIDGE_DIR`, passed to a restart the same way
    /// it reached this process.
    home: PathBuf,
    /// The pairing token read from `<home>/token` at startup. Sent as
    /// `Authorization: Bearer <token>` on every `/job` call: the daemon
    /// requires it since another local macOS account can also reach
    /// 127.0.0.1 (see `mcp::require_bearer_token`).
    token: String,
}

impl ProxyHandler {
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
        match self.post_job(&job).await {
            Ok(value) => value,
            Err(e) if e.is_connect() => {
                eprintln!("turbofig mcp: lost the daemon connection; starting it again once");
                if let Err(restart_err) = self.restart_daemon().await {
                    return serde_json::json!({
                        "ok": false,
                        "error": format!(
                            "the daemon was unreachable and could not be restarted: {restart_err}"
                        )
                    });
                }
                match self.post_job(&job).await {
                    Ok(value) => value,
                    Err(e2) => serde_json::json!({
                        "ok": false,
                        "error": format!(
                            "the daemon restarted but this job still failed: {e2}"
                        )
                    }),
                }
            }
            Err(e) => serde_json::json!({
                "ok": false,
                "error": format!(
                    "could not get the daemon's response; the job may already have run: {e}"
                )
            }),
        }
    }

    /// One `POST /job` call, with no retry. Returns the raw `reqwest::Error`
    /// so `run_job` can tell a connect failure from any other kind.
    async fn post_job(&self, job: &Job) -> Result<serde_json::Value, reqwest::Error> {
        let url = format!("http://127.0.0.1:{}/job", self.mcp_port);
        let resp = self
            .job_client
            .post(url)
            .bearer_auth(&self.token)
            .json(job)
            .send()
            .await?;
        resp.json::<serde_json::Value>().await
    }

    /// Starts the daemon again and waits for it to become healthy.
    async fn restart_daemon(&self) -> Result<(), String> {
        crate::spawn::spawn_detached_daemon(&self.turbofig_binary, &self.home)
            .map_err(|e| format!("could not start the daemon: {e}"))?;
        crate::spawn::wait_for_health(&self.health_client, self.mcp_port).await
    }

    /// Compares `daemon_version` against this proxy's own version, once, at
    /// startup, and restarts the daemon if it is older. Never restarts a
    /// daemon that is newer or the same version: an agent session started
    /// before a `brew upgrade` still runs an old proxy, and it must neither
    /// downgrade the daemon nor fight a newer proxy for control of it.
    async fn handle_version_handoff(&self, daemon_version: &str) -> Result<(), String> {
        let mine = own_version();
        match compare_versions(daemon_version, &mine) {
            Some(Ordering::Less) => {
                eprintln!(
                    "turbofig mcp: the daemon ({daemon_version}) is older than this proxy ({mine}); restarting it"
                );
                self.restart_for_upgrade().await
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
    /// `/control` path, waits for it to actually go away, then makes sure a
    /// new one is running.
    ///
    /// Tolerant of every kind of race on purpose:
    /// - A concurrent proxy may have already triggered the same restart;
    ///   `/control` replies the exact same `202 {"draining":true}` either
    ///   way, so this treats it exactly like a normal restart ack (still
    ///   waits, still spawns).
    /// - The daemon may not go away at all (the restart request failed to
    ///   reach it, or drained past `UNREACHABLE_DEADLINE`): this logs a
    ///   warning and falls through to use whatever is still running, rather
    ///   than blindly spawning a second daemon to fight the first over the
    ///   port.
    /// - This proxy's own spawn attempt may lose the port-bind race to
    ///   another proxy's spawn, or (under `TURBOFIG_SUPERVISED=1`) to
    ///   launchd relaunching the stable path on its own: a lost race here is
    ///   silent and never surfaces as an error, since `wait_for_health`
    ///   below only cares that *some* daemon answers, not which process it
    ///   is.
    async fn restart_for_upgrade(&self) -> Result<(), String> {
        let Some(token) = crate::token::read_token_file(&self.home).await else {
            eprintln!(
                "turbofig mcp: could not read the pairing token to request a restart; leaving the old daemon running"
            );
            return Ok(());
        };

        let control_url = format!("http://127.0.0.1:{}/control", self.mcp_port);
        match self
            .health_client
            .post(&control_url)
            .bearer_auth(&token)
            .json(&serde_json::json!({"action": "restart"}))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {}
            Ok(resp) => {
                eprintln!(
                    "turbofig mcp: the daemon refused the restart request ({}); leaving it running",
                    resp.status()
                );
                return Ok(());
            }
            Err(e) => {
                eprintln!(
                    "turbofig mcp: could not reach the daemon to request a restart ({e}); leaving it running"
                );
                return Ok(());
            }
        }

        if !self.wait_until_unreachable(UNREACHABLE_DEADLINE).await {
            eprintln!(
                "turbofig mcp: the daemon was still answering {:?} after the restart request; leaving it running",
                UNREACHABLE_DEADLINE
            );
            return Ok(());
        }

        // The old daemon is gone. Start a new one; losing this race to
        // another proxy or to launchd is fine, see this method's doc.
        if let Err(e) = crate::spawn::spawn_detached_daemon(&self.turbofig_binary, &self.home) {
            eprintln!(
                "turbofig mcp: could not start a new daemon after the restart ({e}); hoping another starter wins"
            );
        }

        crate::spawn::wait_for_health(&self.health_client, self.mcp_port).await
    }

    /// Polls `/health` until it stops answering, or `deadline` elapses.
    /// Returns true once unreachable, false on timeout. Thin wrapper over
    /// the shared `spawn::wait_for_unreachable`, which `turbofig stop`
    /// (`main.rs`) also uses for the same "has the daemon actually gone
    /// away yet" question.
    async fn wait_until_unreachable(&self, deadline: Duration) -> bool {
        crate::spawn::wait_for_unreachable(&self.health_client, self.mcp_port, deadline).await
    }
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

    #[tool(description = "Execute JavaScript in the Figma plugin and return the result")]
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

    #[tool(description = "Capture a PNG screenshot of a Figma node")]
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
            .with_server_info(Implementation::from_build_env())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compare_versions_orders_older_and_newer_correctly() {
        assert_eq!(compare_versions("0.1.0", "0.2.0"), Some(Ordering::Less));
        assert_eq!(compare_versions("0.2.0", "0.1.0"), Some(Ordering::Greater));
        assert_eq!(compare_versions("1.2.3", "1.2.3"), Some(Ordering::Equal));
    }

    #[test]
    fn compare_versions_is_none_for_an_unparseable_version() {
        assert_eq!(compare_versions("not-a-version", "0.1.0"), None);
        assert_eq!(compare_versions("0.1.0", "not-a-version"), None);
        assert_eq!(compare_versions("not-a-version", "also-not"), None);
    }
}
