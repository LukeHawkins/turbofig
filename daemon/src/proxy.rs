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
use std::path::PathBuf;

/// Starts the stdio MCP proxy: ensures a daemon is reachable on `mcp_port`
/// (starting one via `spawn::spawn_detached_daemon` if `GET /health` fails),
/// then serves the four tools over stdio until the client disconnects.
///
/// `turbofig_binary` and `home` are only used if the daemon needs to be
/// (re)started, either at startup or mid-session after a lost connection.
pub async fn run(mcp_port: u16, home: PathBuf, turbofig_binary: PathBuf) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .map_err(|e| format!("turbofig mcp: could not build the HTTP client: {e}"))?;

    if !daemon_is_healthy(&client, mcp_port).await {
        eprintln!("turbofig mcp: no daemon reachable on port {mcp_port}; starting one");
        crate::spawn::spawn_detached_daemon(&turbofig_binary, &home)
            .map_err(|e| format!("turbofig mcp: could not start the daemon: {e}"))?;
        crate::spawn::wait_for_health(&client, mcp_port)
            .await
            .map_err(|e| format!("turbofig mcp: {e}"))?;
    }

    let handler = ProxyHandler {
        client,
        mcp_port,
        turbofig_binary,
        home,
    };

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

/// Returns true when `GET /health` answers with a success status.
/// Any failure (connection refused, timeout, a non-success status) is
/// treated the same: not healthy, try starting a daemon.
async fn daemon_is_healthy(client: &reqwest::Client, mcp_port: u16) -> bool {
    let url = format!("http://127.0.0.1:{mcp_port}/health");
    matches!(client.get(&url).send().await, Ok(resp) if resp.status().is_success())
}

/// The stdio MCP handler. Holds no plugin or session state of its own: every
/// tool call is forwarded to the daemon's `POST /job` and the daemon's
/// answer is returned as-is.
#[derive(Clone)]
struct ProxyHandler {
    client: reqwest::Client,
    mcp_port: u16,
    /// The running `turbofig` binary, re-invoked with `serve` if the daemon
    /// needs to be (re)started.
    turbofig_binary: PathBuf,
    /// The daemon's `TURBOFIG_BRIDGE_DIR`, passed to a restart the same way
    /// it reached this process.
    home: PathBuf,
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
        let resp = self.client.post(url).json(job).send().await?;
        resp.json::<serde_json::Value>().await
    }

    /// Starts the daemon again and waits for it to become healthy.
    async fn restart_daemon(&self) -> Result<(), String> {
        crate::spawn::spawn_detached_daemon(&self.turbofig_binary, &self.home)
            .map_err(|e| format!("could not start the daemon: {e}"))?;
        crate::spawn::wait_for_health(&self.client, self.mcp_port).await
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
