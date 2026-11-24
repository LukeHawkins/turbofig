//! The shared "Connection" setting (File bridge or MCP) end to end: a real
//! daemon in a temp home, mock plugins over the WebSocket, the stdio proxy
//! and raw HTTP. Covers the `/mcp` gate, the stdio proxy in File bridge mode,
//! WELCOME, `SET_CONNECTION_MODE`, the broadcast and the `/control` path.

mod common;

use common::{
    connect_plugin, free_port, handshake, spawn_daemon, spawn_proxy, stdio_call_tool,
    tool_call_status, wait_for_health, PluginWs,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message as Tt;

struct Rig {
    home: tempfile::TempDir,
    mcp_port: u16,
    ws_port: u16,
    token: String,
    client: reqwest::Client,
    _daemon: tokio::process::Child,
}

async fn start_rig(seed_legacy_marker: bool) -> Rig {
    let home = tempfile::tempdir().expect("temp home");
    if seed_legacy_marker {
        std::fs::write(home.path().join("mcp-http-off"), "off\n").expect("legacy marker");
    }
    let mcp_port = free_port();
    let ws_port = free_port();
    let daemon = spawn_daemon(home.path(), mcp_port, ws_port, None);
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    wait_for_health(&client, mcp_port).await;
    let token = std::fs::read_to_string(home.path().join("token"))
        .expect("token")
        .trim()
        .to_owned();
    Rig {
        home,
        mcp_port,
        ws_port,
        token,
        client,
        _daemon: daemon,
    }
}

impl Rig {
    async fn post_mcp(&self) -> reqwest::Response {
        self.client
            .post(format!("http://127.0.0.1:{}/mcp", self.mcp_port))
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .bearer_auth(&self.token)
            .json(&json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {
                    "protocolVersion": "2025-03-26",
                    "clientInfo": {"name": "t", "version": "0"},
                    "capabilities": {}
                }
            }))
            .send()
            .await
            .expect("POST /mcp")
    }

    async fn control(&self, body: Value) -> reqwest::Response {
        self.client
            .post(format!("http://127.0.0.1:{}/control", self.mcp_port))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .expect("POST /control")
    }

    fn marker(&self) -> bool {
        self.home.path().join("mcp-mode").exists()
    }

    /// A verified plugin that has sent FILE_INFO and read WELCOME.
    async fn plugin(&self, key: &str) -> (PluginWs, Value) {
        let mut ws = connect_plugin(self.ws_port, &self.token).await;
        ws.send(Tt::Text(
            json!({"type": "FILE_INFO", "fileKey": key, "name": key}).to_string(),
        ))
        .await
        .expect("FILE_INFO");
        let welcome = next_of_type(&mut ws, "WELCOME").await;
        (ws, welcome)
    }
}

async fn next_of_type(ws: &mut PluginWs, ty: &str) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let frame = tokio::time::timeout_at(deadline, ws.next())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {ty}"))
            .expect("socket open")
            .expect("frame ok");
        if let Tt::Text(t) = frame {
            let v: Value = serde_json::from_str(&t).expect("json");
            if v["type"] == ty {
                return v;
            }
        }
    }
}

/// True when no frame of type `ty` arrives within 500 ms.
async fn none_of_type(ws: &mut PluginWs, ty: &str) -> bool {
    let end = tokio::time::Instant::now() + Duration::from_millis(500);
    loop {
        match tokio::time::timeout_at(end, ws.next()).await {
            Err(_) => return true,
            Ok(Some(Ok(Tt::Text(t)))) => {
                let v: Value = serde_json::from_str(&t).expect("json");
                if v["type"] == ty {
                    return false;
                }
            }
            Ok(Some(Ok(_))) => {}
            Ok(_) => return true,
        }
    }
}

async fn set_mode(ws: &mut PluginWs, mode: Value) {
    ws.send(Tt::Text(
        json!({"type": "SET_CONNECTION_MODE", "mode": mode}).to_string(),
    ))
    .await
    .expect("send SET_CONNECTION_MODE");
}

#[tokio::test]
async fn http_mcp_is_gated_in_file_mode_and_a_legacy_marker_is_deleted() {
    let _serial = common::serial_process_test().await;
    let rig = start_rig(true).await;
    assert!(
        !rig.home.path().join("mcp-http-off").exists(),
        "the legacy marker must be deleted at startup"
    );
    let resp = rig.post_mcp().await;
    assert_eq!(resp.status(), 404);
    let v: Value = resp.json().await.expect("json");
    assert_eq!(v["ok"], false);
    assert_eq!(v["code"], "mcp_http_off");
    assert_eq!(
        v["error"],
        "turbofig is in File bridge mode. Switch Connection to MCP in the turbofig plugin (gear) or in Settings."
    );
    let resp = rig
        .control(json!({"action": "set_connection_mode", "mode": "mcp"}))
        .await;
    assert_eq!(resp.status(), 200);
    assert!(rig.marker());
    assert_eq!(rig.post_mcp().await.status(), 200);
}

#[tokio::test]
async fn the_stdio_proxy_works_in_file_mode() {
    let _serial = common::serial_process_test().await;
    let rig = start_rig(false).await;
    assert!(!rig.marker());
    let mut proxy = spawn_proxy(rig.home.path(), rig.mcp_port, rig.ws_port, false, None);
    let (mut writer, mut reader) = handshake(&mut proxy).await;
    let resp = stdio_call_tool(&mut writer, &mut reader, 2, "turbofig_status", json!({})).await;
    assert_eq!(tool_call_status(&resp)["ok"], json!(true));
    assert!(!rig.marker(), "the proxy must not change the mode");
}

#[tokio::test]
async fn welcome_carries_the_mode_and_set_connection_mode_broadcasts_to_every_plugin() {
    let _serial = common::serial_process_test().await;
    let rig = start_rig(false).await;
    let (mut a, welcome_a) = rig.plugin("fk-a").await;
    let (mut b, _) = rig.plugin("fk-b").await;
    assert_eq!(welcome_a["connectionMode"], "file");

    set_mode(&mut a, json!("mcp")).await;
    for ws in [&mut a, &mut b] {
        let m = next_of_type(ws, "CONNECTION_MODE").await;
        assert_eq!(m["mode"], "mcp");
    }
    assert!(rig.marker());
    assert_eq!(rig.post_mcp().await.status(), 200);

    let (_c, welcome_c) = rig.plugin("fk-c").await;
    assert_eq!(welcome_c["connectionMode"], "mcp");

    // The app path: /control also broadcasts.
    let resp = rig
        .control(json!({"action": "set_connection_mode", "mode": "file"}))
        .await;
    assert_eq!(resp.status(), 200);
    for ws in [&mut a, &mut b] {
        let m = next_of_type(ws, "CONNECTION_MODE").await;
        assert_eq!(m["mode"], "file");
    }
    assert!(!rig.marker());
    assert_eq!(rig.post_mcp().await.status(), 404);
}

#[tokio::test]
async fn an_invalid_mode_is_ignored() {
    let _serial = common::serial_process_test().await;
    let rig = start_rig(false).await;
    let (mut a, _) = rig.plugin("fk-a").await;
    for bad in [json!("http"), json!("MCP"), json!(1), json!(null)] {
        set_mode(&mut a, bad).await;
    }
    assert!(none_of_type(&mut a, "CONNECTION_MODE").await);
    assert!(!rig.marker());
    let resp = rig
        .control(json!({"action": "set_connection_mode", "mode": "http"}))
        .await;
    assert_eq!(resp.status(), 400);
    assert!(!rig.marker());
}

#[tokio::test]
async fn an_unverified_socket_cannot_set_the_mode() {
    let _serial = common::serial_process_test().await;
    let rig = start_rig(false).await;
    let nonce = "00112233445566778899aabbccddeeff";
    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}/?nonce={nonce}", rig.ws_port))
            .await
            .expect("connect");
    let first = ws.next().await.expect("CHALLENGE").expect("ok");
    assert!(matches!(first, Tt::Text(_)));
    // No AUTH: send the setter straight away.
    let _ = ws
        .send(Tt::Text(
            json!({"type": "SET_CONNECTION_MODE", "mode": "mcp"}).to_string(),
        ))
        .await;
    // A verified plugin never sees a broadcast.
    let (mut a, _) = rig.plugin("fk-a").await;
    assert!(none_of_type(&mut a, "CONNECTION_MODE").await);
    assert!(!rig.marker());
    // A wrong control token cannot set it either.
    let resp = rig
        .client
        .post(format!("http://127.0.0.1:{}/control", rig.mcp_port))
        .bearer_auth("wrong")
        .json(&json!({"action": "set_connection_mode", "mode": "mcp"}))
        .send()
        .await
        .expect("POST /control");
    assert_eq!(resp.status(), 401);
    assert!(!rig.marker());
}
