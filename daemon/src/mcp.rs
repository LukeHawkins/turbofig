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
    #[schemars(
        description = "Target Figma file key; omit to use the sole connected file or the session's paired file"
    )]
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
    #[schemars(
        description = "Target Figma file key; omit to use the sole connected file or the session's paired file"
    )]
    pub(crate) file_key: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Extra node property names to include alongside the base seven fields"
    )]
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
    #[schemars(description = "JavaScript code to execute in the Figma plugin context")]
    pub(crate) code: String,
    #[serde(rename = "fileKey", default)]
    #[schemars(
        description = "Target Figma file key; omit to use the sole connected file or the session's paired file"
    )]
    pub(crate) file_key: Option<String>,
}

/// Parameters for the turbofig_screenshot tool.
#[derive(Debug, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
pub(crate) struct ScreenshotParams {
    #[serde(default = "default_screenshot_scale")]
    #[schemars(description = "Export scale factor (default 1.0, clamped to 0.1-4.0)")]
    pub(crate) scale: f64,
    #[serde(rename = "nodeId", default)]
    #[schemars(description = "Figma node ID to screenshot; uses current selection if omitted")]
    pub(crate) node_id: Option<String>,
    #[serde(rename = "return", default)]
    #[schemars(
        description = "Return mode: 'file' (default) writes a PNG to disk; 'inline' returns base64"
    )]
    pub(crate) return_mode: ReturnMode,
    #[serde(rename = "fileKey", default)]
    #[schemars(
        description = "Target Figma file key; omit to use the sole connected file or the session's paired file"
    )]
    pub(crate) file_key: Option<String>,
    #[serde(rename = "maxDim", default = "default_max_dim")]
    #[schemars(description = "Longest-edge pixel cap for downscaling; default 1200")]
    pub(crate) max_dim: u32,
    #[serde(rename = "fullRes", default)]
    #[schemars(
        description = "Return full resolution with no downscaling; default false. Combine with return:'inline' for a high-res inline image"
    )]
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
    #[tool(description = "Execute JavaScript in the Figma plugin and return the result")]
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
    #[tool(description = "Capture a PNG screenshot of a Figma node")]
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
            .with_server_info(Implementation::from_build_env())
    }
}

/// Help text returned by GET / and the fallback handler on the MCP HTTP port.
///
/// An AI that discovers port 18846 can read this to bootstrap without the repo.
pub const HELP_TEXT: &str = "\
turbofig: always-on Figma design daemon
=========================================

Two transports. Prefer the file-bridge: it is faster and more token-efficient
(no MCP tool definitions, no JSON-RPC or SSE envelope) and fires no permission
dialog. Use MCP only when the file-bridge is not available.

File-bridge (recommended, dialog-free)
--------------------------------------
Write a JSON job to ~/.turbofig/inbox/<id>.json and read the result from
~/.turbofig/outbox/<id>.json (any unique <id>). Override the folder with
TURBOFIG_BRIDGE_DIR. Ops: execute (JS in \"code\"), get_selection, screenshot,
status. Add \"fileKey\":\"<key>\" to target one of several open files; omit it
for the sole open file.
Example: {\"op\":\"execute\",\"fileKey\":\"<key>\",\"code\":\"return figma.root.name;\"}

MCP (fallback)
--------------
This is plain local HTTP, not HTTPS. Use curl, never a web-fetch tool.
POST /mcp (streamable-http, legacy session mode); include the mcp-session-id
header on every request after initialize, and an Authorization: Bearer
<pairing token> header on every request (read the token from
~/.turbofig/token). The four tools mirror the ops: turbofig_execute,
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
fn host_allowed(host_header: Option<&str>) -> bool {
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

/// `GET /health` response shape. Never includes the pairing token: only
/// `version`, `uptimeSeconds`, and, when the caller is authenticated, the
/// connected-files list (itself built by `AppState::named_connections_json`,
/// which never reads the token either) and this process's `pid`.
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
    headers: axum::http::HeaderMap,
) -> impl axum::response::IntoResponse {
    if !crate::token::bearer_token_matches(&headers, state.token()) {
        return axum::Json(serde_json::json!({
            "version": reported_version(),
            "uptimeSeconds": state.uptime_seconds(),
        }));
    }
    axum::Json(serde_json::json!({
        "version": reported_version(),
        "uptimeSeconds": state.uptime_seconds(),
        "connectedFiles": state.named_connections_json(),
        "pid": std::process::id(),
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
    let output_dir = state.screenshot_dir();
    let value = process_job(job, &state, output_dir.as_deref()).await;
    (axum::http::StatusCode::OK, axum::Json(value))
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

/// Build the axum router with the MCP service mounted at /mcp.
pub fn build_router(state: Arc<AppState>) -> axum::Router {
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
        LocalSessionManager::default().into(),
        config,
    );
    // /job and /mcp require the pairing token; /health, /control, / and the
    // fallback do not go through this layer. `route_layer` applies the
    // middleware only to the routes already added to this sub-router, so a
    // later `.merge()` into the full router never widens its reach.
    let protected = axum::Router::new()
        .route("/job", axum::routing::post(job_handler))
        .nest_service("/mcp", service)
        .route_layer(axum::middleware::from_fn_with_state(
            health_state.clone(),
            require_bearer_token,
        ));

    axum::Router::new()
        .route("/", axum::routing::get(help_handler))
        .route("/health", axum::routing::get(health_handler))
        .route(
            "/control",
            axum::routing::post(crate::control::control_handler),
        )
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
    axum::serve(listener, build_router(state)).await
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

    #[tokio::test]
    async fn job_endpoint_status_matches_the_bridge_result() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let router = build_router(state.clone());

        let direct = process_job(Job::parse(&json!({"op": "status"})).unwrap(), &state, None).await;
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

        let direct = process_job(Job::parse(&body).unwrap(), &state, None).await;
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

        let direct = process_job(Job::parse(&body).unwrap(), &state, None).await;
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

        let direct = process_job(Job::parse(&body).unwrap(), &state, None).await;
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
}
