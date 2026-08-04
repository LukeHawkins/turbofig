use rmcp::{
    model::*,
    tool, tool_handler, tool_router,
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

// ── Shared state ──────────────────────────────────────────────────────────────

/// An active plugin connection.
/// Holds the file key, the document name, and a sender for outbound JSON messages.
pub struct PluginConn {
    pub file_key: String,
    pub name: String,
    pub tx: mpsc::UnboundedSender<String>,
}

/// Shared daemon state passed to both the MCP HTTP server and the WS server.
pub struct AppState {
    /// The currently connected plugin, if any.
    /// Phase 2 supports one live plugin. Phase 4 will extend to multi-file.
    plugin: Mutex<Option<PluginConn>>,
    /// Pending tool-call requests waiting for a RESULT frame from the plugin.
    pending: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
    /// Monotonically increasing request-ID counter.
    counter: AtomicU64,
}

impl AppState {
    /// Create a new, empty AppState.
    pub fn new() -> Self {
        Self {
            plugin: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            counter: AtomicU64::new(1),
        }
    }

    /// Register the connected plugin. Overwrites any prior registration.
    pub fn register_plugin(
        &self,
        file_key: String,
        name: String,
        tx: mpsc::UnboundedSender<String>,
    ) {
        let mut guard = self.plugin.lock().expect("plugin lock");
        *guard = Some(PluginConn { file_key, name, tx });
    }

    /// Remove the plugin registration. Call this when the WS connection closes.
    pub fn clear_plugin(&self) {
        let mut guard = self.plugin.lock().expect("plugin lock");
        *guard = None;
    }

    /// Return a copy of the file key and name of the connected plugin, if any.
    pub fn plugin_snapshot(&self) -> Option<(String, String)> {
        let guard = self.plugin.lock().expect("plugin lock");
        guard.as_ref().map(|c| (c.file_key.clone(), c.name.clone()))
    }

    /// Allocate a unique request ID.
    pub fn next_request_id(&self) -> u64 {
        self.counter.fetch_add(1, Ordering::Relaxed)
    }

    /// Register a pending request. Returns a receiver that resolves when the plugin replies.
    pub fn register_pending(&self, id: u64) -> oneshot::Receiver<Value> {
        let (tx, rx) = oneshot::channel();
        let mut guard = self.pending.lock().expect("pending lock");
        guard.insert(id, tx);
        rx
    }

    /// Resolve a pending request with the plugin's response value.
    /// Silently ignores unknown request IDs.
    pub fn resolve(&self, id: u64, value: Value) {
        let mut guard = self.pending.lock().expect("pending lock");
        if let Some(tx) = guard.remove(&id) {
            let _ = tx.send(value);
        }
    }

    /// Return the plugin's outbound sender and identity, if a plugin is registered.
    /// Returns `(tx, file_key, name)`. The caller may send JSON strings via `tx`.
    pub fn plugin_tx(&self) -> Option<(mpsc::UnboundedSender<String>, String, String)> {
        let guard = self.plugin.lock().expect("plugin lock");
        guard
            .as_ref()
            .map(|c| (c.tx.clone(), c.file_key.clone(), c.name.clone()))
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

// ── MCP handler ───────────────────────────────────────────────────────────────

/// MCP handler that exposes the turbofig_status tool.
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
    /// No plugin connected: `{"ok":true,"plugin":{"connected":false}}`.
    /// Plugin connected: send a STATUS request over the WS channel and await
    /// the RESULT, then return `{"ok":true,"plugin":{"connected":true,...}}`.
    /// `"ok":true` always means the daemon is alive regardless of plugin state.
    #[tool(description = "Return daemon status")]
    async fn turbofig_status(&self) -> Result<CallToolResult, McpError> {
        // Disconnected path: no plugin registered.
        let Some((tx, file_key, name)) = self.state.plugin_tx() else {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                json!({"ok": true, "plugin": {"connected": false}}).to_string(),
            )]));
        };

        // Connected path: round-trip to the plugin.
        let id = self.state.next_request_id();
        let rx = self.state.register_pending(id);
        let request = json!({"type": "STATUS", "requestId": id});

        if tx.send(request.to_string()).is_err() {
            // Channel closed between snapshot and send; treat as disconnected.
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                json!({"ok": true, "plugin": {"connected": false}}).to_string(),
            )]));
        }

        match rx.await {
            Ok(result) => {
                let fk = result
                    .get("fileKey")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&file_key)
                    .to_owned();
                let nm = result
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&name)
                    .to_owned();
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    json!({
                        "ok": true,
                        "plugin": {"connected": true, "fileKey": fk, "name": nm}
                    })
                    .to_string(),
                )]))
            }
            Err(_) => {
                // Sender was dropped before the reply arrived; treat as disconnected.
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    json!({"ok": true, "plugin": {"connected": false}}).to_string(),
                )]))
            }
        }
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
                            state.register_plugin(file_key, name, tx.clone());
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

    // Socket closed: remove the plugin registration.
    state.clear_plugin();
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
}
