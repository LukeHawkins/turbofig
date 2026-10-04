//! The plugin WebSocket server: one connection per open Figma file.

use crate::config::port_from_env;
use crate::state::AppState;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
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
/// Maximum size (in bytes) of one WebSocket message or frame.
/// Matches the plugin's own 16 MiB RESULT cap with headroom, and bounds how
/// much memory one misbehaving or malicious connection can force us to hold.
const MAX_WS_MESSAGE_BYTES: usize = 32 * 1024 * 1024;

/// Build the WELCOME message sent to a plugin after it sends FILE_INFO.
/// Returns a JSON string with `type`, `version`, and `mcpPort` fields.
/// `mcpPort` is the HTTP MCP port so the plugin panel can show a ready-to-paste connect prompt.
pub(crate) fn welcome_message() -> String {
    json!({
        "type": "WELCOME",
        "version": env!("CARGO_PKG_VERSION"),
        "mcpPort": port_from_env()
    })
    .to_string()
}

/// Handle an upgraded WebSocket connection from the Figma plugin.
async fn handle_socket(socket: WebSocket, state: Arc<AppState>) {
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

    let mut ping_interval = tokio::time::interval(PING_INTERVAL);
    // The first tick fires immediately; consume it so the first real ping
    // waits a full interval instead of firing the moment the socket opens.
    ping_interval.tick().await;
    let mut last_pong = tokio::time::Instant::now();

    'read: loop {
        tokio::select! {
            _ = ping_interval.tick() => {
                if last_pong.elapsed() > PONG_TIMEOUT {
                    // Stale connection: no pong in PONG_TIMEOUT. Drop it so a
                    // half-dead socket does not keep routing calls into a
                    // void until a full request-timeout finally notices.
                    break 'read;
                }
                let _ = ping_tx.send(());
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
            let name = json
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            state.set_connection_info(conn_id, file_key, name);
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
        // Unknown type: ignore safely, never panic.
        _ => {}
    }
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

/// axum handler that upgrades an HTTP request to a WebSocket connection.
/// Rejects the upgrade with 403 when the Origin header is present and is
/// neither absent nor "null", so an arbitrary web page cannot open this socket
/// and drive the Figma plugin (the MCP spec's Origin-validation requirement).
async fn ws_handler(
    ws: WebSocketUpgrade,
    headers: axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
) -> axum::response::Response {
    if !ws_origin_allowed(headers.get(axum::http::header::ORIGIN)) {
        return (axum::http::StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }
    ws.max_message_size(MAX_WS_MESSAGE_BYTES)
        .max_frame_size(MAX_WS_MESSAGE_BYTES)
        .on_upgrade(move |socket| handle_socket(socket, state))
        .into_response()
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
    fn dispatch_ignores_result_from_an_unregistered_conn_id() {
        let state = Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let real_conn = state.add_connection(tx.clone());
        state.set_connection_info(real_conn, "fk1".to_owned(), "Real".to_owned());
        let (id, mut pending_rx) = state
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
}
