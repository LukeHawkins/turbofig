//! Integration tests for the WebSocket plugin transport.
//!
//! Each test binds an ephemeral port, starts serve_ws with a fresh AppState,
//! and exercises the FILE_INFO registration and socket-close cleanup.
//! No port 18847 is ever hardcoded here.

use futures_util::SinkExt;
use std::sync::Arc;
use tokio_tungstenite::{connect_async, tungstenite::Message as TtMessage};

/// FILE_INFO registers the plugin; dropping the client clears it.
#[tokio::test]
async fn test_ws_file_info_registers_plugin() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral WS port");
    let addr = listener.local_addr().expect("read local addr");
    let state = Arc::new(turbofig_mcp::AppState::new());
    let state_srv = state.clone();

    tokio::spawn(async move {
        turbofig_mcp::serve_ws(listener, state_srv)
            .await
            .expect("serve_ws error in test");
    });

    // Connect a tungstenite client.
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{}/", addr.port()))
        .await
        .expect("WS connect failed");

    // Send a FILE_INFO frame.
    let msg = serde_json::json!({
        "type": "FILE_INFO",
        "fileKey": "abc123",
        "name": "My Design File"
    });
    ws.send(TtMessage::Text(msg.to_string()))
        .await
        .expect("send FILE_INFO");

    // Allow the server a short time to process the frame.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // The plugin must now be registered with the correct key and name.
    assert_eq!(
        state.plugin_snapshot(),
        Some(("abc123".to_owned(), "My Design File".to_owned())),
        "plugin must be registered after FILE_INFO"
    );

    // Drop the client to trigger a socket close.
    drop(ws);

    // Allow the server a short time to run clear_plugin.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // The plugin must now be cleared.
    assert_eq!(
        state.plugin_snapshot(),
        None,
        "plugin must be cleared after socket close"
    );
}

/// An unknown message type must not panic the server; subsequent FILE_INFO still works.
#[tokio::test]
async fn test_ws_unknown_message_type_is_ignored() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral WS port");
    let addr = listener.local_addr().expect("read local addr");
    let state = Arc::new(turbofig_mcp::AppState::new());
    let state_srv = state.clone();

    tokio::spawn(async move {
        turbofig_mcp::serve_ws(listener, state_srv)
            .await
            .expect("serve_ws error in test");
    });

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{}/", addr.port()))
        .await
        .expect("WS connect failed");

    // Send an unknown type. The server must not panic.
    let unknown = serde_json::json!({"type": "MYSTERY_TYPE", "data": 42});
    ws.send(TtMessage::Text(unknown.to_string()))
        .await
        .expect("send unknown type");

    // Send FILE_INFO afterwards. The connection must still be live.
    let fi = serde_json::json!({
        "type": "FILE_INFO",
        "fileKey": "xyz789",
        "name": "Other File"
    });
    ws.send(TtMessage::Text(fi.to_string()))
        .await
        .expect("send FILE_INFO after unknown type");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert_eq!(
        state.plugin_snapshot(),
        Some(("xyz789".to_owned(), "Other File".to_owned())),
        "plugin must register after an unknown message type was received"
    );
}
