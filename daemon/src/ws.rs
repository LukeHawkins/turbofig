//! The plugin WebSocket server: one connection per open Figma file.

use crate::config::{bridge_dir_display, port_from_env};
use crate::daemon_auth::{self, LABEL_WS_DAEMON, LABEL_WS_PLUGIN};
use crate::mcp::host_allowed;
use crate::state::AppState;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Query, State, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Interval between keepalive pings sent to each connected plugin.
const PING_INTERVAL: Duration = Duration::from_secs(15);
/// How long a connection may go without a pong before it is dropped as stale.
const PONG_TIMEOUT: Duration = Duration::from_secs(45);
/// Interval between HEARTBEAT text frames sent to each authenticated plugin.
/// A browser answers protocol pings by itself, so plugin JavaScript cannot
/// see them. This text frame lets the plugin detect a dead link.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

/// Liveness timings for one WebSocket listener. Tests shorten them.
#[derive(Debug, Clone, Copy)]
pub struct WsTimings {
    /// Interval between protocol pings.
    pub ping_interval: Duration,
    /// Longest gap without a pong before the connection is dropped.
    pub pong_timeout: Duration,
    /// Interval between HEARTBEAT text frames.
    pub heartbeat_interval: Duration,
}

impl Default for WsTimings {
    fn default() -> Self {
        Self {
            ping_interval: PING_INTERVAL,
            pong_timeout: PONG_TIMEOUT,
            heartbeat_interval: HEARTBEAT_INTERVAL,
        }
    }
}

/// The HEARTBEAT frame. An older plugin drops an unknown frame type.
pub(crate) fn heartbeat_message() -> String {
    json!({ "type": "HEARTBEAT" }).to_string()
}
/// Maximum size (in bytes) of one WebSocket message or frame.
/// Matches the plugin's own 16 MiB RESULT cap with headroom, and bounds how
/// much memory one misbehaving or malicious connection can force us to hold.
const MAX_WS_MESSAGE_BYTES: usize = 32 * 1024 * 1024;
/// Maximum length (in bytes) of a `fileKey` in FILE_INFO. Longer frames are dropped.
const MAX_FILE_KEY_BYTES: usize = 256;
/// How long a nonce client may take to answer CHALLENGE with AUTH.
/// Maximum number of upgraded sockets that may wait for AUTH at once.
pub(crate) const MAX_PENDING_HANDSHAKES: usize = 8;
const AUTH_TIMEOUT: Duration = Duration::from_secs(2);

/// Per-listener context: the bound WS port and the pending handshakes.
/// When all slots are taken, the oldest pending handshake is closed, so an
/// unauthenticated client cannot keep the real plugin out.
pub(crate) struct WsContext {
    port: u16,
    timings: WsTimings,
    next_id: std::sync::atomic::AtomicU64,
    pending: std::sync::Mutex<std::collections::VecDeque<(u64, tokio::sync::oneshot::Sender<()>)>>,
}

/// One pending handshake: its permit, its registry entry and its close signal.
struct PendingSlot {
    _permit: tokio::sync::OwnedSemaphorePermit,
    ctx: Arc<WsContext>,
    id: u64,
    kill: tokio::sync::oneshot::Receiver<()>,
}

impl PendingSlot {
    fn new(permit: tokio::sync::OwnedSemaphorePermit, ctx: Arc<WsContext>) -> Self {
        let (id, kill) = ctx.register();
        Self {
            _permit: permit,
            ctx,
            id,
            kill,
        }
    }
}

impl Drop for PendingSlot {
    /// Runs also when axum drops the upgrade callback after a failed upgrade.
    fn drop(&mut self) {
        self.ctx.remove(self.id);
    }
}

impl WsContext {
    #[cfg(test)]
    pub(crate) fn new(port: u16) -> Arc<Self> {
        Self::with_timings(port, WsTimings::default())
    }

    pub(crate) fn with_timings(port: u16, timings: WsTimings) -> Arc<Self> {
        Arc::new(Self {
            port,
            timings,
            next_id: std::sync::atomic::AtomicU64::new(0),
            pending: std::sync::Mutex::new(std::collections::VecDeque::new()),
        })
    }

    fn register(&self) -> (u64, tokio::sync::oneshot::Receiver<()>) {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back((id, tx));
        (id, rx)
    }

    fn remove(&self, id: u64) {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(i, _)| *i != id);
    }

    /// Signal the oldest pending handshake to close.
    fn evict_oldest(&self) {
        loop {
            let oldest = self
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pop_front();
            // Skip dead entries: their receiver is gone already.
            match oldest {
                Some((_, tx)) => {
                    if tx.send(()).is_ok() {
                        return;
                    }
                }
                None => return,
            }
        }
    }
}

/// Build the WELCOME message sent to a plugin after it sends FILE_INFO.
/// Returns a JSON string with `type`, `version`, `mcpPort`, and `bridgeHome`
/// fields. `mcpPort` is the HTTP MCP port and `bridgeHome` is the file-bridge
/// home directory (see `config::bridge_dir_display`), so the plugin panel can
/// show a ready-to-paste connect prompt that names the real bridge directory
/// even under a custom `TURBOFIG_BRIDGE_DIR`.
pub(crate) fn welcome_message() -> String {
    json!({
        "type": "WELCOME",
        "version": env!("CARGO_PKG_VERSION"),
        "mcpPort": port_from_env(),
        "bridgeHome": bridge_dir_display()
    })
    .to_string()
}

/// Handle an upgraded WebSocket connection from the Figma plugin.
async fn handle_socket(socket: WebSocket, state: Arc<AppState>, timings: WsTimings) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let (ping_tx, mut ping_rx) = mpsc::unbounded_channel::<()>();

    // Allocate a stable conn_id for this connection immediately.
    let conn_id = state.add_connection(tx.clone());

    // Write task: forward outbound text frames and keepalive pings.
    tokio::spawn(async move {
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(m) => {
                        if sink.send(Message::Text(m.into())).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                },
                p = ping_rx.recv() => match p {
                    Some(()) => {
                        if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                },
            }
        }
    });

    let mut ping_interval = tokio::time::interval(timings.ping_interval);
    // The first tick fires immediately; consume it so the first real ping
    // waits a full interval instead of firing the moment the socket opens.
    ping_interval.tick().await;
    let mut heartbeat_interval = tokio::time::interval(timings.heartbeat_interval);
    heartbeat_interval.tick().await;
    let mut last_pong = tokio::time::Instant::now();

    'read: loop {
        tokio::select! {
            _ = ping_interval.tick() => {
                if last_pong.elapsed() > timings.pong_timeout {
                    // Stale connection: no pong in the pong timeout. Drop it so a
                    // half-dead socket does not keep routing calls into a
                    // void until a full request-timeout finally notices.
                    break 'read;
                }
                let _ = ping_tx.send(());
            }
            _ = heartbeat_interval.tick() => {
                // Only authenticated connections reach this function.
                let _ = tx.send(heartbeat_message());
            }
            frame = stream.next() => {
                match frame {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(json) = serde_json::from_str::<Value>(&text) {
                            dispatch(&json, &state, &tx, conn_id);
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {
                        last_pong = tokio::time::Instant::now();
                    }
                    Some(Ok(Message::Close(_))) | None => break 'read,
                    Some(Err(_)) => break 'read,
                    _ => {}
                }
            }
        }
    }

    // Socket closed: remove the connection and fail only its in-flight requests.
    // Other files' in-flight requests are unaffected.
    state.remove_connection(conn_id);
    state.cancel_pending_for_conn(conn_id);
}

/// Dispatch one parsed inbound frame by `type`. Never panics on malformed input.
///
/// Ignores every frame from a conn_id no longer in the registry: a message
/// that arrives just after its own connection closed (or, in principle, a
/// forged conn_id) must never touch state on behalf of a connection that is
/// not actually live.
fn dispatch(json: &Value, state: &Arc<AppState>, tx: &mpsc::UnboundedSender<String>, conn_id: u64) {
    if !state.connection_exists(conn_id) {
        return;
    }
    match json.get("type").and_then(|t| t.as_str()) {
        Some("FILE_INFO") => {
            let file_key = json
                .get("fileKey")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            if file_key.len() > MAX_FILE_KEY_BYTES {
                return;
            }
            let name = json
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            let plugin_version = json
                .get("pluginVersion")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            state.set_connection_info_with_version(conn_id, file_key, name, plugin_version);
            // Push WELCOME so the plugin UI can display version/session info.
            // Ignore send errors: the write task may have already exited.
            let _ = tx.send(welcome_message());
        }
        Some("RESULT") => {
            if let Some(id) = json.get("requestId").and_then(|v| v.as_u64()) {
                // conn_id ties this RESULT to the connection it was sent on,
                // so a reply forged by a different connection (e.g. a
                // null-origin WebSocket opened by a malicious web page) for
                // a guessed or brute-forced request id is dropped, not
                // resolved. See state::AppState::resolve.
                state.resolve(id, conn_id, json.clone());
            }
        }
        Some("STARTED") => {
            if let Some(id) = json.get("requestId").and_then(|v| v.as_u64()) {
                // Same conn_id ownership check as RESULT: a STARTED claiming
                // an id owned by a different connection is ignored. See
                // state::AppState::mark_started.
                state.mark_started(id, conn_id);
            }
        }
        // Unknown type: ignore safely, never panic.
        _ => {}
    }
}

/// Query parameters read on the WS upgrade. Any other parameter is ignored.
/// A `token` parameter (the removed 0.1.0 plugin auth) is refused with 401.
#[derive(serde::Deserialize)]
struct WsAuthQuery {
    #[serde(default)]
    token: Option<String>,
    /// Plugin nonce for the challenge handshake.
    #[serde(default)]
    nonce: Option<String>,
}

/// Returns true when `origin` is acceptable for the plugin WebSocket.
/// Accepts a missing Origin header (non-browser clients send none) and the
/// literal string "null" (the Figma plugin UI runs in a sandboxed iframe,
/// which the browser reports as a null origin). Rejects every other value.
fn ws_origin_allowed(origin: Option<&axum::http::HeaderValue>) -> bool {
    match origin {
        None => true,
        Some(v) => v.as_bytes() == b"null",
    }
}

/// Returns true when the request carries at most one Origin header and that
/// one passes `ws_origin_allowed`. A second Origin header is always refused.
fn origin_headers_allowed(headers: &axum::http::HeaderMap) -> bool {
    let mut all = headers.get_all(axum::http::header::ORIGIN).iter();
    let first = all.next();
    all.next().is_none() && ws_origin_allowed(first)
}

/// Run the challenge handshake on a fresh socket. Returns true when the
/// plugin proved the token within `timeout`. The CHALLENGE proof covers the
/// nonce and the bound WS `port`, so a squatter on another port cannot relay it.
/// Sends CHALLENGE first, then
/// accepts only one AUTH text frame. Anything else returns false.
async fn challenge_handshake(
    socket: &mut WebSocket,
    token: &str,
    nonce_p: &str,
    port: u16,
    timeout: Duration,
) -> bool {
    let nonce_d = daemon_auth::new_nonce();
    let challenge = json!({
        "type": "CHALLENGE",
        "nonce": nonce_d,
        "proof": daemon_auth::proof(token, LABEL_WS_DAEMON, &format!("{nonce_p}:{port}")),
    })
    .to_string();
    if socket.send(Message::Text(challenge.into())).await.is_err() {
        return false;
    }
    let Ok(Some(Ok(Message::Text(text)))) = tokio::time::timeout(timeout, socket.recv()).await
    else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    if json.get("type").and_then(|t| t.as_str()) != Some("AUTH") {
        return false;
    }
    match json.get("proof").and_then(|p| p.as_str()) {
        Some(p) => daemon_auth::verify(token, LABEL_WS_PLUGIN, &nonce_d, p),
        None => false,
    }
}

/// Upgraded nonce connection: authenticate first, then serve the plugin.
async fn handle_challenge_socket(
    mut socket: WebSocket,
    state: Arc<AppState>,
    nonce_p: String,
    timeout: Duration,
    mut slot: PendingSlot,
) {
    let port = slot.ctx.port;
    let timings = slot.ctx.timings;
    let ok = tokio::select! {
        ok = challenge_handshake(&mut socket, state.token(), &nonce_p, port, timeout) => ok,
        _ = &mut slot.kill => false,
    };
    // Release the pending slot as soon as AUTH succeeds, fails or times out.
    drop(slot);
    if !ok {
        // Drop the socket: closes it with no detail.
        return;
    }
    state.mark_plugin_seen();
    handle_socket(socket, state, timings).await;
}

/// axum handler that upgrades an HTTP request to a WebSocket connection.
///
/// Rejects the upgrade with 403 when the Origin header is present and is
/// neither absent nor "null" (the MCP spec's Origin-validation requirement).
/// Then rejects with 401 unless the request carries a valid `nonce` and no
/// `token` parameter. The plugin then proves the pairing token in the
/// CHALLENGE/AUTH handshake: Origin alone cannot tell the real
/// Figma plugin UI apart from a sandboxed `<iframe>` on a malicious web page,
/// since both report Origin `null` (see `token.rs`).
async fn ws_handler(
    ws: WebSocketUpgrade,
    headers: axum::http::HeaderMap,
    Query(auth): Query<WsAuthQuery>,
    State(state): State<Arc<AppState>>,
    axum::Extension(ctx): axum::Extension<Arc<WsContext>>,
) -> axum::response::Response {
    upgrade(ws, headers, auth, state, AUTH_TIMEOUT, ctx).await
}

/// Shared upgrade logic. `auth_timeout` is a parameter so tests can shorten it.
async fn upgrade(
    ws: WebSocketUpgrade,
    headers: axum::http::HeaderMap,
    auth: WsAuthQuery,
    state: Arc<AppState>,
    auth_timeout: Duration,
    ctx: Arc<WsContext>,
) -> axum::response::Response {
    if !origin_headers_allowed(&headers) {
        return (axum::http::StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok());
    if !host_allowed(host) {
        return (axum::http::StatusCode::FORBIDDEN, "host not allowed").into_response();
    }
    let ws = ws
        .max_message_size(MAX_WS_MESSAGE_BYTES)
        .max_frame_size(MAX_WS_MESSAGE_BYTES);
    if auth.token.is_some() {
        // Old plugins sent the token in the URL. That path is removed.
        return unauthorized();
    }
    match auth.nonce {
        Some(nonce) if daemon_auth::valid_nonce(&nonce) => {
            let permit = match state.pending_handshakes.clone().try_acquire_owned() {
                Ok(p) => p,
                Err(_) => {
                    // All slots taken: close the oldest pending handshake.
                    ctx.evict_oldest();
                    let wait = state.pending_handshakes.clone().acquire_owned();
                    match tokio::time::timeout(Duration::from_secs(1), wait).await {
                        Ok(Ok(p)) => p,
                        _ => {
                            return (
                                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                                "too many pending handshakes",
                            )
                                .into_response();
                        }
                    }
                }
            };
            let slot = PendingSlot::new(permit, ctx);
            ws.on_upgrade(move |socket| {
                handle_challenge_socket(socket, state, nonce, auth_timeout, slot)
            })
            .into_response()
        }
        _ => unauthorized(),
    }
}

fn unauthorized() -> axum::response::Response {
    (
        axum::http::StatusCode::UNAUTHORIZED,
        "invalid or missing token",
    )
        .into_response()
}

/// Serve the WebSocket endpoint on the given TCP listener.
pub async fn serve_ws(
    listener: tokio::net::TcpListener,
    state: Arc<AppState>,
) -> std::io::Result<()> {
    serve_ws_with_timings(listener, state, WsTimings::default()).await
}

/// Like `serve_ws`, with caller-chosen liveness timings (used by tests).
pub async fn serve_ws_with_timings(
    listener: tokio::net::TcpListener,
    state: Arc<AppState>,
    timings: WsTimings,
) -> std::io::Result<()> {
    let ctx = WsContext::with_timings(listener.local_addr()?.port(), timings);
    let router = axum::Router::new()
        .route("/", axum::routing::get(ws_handler))
        .layer(axum::Extension(ctx))
        .with_state(state);
    let listener = crate::listener::LimitedListener::new(
        listener,
        crate::listener::WS_MAX_CONNECTIONS,
        crate::listener::WS_IDLE_TIMEOUT,
    );
    axum::serve(listener, router).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::port_from_env;

    #[test]
    fn welcome_message_has_correct_type_and_version() {
        let msg = welcome_message();
        let v: Value = serde_json::from_str(&msg).expect("welcome_message must be valid JSON");
        assert_eq!(v["type"], json!("WELCOME"), "type must be WELCOME");
        assert_eq!(
            v["version"],
            json!(env!("CARGO_PKG_VERSION")),
            "version must match CARGO_PKG_VERSION"
        );
        assert!(
            v["mcpPort"].is_number(),
            "mcpPort must be present and numeric"
        );
        assert_eq!(
            v["mcpPort"].as_u64(),
            Some(u64::from(port_from_env())),
            "mcpPort must equal port_from_env()"
        );
        assert_eq!(
            v["bridgeHome"],
            json!(bridge_dir_display()),
            "bridgeHome must equal bridge_dir_display()"
        );
    }

    #[test]
    fn ws_origin_allowed_accepts_absent_and_null() {
        assert!(ws_origin_allowed(None));
        assert!(ws_origin_allowed(Some(
            &axum::http::HeaderValue::from_static("null")
        )));
    }

    #[test]
    fn ws_origin_allowed_rejects_a_real_origin() {
        assert!(!ws_origin_allowed(Some(
            &axum::http::HeaderValue::from_static("https://evil.example")
        )));
    }

    #[test]
    fn dispatch_ignores_file_info_from_an_unregistered_conn_id() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let msg = json!({"type": "FILE_INFO", "fileKey": "fk1", "name": "Ghost"});
        dispatch(&msg, &state, &tx, 999);
        assert!(
            state.list_connections().is_empty(),
            "an unregistered conn_id must not create a connection entry"
        );
    }

    #[test]
    fn dispatch_file_info_stores_the_reported_plugin_version() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let conn_id = state.add_connection(tx.clone());
        let msg = json!({
            "type": "FILE_INFO",
            "fileKey": "fk1",
            "name": "Doc",
            "pluginVersion": "0.9.9"
        });
        dispatch(&msg, &state, &tx, conn_id);
        let json = state.named_connections_json();
        assert_eq!(json[0]["pluginVersion"], "0.9.9");
    }

    #[test]
    fn dispatch_file_info_without_plugin_version_stores_an_empty_string() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let conn_id = state.add_connection(tx.clone());
        let msg = json!({"type": "FILE_INFO", "fileKey": "fk1", "name": "Doc"});
        dispatch(&msg, &state, &tx, conn_id);
        let json = state.named_connections_json();
        assert_eq!(json[0]["pluginVersion"], "");
        assert!(json[0].get("warning").is_none());
    }

    #[test]
    fn dispatch_ignores_result_from_an_unregistered_conn_id() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let real_conn = state.add_connection(tx.clone());
        state.set_connection_info(real_conn, "fk1".to_owned(), "Real".to_owned());
        let (id, mut pending_rx, _started) = state
            .register_pending_if_connected(real_conn)
            .expect("real connection live");

        // A different, unregistered conn_id claims the same request id.
        let msg = json!({"type": "RESULT", "requestId": id, "ok": true});
        dispatch(&msg, &state, &tx, 999);

        assert!(
            pending_rx.try_recv().is_err(),
            "a RESULT from an unregistered conn_id must not resolve a pending request"
        );
    }

    #[test]
    fn dispatch_started_marks_the_pending_entry_for_the_owning_connection() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            200,
        )));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let conn_id = state.add_connection(tx.clone());
        state.set_connection_info(conn_id, "fk1".to_owned(), "Doc".to_owned());
        let (id, _rx2, started) = state
            .register_pending_if_connected(conn_id)
            .expect("conn live");

        let msg = json!({"type": "STARTED", "requestId": id});
        dispatch(&msg, &state, &tx, conn_id);

        assert!(
            started.load(std::sync::atomic::Ordering::SeqCst),
            "a STARTED from the owning connection must set the started flag"
        );
    }

    #[test]
    fn dispatch_started_from_another_connection_is_ignored() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            200,
        )));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let conn_id = state.add_connection(tx.clone());
        let (id, _rx2, started) = state
            .register_pending_if_connected(conn_id)
            .expect("conn live");

        let msg = json!({"type": "STARTED", "requestId": id});
        dispatch(&msg, &state, &tx, conn_id + 999);

        assert!(
            !started.load(std::sync::atomic::Ordering::SeqCst),
            "a STARTED from a different connection must not set the started flag"
        );
    }

    use tokio_tungstenite::tungstenite::Message as Tt;

    const NONCE_P: &str = "00112233445566778899aabbccddeeff";

    /// Serve the real `upgrade` logic with a caller-chosen AUTH timeout.
    async fn serve_with_timeout(state: Arc<AppState>, timeout: Duration) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let ctx = WsContext::new(port);
        let handler = move |ws: WebSocketUpgrade,
                            headers: axum::http::HeaderMap,
                            Query(auth): Query<WsAuthQuery>,
                            State(st): State<Arc<AppState>>| {
            let ctx = ctx.clone();
            async move { upgrade(ws, headers, auth, st, timeout, ctx).await }
        };
        let router = axum::Router::new()
            .route("/", axum::routing::get(handler))
            .with_state(state);
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        port
    }

    #[tokio::test]
    async fn auth_timeout_closes_the_socket() {
        let state = Arc::new(AppState::new());
        let port = serve_with_timeout(state.clone(), Duration::from_millis(150)).await;
        let (mut ws, _) =
            tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/?nonce={NONCE_P}"))
                .await
                .unwrap();
        let first = ws.next().await.unwrap().unwrap();
        assert!(first.to_text().unwrap().contains("CHALLENGE"));
        // No AUTH is sent. The daemon must close within the deadline.
        let end = tokio::time::timeout(Duration::from_secs(3), ws.next())
            .await
            .expect("socket must close after the AUTH timeout");
        assert!(!matches!(end, Some(Ok(Tt::Text(_)))));
        assert!(state.list_connections().is_empty());
    }

    #[tokio::test]
    async fn authenticated_nonce_connection_runs_a_job_round_trip() {
        let state = Arc::new(AppState::new());
        let port = serve_with_timeout(state.clone(), Duration::from_secs(5)).await;
        let (mut ws, _) =
            tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/?nonce={NONCE_P}"))
                .await
                .unwrap();
        let ch: Value =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        let nonce_d = ch["nonce"].as_str().unwrap();
        assert_eq!(
            ch["proof"],
            daemon_auth::proof(state.token(), LABEL_WS_DAEMON, &format!("{NONCE_P}:{port}"))
        );
        let auth = json!({
            "type": "AUTH",
            "proof": daemon_auth::proof(state.token(), LABEL_WS_PLUGIN, nonce_d),
        });
        ws.send(Tt::Text(auth.to_string())).await.unwrap();
        ws.send(Tt::Text(
            json!({"type": "FILE_INFO", "fileKey": "fk", "name": "Doc"}).to_string(),
        ))
        .await
        .unwrap();
        let welcome = ws.next().await.unwrap().unwrap();
        assert!(welcome.to_text().unwrap().contains("WELCOME"));

        let (conn_id, tx, _, _, _) = state.connections_named().remove(0);
        let st = state.clone();
        let call = tokio::spawn(async move {
            crate::plugin_call::call_plugin(
                &st,
                conn_id,
                &tx,
                json!({"type": "EXECUTE"}),
                Duration::from_secs(5),
            )
            .await
        });
        let job: Value =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(job["type"], "EXECUTE");
        let reply = json!({"type": "RESULT", "requestId": job["requestId"], "ok": true});
        ws.send(Tt::Text(reply.to_string())).await.unwrap();
        let (_, outcome) = call.await.unwrap();
        assert!(matches!(outcome, crate::plugin_call::CallOutcome::Reply(_)));
    }

    #[test]
    fn origin_headers_allowed_rejects_two_origin_headers() {
        let mut h = axum::http::HeaderMap::new();
        h.append(axum::http::header::ORIGIN, "null".parse().unwrap());
        assert!(origin_headers_allowed(&h));
        h.append(
            axum::http::header::ORIGIN,
            "https://evil.example".parse().unwrap(),
        );
        assert!(!origin_headers_allowed(&h));
    }

    #[test]
    fn dispatch_drops_file_info_with_an_oversized_file_key() {
        let state = Arc::new(AppState::with_timeout(Duration::from_millis(100)));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let conn_id = state.add_connection(tx.clone());
        let long = "k".repeat(MAX_FILE_KEY_BYTES + 1);
        dispatch(
            &json!({"type": "FILE_INFO", "fileKey": long, "name": "D"}),
            &state,
            &tx,
            conn_id,
        );
        assert!(state.connections_named().is_empty());
        let ok = "k".repeat(MAX_FILE_KEY_BYTES);
        dispatch(
            &json!({"type": "FILE_INFO", "fileKey": ok, "name": "D"}),
            &state,
            &tx,
            conn_id,
        );
        assert_eq!(state.connections_named().len(), 1);
    }

    async fn pending_client(
        port: u16,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        let (mut ws, _) =
            tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/?nonce={NONCE_P}"))
                .await
                .unwrap();
        // Wait for CHALLENGE so the permit is certainly held.
        ws.next().await.unwrap().unwrap();
        ws
    }

    #[tokio::test]
    async fn a_full_pending_set_evicts_the_oldest_and_admits_a_new_handshake() {
        let state = Arc::new(AppState::new());
        let port = serve_with_timeout(state.clone(), Duration::from_secs(30)).await;
        let mut held = Vec::new();
        for _ in 0..MAX_PENDING_HANDSHAKES {
            held.push(pending_client(port).await);
        }
        // The new handshake succeeds although all slots were taken.
        let mut real = pending_client(port).await;
        // The oldest pending socket is closed.
        let mut oldest = held.remove(0);
        let end = tokio::time::timeout(Duration::from_secs(2), oldest.next())
            .await
            .expect("the oldest must close");
        assert!(matches!(end, None | Some(Err(_)) | Some(Ok(Tt::Close(_)))));
        // The newest is still pending, not closed.
        assert!(
            tokio::time::timeout(Duration::from_millis(200), real.next())
                .await
                .is_err()
        );
        // The removed legacy token path gets 401, cap or not.
        let legacy = format!("ws://127.0.0.1:{port}/?token={}", state.token());
        match tokio_tungstenite::connect_async(legacy).await {
            Err(tokio_tungstenite::tungstenite::Error::Http(r)) => assert_eq!(r.status(), 401),
            other => panic!("expected 401, got {other:?}"),
        }
        drop(held);
    }

    #[tokio::test]
    async fn a_dropped_slot_leaves_no_entry_and_dead_entries_are_skipped() {
        let sem = Arc::new(tokio::sync::Semaphore::new(4));
        let ctx = WsContext::new(1);
        // Failed upgrades: the slot drops without running the callback.
        for _ in 0..50 {
            let p = sem.clone().acquire_owned().await.unwrap();
            drop(PendingSlot::new(p, ctx.clone()));
        }
        assert!(ctx.pending.lock().unwrap().is_empty());
        // A dead entry (receiver gone, entry left) must not absorb an eviction.
        let (_dead_id, dead_rx) = ctx.register();
        drop(dead_rx);
        let (_live_id, mut live_rx) = ctx.register();
        ctx.evict_oldest();
        assert!(live_rx.try_recv().is_ok());
    }

    #[tokio::test]
    async fn a_permit_frees_after_the_auth_timeout() {
        let state = Arc::new(AppState::new());
        let port = serve_with_timeout(state.clone(), Duration::from_millis(300)).await;
        let held = pending_client(port).await;
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(
            state.pending_handshakes.available_permits(),
            MAX_PENDING_HANDSHAKES
        );
        drop(held);
    }
}
