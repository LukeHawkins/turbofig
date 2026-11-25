//! Integration tests for the WebSocket challenge handshake and the upgrade
//! guards (Host, Origin). Each test binds an ephemeral port.

mod common;

use common::wait_until;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as Tt;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Client = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

const NONCE_P: &str = "00112233445566778899aabbccddeeff";
const LABEL_WS_DAEMON: &str = "turbofig-ws-daemon:";
const LABEL_WS_PLUGIN: &str = "turbofig-ws-plugin:";

/// Independent HMAC-SHA256 hex proof, so the test does not reuse the daemon code.
fn proof(token: &str, label: &str, nonce: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut k = [0u8; 64];
    k[..token.len().min(64)].copy_from_slice(&token.as_bytes()[..token.len().min(64)]);
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    let inner = Sha256::new()
        .chain_update(&ipad)
        .chain_update(label.as_bytes())
        .chain_update(nonce.as_bytes())
        .finalize();
    let out = Sha256::new()
        .chain_update(&opad)
        .chain_update(inner)
        .finalize();
    out.iter().map(|b| format!("{b:02x}")).collect()
}

async fn start() -> (Arc<turbofig::AppState>, u16) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let state = Arc::new(turbofig::AppState::new());
    let st = state.clone();
    tokio::spawn(async move {
        turbofig::serve_ws(listener, st).await.unwrap();
    });
    (state, port)
}

async fn connect_nonce(port: u16) -> Client {
    connect_async(format!("ws://127.0.0.1:{port}/?nonce={NONCE_P}"))
        .await
        .expect("nonce connect")
        .0
}

async fn next_json(ws: &mut Client) -> serde_json::Value {
    match ws.next().await.expect("frame").expect("ok frame") {
        Tt::Text(t) => serde_json::from_str(&t).unwrap(),
        other => panic!("expected text, got {other:?}"),
    }
}

/// True when the server closes the socket without sending any further text.
async fn closes(ws: &mut Client) -> bool {
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(3), ws.next()).await {
            Err(_) => return false,
            Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(Tt::Close(_)))) => return true,
            Ok(Some(Ok(Tt::Text(_)))) => return false,
            Ok(Some(Ok(_))) => {}
        }
    }
}

async fn authenticate(ws: &mut Client, token: &str) {
    let ch = next_json(ws).await;
    let nonce_d = ch["nonce"].as_str().unwrap().to_owned();
    let auth =
        serde_json::json!({"type": "AUTH", "proof": proof(token, LABEL_WS_PLUGIN, &nonce_d)});
    ws.send(Tt::Text(auth.to_string())).await.unwrap();
}

#[tokio::test]
async fn challenge_proof_matches_the_token_and_nonce() {
    let (state, port) = start().await;
    let mut ws = connect_nonce(port).await;
    let ch = next_json(&mut ws).await;
    assert_eq!(ch["type"], "CHALLENGE");
    assert_eq!(
        ch["proof"],
        proof(state.token(), LABEL_WS_DAEMON, &format!("{NONCE_P}:{port}"))
    );
    assert!(ch["nonce"].as_str().unwrap().len() >= 32);
}

#[tokio::test]
async fn full_handshake_registers_and_sends_welcome() {
    let (state, port) = start().await;
    let mut ws = connect_nonce(port).await;
    authenticate(&mut ws, state.token()).await;
    let info = serde_json::json!({"type": "FILE_INFO", "fileKey": "fk", "name": "Doc"});
    ws.send(Tt::Text(info.to_string())).await.unwrap();
    assert_eq!(next_json(&mut ws).await["type"], "WELCOME");
    assert_eq!(
        state.plugin_snapshot(),
        Some(("fk".to_owned(), "Doc".to_owned()))
    );
}

#[tokio::test]
async fn wrong_auth_proof_closes_and_registers_nothing() {
    let (state, port) = start().await;
    let mut ws = connect_nonce(port).await;
    let _ = next_json(&mut ws).await;
    let bad = serde_json::json!({"type": "AUTH", "proof": "0".repeat(64)});
    ws.send(Tt::Text(bad.to_string())).await.unwrap();
    assert!(closes(&mut ws).await);
    assert!(state.list_connections().is_empty());
}

#[tokio::test]
async fn a_frame_other_than_auth_first_closes() {
    let (state, port) = start().await;
    let mut ws = connect_nonce(port).await;
    let ch = next_json(&mut ws).await;
    // Carry a valid proof, so only the frame type can cause the close.
    let proof = proof(
        state.token(),
        LABEL_WS_PLUGIN,
        ch["nonce"].as_str().unwrap(),
    );
    let info =
        serde_json::json!({"type": "FILE_INFO", "proof": proof, "fileKey": "fk", "name": "Doc"});
    ws.send(Tt::Text(info.to_string())).await.unwrap();
    assert!(closes(&mut ws).await);
    assert!(state.list_connections().is_empty());
}

#[tokio::test]
async fn a_binary_frame_first_closes() {
    let (_state, port) = start().await;
    let mut ws = connect_nonce(port).await;
    let _ = next_json(&mut ws).await;
    ws.send(Tt::Binary(vec![1, 2, 3])).await.unwrap();
    assert!(closes(&mut ws).await);
}

#[tokio::test]
async fn nothing_is_registered_before_auth() {
    let (state, port) = start().await;
    let mut ws = connect_nonce(port).await;
    let _ = next_json(&mut ws).await;
    assert!(state.list_connections().is_empty());
}

#[tokio::test]
async fn invalid_or_missing_nonce_gets_401() {
    let (_state, port) = start().await;
    for q in ["nonce=zz", "nonce=abcd", "", "nonce="] {
        let err = connect_async(format!("ws://127.0.0.1:{port}/?{q}"))
            .await
            .expect_err("must be refused");
        match err {
            tokio_tungstenite::tungstenite::Error::Http(r) => assert_eq!(r.status(), 401, "{q}"),
            e => panic!("unexpected error {e:?}"),
        }
    }
}

/// A 0.1.0 plugin connects with `?token=`. A valid token gets one WELCOME
/// frame (so the old plugin can show its version-mismatch banner) and
/// nothing else: no job, no status, no file info, and no registered
/// connection for routing to find.
#[tokio::test]
async fn legacy_token_with_the_correct_value_gets_only_a_welcome() {
    let (state, port) = start().await;
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{port}/?token={}", state.token()))
        .await
        .expect("a valid legacy token is accepted");
    let welcome = next_json(&mut ws).await;
    assert_eq!(welcome["type"], "WELCOME");
    assert_eq!(welcome["version"], env!("CARGO_PKG_VERSION"));
    let keys: Vec<&String> = welcome.as_object().unwrap().keys().collect();
    assert_eq!(
        keys.len(),
        2,
        "WELCOME carries type and version only: {welcome}"
    );

    // The old plugin identifies its file. The daemon must ignore it.
    ws.send(Tt::Text(
        r#"{"type":"FILE_INFO","fileKey":"abc","name":"Old file","pluginVersion":"0.1.0"}"#.into(),
    ))
    .await
    .unwrap();
    // No reply of any kind, and nothing registered for routing or status.
    let quiet = tokio::time::timeout(std::time::Duration::from_millis(400), ws.next()).await;
    assert!(quiet.is_err(), "no frame may follow the WELCOME: {quiet:?}");
    assert!(state.list_connections().is_empty());
    // A job for that file finds no plugin at all.
    let result = turbofig::run_execute(&state, None, Some("abc"), "return 1").await;
    assert_eq!(
        result["ok"], false,
        "no job may reach a compat socket: {result}"
    );
    let quiet = tokio::time::timeout(std::time::Duration::from_millis(200), ws.next()).await;
    assert!(quiet.is_err(), "no job frame may follow: {quiet:?}");
}

/// A missing token and a wrong token both stay refused.
#[tokio::test]
async fn legacy_token_missing_or_wrong_gets_401() {
    let (_state, port) = start().await;
    for q in ["token=", "token=short", "token=wrong"] {
        let err = connect_async(format!("ws://127.0.0.1:{port}/?{q}"))
            .await
            .expect_err("must be refused");
        match err {
            tokio_tungstenite::tungstenite::Error::Http(r) => assert_eq!(r.status(), 401, "{q}"),
            e => panic!("unexpected error {e:?}"),
        }
    }
}

#[tokio::test]
async fn legacy_token_with_a_wrong_value_gets_401() {
    let (_state, port) = start().await;
    let err = connect_async(format!(
        "ws://127.0.0.1:{port}/?token=wrong&nonce={NONCE_P}"
    ))
    .await
    .expect_err("must be refused");
    match err {
        tokio_tungstenite::tungstenite::Error::Http(r) => assert_eq!(r.status(), 401),
        e => panic!("unexpected error {e:?}"),
    }
}

async fn status_of(port: u16, edit: impl FnOnce(&mut http::Request<()>)) -> u16 {
    let mut req = format!("ws://127.0.0.1:{port}/?nonce={NONCE_P}")
        .into_client_request()
        .unwrap();
    edit(&mut req);
    match connect_async(req).await {
        Ok((_, r)) => r.status().as_u16(),
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => r.status().as_u16(),
        Err(e) => panic!("unexpected error {e:?}"),
    }
}

#[tokio::test]
async fn host_allow_list_applies_to_the_upgrade() {
    let (_state, port) = start().await;
    let bad = status_of(port, |r| {
        r.headers_mut()
            .insert("host", "attacker.example".parse().unwrap());
    })
    .await;
    assert_eq!(bad, 403);
    assert_eq!(status_of(port, |_| {}).await, 101);
}

#[tokio::test]
async fn two_origin_headers_are_refused() {
    let (_state, port) = start().await;
    let two = status_of(port, |r| {
        r.headers_mut().append("origin", "null".parse().unwrap());
        r.headers_mut()
            .append("origin", "https://evil.example".parse().unwrap());
    })
    .await;
    assert_eq!(two, 403);
    let one = status_of(port, |r| {
        r.headers_mut().append("origin", "null".parse().unwrap());
    })
    .await;
    assert_eq!(one, 101);
}

#[tokio::test]
async fn a_long_file_key_is_not_registered() {
    let (state, port) = start().await;
    let mut ws = common::connect_plugin(port, state.token()).await;
    let long = "k".repeat(257);
    let info = serde_json::json!({"type": "FILE_INFO", "fileKey": long, "name": "Doc"});
    ws.send(Tt::Text(info.to_string())).await.unwrap();
    let ok = serde_json::json!({"type": "FILE_INFO", "fileKey": "k".repeat(256), "name": "Ok"});
    ws.send(Tt::Text(ok.to_string())).await.unwrap();
    wait_until(
        || state.plugin_snapshot().is_some_and(|(k, _)| k.len() == 256),
        common::WAIT_DEADLINE_MS,
        "the 256 byte key to register",
    )
    .await;
}

#[tokio::test]
async fn a_full_pending_set_does_not_keep_the_real_plugin_out() {
    let (state, port) = start().await;
    let mut stale = Vec::new();
    for _ in 0..8 {
        let mut ws = connect_nonce(port).await;
        let _ = next_json(&mut ws).await;
        stale.push(ws);
    }
    let mut real = connect_nonce(port).await;
    authenticate(&mut real, state.token()).await;
    let info = serde_json::json!({"type": "FILE_INFO", "fileKey": "fk", "name": "Doc"});
    real.send(Tt::Text(info.to_string())).await.unwrap();
    assert_eq!(next_json(&mut real).await["type"], "WELCOME");
    assert!(closes(&mut stale[0]).await);
}
