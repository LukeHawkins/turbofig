//! The MCP HTTP transport: tool parameter types, the `TurbofigHandler`, the
//! self-describing help payload, and the axum router (`/mcp`, `/job`,
//! `/control`, plus Origin and Host validation).

use crate::bridge::{job::Job, process_job};
use crate::ops::{run_execute, run_get_selection, run_screenshot, run_status};
use crate::state::AppState;
use rmcp::{
    handler::server::tool::Extension,
    handler::server::wrapper::Parameters,
    model::*,
    schemars, tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    },
    ErrorData as McpError, ServerHandler,
};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// Read the `mcp-session-id` header from the HTTP request parts.
/// Returns None when the header is absent, is not valid UTF-8, or is empty.
/// An empty header must map to None so it never records a bogus shared pairing.
fn session_id_from_parts(parts: &http::request::Parts) -> Option<&str> {
    parts
        .headers
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
}

/// Screenshot return mode. A typed enum, not a free string: an unrecognized
/// value (a typo) fails parameter deserialization loudly instead of silently
/// falling back to file mode.
#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Deserialize,
    serde::Serialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ReturnMode {
    #[default]
    File,
    Inline,
}

impl ReturnMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ReturnMode::File => "file",
            ReturnMode::Inline => "inline",
        }
    }
}

/// Parameters for tools that take only an optional target file key.
/// Used by turbofig_status only. turbofig_get_selection uses SelectionParams.
#[derive(Debug, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
pub(crate) struct FileTargetParams {
    #[serde(rename = "fileKey", default)]
    #[schemars(description = "File key; omit when one file is open")]
    pub(crate) file_key: Option<String>,
}

/// Parameters for turbofig_get_selection.
///
/// Extends the base file-key routing with optional field selection and
/// child-traversal depth. Omit fields and depth to get the compact default
/// (seven base fields, top-level nodes only).
#[derive(Debug, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
pub(crate) struct SelectionParams {
    #[serde(rename = "fileKey", default)]
    #[schemars(description = "File key; omit when one file is open")]
    pub(crate) file_key: Option<String>,
    #[serde(default)]
    #[schemars(description = "Extra props to add to each node")]
    pub(crate) fields: Option<Vec<String>>,
    #[serde(default)]
    #[schemars(
        description = "Child traversal depth (0 = top-level only, max 5). Omit for the default compact shape"
    )]
    pub(crate) depth: Option<u32>,
}

/// Parameters for the turbofig_execute tool.
#[derive(Debug, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
pub(crate) struct ExecuteParams {
    #[schemars(description = "Async JS code")]
    pub(crate) code: String,
    #[serde(rename = "fileKey", default)]
    #[schemars(description = "File key; omit when one file is open")]
    pub(crate) file_key: Option<String>,
}

/// Parameters for the turbofig_screenshot tool.
#[derive(Debug, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
pub(crate) struct ScreenshotParams {
    #[serde(default = "default_screenshot_scale")]
    #[schemars(description = "0.1-4, default 1")]
    pub(crate) scale: f64,
    #[serde(rename = "nodeId", default)]
    #[schemars(description = "Node id; default is the selection")]
    pub(crate) node_id: Option<String>,
    #[serde(rename = "return", default)]
    #[schemars(description = "file (default) or inline")]
    pub(crate) return_mode: ReturnMode,
    #[serde(rename = "fileKey", default)]
    #[schemars(description = "File key; omit when one file is open")]
    pub(crate) file_key: Option<String>,
    #[serde(rename = "maxDim", default = "default_max_dim")]
    #[schemars(description = "Longest-edge pixel cap, default 1200")]
    pub(crate) max_dim: u32,
    #[serde(rename = "fullRes", default)]
    #[schemars(description = "Skip the maxDim downscale")]
    pub(crate) full_res: bool,
}

fn default_screenshot_scale() -> f64 {
    1.0
}

fn default_max_dim() -> u32 {
    1200
}

/// MCP handler that exposes the four turbofig tools.
///
/// The #[tool_router] macro generates a static tool_router() constructor.
/// The handler holds the shared AppState so a tool call can route a request
/// to the live plugin and await its reply.
#[derive(Clone)]
pub struct TurbofigHandler {
    state: Arc<AppState>,
}

impl Default for TurbofigHandler {
    fn default() -> Self {
        Self::new(Arc::new(AppState::new()))
    }
}

#[tool_router]
impl TurbofigHandler {
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }

    /// Return daemon liveness status as JSON.
    ///
    /// Delegates to `run_status` for the shared logic; wraps the result in a
    /// `CallToolResult` for the MCP wire format.
    /// `"ok":true` always means the daemon is alive regardless of plugin state.
    #[tool(description = "Return daemon status")]
    async fn turbofig_status(
        &self,
        Parameters(FileTargetParams { file_key }): Parameters<FileTargetParams>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, McpError> {
        let _job = self.state.begin_job();
        let session_id = session_id_from_parts(&parts);
        let value = run_status(&self.state, session_id, file_key.as_deref()).await;
        Ok(CallToolResult::success(vec![ContentBlock::text(
            value.to_string(),
        )]))
    }

    /// Execute JavaScript in the Figma plugin and return the result as JSON.
    ///
    /// Delegates to `run_execute` for the shared logic; wraps the result in a
    /// `CallToolResult` for the MCP wire format.
    #[tool(
        description = "Run async JS in the Figma plugin; `figma` and `tf.*` are in scope. Use return. Return ids, not nodes."
    )]
    async fn turbofig_execute(
        &self,
        Parameters(ExecuteParams { code, file_key }): Parameters<ExecuteParams>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, McpError> {
        let _job = self.state.begin_job();
        let session_id = session_id_from_parts(&parts);
        let value = run_execute(&self.state, session_id, file_key.as_deref(), &code).await;
        Ok(call_tool_result(value))
    }

    /// Return the current Figma selection as a JSON array.
    ///
    /// Delegates to `run_get_selection` for the shared logic; wraps the result
    /// in a `CallToolResult` for the MCP wire format.
    /// Pass `fields` to include extra node properties beyond the base seven.
    /// Pass `depth` (max 5) to traverse child nodes.
    #[tool(description = "Return the current Figma selection")]
    async fn turbofig_get_selection(
        &self,
        Parameters(SelectionParams {
            file_key,
            fields,
            depth,
        }): Parameters<SelectionParams>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, McpError> {
        let _job = self.state.begin_job();
        let session_id = session_id_from_parts(&parts);
        let value = run_get_selection(
            &self.state,
            session_id,
            file_key.as_deref(),
            fields.as_deref(),
            depth,
        )
        .await;
        Ok(call_tool_result(value))
    }

    /// Capture a PNG screenshot of a Figma node and return the path or inline base64.
    ///
    /// Delegates to `run_screenshot` for the shared logic; wraps the result in a
    /// `CallToolResult` for the MCP wire format.
    #[tool(description = "PNG screenshot of a node. File mode (default) returns a file path.")]
    async fn turbofig_screenshot(
        &self,
        Parameters(ScreenshotParams {
            scale,
            node_id,
            return_mode,
            file_key,
            max_dim,
            full_res,
        }): Parameters<ScreenshotParams>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, McpError> {
        let _job = self.state.begin_job();
        let session_id = session_id_from_parts(&parts);
        let value = run_screenshot(
            &self.state,
            session_id,
            file_key.as_deref(),
            scale,
            node_id.as_deref(),
            return_mode.as_str(),
            self.state.screenshot_dir().as_deref(),
            max_dim,
            full_res,
        )
        .await;
        Ok(call_tool_result(value))
    }
}

/// Wrap a JSON op result as a `CallToolResult`.
///
/// An `{"ok":false,...}` result must surface as an MCP tool *error*
/// (`isError: true`), not success: the call failed and the client must be
/// able to tell without inspecting the text body.
pub(crate) fn call_tool_result(value: Value) -> CallToolResult {
    let is_err = !value.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    let content = vec![ContentBlock::text(value.to_string())];
    if is_err {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    }
}

#[tool_handler]
impl ServerHandler for TurbofigHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(turbofig_server_info())
    }
}

/// This daemon's own `serverInfo` (name "turbofig", this crate's version).
///
/// `Implementation::from_build_env()` reads `CARGO_CRATE_NAME` and
/// `CARGO_PKG_VERSION` from inside the `rmcp` crate's own source, at `rmcp`'s
/// own compile time (an `env!` macro always resolves where it is *written*,
/// not where it is called from), so it always reports `rmcp 3.1.0` rather
/// than this daemon's own name and version. Both MCP transports (this HTTP
/// one, and the stdio proxy in `proxy.rs`) must report `turbofig` and this
/// crate's real version, so build it explicitly instead.
pub(crate) fn turbofig_server_info() -> Implementation {
    Implementation::new("turbofig", env!("CARGO_PKG_VERSION"))
}

/// Help text returned by GET / and the fallback handler on the MCP HTTP port.
///
/// An AI that discovers port 18846 can read this to bootstrap without the repo.
pub const HELP_TEXT: &str = "\
turbofig: always-on Figma design daemon
=========================================

Two transports. Prefer the file-bridge. Use MCP only when it is not available.

File-bridge (recommended, dialog-free)
--------------------------------------
With a shell: run `turbofig js <<'EOF'` with the JS on stdin and a closing
EOF. It prints the result JSON. --shot also screenshots the returned node. Add --file <fileKey> if several files are open.
With only file tools: write a JSON job to ~/.turbofig/inbox/<id>.json and read the result from
~/.turbofig/outbox/<id>.json (any unique <id>). Override the folder with
TURBOFIG_BRIDGE_DIR. Ops: execute (JS in \"code\"), get_selection, screenshot,
status. Add \"fileKey\":\"<key>\" to target one of several open files; omit it
for the sole open file.
Example: {\"op\":\"execute\",\"fileKey\":\"<key>\",\"code\":\"return figma.root.name;\"}

MCP (fallback)
--------------
This is plain local HTTP, not HTTPS. Use curl, never a web-fetch tool.
POST /mcp is served only when Connection is MCP (turbofig Settings, or the
gear in the Figma plugin). In File bridge mode it returns 404 mcp_http_off.
With a shell, run `turbofig status` first: it checks that this port is
your own daemon, so you never send the token to another process.
POST /mcp (streamable-http, legacy session mode). Send initialize, then
notifications/initialized. Include the mcp-session-id header on every
request after initialize, and an Authorization: Bearer <pairing token>
header on every request (read the token from ~/.turbofig/token). The four tools mirror the ops: turbofig_execute,
turbofig_get_selection, turbofig_screenshot, turbofig_status. Each takes an
optional fileKey.

Ports (both env-overridable)
-----------------------------
  18846  HTTP MCP port  (TURBOFIG_MCP_PORT)
  18847  WebSocket port for plugins  (TURBOFIG_WS_PORT)
";

/// Handler for GET / and the fallback route. Returns the help payload as plain text.
async fn help_handler() -> impl axum::response::IntoResponse {
    (
        axum::http::StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8",
        )],
        HELP_TEXT,
    )
}

/// Host values the daemon accepts on every HTTP route, not only `/mcp`.
/// Matches `build_router`'s `allowed_hosts` for the nested MCP service, so a
/// DNS-rebinding attack is blocked the same way everywhere, including
/// `/health`.
const ALLOWED_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

/// Returns the bare host from a `Host` header value, stripping a trailing
/// `:<port>` and, for a bracketed IPv6 literal (`[::1]:18846`), the brackets.
fn host_without_port(host_header: &str) -> &str {
    if let Some(rest) = host_header.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            return &rest[..end];
        }
        return rest;
    }
    host_header
        .rsplit_once(':')
        .map_or(host_header, |(host, _)| host)
}

/// Returns true when the `Host` header names one of `ALLOWED_HOSTS`.
/// A missing `Host` header is rejected: HTTP/1.1 requires it, and a request
/// that somehow lacks one gives no host to allow-list against.
pub(crate) fn host_allowed(host_header: Option<&str>) -> bool {
    match host_header {
        Some(h) => ALLOWED_HOSTS.contains(&host_without_port(h)),
        None => false,
    }
}

/// Rejects any HTTP request whose `Host` header is not in `ALLOWED_HOSTS`,
/// with 403 Forbidden. This mirrors `build_router`'s `allowed_hosts` config
/// (enforced by the nested MCP service only) for every route on this router,
/// so `/`, `/health`, and the MCP fallback all get the same DNS-rebinding
/// defence as `/mcp`.
async fn reject_bad_host(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let host = req
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok());
    if !host_allowed(host) {
        return (axum::http::StatusCode::FORBIDDEN, "host not allowed").into_response();
    }
    next.run(req).await
}

/// The TCP port the HTTP listener is bound to. `serve_with_state` adds it as a
/// request extension; `/health` signs it into its proof.
#[derive(Clone, Copy, Debug)]
pub struct BoundPort(pub u16);

/// `GET /health` response shape. Never includes the pairing token: only
/// `version`, `uptimeSeconds`, and, when the caller is authenticated, the
/// connected-files list (itself built by `AppState::named_connections_json`,
/// which never reads the token either), this process's `pid`, and
/// `supervised` (`supervisor::is_supervised`). `restart_for_upgrade`
/// (`proxy.rs`) reads `supervised` to decide whether to wait for launchd's
/// own relaunch instead of racing it with its own `spawn_detached_daemon`
/// call.
///
/// A caller with no bearer token, or the wrong one, gets the reduced payload
/// (`version` and `uptimeSeconds` only): another local account on the same
/// Mac can reach 127.0.0.1, so the full payload (which file is open, by
/// name, and the daemon's pid) is only for a caller that already holds the
/// pairing token. This endpoint itself never fails with 401: reporting
/// liveness to an unauthenticated caller is the point of `/health`, it is
/// only the detail that is gated.
async fn health_handler(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    bound: Option<axum::Extension<BoundPort>>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let mut resp = health_body(&state, &headers).into_response();
    // A valid nonce gets a proof that only the token holder can compute, so a
    // client can check this is the real daemon before it sends the token.
    // The proof names the port this daemon is bound to, so a relay on another
    // port cannot reuse it. With no bound port known, no proof is signed.
    if let (Some(nonce), Some(axum::Extension(BoundPort(port)))) = (
        headers
            .get(crate::daemon_auth::NONCE_HEADER)
            .and_then(|v| v.to_str().ok())
            .filter(|n| crate::daemon_auth::valid_nonce(n)),
        bound,
    ) {
        let sig = crate::daemon_auth::proof(
            state.token(),
            crate::daemon_auth::LABEL_HTTP_DAEMON,
            &format!("{nonce}:{port}"),
        );
        if let Ok(value) = axum::http::HeaderValue::from_str(&sig) {
            resp.headers_mut()
                .insert(crate::daemon_auth::PROOF_HEADER, value);
        }
    }
    resp
}

fn health_body(state: &AppState, headers: &axum::http::HeaderMap) -> axum::Json<serde_json::Value> {
    if !crate::token::bearer_token_matches(headers, state.token()) {
        return axum::Json(serde_json::json!({
            "version": reported_version(),
            "uptimeSeconds": state.uptime_seconds(),
        }));
    }
    axum::Json(serde_json::json!({
        "version": reported_version(),
        "uptimeSeconds": state.uptime_seconds(),
        "connectedFiles": state.named_connections_json(),
        "saturated": state.any_saturated(),
        "pid": std::process::id(),
        "supervised": crate::supervisor::is_supervised(),
    }))
}

/// Rejects `/job` and `/mcp` without a valid `Authorization: Bearer <pairing
/// token>` header, with 401 and a short JSON error. Never logs the token,
/// win or lose. Closes the gap where another local macOS account, which can
/// also reach 127.0.0.1, could otherwise run plugin JavaScript in the
/// owner's Figma file. Applied only to `/job` and `/mcp` (see `build_router`);
/// `/health` and `/control` shape their own response or check instead, and
/// `/` and the fallback carry no capability worth protecting.
async fn require_bearer_token(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if !crate::token::bearer_token_matches(req.headers(), state.token()) {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            axum::Json(
                serde_json::json!({"ok": false, "error": "missing or invalid bearer token"}),
            ),
        )
            .into_response();
    }
    next.run(req).await
}

/// Answers `/mcp` with 404 and a short JSON body unless Connection is MCP.
/// The stdio proxy (`turbofig mcp`) never calls `/mcp`: it posts to `/job`,
/// so it works in both modes. Runs after the bearer check, so an unauthenticated caller
/// learns nothing. `/job`, `/health` and `/control` never pass through here.
async fn mcp_http_gate(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if !state.mcp_http_enabled() {
        return (
            axum::http::StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({
                "ok": false,
                "code": "mcp_http_off",
                "error": "turbofig is in File bridge mode. Switch Connection to MCP in the turbofig plugin (gear) or in Settings.",
            })),
        )
            .into_response();
    }
    next.run(req).await
}

/// The version `/health` reports.
///
/// Always the real build version (`CARGO_PKG_VERSION`), with one escape
/// hatch: in a **debug build only**, `TURBOFIG_TEST_VERSION_OVERRIDE`
/// overrides it. This exists solely so the version-handoff integration
/// tests (`daemon/tests/version_handoff.rs`) can stand up a daemon that
/// reports an arbitrary "old" or "new" version without needing two actual
/// binary builds. `#[cfg(debug_assertions)]` keeps the whole branch out of a
/// release binary (`cargo build --release` compiles with
/// `debug_assertions` off), so a production daemon can never be made to
/// misreport its own version this way.
fn reported_version() -> String {
    #[cfg(debug_assertions)]
    if let Ok(v) = std::env::var("TURBOFIG_TEST_VERSION_OVERRIDE") {
        return v;
    }
    env!("CARGO_PKG_VERSION").to_owned()
}

/// `POST /job` handler: runs one bridge-shaped job over plain HTTP and
/// returns the same result JSON the filesystem bridge writes to its outbox.
///
/// The request body is the bridge `Job` JSON (the `#[serde(tag = "op")]`
/// enum): `{"op":"execute","code":"..."}`, `{"op":"status"}`, and so on,
/// with an optional `fileKey`. This, the file-bridge, and the MCP tools all
/// go through the same `run_*` ops, so the three transports share one
/// contract. A body that fails to parse as a `Job` gives `400` with the same
/// `{"ok":false,"error":...}` shape the bridge writes for a schema-invalid
/// job. A screenshot in file mode writes its PNG to the daemon's configured
/// screenshot directory, the same as the MCP tool does, not the bridge's
/// outbox (this endpoint has no outbox).
async fn job_handler(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::Json(raw): axum::Json<Value>,
) -> impl axum::response::IntoResponse {
    let _job = state.begin_job();
    let job = match Job::parse(&raw) {
        Ok(j) => j,
        Err(e) => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({"ok": false, "error": e})),
            );
        }
    };
    let session_id = job_session_id(&headers).map(stdio_session_key);
    let output_dir = state.screenshot_dir();
    let value = process_job(job, &state, session_id.as_deref(), output_dir.as_deref()).await;
    (axum::http::StatusCode::OK, axum::Json(value))
}

/// Reads the `X-Turbofig-Session` header `POST /job` carries, when present.
/// Returns `None` for a missing header, a non-UTF-8 header, or an empty one:
/// an empty header can never name a real session, so it must fall back to
/// auto-pick or ambiguity exactly like a missing header, the same rule
/// `session_id_from_parts` applies to the HTTP MCP transport's
/// `mcp-session-id`.
///
/// `POST /job` has no built-in notion of a session the way the `/mcp`
/// transport's `mcp-session-id` does; this header is how the stdio MCP
/// proxy (`proxy.rs`) gives its own job calls the same per-process fileKey
/// pairing an HTTP MCP session gets (see `routing::resolve_route`), instead
/// of every stdio call always routing with no session at all. The raw value
/// returned here is never used as a map key directly: `job_handler` always
/// runs it through `stdio_session_key` first.
fn job_session_id(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get("x-turbofig-session")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
}

/// Prefixes a stdio `X-Turbofig-Session` id before it is used as a key into
/// `AppState`'s shared session-pairing map. That map is also keyed by the
/// HTTP transport's `mcp-session-id` (`session_id_from_parts`); without a
/// distinct prefix, a stdio proxy id and an HTTP session id that happened to
/// be the same literal string would collide and share one fileKey pairing.
fn stdio_session_key(id: &str) -> String {
    format!("stdio:{id}")
}

/// Rejects any HTTP request that carries an Origin header, with 403 Forbidden.
/// A browser always sends an Origin header on a cross-origin fetch; a non-browser
/// MCP client (curl, a native MCP client, the file-bridge) sends none. So this
/// blocks a malicious web page's fetch() from reaching the daemon and driving the
/// Figma plugin through `turbofig_execute`, without affecting any real caller
/// (the MCP spec's Origin-validation requirement; see DECISIONS.md).
async fn reject_browser_origin(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if req.headers().contains_key(axum::http::header::ORIGIN) {
        return (
            axum::http::StatusCode::FORBIDDEN,
            "browser requests are not allowed",
        )
            .into_response();
    }
    next.run(req).await
}

use axum::response::IntoResponse;

/// How long an HTTP MCP session may stay idle before rmcp closes it.
/// rmcp defaults to 300 s, so a client that pauses 5 minutes lost its
/// session. The value equals the pairing-map TTL, but the two clocks differ:
/// rmcp counts idle time, while the pairing TTL counts from the last
/// `session_insert`. A session and its pairing may expire at different times.
pub(crate) const MCP_SESSION_KEEP_ALIVE: Duration = crate::state::SESSION_TTL;

/// Build the session manager with an idle keep-alive. Other settings keep
/// the rmcp defaults.
fn session_manager(keep_alive: Duration) -> LocalSessionManager {
    let mut manager = LocalSessionManager::default();
    manager.session_config.keep_alive = Some(keep_alive);
    manager
}

/// Build the axum router with the MCP service mounted at /mcp.
pub fn build_router(state: Arc<AppState>) -> axum::Router {
    build_router_with_keep_alive(state, MCP_SESSION_KEEP_ALIVE)
}

/// `build_router` with a caller-chosen MCP session idle limit (used by tests).
fn build_router_with_keep_alive(state: Arc<AppState>, keep_alive: Duration) -> axum::Router {
    // StreamableHttpServerConfig is #[non_exhaustive], so construct via Default
    // then set fields directly.
    let mut config = StreamableHttpServerConfig::default();
    // Require mcp-session-id on all non-initialize requests.
    config.legacy_session_mode = true;
    // Pin the Host allow-list explicitly rather than relying on the crate's
    // default: a future rmcp upgrade that changes or widens that default
    // must not silently loosen this daemon's anti-DNS-rebinding guard.
    config.allowed_hosts = vec![
        "localhost".to_owned(),
        "127.0.0.1".to_owned(),
        "::1".to_owned(),
    ];
    let health_state = state.clone();
    let service = StreamableHttpService::new(
        move || Ok(TurbofigHandler::new(state.clone())),
        session_manager(keep_alive).into(),
        config,
    );
    // /job, /control and /mcp require the pairing token, checked before any
    // body is read; /health, / and the fallback do not go through this layer. `route_layer` applies the
    // middleware only to the routes already added to this sub-router, so a
    // later `.merge()` into the full router never widens its reach.
    let protected = axum::Router::new()
        .route("/job", axum::routing::post(job_handler))
        .route(
            "/control",
            axum::routing::post(crate::control::control_handler),
        )
        .merge(
            axum::Router::new()
                .nest_service("/mcp", service)
                .route_layer(axum::middleware::from_fn_with_state(
                    health_state.clone(),
                    mcp_http_gate,
                )),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            health_state.clone(),
            require_bearer_token,
        ));

    axum::Router::new()
        .route("/", axum::routing::get(help_handler))
        .route("/health", axum::routing::get(health_handler))
        .merge(protected)
        .fallback(help_handler)
        .with_state(health_state)
        .layer(axum::middleware::from_fn(reject_bad_host))
        .layer(axum::middleware::from_fn(reject_browser_origin))
}

/// Serve the MCP router on the given TCP listener.
/// Creates a private AppState. Use serve_with_state to share state with the WS server.
pub async fn serve(listener: tokio::net::TcpListener) -> std::io::Result<()> {
    let state = Arc::new(AppState::new());
    serve_with_state(listener, state).await
}

/// Serve the MCP router on the given TCP listener, using the provided AppState.
pub async fn serve_with_state(
    listener: tokio::net::TcpListener,
    state: Arc<AppState>,
) -> std::io::Result<()> {
    let port = listener.local_addr()?.port();
    let listener = crate::listener::LimitedListener::new(
        listener,
        crate::listener::HTTP_MAX_CONNECTIONS,
        crate::listener::HTTP_IDLE_TIMEOUT,
    );
    let app = build_router(state)
        .layer(axum::Extension(BoundPort(port)))
        .layer(axum::middleware::from_fn(crate::listener::track_inflight))
        .into_make_service_with_connect_info::<crate::listener::ConnInfo>();
    axum::serve(listener, app).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tower::ServiceExt;

    #[test]
    fn session_id_absent_header_is_none() {
        let parts = parts_with_session(None);
        assert_eq!(session_id_from_parts(&parts), None);
    }

    #[test]
    fn session_id_empty_header_is_none() {
        let parts = parts_with_session(Some(""));
        assert_eq!(session_id_from_parts(&parts), None);
    }

    #[test]
    fn session_id_valid_header_is_some() {
        let parts = parts_with_session(Some("sess-123"));
        assert_eq!(session_id_from_parts(&parts), Some("sess-123"));
    }

    fn parts_with_session(value: Option<&str>) -> http::request::Parts {
        let mut builder = http::Request::builder();
        if let Some(v) = value {
            builder = builder.header("mcp-session-id", v);
        }
        builder.body(()).expect("build request").into_parts().0
    }

    #[test]
    fn job_session_id_absent_header_is_none() {
        let headers = axum::http::HeaderMap::new();
        assert_eq!(job_session_id(&headers), None);
    }

    #[test]
    fn job_session_id_empty_header_is_none() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-turbofig-session", "".parse().unwrap());
        assert_eq!(job_session_id(&headers), None);
    }

    #[test]
    fn job_session_id_valid_header_is_some() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-turbofig-session", "proxy-abc123".parse().unwrap());
        assert_eq!(job_session_id(&headers), Some("proxy-abc123"));
    }

    #[test]
    fn stdio_session_key_prefixes_the_raw_id() {
        assert_eq!(stdio_session_key("abc123"), "stdio:abc123");
    }

    #[test]
    fn stdio_session_key_cannot_collide_with_an_http_session_of_the_same_literal_id() {
        // The HTTP transport uses the raw mcp-session-id as the map key
        // (`session_id_from_parts`); the stdio key must never equal it for
        // the same literal id, or the two sessions would share one pairing.
        let id = "same-id";
        assert_ne!(stdio_session_key(id), id);
    }

    #[test]
    fn host_without_port_strips_a_plain_port() {
        assert_eq!(host_without_port("127.0.0.1:18846"), "127.0.0.1");
        assert_eq!(host_without_port("localhost"), "localhost");
    }

    #[test]
    fn host_without_port_strips_bracketed_ipv6_and_its_port() {
        assert_eq!(host_without_port("[::1]:18846"), "::1");
        assert_eq!(host_without_port("[::1]"), "::1");
    }

    #[test]
    fn host_allowed_accepts_every_entry_in_the_allow_list() {
        assert!(host_allowed(Some("localhost")));
        assert!(host_allowed(Some("127.0.0.1")));
        assert!(host_allowed(Some("127.0.0.1:18846")));
        assert!(host_allowed(Some("[::1]:18846")));
    }

    #[test]
    fn host_allowed_rejects_an_unknown_host_or_a_missing_header() {
        assert!(!host_allowed(Some("evil.example")));
        assert!(!host_allowed(None));
    }

    /// `GET /health` with no token (or the wrong one) must return only
    /// `version` and `uptimeSeconds`: another local macOS account can reach
    /// 127.0.0.1, so the connected-file names and the daemon's pid are
    /// gated behind the pairing token.
    #[tokio::test]
    async fn health_endpoint_without_a_token_returns_the_reduced_payload() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let token = state.token().to_owned();
        let router = build_router(state);

        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/health")
            .header(axum::http::header::HOST, "127.0.0.1")
            .body(axum::body::Body::empty())
            .expect("build request");

        let resp = router.oneshot(req).await.expect("router must respond");
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        let body: Value = serde_json::from_slice(&bytes).expect("valid JSON");

        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
        assert!(body["uptimeSeconds"].is_number());
        assert!(
            body.get("connectedFiles").is_none(),
            "connectedFiles must not appear without a valid token: {body}"
        );
        assert!(
            body.get("pid").is_none(),
            "pid must not appear without a valid token: {body}"
        );
        assert!(!bytes_contains(&bytes, token.as_bytes()));
    }

    /// `GET /health` with a wrong token must get the same reduced payload as
    /// no token at all, not an error: `/health` never fails with 401.
    #[tokio::test]
    async fn health_endpoint_with_a_wrong_token_returns_the_reduced_payload() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let router = build_router(state);

        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/health")
            .header(axum::http::header::HOST, "127.0.0.1")
            .header(
                axum::http::header::AUTHORIZATION,
                "Bearer not-the-real-token",
            )
            .body(axum::body::Body::empty())
            .expect("build request");

        let resp = router.oneshot(req).await.expect("router must respond");
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        let body: Value = serde_json::from_slice(&bytes).expect("valid JSON");
        assert!(body.get("connectedFiles").is_none());
    }

    /// `GET /health` with the real token gets the full payload: connected
    /// files (empty here) and this process's pid, and still never the token
    /// itself.
    #[tokio::test]
    async fn health_endpoint_with_a_valid_token_returns_the_full_payload() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let token = state.token().to_owned();
        let router = build_router(state);

        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/health")
            .header(axum::http::header::HOST, "127.0.0.1")
            .header(axum::http::header::AUTHORIZATION, format!("Bearer {token}"))
            .body(axum::body::Body::empty())
            .expect("build request");

        let resp = router.oneshot(req).await.expect("router must respond");
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        let body: Value = serde_json::from_slice(&bytes).expect("valid JSON");

        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
        assert!(body["uptimeSeconds"].is_number());
        assert_eq!(body["connectedFiles"], serde_json::json!([]));
        assert_eq!(body["pid"], serde_json::json!(std::process::id()));
        assert_eq!(
            body["supervised"],
            serde_json::json!(crate::supervisor::is_supervised())
        );
        assert!(!bytes_contains(&bytes, token.as_bytes()));
    }

    #[tokio::test]
    async fn health_endpoint_rejects_a_browser_origin() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let router = build_router(state);

        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/health")
            .header(axum::http::header::HOST, "127.0.0.1")
            .header(axum::http::header::ORIGIN, "https://evil.example")
            .body(axum::body::Body::empty())
            .expect("build request");

        let resp = router.oneshot(req).await.expect("router must respond");
        assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn health_endpoint_rejects_a_foreign_host() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let router = build_router(state);

        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/health")
            .header(axum::http::header::HOST, "evil.example")
            .body(axum::body::Body::empty())
            .expect("build request");

        let resp = router.oneshot(req).await.expect("router must respond");
        assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);
    }

    /// A bad Host or Origin must get 403 and no proof, even with a valid nonce.
    #[tokio::test]
    async fn health_does_not_sign_for_a_bad_host_or_origin() {
        for (host, origin) in [
            ("evil.example", None),
            ("127.0.0.1", Some("https://evil.example")),
        ] {
            let state = Arc::new(AppState::new());
            let mut builder = axum::http::Request::builder()
                .method("GET")
                .uri("/health")
                .header(axum::http::header::HOST, host)
                .header(crate::daemon_auth::NONCE_HEADER, TEST_NONCE);
            if let Some(o) = origin {
                builder = builder.header(axum::http::header::ORIGIN, o);
            }
            let req = builder
                .body(axum::body::Body::empty())
                .expect("build request");
            let resp = build_router(state).oneshot(req).await.expect("respond");
            assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);
            assert!(resp
                .headers()
                .get(crate::daemon_auth::PROOF_HEADER)
                .is_none());
        }
    }

    fn bytes_contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    #[test]
    fn return_mode_rejects_unknown_values() {
        let err = serde_json::from_str::<ReturnMode>("\"inlin\"").unwrap_err();
        assert!(err.to_string().contains("unknown variant"));
    }

    #[test]
    fn return_mode_accepts_file_and_inline() {
        assert_eq!(
            serde_json::from_str::<ReturnMode>("\"file\"").unwrap(),
            ReturnMode::File
        );
        assert_eq!(
            serde_json::from_str::<ReturnMode>("\"inline\"").unwrap(),
            ReturnMode::Inline
        );
    }

    /// A request carrying a Host header outside the allow-list must be
    /// rejected by the MCP service with 403, closing a DNS-rebinding
    /// attack: a malicious page served from an attacker domain that
    /// resolves to 127.0.0.1 would otherwise reach the daemon.
    #[tokio::test]
    async fn foreign_host_header_is_rejected() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let router = build_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/mcp")
            .header(axum::http::header::HOST, "evil.example")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from("{}"))
            .expect("build request");

        let resp = router.oneshot(req).await.expect("router must respond");
        assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn allowed_host_header_is_not_rejected_by_the_host_check() {
        // A valid Host still reaches the MCP service (which may then reject
        // the request for other protocol reasons, e.g. a missing session);
        // it must not come back 403 for the Host check specifically.
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let router = build_router(state);

        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/")
            .header(axum::http::header::HOST, "127.0.0.1")
            .body(axum::body::Body::empty())
            .expect("build request");

        let resp = router.oneshot(req).await.expect("router must respond");
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
    }

    /// POST a job body to `/job` on `router`, with a valid bearer `token`,
    /// and return (status, body).
    async fn post_job(
        router: axum::Router,
        token: &str,
        body: Value,
    ) -> (axum::http::StatusCode, Value) {
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/job")
            .header(axum::http::header::HOST, "127.0.0.1")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .header(axum::http::header::AUTHORIZATION, format!("Bearer {token}"))
            .body(axum::body::Body::from(body.to_string()))
            .expect("build request");
        let resp = router.oneshot(req).await.expect("router must respond");
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        let value: Value = serde_json::from_slice(&bytes).expect("valid JSON body");
        (status, value)
    }

    /// POST a job body to `/job` on `router`, with a valid bearer `token` and,
    /// when `session_id` is `Some`, an `X-Turbofig-Session` header. Returns
    /// (status, body).
    async fn post_job_with_session(
        router: axum::Router,
        token: &str,
        session_id: Option<&str>,
        body: Value,
    ) -> (axum::http::StatusCode, Value) {
        let mut builder = axum::http::Request::builder()
            .method("POST")
            .uri("/job")
            .header(axum::http::header::HOST, "127.0.0.1")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .header(axum::http::header::AUTHORIZATION, format!("Bearer {token}"));
        if let Some(sid) = session_id {
            builder = builder.header("x-turbofig-session", sid);
        }
        let req = builder
            .body(axum::body::Body::from(body.to_string()))
            .expect("build request");
        let resp = router.oneshot(req).await.expect("router must respond");
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        let value: Value = serde_json::from_slice(&bytes).expect("valid JSON body");
        (status, value)
    }

    /// With 2 files connected and no `fileKey`, `/job` without a session
    /// header cannot tell which file a call means (this is the plain
    /// `AppState`-level behaviour `routing::resolve_route`'s own tests
    /// already cover; asserted here too so the contrast with the next test
    /// is in one place).
    #[tokio::test]
    async fn job_endpoint_with_no_session_header_and_two_files_is_ambiguous() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let (tx1, _rx1) = tokio::sync::mpsc::unbounded_channel::<String>();
        let (tx2, _rx2) = tokio::sync::mpsc::unbounded_channel::<String>();
        let conn1 = state.add_connection(tx1);
        state.set_connection_info(conn1, "fk1".to_owned(), "File 1".to_owned());
        let conn2 = state.add_connection(tx2);
        state.set_connection_info(conn2, "fk2".to_owned(), "File 2".to_owned());
        let token = state.token().to_owned();
        let router = build_router(state);

        let (status, body) =
            post_job_with_session(router, &token, None, json!({"op": "status"})).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(
            body["plugin"]["connected"],
            json!(true),
            "status with no session and 2 files must not resolve to one specific file: {body}"
        );
        assert!(body["plugin"].get("fileKey").is_none());
    }

    /// A call that explicitly targets `fileKey:"fk2"` and carries an
    /// `X-Turbofig-Session` header pairs that session to `fk2`; a later call
    /// on the same session with no `fileKey` must then route back to `fk2`,
    /// never fail ambiguous, exactly like an HTTP MCP session's
    /// `mcp-session-id` already does (`routing::resolve_route`). This is the
    /// stdio MCP proxy's 2-files scenario: every `turbofig mcp` process now
    /// sends this header with its own session id on every `/job` call.
    #[tokio::test]
    async fn job_endpoint_routes_by_the_x_turbofig_session_header_with_two_files_connected() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let (tx1, _rx1) = tokio::sync::mpsc::unbounded_channel::<String>();
        let (tx2, _rx2) = tokio::sync::mpsc::unbounded_channel::<String>();
        let conn1 = state.add_connection(tx1);
        state.set_connection_info(conn1, "fk1".to_owned(), "File 1".to_owned());
        let conn2 = state.add_connection(tx2);
        state.set_connection_info(conn2, "fk2".to_owned(), "File 2".to_owned());
        let token = state.token().to_owned();
        let router = build_router(state);

        let (status1, body1) = post_job_with_session(
            router.clone(),
            &token,
            Some("proxy-session-a"),
            json!({"op": "status", "fileKey": "fk2"}),
        )
        .await;
        assert_eq!(status1, axum::http::StatusCode::OK);
        assert_eq!(
            body1["plugin"]["fileKey"],
            json!("fk2"),
            "the explicit fileKey call must resolve to fk2: {body1}"
        );

        let (status2, body2) = post_job_with_session(
            router,
            &token,
            Some("proxy-session-a"),
            json!({"op": "status"}),
        )
        .await;
        assert_eq!(status2, axum::http::StatusCode::OK);
        assert_eq!(
            body2["plugin"]["fileKey"],
            json!("fk2"),
            "the paired session must re-route to fk2 with no explicit fileKey: {body2}"
        );
    }

    #[tokio::test]
    async fn job_endpoint_status_matches_the_bridge_result() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let router = build_router(state.clone());

        let direct = process_job(
            Job::parse(&json!({"op": "status"})).unwrap(),
            &state,
            None,
            None,
        )
        .await;
        let (status, via_http) = post_job(router, state.token(), json!({"op": "status"})).await;

        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(via_http, direct);
        assert_eq!(via_http["ok"], json!(true));
    }

    #[tokio::test]
    async fn job_endpoint_execute_matches_the_bridge_result() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let router = build_router(state.clone());
        let body = json!({"op": "execute", "code": "return 1;"});

        let direct = process_job(Job::parse(&body).unwrap(), &state, None, None).await;
        let (status, via_http) = post_job(router, state.token(), body).await;

        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(via_http, direct);
        assert_eq!(via_http["ok"], json!(false), "no plugin is connected");
    }

    #[tokio::test]
    async fn job_endpoint_get_selection_matches_the_bridge_result() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let router = build_router(state.clone());
        let body = json!({"op": "get_selection"});

        let direct = process_job(Job::parse(&body).unwrap(), &state, None, None).await;
        let (status, via_http) = post_job(router, state.token(), body).await;

        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(via_http, direct);
    }

    #[tokio::test]
    async fn job_endpoint_screenshot_matches_the_bridge_result() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let router = build_router(state.clone());
        let body = json!({"op": "screenshot"});

        let direct = process_job(Job::parse(&body).unwrap(), &state, None, None).await;
        let (status, via_http) = post_job(router, state.token(), body).await;

        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(via_http, direct);
    }

    #[tokio::test]
    async fn job_endpoint_rejects_a_malformed_job_with_400() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let router = build_router(state.clone());

        let (status, body) =
            post_job(router, state.token(), json!({"op": "delete_everything"})).await;

        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(body["ok"], json!(false));
    }

    #[tokio::test]
    async fn job_endpoint_rejects_a_browser_origin() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let router = build_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/job")
            .header(axum::http::header::HOST, "127.0.0.1")
            .header(axum::http::header::ORIGIN, "https://evil.example")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(json!({"op": "status"}).to_string()))
            .expect("build request");

        let resp = router.oneshot(req).await.expect("router must respond");
        assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn job_endpoint_rejects_a_foreign_host() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let router = build_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/job")
            .header(axum::http::header::HOST, "evil.example")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(json!({"op": "status"}).to_string()))
            .expect("build request");

        let resp = router.oneshot(req).await.expect("router must respond");
        assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn job_endpoint_without_a_token_is_401() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let router = build_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/job")
            .header(axum::http::header::HOST, "127.0.0.1")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(json!({"op": "status"}).to_string()))
            .expect("build request");

        let resp = router.oneshot(req).await.expect("router must respond");
        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn job_endpoint_with_a_wrong_token_is_401() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let router = build_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/job")
            .header(axum::http::header::HOST, "127.0.0.1")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .header(
                axum::http::header::AUTHORIZATION,
                "Bearer not-the-real-token",
            )
            .body(axum::body::Body::from(json!({"op": "status"}).to_string()))
            .expect("build request");

        let resp = router.oneshot(req).await.expect("router must respond");
        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn mcp_endpoint_without_a_token_is_401() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let router = build_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/mcp")
            .header(axum::http::header::HOST, "127.0.0.1")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .header("Accept", "application/json, text/event-stream")
            .body(axum::body::Body::from(
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": "2025-03-26",
                        "clientInfo": {"name": "test-client", "version": "0.1.0"},
                        "capabilities": {}
                    }
                })
                .to_string(),
            ))
            .expect("build request");

        let resp = router.oneshot(req).await.expect("router must respond");
        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
    }

    /// `initialize` over the HTTP MCP transport must report this daemon's
    /// own name and version, never `rmcp`'s (see `turbofig_server_info`'s
    /// doc comment for why `Implementation::from_build_env()` alone gets
    /// this wrong).
    #[tokio::test]
    async fn mcp_endpoint_initialize_reports_the_turbofig_server_info() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let token = state.token().to_owned();
        let router = build_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/mcp")
            .header(axum::http::header::HOST, "127.0.0.1")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header(axum::http::header::AUTHORIZATION, format!("Bearer {token}"))
            .body(axum::body::Body::from(
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": "2025-03-26",
                        "clientInfo": {"name": "test-client", "version": "0.1.0"},
                        "capabilities": {}
                    }
                })
                .to_string(),
            ))
            .expect("build request");

        let resp = router.oneshot(req).await.expect("router must respond");
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        let body = String::from_utf8_lossy(&bytes);
        let expected_version = format!("\"version\":\"{}\"", env!("CARGO_PKG_VERSION"));
        assert!(
            body.contains("\"name\":\"turbofig\""),
            "serverInfo must name turbofig: {body}"
        );
        assert!(
            body.contains(&expected_version),
            "serverInfo must carry this crate's version, not rmcp's: {body}"
        );
        assert!(!body.contains("\"name\":\"rmcp\""));
    }

    /// POST one JSON-RPC body to `/mcp` with the token and an optional session id.
    async fn post_mcp(
        router: axum::Router,
        token: &str,
        session: Option<&str>,
        body: Value,
    ) -> axum::response::Response {
        let mut builder = axum::http::Request::builder()
            .method("POST")
            .uri("/mcp")
            .header(axum::http::header::HOST, "127.0.0.1")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header(axum::http::header::AUTHORIZATION, format!("Bearer {token}"));
        if let Some(sid) = session {
            builder = builder.header("mcp-session-id", sid);
        }
        let req = builder
            .body(axum::body::Body::from(body.to_string()))
            .expect("build request");
        router.oneshot(req).await.expect("router must respond")
    }

    /// A state whose home holds (MCP mode) or lacks (File bridge mode) the
    /// `mcp-mode` marker.
    fn state_with_mcp_http(enabled: bool) -> (Arc<AppState>, tempfile::TempDir) {
        use crate::plugin_files::ConnectionMode;
        let home = tempfile::tempdir().expect("tempdir");
        let mode = if enabled {
            ConnectionMode::Mcp
        } else {
            ConnectionMode::File
        };
        crate::plugin_files::set_connection_mode(home.path(), mode).expect("set marker");
        let state = Arc::new(AppState::new_with_token("t".repeat(64), home.path()));
        (state, home)
    }

    fn mcp_init_body() -> Value {
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-03-26","capabilities":{},
            "clientInfo":{"name":"t","version":"0"}}})
    }

    #[tokio::test]
    async fn mcp_returns_404_json_in_file_bridge_mode() {
        let (state, _home) = state_with_mcp_http(false);
        let token = state.token().to_owned();
        let router = build_router(state);
        for method in ["POST", "GET", "DELETE"] {
            let req = axum::http::Request::builder()
                .method(method)
                .uri("/mcp")
                .header(axum::http::header::HOST, "127.0.0.1")
                .header(axum::http::header::AUTHORIZATION, format!("Bearer {token}"))
                .body(axum::body::Body::empty())
                .expect("build request");
            let resp = router.clone().oneshot(req).await.expect("respond");
            assert_eq!(resp.status(), 404, "{method}");
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .expect("body");
            let v: Value = serde_json::from_slice(&bytes).expect("json");
            assert_eq!(v["ok"], false);
            assert_eq!(v["code"], "mcp_http_off");
            assert_eq!(
                v["error"],
                "turbofig is in File bridge mode. Switch Connection to MCP in the turbofig plugin (gear) or in Settings."
            );
        }
    }

    #[tokio::test]
    async fn mcp_is_served_in_mcp_mode() {
        let (state, _home) = state_with_mcp_http(true);
        let token = state.token().to_owned();
        let resp = post_mcp(build_router(state), &token, None, mcp_init_body()).await;
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn the_mode_applies_to_the_next_request_both_ways() {
        let (state, home) = state_with_mcp_http(true);
        let token = state.token().to_owned();
        let router = build_router(state);
        crate::plugin_files::set_connection_mode(
            home.path(),
            crate::plugin_files::ConnectionMode::File,
        )
        .expect("file");
        let resp = post_mcp(router.clone(), &token, None, mcp_init_body()).await;
        assert_eq!(resp.status(), 404);
        crate::plugin_files::set_connection_mode(
            home.path(),
            crate::plugin_files::ConnectionMode::Mcp,
        )
        .expect("mcp");
        let resp = post_mcp(router, &token, None, mcp_init_body()).await;
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn file_bridge_mode_without_a_token_is_still_401() {
        let (state, _home) = state_with_mcp_http(false);
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/mcp")
            .header(axum::http::header::HOST, "127.0.0.1")
            .body(axum::body::Body::empty())
            .expect("build request");
        let resp = build_router(state).oneshot(req).await.expect("respond");
        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn job_and_health_are_unaffected_in_file_bridge_mode() {
        let (state, _home) = state_with_mcp_http(false);
        let token = state.token().to_owned();
        let router = build_router(state);
        let (status, body) = post_job(router.clone(), &token, json!({"op": "status"})).await;
        assert_ne!(status, 404, "/job must not be gated: {body}");
        let req = axum::http::Request::builder()
            .uri("/health")
            .header(axum::http::header::HOST, "127.0.0.1")
            .body(axum::body::Body::empty())
            .expect("build request");
        let resp = router.oneshot(req).await.expect("respond");
        assert_eq!(resp.status(), 200);
    }

    /// Open a session, then idle `idle` of virtual time, then call `tools/list`.
    /// Returns true when that follow-up call got a real answer.
    async fn session_alive_after_idle(keep_alive: Duration, idle: Duration) -> bool {
        let state = Arc::new(AppState::with_timeout(Duration::from_millis(100)));
        let token = state.token().to_owned();
        let router = build_router_with_keep_alive(state, keep_alive);
        let init = json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "clientInfo": {"name": "test-client", "version": "0.1.0"},
                "capabilities": {}
            }
        });
        let resp = post_mcp(router.clone(), &token, None, init).await;
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let sid = resp
            .headers()
            .get("mcp-session-id")
            .expect("session id header")
            .to_str()
            .expect("ascii id")
            .to_owned();
        drop(resp);
        let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        let resp = post_mcp(router.clone(), &token, Some(&sid), note).await;
        assert_eq!(resp.status(), axum::http::StatusCode::ACCEPTED);
        // Under paused time this sleep auto-advances the clock once every task is idle,
        // so the worker's own idle timer runs in its natural order.
        tokio::time::sleep(idle).await;
        let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
        let resp = post_mcp(router, &token, Some(&sid), list).await;
        // A closed session answers 404. A live one answers with a JSON-RPC result.
        if resp.status() != axum::http::StatusCode::OK {
            return false;
        }
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        String::from_utf8_lossy(&bytes).contains("\"result\"")
    }

    #[test]
    fn the_mcp_session_keep_alive_is_24_hours() {
        assert_eq!(MCP_SESSION_KEEP_ALIVE, Duration::from_secs(24 * 60 * 60));
        assert_eq!(MCP_SESSION_KEEP_ALIVE, crate::state::SESSION_TTL);
        let manager = session_manager(MCP_SESSION_KEEP_ALIVE);
        assert_eq!(
            manager.session_config.keep_alive,
            Some(Duration::from_secs(24 * 60 * 60))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_mcp_session_survives_an_idle_period_longer_than_the_rmcp_default() {
        // 301 s is past rmcp's 300 s default and well inside the 24 h limit.
        assert!(session_alive_after_idle(MCP_SESSION_KEEP_ALIVE, Duration::from_secs(301)).await);
        // And a 10 hour idle gap also survives.
        assert!(
            session_alive_after_idle(MCP_SESSION_KEEP_ALIVE, Duration::from_secs(10 * 3600)).await
        );
    }

    #[tokio::test]
    async fn an_mcp_session_idle_past_a_short_keep_alive_is_closed_in_real_time() {
        assert!(
            !session_alive_after_idle(Duration::from_millis(100), Duration::from_millis(400)).await
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_mcp_session_idle_past_a_300_s_keep_alive_is_closed() {
        // Control: with the rmcp default of 300 s the same idle gap kills the session.
        assert!(
            !session_alive_after_idle(Duration::from_secs(300), Duration::from_secs(301)).await
        );
    }

    #[tokio::test]
    async fn mcp_endpoint_with_a_wrong_token_is_401() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let router = build_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/mcp")
            .header(axum::http::header::HOST, "127.0.0.1")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header(
                axum::http::header::AUTHORIZATION,
                "Bearer not-the-real-token",
            )
            .body(axum::body::Body::from(
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": "2025-03-26",
                        "clientInfo": {"name": "test-client", "version": "0.1.0"},
                        "capabilities": {}
                    }
                })
                .to_string(),
            ))
            .expect("build request");

        let resp = router.oneshot(req).await.expect("router must respond");
        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
    }

    /// POST a control body to `/control` on `router`, optionally with a
    /// bearer token, and return (status, body).
    async fn post_control(
        router: axum::Router,
        token: Option<&str>,
        body: Value,
    ) -> (axum::http::StatusCode, Value) {
        let mut builder = axum::http::Request::builder()
            .method("POST")
            .uri("/control")
            .header(axum::http::header::HOST, "127.0.0.1")
            .header(axum::http::header::CONTENT_TYPE, "application/json");
        if let Some(t) = token {
            builder = builder.header(axum::http::header::AUTHORIZATION, format!("Bearer {t}"));
        }
        let req = builder
            .body(axum::body::Body::from(body.to_string()))
            .expect("build request");
        let resp = router.oneshot(req).await.expect("router must respond");
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        let value: Value = serde_json::from_slice(&bytes).expect("valid JSON body");
        (status, value)
    }

    const TEST_NONCE: &str = "00112233445566778899aabbccddeeff";

    /// GET `uri` with an optional nonce header and return the response.
    async fn get_with_nonce(
        state: Arc<AppState>,
        uri: &str,
        nonce: Option<&str>,
    ) -> axum::response::Response {
        let mut builder = axum::http::Request::builder()
            .method("GET")
            .uri(uri)
            .header(axum::http::header::HOST, "127.0.0.1");
        if let Some(n) = nonce {
            builder = builder.header(crate::daemon_auth::NONCE_HEADER, n);
        }
        let req = builder
            .body(axum::body::Body::empty())
            .expect("build request");
        build_router(state)
            .layer(axum::Extension(BoundPort(TEST_PORT)))
            .oneshot(req)
            .await
            .expect("respond")
    }

    const TEST_PORT: u16 = 18846;

    #[tokio::test]
    async fn health_signs_a_valid_nonce_without_a_bearer_token() {
        let state = Arc::new(AppState::new());
        let token = state.token().to_owned();
        let resp = get_with_nonce(state, "/health", Some(TEST_NONCE)).await;
        let got = resp
            .headers()
            .get(crate::daemon_auth::PROOF_HEADER)
            .expect("proof header")
            .to_str()
            .expect("ascii")
            .to_owned();
        let want = crate::daemon_auth::proof(
            &token,
            crate::daemon_auth::LABEL_HTTP_DAEMON,
            &format!("{TEST_NONCE}:{TEST_PORT}"),
        );
        assert_eq!(got, want);
        let other = crate::daemon_auth::proof(
            &token,
            crate::daemon_auth::LABEL_HTTP_DAEMON,
            &format!("{TEST_NONCE}:{}", TEST_PORT + 1),
        );
        assert_ne!(got, other);
    }

    #[tokio::test]
    async fn health_does_not_sign_a_bad_or_absent_nonce() {
        for nonce in [
            None,
            Some("short"),
            Some("zz112233445566778899aabbccddeeff"),
        ] {
            let state = Arc::new(AppState::new());
            let resp = get_with_nonce(state, "/health", nonce).await;
            assert_eq!(resp.status(), axum::http::StatusCode::OK);
            assert!(resp
                .headers()
                .get(crate::daemon_auth::PROOF_HEADER)
                .is_none());
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .expect("body");
            let v: Value = serde_json::from_slice(&bytes).expect("json");
            assert!(v.get("version").is_some());
        }
    }

    #[tokio::test]
    async fn only_health_signs_a_nonce() {
        for uri in ["/", "/nope"] {
            let state = Arc::new(AppState::new());
            let resp = get_with_nonce(state, uri, Some(TEST_NONCE)).await;
            assert!(resp
                .headers()
                .get(crate::daemon_auth::PROOF_HEADER)
                .is_none());
        }
    }

    /// A body over axum's default limit must still get 401, not 413: the
    /// token check runs before the handler reads the body.
    #[tokio::test]
    async fn control_endpoint_checks_the_token_before_reading_a_huge_body() {
        let state = Arc::new(AppState::new());
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/control")
            .header(axum::http::header::HOST, "127.0.0.1")
            .body(axum::body::Body::from(vec![b'a'; 3 * 1024 * 1024]))
            .expect("build request");
        let resp = build_router(state).oneshot(req).await.expect("respond");
        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let v: Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(v["error"], json!("missing or invalid bearer token"));
    }

    #[tokio::test]
    async fn control_endpoint_without_a_token_is_401() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let router = build_router(state);

        let (status, body) = post_control(router, None, json!({"action": "stop"})).await;

        assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
        assert_eq!(body["ok"], json!(false));
    }

    #[tokio::test]
    async fn control_endpoint_with_a_wrong_token_is_401() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let router = build_router(state);

        let (status, body) = post_control(
            router,
            Some("not-the-real-token"),
            json!({"action": "stop"}),
        )
        .await;

        assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
        assert_eq!(body["ok"], json!(false));
    }

    /// A malformed body must never leak past the token check: without a
    /// valid token, a caller gets 401, never a 400/422 that would reveal the
    /// body was even looked at. See `control::control_handler`'s body doc
    /// comment.
    #[tokio::test]
    async fn control_endpoint_with_a_malformed_body_and_no_token_is_401_not_400() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let router = build_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/control")
            .header(axum::http::header::HOST, "127.0.0.1")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from("this is not json"))
            .expect("build request");
        let resp = router.oneshot(req).await.expect("router must respond");

        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
    }
}
