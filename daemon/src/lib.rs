mod bridge;
pub use bridge::{bridge_dir_from_env, serve_bridge};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
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
use serde_json::{json, Value};

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

// ── MCP port helpers ──────────────────────────────────────────────────────────

/// Parse a port number from an optional string value.
/// Returns 18846 when the input is None, cannot be parsed as u16, or is zero.
fn port_from_str(s: Option<&str>) -> u16 {
    s.and_then(|v| v.parse::<u16>().ok())
        .filter(|&p| p != 0)
        .unwrap_or(18846)
}

/// Read the MCP port from TURBOFIG_MCP_PORT. Default is 18846.
pub fn port_from_env() -> u16 {
    port_from_str(std::env::var("TURBOFIG_MCP_PORT").ok().as_deref())
}

// ── WS port helpers ───────────────────────────────────────────────────────────

/// Parse a WS port number from an optional string value.
/// Returns 18847 when the input is None, cannot be parsed as u16, or is zero.
fn ws_port_from_str(s: Option<&str>) -> u16 {
    s.and_then(|v| v.parse::<u16>().ok())
        .filter(|&p| p != 0)
        .unwrap_or(18847)
}

/// Read the WS port from TURBOFIG_WS_PORT. Default is 18847.
pub fn ws_port_from_env() -> u16 {
    ws_port_from_str(std::env::var("TURBOFIG_WS_PORT").ok().as_deref())
}

// ── Request timeout helpers ───────────────────────────────────────────────────

/// Parse a request timeout in milliseconds from an optional string value.
/// Returns 30000 ms when the input is None, cannot be parsed as u64, or is zero.
fn request_timeout_from_str(s: Option<&str>) -> Duration {
    s.and_then(|v| v.parse::<u64>().ok())
        .filter(|&ms| ms != 0)
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(30_000))
}

/// Read the request timeout from TURBOFIG_REQUEST_TIMEOUT_MS. Default is 30000 ms.
pub fn request_timeout_from_env() -> Duration {
    request_timeout_from_str(std::env::var("TURBOFIG_REQUEST_TIMEOUT_MS").ok().as_deref())
}

// ── Shared state ──────────────────────────────────────────────────────────────

/// An active plugin connection.
/// Holds the file key, the document name, and a sender for outbound JSON messages.
pub struct PluginConn {
    pub file_key: String,
    pub name: String,
    pub tx: mpsc::UnboundedSender<String>,
}

/// Routing error returned by resolve_route.
#[derive(Debug)]
pub enum RouteError {
    /// No plugin is connected.
    NoPlugin,
    /// More than one file is connected and no explicit target was given.
    Ambiguous(Vec<String>),
    /// The requested file key is not connected.
    NotFound(String, Vec<String>),
}

/// Convert a RouteError into the caller-facing JSON shape.
fn route_error_to_json(e: RouteError) -> Value {
    match e {
        RouteError::NoPlugin => json!({"ok": false, "error": "no plugin connected"}),
        RouteError::Ambiguous(fks) => json!({
            "ok": false,
            "error": "multiple files connected; specify fileKey",
            "files": fks
        }),
        RouteError::NotFound(fk, available) => json!({
            "ok": false,
            "error": format!("file not connected: {fk}"),
            "files": available
        }),
    }
}

/// Shared daemon state passed to both the MCP HTTP server and the WS server.
pub struct AppState {
    /// Registry of all active WebSocket connections.
    /// One entry per connected plugin. The file_key is empty until FILE_INFO arrives.
    connections: Mutex<HashMap<u64, PluginConn>>,
    /// Allocates stable connection IDs.
    conn_counter: AtomicU64,
    /// MCP session -> file_key pairing recorded by resolve_route.
    sessions: Mutex<HashMap<String, String>>,
    /// Pending tool-call requests waiting for a RESULT frame from the plugin.
    /// Value is (conn_id, oneshot sender). The conn_id lets one file closing cancel
    /// only its own in-flight requests without touching other files.
    pending: Mutex<HashMap<u64, (u64, oneshot::Sender<Value>)>>,
    /// Monotonically increasing request-ID counter.
    counter: AtomicU64,
    /// How long to wait for a plugin reply before returning a timeout response.
    pub request_timeout: Duration,
    /// Directory where screenshot PNGs are written in file mode, if configured.
    screenshot_dir: Option<std::path::PathBuf>,
}

impl AppState {
    /// Private constructor. Both public constructors delegate here.
    fn build(timeout: Duration, screenshot_dir: Option<std::path::PathBuf>) -> Self {
        Self {
            connections: Mutex::new(HashMap::new()),
            conn_counter: AtomicU64::new(1),
            sessions: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            counter: AtomicU64::new(1),
            request_timeout: timeout,
            screenshot_dir,
        }
    }

    /// Create a new AppState. Reads the timeout from TURBOFIG_REQUEST_TIMEOUT_MS.
    /// Sets screenshot_dir to `~/.turbofig/outbox`.
    pub fn new() -> Self {
        Self::build(
            request_timeout_from_env(),
            Some(bridge_dir_from_env().join("outbox")),
        )
    }

    /// Create a new AppState with an explicit request timeout.
    /// Use this in tests to set a short timeout without touching global env.
    /// Sets screenshot_dir to None; tests supply their own output dir.
    pub fn with_timeout(d: Duration) -> Self {
        Self::build(d, None)
    }

    /// Return a clone of the screenshot output directory, if configured.
    pub fn screenshot_dir(&self) -> Option<std::path::PathBuf> {
        self.screenshot_dir.clone()
    }

    /// Register a new WebSocket connection. Returns a stable connection ID.
    /// The file_key and name start empty and are set when FILE_INFO arrives.
    pub fn add_connection(&self, tx: mpsc::UnboundedSender<String>) -> u64 {
        let conn_id = self.conn_counter.fetch_add(1, Ordering::Relaxed);
        let mut guard = self.connections.lock().expect("connections lock");
        guard.insert(
            conn_id,
            PluginConn {
                file_key: String::new(),
                name: String::new(),
                tx,
            },
        );
        conn_id
    }

    /// Update the file_key and name for an existing connection.
    /// Call this when FILE_INFO arrives.
    pub fn set_connection_info(&self, conn_id: u64, file_key: String, name: String) {
        let mut guard = self.connections.lock().expect("connections lock");
        if let Some(conn) = guard.get_mut(&conn_id) {
            conn.file_key = file_key;
            conn.name = name;
        }
    }

    /// Remove a connection from the registry. Call this when the socket closes.
    pub fn remove_connection(&self, conn_id: u64) {
        let mut guard = self.connections.lock().expect("connections lock");
        guard.remove(&conn_id);
    }

    /// Return all connections as (conn_id, file_key, name) tuples.
    pub fn list_connections(&self) -> Vec<(u64, String, String)> {
        let guard = self.connections.lock().expect("connections lock");
        guard
            .iter()
            .map(|(id, c)| (*id, c.file_key.clone(), c.name.clone()))
            .collect()
    }

    /// Return the sole connection's (file_key, name) when exactly one exists.
    /// Returns None when there are zero or more than one connections.
    /// Kept for existing single-plugin WebSocket tests.
    pub fn plugin_snapshot(&self) -> Option<(String, String)> {
        let guard = self.connections.lock().expect("connections lock");
        if guard.len() == 1 {
            guard
                .values()
                .next()
                .map(|c| (c.file_key.clone(), c.name.clone()))
        } else {
            None
        }
    }

    /// Allocate a unique request ID.
    pub fn next_request_id(&self) -> u64 {
        self.counter.fetch_add(1, Ordering::Relaxed)
    }

    /// Register a pending request tagged with the owning connection.
    /// Returns a receiver that resolves when the plugin replies.
    pub fn register_pending(&self, id: u64, conn_id: u64) -> oneshot::Receiver<Value> {
        let (tx, rx) = oneshot::channel();
        let mut guard = self.pending.lock().expect("pending lock");
        guard.insert(id, (conn_id, tx));
        rx
    }

    /// Resolve a pending request with the plugin's response value.
    /// Silently ignores unknown request IDs.
    pub fn resolve(&self, id: u64, value: Value) {
        let mut guard = self.pending.lock().expect("pending lock");
        if let Some((_, tx)) = guard.remove(&id) {
            let _ = tx.send(value);
        }
    }

    /// Remove and drop the oneshot sender for `id`.
    /// Call this when a request times out to prevent a pending-map leak.
    /// Silently ignores unknown IDs.
    pub fn cancel_pending(&self, id: u64) {
        let mut guard = self.pending.lock().expect("pending lock");
        guard.remove(&id);
    }

    /// Drop every pending sender whose conn_id matches.
    /// Call this when a socket closes so only that file's in-flight requests fail.
    /// Other files' pending requests are not affected.
    pub fn cancel_pending_for_conn(&self, conn_id: u64) {
        let mut guard = self.pending.lock().expect("pending lock");
        guard.retain(|_, (cid, _)| *cid != conn_id);
    }

    /// Resolve which connection a call targets.
    ///
    /// session_id: the mcp-session-id (None for the bridge or when absent).
    /// explicit:   an explicit target fileKey from a tool param or bridge job.
    ///
    /// Returns the resolved connection details or a RouteError.
    /// Only connections whose file_key is non-empty count as valid targets.
    /// A just-connected socket with no FILE_INFO is not a valid target.
    fn resolve_route(
        &self,
        session_id: Option<&str>,
        explicit: Option<&str>,
    ) -> Result<(u64, mpsc::UnboundedSender<String>, String, String), RouteError> {
        // Normalize an empty explicit fileKey to no target, the same as an
        // empty session id. An empty string can never name a real file, so it
        // must fall through to the session pairing or the sole connection
        // instead of always failing with "file not connected".
        let explicit = explicit.filter(|s| !s.is_empty());
        // Step 1: Determine the desired file_key.
        let desired: Option<String> = if let Some(fk) = explicit {
            // Explicit target given. Record pairing for this session.
            if let Some(sid) = session_id {
                let mut sessions = self.sessions.lock().expect("sessions lock");
                sessions.insert(sid.to_owned(), fk.to_owned());
            }
            Some(fk.to_owned())
        } else if let Some(sid) = session_id {
            // No explicit target. Check for an existing session pairing.
            let sessions = self.sessions.lock().expect("sessions lock");
            sessions.get(sid).cloned()
        } else {
            None
        };
        // Sessions lock is released here.

        // Step 2: Collect named connections (non-empty file_key only).
        let named: Vec<(u64, mpsc::UnboundedSender<String>, String, String)> = {
            let connections = self.connections.lock().expect("connections lock");
            let mut v: Vec<_> = connections
                .iter()
                .filter(|(_, c)| !c.file_key.is_empty())
                .map(|(id, c)| (*id, c.tx.clone(), c.file_key.clone(), c.name.clone()))
                .collect();
            // Sort by file_key so the pick, the ambiguous list, and the
            // available list are deterministic across runs. A tie on file_key
            // (the same file open twice) breaks by conn_id for a stable pick.
            v.sort_by(|a, b| a.2.cmp(&b.2).then(a.0.cmp(&b.0)));
            v
        };
        // Connections lock is released here.

        if let Some(ref fk) = desired {
            // Find the connection with this file_key.
            let found = named
                .iter()
                .find(|(_, _, fk2, _)| fk2 == fk)
                .map(|(id, tx, fk2, nm)| (*id, tx.clone(), fk2.clone(), nm.clone()));

            if let Some(result) = found {
                return Ok(result);
            }

            // Not found: clear any stale pairing for this session.
            if let Some(sid) = session_id {
                let mut sessions = self.sessions.lock().expect("sessions lock");
                sessions.remove(sid);
            }
            let available: Vec<String> = named.into_iter().map(|(_, _, fk2, _)| fk2).collect();
            return Err(RouteError::NotFound(fk.clone(), available));
        }

        // No desired file_key: auto-pick from named connections.
        match named.len() {
            0 => Err(RouteError::NoPlugin),
            1 => {
                let (conn_id, tx, fk, nm) = named.into_iter().next().unwrap();
                // Record pairing so subsequent calls on this session go to same file.
                if let Some(sid) = session_id {
                    let mut sessions = self.sessions.lock().expect("sessions lock");
                    sessions.insert(sid.to_owned(), fk.clone());
                }
                Ok((conn_id, tx, fk, nm))
            }
            _ => {
                let fks: Vec<String> = named.into_iter().map(|(_, _, fk, _)| fk).collect();
                Err(RouteError::Ambiguous(fks))
            }
        }
    }

    /// Return all named connections as JSON objects for status and error responses.
    fn named_connections_json(&self) -> Vec<Value> {
        let guard = self.connections.lock().expect("connections lock");
        let mut named: Vec<(&str, &str)> = guard
            .values()
            .filter(|c| !c.file_key.is_empty())
            .map(|c| (c.file_key.as_str(), c.name.as_str()))
            .collect();
        // Sort by file_key so status output is deterministic across runs.
        named.sort_by(|a, b| a.0.cmp(b.0));
        named
            .into_iter()
            .map(|(fk, name)| json!({"fileKey": fk, "name": name}))
            .collect()
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

// ── Shared status logic ───────────────────────────────────────────────────────

/// Run the status check and return a JSON value.
///
/// Resolves the target connection via session_id and file_key.
/// - Resolved -> sends STATUS, awaits RESULT, returns the connected shape with
///   a `"plugins"` list. Returns `responsive:false` on timeout.
/// - NoPlugin -> `{"ok":true,"plugin":{"connected":false},"plugins":[]}`.
/// - Ambiguous -> `{"ok":true,"plugin":{"connected":true},"plugins":[...]}`.
/// - NotFound -> `{"ok":true,"plugin":{"connected":false},"plugins":[...]}`.
///
/// `"ok":true` always means the daemon is alive regardless of plugin state.
/// Reused by both `turbofig_status` (MCP) and the filesystem bridge.
pub async fn run_status(
    state: &Arc<AppState>,
    session_id: Option<&str>,
    file_key: Option<&str>,
) -> Value {
    let (conn_id, tx, fk, name) = match state.resolve_route(session_id, file_key) {
        Ok(r) => r,
        Err(RouteError::NoPlugin) => {
            return json!({"ok": true, "plugin": {"connected": false}, "plugins": []});
        }
        Err(RouteError::Ambiguous(_)) => {
            let plugins = state.named_connections_json();
            return json!({"ok": true, "plugin": {"connected": true}, "plugins": plugins});
        }
        Err(RouteError::NotFound(_, _)) => {
            let plugins = state.named_connections_json();
            return json!({"ok": true, "plugin": {"connected": false}, "plugins": plugins});
        }
    };

    let id = state.next_request_id();
    let rx = state.register_pending(id, conn_id);
    let request = json!({"type": "STATUS", "requestId": id});

    if tx.send(request.to_string()).is_err() {
        // The receiver dropped between the snapshot and the send.
        state.cancel_pending(id);
        let plugins = state.named_connections_json();
        return json!({"ok": true, "plugin": {"connected": false}, "plugins": plugins});
    }

    match tokio::time::timeout(state.request_timeout, rx).await {
        Ok(Ok(result)) => {
            let fk = result
                .get("fileKey")
                .and_then(|v| v.as_str())
                .unwrap_or(&fk)
                .to_owned();
            let nm = result
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(&name)
                .to_owned();
            let plugins = state.named_connections_json();
            json!({
                "ok": true,
                "plugin": {"connected": true, "fileKey": fk, "name": nm},
                "plugins": plugins
            })
        }
        Ok(Err(_)) => {
            let plugins = state.named_connections_json();
            json!({"ok": true, "plugin": {"connected": false}, "plugins": plugins})
        }
        Err(_elapsed) => {
            state.cancel_pending(id);
            let plugins = state.named_connections_json();
            // Name the unresponsive file so the caller knows which one is silent.
            json!({
                "ok": true,
                "plugin": {"connected": true, "responsive": false, "fileKey": fk, "name": name},
                "plugins": plugins
            })
        }
    }
}

// ── Shared execute logic ──────────────────────────────────────────────────────

/// Send `code` to the target plugin and return the result as a JSON value.
///
/// No plugin connected -> `{"ok":false,"error":"no plugin connected"}`.
/// Plugin replies -> pass the RESULT through.
/// Timeout -> `{"ok":false,"error":"plugin timed out"}`.
///
/// Reused by both `turbofig_execute` (MCP) and the filesystem bridge.
pub async fn run_execute(
    state: &Arc<AppState>,
    session_id: Option<&str>,
    file_key: Option<&str>,
    code: &str,
) -> Value {
    let (conn_id, tx, _, _) = match state.resolve_route(session_id, file_key) {
        Ok(r) => r,
        Err(e) => return route_error_to_json(e),
    };

    let id = state.next_request_id();
    let rx = state.register_pending(id, conn_id);
    let request = json!({"type": "EXECUTE", "requestId": id, "code": code});

    if tx.send(request.to_string()).is_err() {
        state.cancel_pending(id);
        return json!({"ok": false, "error": "plugin send failed"});
    }

    match tokio::time::timeout(state.request_timeout, rx).await {
        Ok(Ok(reply)) => {
            if reply.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                let result = reply.get("result").cloned().unwrap_or(Value::Null);
                json!({"ok": true, "result": result})
            } else {
                let error = reply
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("eval failed")
                    .to_owned();
                json!({"ok": false, "error": error})
            }
        }
        Ok(Err(_)) => {
            json!({"ok": false, "error": "plugin disconnected"})
        }
        Err(_elapsed) => {
            state.cancel_pending(id);
            json!({"ok": false, "error": "plugin timed out"})
        }
    }
}

// ── Shared get_selection logic ────────────────────────────────────────────────

/// Send GET_SELECTION to the target plugin and return the result as a JSON value.
///
/// No plugin connected -> `{"ok":false,"error":"no plugin connected"}`.
/// Plugin replies with ok:true -> `{"ok":true,"selection":[...]}`.
/// Plugin replies with ok:false -> `{"ok":false,"error":"..."}`.
/// Recv error -> `{"ok":false,"error":"plugin disconnected"}`.
/// Timeout -> cancel_pending then `{"ok":false,"error":"plugin timed out"}`.
///
/// Reused by both `turbofig_get_selection` (MCP) and the filesystem bridge.
pub async fn run_get_selection(
    state: &Arc<AppState>,
    session_id: Option<&str>,
    file_key: Option<&str>,
) -> Value {
    let (conn_id, tx, _, _) = match state.resolve_route(session_id, file_key) {
        Ok(r) => r,
        Err(e) => return route_error_to_json(e),
    };

    let id = state.next_request_id();
    let rx = state.register_pending(id, conn_id);
    let request = json!({"type": "GET_SELECTION", "requestId": id});

    if tx.send(request.to_string()).is_err() {
        state.cancel_pending(id);
        return json!({"ok": false, "error": "plugin send failed"});
    }

    match tokio::time::timeout(state.request_timeout, rx).await {
        Ok(Ok(reply)) => {
            if reply.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                let selection = reply
                    .get("selection")
                    .cloned()
                    .unwrap_or_else(|| Value::Array(vec![]));
                json!({"ok": true, "selection": selection})
            } else {
                let error = reply
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("get_selection failed")
                    .to_owned();
                json!({"ok": false, "error": error})
            }
        }
        Ok(Err(_)) => {
            json!({"ok": false, "error": "plugin disconnected"})
        }
        Err(_elapsed) => {
            state.cancel_pending(id);
            json!({"ok": false, "error": "plugin timed out"})
        }
    }
}

// ── Shared screenshot logic ───────────────────────────────────────────────────

/// Capture a PNG screenshot of a Figma node and return a JSON value.
///
/// No plugin connected -> `{"ok":false,"error":"no plugin connected"}`.
/// Plugin replies with ok:false -> `{"ok":false,"error":"..."}`.
/// Timeout -> cancel_pending then `{"ok":false,"error":"plugin timed out"}`.
/// On success with return_mode "inline": returns `{"ok":true,"w":w,"h":h,"png":<base64>}`.
/// On success with return_mode "file": decodes base64, writes to output_dir/<requestId>.png,
///   returns `{"ok":true,"path":"...","w":w,"h":h}`.
///
/// Reused by both `turbofig_screenshot` (MCP) and the filesystem bridge.
pub async fn run_screenshot(
    state: &Arc<AppState>,
    session_id: Option<&str>,
    file_key: Option<&str>,
    scale: f64,
    node_id: Option<&str>,
    return_mode: &str,
    output_dir: Option<&std::path::Path>,
) -> Value {
    let (conn_id, tx, _, _) = match state.resolve_route(session_id, file_key) {
        Ok(r) => r,
        Err(e) => return route_error_to_json(e),
    };

    let id = state.next_request_id();
    let rx = state.register_pending(id, conn_id);
    let request = json!({
        "type": "SCREENSHOT",
        "requestId": id,
        "scale": scale,
        "nodeId": node_id
    });

    if tx.send(request.to_string()).is_err() {
        state.cancel_pending(id);
        return json!({"ok": false, "error": "plugin send failed"});
    }

    match tokio::time::timeout(state.request_timeout, rx).await {
        Ok(Ok(reply)) => {
            if !reply.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                let error = reply
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("screenshot failed")
                    .to_owned();
                return json!({"ok": false, "error": error});
            }
            let png = match reply.get("png").and_then(|v| v.as_str()) {
                Some(s) => s.to_owned(),
                None => return json!({"ok": false, "error": "screenshot failed"}),
            };
            // Read as f64: a node's width and height can be fractional, and
            // as_u64 would silently drop a fractional value to 0.
            let w = reply.get("w").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let h = reply.get("h").and_then(|v| v.as_f64()).unwrap_or(0.0);

            if return_mode == "inline" {
                json!({"ok": true, "w": w, "h": h, "png": png})
            } else {
                // File mode: decode base64 and write to disk.
                let Some(dir) = output_dir else {
                    return json!({"ok": false, "error": "file mode needs an output dir"});
                };
                let bytes = match B64.decode(&png) {
                    Ok(b) => b,
                    Err(_) => return json!({"ok": false, "error": "invalid base64 png"}),
                };
                if let Err(e) = tokio::fs::create_dir_all(dir).await {
                    return json!({"ok": false, "error": format!("write failed: {e}")});
                }
                let path = dir.join(format!("{id}.png"));
                if let Err(e) = tokio::fs::write(&path, &bytes).await {
                    return json!({"ok": false, "error": format!("write failed: {e}")});
                }
                json!({"ok": true, "path": path.to_string_lossy(), "w": w, "h": h})
            }
        }
        Ok(Err(_)) => {
            json!({"ok": false, "error": "plugin disconnected"})
        }
        Err(_elapsed) => {
            state.cancel_pending(id);
            json!({"ok": false, "error": "plugin timed out"})
        }
    }
}

// ── MCP handler ───────────────────────────────────────────────────────────────

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

/// Parameters for tools that take only an optional target file key.
/// Used by turbofig_status and turbofig_get_selection.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct FileTargetParams {
    #[serde(rename = "fileKey", default)]
    #[schemars(
        description = "Target Figma file key; omit to use the sole connected file or the session's paired file"
    )]
    file_key: Option<String>,
}

/// Parameters for the turbofig_execute tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ExecuteParams {
    #[schemars(description = "JavaScript code to execute in the Figma plugin context")]
    code: String,
    #[serde(rename = "fileKey", default)]
    #[schemars(
        description = "Target Figma file key; omit to use the sole connected file or the session's paired file"
    )]
    file_key: Option<String>,
}

/// Parameters for the turbofig_screenshot tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ScreenshotParams {
    #[serde(default = "default_screenshot_scale")]
    #[schemars(description = "Export scale factor (default 1.0)")]
    scale: f64,
    #[serde(rename = "nodeId", default)]
    #[schemars(description = "Figma node ID to screenshot; uses current selection if omitted")]
    node_id: Option<String>,
    #[serde(rename = "return", default = "default_return_mode")]
    #[schemars(
        description = "Return mode: 'file' (default) writes a PNG to disk; 'inline' returns base64"
    )]
    return_mode: String,
    #[serde(rename = "fileKey", default)]
    #[schemars(
        description = "Target Figma file key; omit to use the sole connected file or the session's paired file"
    )]
    file_key: Option<String>,
}

fn default_screenshot_scale() -> f64 {
    1.0
}

fn default_return_mode() -> String {
    "file".to_owned()
}

/// MCP handler that exposes the four turbofig tools.
///
/// The #[tool_router] macro generates a static tool_router() constructor.
/// The handler holds the shared AppState so a tool call can route a request
/// to the live plugin and await its reply.
#[derive(Clone)]
pub struct StatusHandler {
    state: Arc<AppState>,
}

impl Default for StatusHandler {
    fn default() -> Self {
        Self::new(Arc::new(AppState::new()))
    }
}

#[tool_router]
impl StatusHandler {
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
        let session_id = session_id_from_parts(&parts);
        let value = run_execute(&self.state, session_id, file_key.as_deref(), &code).await;
        Ok(CallToolResult::success(vec![ContentBlock::text(
            value.to_string(),
        )]))
    }

    /// Return the current Figma selection as a JSON array.
    ///
    /// Delegates to `run_get_selection` for the shared logic; wraps the result
    /// in a `CallToolResult` for the MCP wire format.
    #[tool(description = "Return the current Figma selection")]
    async fn turbofig_get_selection(
        &self,
        Parameters(FileTargetParams { file_key }): Parameters<FileTargetParams>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, McpError> {
        let session_id = session_id_from_parts(&parts);
        let value = run_get_selection(&self.state, session_id, file_key.as_deref()).await;
        Ok(CallToolResult::success(vec![ContentBlock::text(
            value.to_string(),
        )]))
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
        }): Parameters<ScreenshotParams>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, McpError> {
        let session_id = session_id_from_parts(&parts);
        let value = run_screenshot(
            &self.state,
            session_id,
            file_key.as_deref(),
            scale,
            node_id.as_deref(),
            &return_mode,
            self.state.screenshot_dir().as_deref(),
        )
        .await;
        Ok(CallToolResult::success(vec![ContentBlock::text(
            value.to_string(),
        )]))
    }
}

#[tool_handler]
impl ServerHandler for StatusHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
    }
}

// ── MCP HTTP server ───────────────────────────────────────────────────────────

/// Build the axum router with the MCP service mounted at /mcp.
pub fn build_router(state: Arc<AppState>) -> axum::Router {
    // StreamableHttpServerConfig is #[non_exhaustive], so construct via Default
    // then set the field directly.
    let mut config = StreamableHttpServerConfig::default();
    // Require mcp-session-id on all non-initialize requests.
    config.legacy_session_mode = true;
    let service = StreamableHttpService::new(
        move || Ok(StatusHandler::new(state.clone())),
        LocalSessionManager::default().into(),
        config,
    );
    axum::Router::new().nest_service("/mcp", service)
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

// ── WebSocket server ──────────────────────────────────────────────────────────

/// Handle an upgraded WebSocket connection from the Figma plugin.
async fn handle_socket(socket: WebSocket, state: Arc<AppState>) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    // Allocate a stable conn_id for this connection immediately.
    let conn_id = state.add_connection(tx.clone());

    // Write task: drain the mpsc receiver and forward each message to the socket.
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if sink.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
        }
    });

    // Read loop: parse each incoming text frame as JSON and dispatch by type.
    while let Some(result) = stream.next().await {
        match result {
            Ok(Message::Text(text)) => {
                if let Ok(json) = serde_json::from_str::<Value>(&text) {
                    match json.get("type").and_then(|t| t.as_str()) {
                        Some("FILE_INFO") => {
                            let file_key = json
                                .get("fileKey")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_owned();
                            let name = json
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_owned();
                            state.set_connection_info(conn_id, file_key, name);
                        }
                        Some("RESULT") => {
                            if let Some(id) = json.get("requestId").and_then(|v| v.as_u64()) {
                                state.resolve(id, json);
                            }
                        }
                        // Unknown type: ignore safely, never panic.
                        _ => {}
                    }
                }
            }
            Ok(Message::Close(_)) | Err(_) => break,
            _ => {}
        }
    }

    // Socket closed: remove the connection and fail only its in-flight requests.
    // Other files' in-flight requests are unaffected.
    state.remove_connection(conn_id);
    state.cancel_pending_for_conn(conn_id);
}

/// axum handler that upgrades an HTTP request to a WebSocket connection.
async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Serve the WebSocket endpoint on the given TCP listener.
pub async fn serve_ws(
    listener: tokio::net::TcpListener,
    state: Arc<AppState>,
) -> std::io::Result<()> {
    let router = axum::Router::new()
        .route("/", axum::routing::get(ws_handler))
        .with_state(state);
    axum::serve(listener, router).await
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Session-id extraction tests

    /// Build request parts with an optional mcp-session-id header value.
    fn parts_with_session(value: Option<&str>) -> http::request::Parts {
        let mut builder = http::Request::builder();
        if let Some(v) = value {
            builder = builder.header("mcp-session-id", v);
        }
        builder.body(()).expect("build request").into_parts().0
    }

    #[test]
    fn session_id_absent_header_is_none() {
        let parts = parts_with_session(None);
        assert_eq!(session_id_from_parts(&parts), None);
    }

    #[test]
    fn session_id_empty_header_is_none() {
        // A present-but-empty header must not become a real session id.
        let parts = parts_with_session(Some(""));
        assert_eq!(session_id_from_parts(&parts), None);
    }

    #[test]
    fn session_id_valid_header_is_some() {
        let parts = parts_with_session(Some("sess-123"));
        assert_eq!(session_id_from_parts(&parts), Some("sess-123"));
    }

    // MCP port tests

    #[test]
    fn port_defaults_to_18846_when_unset() {
        assert_eq!(port_from_str(None), 18846);
    }

    #[test]
    fn port_parses_valid_number() {
        assert_eq!(port_from_str(Some("9000")), 9000);
    }

    #[test]
    fn port_falls_back_on_garbage_input() {
        assert_eq!(port_from_str(Some("notaport")), 18846);
    }

    #[test]
    fn port_rejects_zero_and_falls_back() {
        // Port 0 means an OS-assigned ephemeral port, never a meaningful daemon port.
        // Reject it and fall back to 18846.
        assert_eq!(port_from_str(Some("0")), 18846);
    }

    // Request timeout tests

    #[test]
    fn request_timeout_defaults_to_30s_when_unset() {
        assert_eq!(
            request_timeout_from_str(None),
            Duration::from_millis(30_000)
        );
    }

    #[test]
    fn request_timeout_parses_valid_number() {
        assert_eq!(
            request_timeout_from_str(Some("5000")),
            Duration::from_millis(5_000)
        );
    }

    #[test]
    fn request_timeout_falls_back_on_garbage_input() {
        assert_eq!(
            request_timeout_from_str(Some("notanumber")),
            Duration::from_millis(30_000)
        );
    }

    #[test]
    fn request_timeout_rejects_zero_and_falls_back() {
        // Zero milliseconds is not a valid timeout; fall back to the default.
        assert_eq!(
            request_timeout_from_str(Some("0")),
            Duration::from_millis(30_000)
        );
    }

    // WS port tests

    #[test]
    fn ws_port_defaults_to_18847_when_unset() {
        assert_eq!(ws_port_from_str(None), 18847);
    }

    #[test]
    fn ws_port_parses_valid_number() {
        assert_eq!(ws_port_from_str(Some("9001")), 9001);
    }

    #[test]
    fn ws_port_falls_back_on_garbage_input() {
        assert_eq!(ws_port_from_str(Some("notaport")), 18847);
    }

    #[test]
    fn ws_port_rejects_zero_and_falls_back() {
        // Port 0 is an OS-assigned ephemeral port; reject it and fall back to 18847.
        assert_eq!(ws_port_from_str(Some("0")), 18847);
    }

    // resolve_route unit tests

    #[test]
    fn resolve_route_no_connections_returns_no_plugin() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        assert!(matches!(
            state.resolve_route(None, None),
            Err(RouteError::NoPlugin)
        ));
    }

    #[test]
    fn resolve_route_one_named_no_session_returns_it() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let conn_id = state.add_connection(tx);
        state.set_connection_info(conn_id, "fk1".to_owned(), "File 1".to_owned());

        let result = state.resolve_route(None, None);
        assert!(result.is_ok(), "expected Ok");
        let (got_conn, _, got_fk, got_name) = result.unwrap();
        assert_eq!(got_conn, conn_id);
        assert_eq!(got_fk, "fk1");
        assert_eq!(got_name, "File 1");
    }

    #[test]
    fn resolve_route_one_named_session_records_pairing_and_re_routes() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let conn_id = state.add_connection(tx);
        state.set_connection_info(conn_id, "fk1".to_owned(), "File 1".to_owned());

        // First call with session: auto-pick and record pairing.
        let result = state.resolve_route(Some("session-a"), None);
        assert!(result.is_ok(), "first call must succeed");
        let (_, _, fk, _) = result.unwrap();
        assert_eq!(fk, "fk1");

        // Second call: session is now paired to fk1. Returns same file.
        let result2 = state.resolve_route(Some("session-a"), None);
        assert!(result2.is_ok(), "second call must succeed via pairing");
        let (_, _, fk2, _) = result2.unwrap();
        assert_eq!(fk2, "fk1");
    }

    #[test]
    fn resolve_route_two_named_no_session_returns_ambiguous() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx1, _rx1) = mpsc::unbounded_channel::<String>();
        let (tx2, _rx2) = mpsc::unbounded_channel::<String>();
        let conn1 = state.add_connection(tx1);
        let conn2 = state.add_connection(tx2);
        state.set_connection_info(conn1, "fk1".to_owned(), "File 1".to_owned());
        state.set_connection_info(conn2, "fk2".to_owned(), "File 2".to_owned());

        match state.resolve_route(None, None) {
            Err(RouteError::Ambiguous(fks)) => {
                assert!(fks.contains(&"fk1".to_owned()), "fk1 must be in the list");
                assert!(fks.contains(&"fk2".to_owned()), "fk2 must be in the list");
            }
            other => panic!("expected Ambiguous, got ok={}", other.is_ok()),
        }
    }

    #[test]
    fn resolve_route_empty_explicit_file_key_falls_through_to_auto_pick() {
        // An empty explicit fileKey must behave like no target, so a single
        // connected plugin is still auto-picked instead of a NotFound error.
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx1, _rx1) = mpsc::unbounded_channel::<String>();
        let conn1 = state.add_connection(tx1);
        state.set_connection_info(conn1, "fk1".to_owned(), "File 1".to_owned());

        let (conn_id, _tx, fk, _nm) = state
            .resolve_route(None, Some(""))
            .expect("empty explicit fileKey must auto-pick the sole plugin");
        assert_eq!(conn_id, conn1);
        assert_eq!(fk, "fk1");
    }

    #[test]
    fn resolve_route_duplicate_file_key_is_ambiguous_and_pick_is_deterministic() {
        // The same file open twice: two connections share one non-empty fileKey.
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx1, _rx1) = mpsc::unbounded_channel::<String>();
        let (tx2, _rx2) = mpsc::unbounded_channel::<String>();
        let conn1 = state.add_connection(tx1);
        let conn2 = state.add_connection(tx2);
        state.set_connection_info(conn1, "dup".to_owned(), "First".to_owned());
        state.set_connection_info(conn2, "dup".to_owned(), "Second".to_owned());

        // No target: two named connections means ambiguous, never a panic.
        match state.resolve_route(None, None) {
            Err(RouteError::Ambiguous(fks)) => assert_eq!(fks, vec!["dup", "dup"]),
            other => panic!("expected Ambiguous, got ok={}", other.is_ok()),
        }

        // Explicit target matches both. The pick must be stable across calls:
        // the lowest conn_id (conn1) wins by the file_key-then-conn_id sort.
        let first = state.resolve_route(None, Some("dup")).expect("route ok");
        let again = state.resolve_route(None, Some("dup")).expect("route ok");
        assert_eq!(first.0, conn1, "explicit pick must be the lowest conn_id");
        assert_eq!(
            first.0, again.0,
            "the pick must be deterministic across calls"
        );
    }

    #[test]
    fn resolve_route_explicit_fk2_returns_fk2_and_records_session_pairing() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx1, _rx1) = mpsc::unbounded_channel::<String>();
        let (tx2, _rx2) = mpsc::unbounded_channel::<String>();
        let conn1 = state.add_connection(tx1);
        let conn2 = state.add_connection(tx2);
        state.set_connection_info(conn1, "fk1".to_owned(), "File 1".to_owned());
        state.set_connection_info(conn2, "fk2".to_owned(), "File 2".to_owned());

        // Explicit fk2 with a session: must return fk2 and record pairing.
        let result = state.resolve_route(Some("session-x"), Some("fk2"));
        assert!(result.is_ok(), "explicit fk2 must resolve");
        let (_, _, got_fk, _) = result.unwrap();
        assert_eq!(got_fk, "fk2");

        // Session should now be paired to fk2. Next call with no explicit returns fk2.
        let result2 = state.resolve_route(Some("session-x"), None);
        assert!(result2.is_ok(), "paired session must resolve to fk2");
        let (_, _, got_fk2, _) = result2.unwrap();
        assert_eq!(got_fk2, "fk2");
    }

    #[test]
    fn resolve_route_explicit_unknown_returns_not_found_and_clears_stale_pairing() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let conn = state.add_connection(tx);
        state.set_connection_info(conn, "fk1".to_owned(), "File 1".to_owned());

        // Establish a pairing first.
        let _ = state.resolve_route(Some("session-y"), Some("fk1"));

        // Request an unknown fk: must return NotFound and clear the pairing.
        match state.resolve_route(Some("session-y"), Some("ghost")) {
            Err(RouteError::NotFound(fk, avail)) => {
                assert_eq!(fk, "ghost");
                assert!(avail.contains(&"fk1".to_owned()), "available must list fk1");
            }
            other => panic!("expected NotFound, got ok={}", other.is_ok()),
        }

        // Pairing is cleared: next call with no explicit auto-picks fk1.
        let result = state.resolve_route(Some("session-y"), None);
        assert!(
            result.is_ok(),
            "after clearing stale pairing must auto-pick fk1"
        );
        let (_, _, fk, _) = result.unwrap();
        assert_eq!(fk, "fk1");
    }

    #[test]
    fn resolve_route_unnamed_connection_not_counted_for_auto_pick() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        // add_connection gives empty file_key; no FILE_INFO sent.
        let _conn = state.add_connection(tx);

        // An unnamed connection must not count as a valid target.
        assert!(matches!(
            state.resolve_route(None, None),
            Err(RouteError::NoPlugin)
        ));
    }

    #[tokio::test]
    async fn cancel_pending_for_conn_drops_only_that_conns_entries() {
        let state = Arc::new(AppState::with_timeout(Duration::from_millis(100)));
        let id1 = state.next_request_id();
        let id2 = state.next_request_id();
        let rx1 = state.register_pending(id1, 10u64);
        let rx2 = state.register_pending(id2, 20u64);

        // Cancel conn 10. Only rx1 must fail; rx2 must still be resolvable.
        state.cancel_pending_for_conn(10u64);

        assert!(rx1.await.is_err(), "rx1 must be cancelled");

        state.resolve(id2, json!({"ok": true}));
        let val = rx2.await.expect("rx2 must still resolve");
        assert_eq!(val["ok"], json!(true));
    }

    #[test]
    fn two_connections_coexist_in_registry() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx1, _rx1) = mpsc::unbounded_channel::<String>();
        let (tx2, _rx2) = mpsc::unbounded_channel::<String>();
        let conn1 = state.add_connection(tx1);
        let conn2 = state.add_connection(tx2);
        state.set_connection_info(conn1, "fk1".to_owned(), "File 1".to_owned());
        state.set_connection_info(conn2, "fk2".to_owned(), "File 2".to_owned());

        let connections = state.list_connections();
        assert_eq!(connections.len(), 2, "both connections must be listed");
        let fks: Vec<&str> = connections.iter().map(|(_, fk, _)| fk.as_str()).collect();
        assert!(fks.contains(&"fk1") && fks.contains(&"fk2"));
    }
}
