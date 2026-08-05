//! Integration tests for simultaneous multiple plugin connections.
//!
//! Each test binds ephemeral ports, starts serve_ws with a shared AppState,
//! and connects two mock plugins with distinct fileKeys. Tests verify:
//!   1. Both plugins remain registered concurrently.
//!   2. run_execute routes each call to the correct plugin with no cross-talk.
//!   3. When fk1 closes, fk2 still answers a routed run_execute.
//!
//! No fixed port numbers are used. Synchronisation waits on observable state,
//! not on bare fixed sleeps.

use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::{connect_async, tungstenite::Message as TtMessage};

// ── helpers ───────────────────────────────────────────────────────────────────

/// Start a WS server with a fresh AppState. Returns (ws_port, state).
async fn start_ws_stack() -> (u16, Arc<turbofig::AppState>) {
    // Use a short timeout so routing failures surface quickly in tests.
    let state = Arc::new(turbofig::AppState::with_timeout(Duration::from_secs(5)));

    let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral WS port");
    let ws_port = ws_listener.local_addr().expect("ws local addr").port();

    let ws_state = state.clone();
    tokio::spawn(async move {
        turbofig::serve_ws(ws_listener, ws_state)
            .await
            .expect("serve_ws error in test");
    });

    (ws_port, state)
}

/// Connect a mock plugin with `file_key`. The plugin replies to every EXECUTE
/// frame with `{"ok":true,"result":{"from":<reply_tag>}}`.
///
/// A background task owns the socket and keeps it open for the whole test. To
/// close a plugin deliberately, connect a raw socket directly and drop it.
async fn connect_mock_plugin(ws_port: u16, file_key: &str, reply_tag: &'static str) {
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{ws_port}/"))
        .await
        .expect("mock plugin connect");

    let fi = serde_json::json!({
        "type": "FILE_INFO",
        "fileKey": file_key,
        "name": file_key
    });
    ws.send(TtMessage::Text(fi.to_string()))
        .await
        .expect("send FILE_INFO");

    // Wait for registration to propagate before returning.
    tokio::time::sleep(Duration::from_millis(60)).await;

    tokio::spawn(async move {
        while let Some(Ok(msg)) = ws.next().await {
            if let TtMessage::Text(text) = msg {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    if json.get("type").and_then(|t| t.as_str()) == Some("EXECUTE") {
                        if let Some(id) = json.get("requestId").and_then(|v| v.as_u64()) {
                            let reply = serde_json::json!({
                                "type": "RESULT",
                                "requestId": id,
                                "ok": true,
                                "result": {"from": reply_tag}
                            });
                            let _ = ws.send(TtMessage::Text(reply.to_string())).await;
                        }
                    }
                }
            }
        }
    });
}

/// Wait until `state.list_connections()` has at least `n` entries or the
/// deadline passes. Returns the count observed.
async fn wait_for_connections(state: &Arc<turbofig::AppState>, n: usize) -> usize {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let count = state.list_connections().len();
        if count >= n {
            return count;
        }
        if tokio::time::Instant::now() >= deadline {
            return count;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Wait until `state.list_connections()` contains an entry with the given
/// file_key, or the deadline passes. Returns true when found.
async fn wait_for_file_key(state: &Arc<turbofig::AppState>, file_key: &str) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let found = state
            .list_connections()
            .iter()
            .any(|(_, fk, _)| fk == file_key);
        if found {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Wait until `state.list_connections()` does NOT contain the given file_key.
async fn wait_for_file_key_gone(state: &Arc<turbofig::AppState>, file_key: &str) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let found = state
            .list_connections()
            .iter()
            .any(|(_, fk, _)| fk == file_key);
        if !found {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Two plugins (fk1, fk2) connect to the same serve_ws at the same time.
/// Both must remain registered concurrently. list_connections must report 2.
#[tokio::test]
async fn test_two_plugins_registered_concurrently() {
    let (ws_port, state) = start_ws_stack().await;

    // Connect both plugins. Each call waits for its own registration.
    connect_mock_plugin(ws_port, "fk1", "fk1-reply").await;
    connect_mock_plugin(ws_port, "fk2", "fk2-reply").await;

    // Both must appear in list_connections at the same time.
    let count = wait_for_connections(&state, 2).await;
    assert_eq!(
        count, 2,
        "both plugins must be registered concurrently, got count={count}"
    );

    // Each fileKey must be present.
    let conns = state.list_connections();
    let keys: Vec<&str> = conns.iter().map(|(_, k, _)| k.as_str()).collect();
    assert!(
        keys.contains(&"fk1"),
        "fk1 must appear in list_connections, got: {keys:?}"
    );
    assert!(
        keys.contains(&"fk2"),
        "fk2 must appear in list_connections, got: {keys:?}"
    );
}

/// With two plugins registered, concurrent run_execute calls to fk1 and fk2
/// each receive a reply from the correct plugin. No cross-talk occurs.
#[tokio::test]
async fn test_concurrent_routing_no_cross_talk() {
    let (ws_port, state) = start_ws_stack().await;

    connect_mock_plugin(ws_port, "fk1", "fk1-reply").await;
    connect_mock_plugin(ws_port, "fk2", "fk2-reply").await;

    // Wait until both plugins are registered before routing.
    let count = wait_for_connections(&state, 2).await;
    assert_eq!(count, 2, "both plugins must be registered before routing");

    // Fire both run_execute calls at the same time.
    let state1 = state.clone();
    let state2 = state.clone();
    let (result1, result2) = tokio::join!(
        async move { turbofig::run_execute(&state1, None, Some("fk1"), "return 1;").await },
        async move { turbofig::run_execute(&state2, None, Some("fk2"), "return 1;").await },
    );

    // Each result must come from its own plugin.
    assert_eq!(
        result1["ok"],
        serde_json::json!(true),
        "fk1 execute must succeed, got: {result1}"
    );
    assert_eq!(
        result1["result"]["from"],
        serde_json::json!("fk1-reply"),
        "fk1 result must come from fk1-reply, got: {result1}"
    );

    assert_eq!(
        result2["ok"],
        serde_json::json!(true),
        "fk2 execute must succeed, got: {result2}"
    );
    assert_eq!(
        result2["result"]["from"],
        serde_json::json!("fk2-reply"),
        "fk2 result must come from fk2-reply, got: {result2}"
    );
}

/// After plugin fk1's socket closes, plugin fk2 still answers run_execute
/// targeting fk2. The close must not break fk2's registration.
#[tokio::test]
async fn test_fk2_survives_fk1_close() {
    let (ws_port, state) = start_ws_stack().await;

    // Connect fk2 first. It stays open for the full test.
    connect_mock_plugin(ws_port, "fk2", "fk2-reply").await;

    // Connect fk1 as a raw socket so we can close it deliberately.
    let (mut fk1_ws, _) = connect_async(format!("ws://127.0.0.1:{ws_port}/"))
        .await
        .expect("fk1 connect");
    fk1_ws
        .send(TtMessage::Text(
            serde_json::json!({
                "type": "FILE_INFO",
                "fileKey": "fk1",
                "name": "fk1"
            })
            .to_string(),
        ))
        .await
        .expect("send fk1 FILE_INFO");

    // Wait until both plugins are registered.
    let found_fk1 = wait_for_file_key(&state, "fk1").await;
    assert!(found_fk1, "fk1 must register before closing");
    let found_fk2 = wait_for_file_key(&state, "fk2").await;
    assert!(found_fk2, "fk2 must register");

    // Close fk1's socket. The daemon must detect the close and deregister it.
    drop(fk1_ws);
    let fk1_gone = wait_for_file_key_gone(&state, "fk1").await;
    assert!(fk1_gone, "fk1 must deregister after socket close");

    // fk2 must still be registered.
    let fk2_still_present = wait_for_file_key(&state, "fk2").await;
    assert!(
        fk2_still_present,
        "fk2 must remain registered after fk1 closes"
    );

    // run_execute targeting fk2 must still succeed.
    let result = turbofig::run_execute(&state, None, Some("fk2"), "return 1;").await;
    assert_eq!(
        result["ok"],
        serde_json::json!(true),
        "fk2 execute must succeed after fk1 closes, got: {result}"
    );
    assert_eq!(
        result["result"]["from"],
        serde_json::json!("fk2-reply"),
        "fk2 result must come from fk2-reply after fk1 closes, got: {result}"
    );
}
