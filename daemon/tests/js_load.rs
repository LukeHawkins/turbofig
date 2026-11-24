//! Load tests for `turbofig js`: a real daemon, a mock plugin that answers
//! EXECUTE and SCREENSHOT like the real plugin, and many real `turbofig js`
//! children. Each call carries a unique marker; the test proves each call
//! got its own answer and that the bridge folders are clean afterwards.
//!
//! Default tests run in CI. The heavy variant is `#[ignore]`:
//! `cargo test -p turbofig --test js_load -- --ignored`.

mod common;

use base64::Engine as _;
use common::{connect_plugin, free_port, wait_for_health, DaemonGuard, BIN};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use std::time::Instant;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio_tungstenite::tungstenite::Message as Tt;

/// A valid 1x1 PNG.
const TINY_PNG_B64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// Aborts the mock plugin task on drop, so its socket closes before the
/// daemon guard stops the daemon (a connected plugin slows the drain).
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Rig {
    home: tempfile::TempDir,
    mcp_port: u16,
    ws_port: u16,
    _plugin: AbortOnDrop,
    _guard: DaemonGuard,
}

impl Rig {
    /// Closes the mock plugin socket, then stops the daemon (drop order).
    async fn close(mut self) {
        self._plugin.0.abort();
        let _ = (&mut self._plugin.0).await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        common::stop_daemon(&client, self.mcp_port, self.home.path()).await;
    }
}

async fn run_js(
    rig: &Rig,
    args: &[&str],
    stdin: &str,
) -> (std::process::ExitStatus, String, String) {
    let mut child = Command::new(BIN)
        .arg("js")
        .args(args)
        .env("HOME", rig.home.path())
        .env("TURBOFIG_MCP_PORT", rig.mcp_port.to_string())
        .env("TURBOFIG_WS_PORT", rig.ws_port.to_string())
        .env("TURBOFIG_BRIDGE_DIR", rig.home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn turbofig js");
    let mut si = child.stdin.take().unwrap();
    si.write_all(stdin.as_bytes()).await.unwrap();
    drop(si);
    let out = child.wait_with_output().await.unwrap();
    (
        out.status,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The marker inside `return "<marker>"` (between the first and last quote).
fn marker_of(code: &str) -> String {
    let a = code.find('"').expect("code holds a quote") + 1;
    let b = code.rfind('"').expect("code holds a closing quote");
    code[a..b].to_owned()
}

/// Starts a daemon on temp HOME and free ports, then connects a mock plugin
/// that answers like the real one:
///   EXECUTE    -> `{"ok":true,"result":{"marker":<marker>,"id":"12:34"}}`
///   SCREENSHOT -> `{"ok":true,"png":<base64 PNG>,"w":1,"h":1}`
async fn rig(max_inflight: u32) -> Rig {
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    // `TURBOFIG_MAX_INFLIGHT` is read by the daemon `turbofig js` starts, so
    // start it with the highest admission limit: the load tests run up to 50
    // calls at once. The default limit (4) is tested in its own test.
    let mut child = std::process::Command::new(BIN);
    child
        .arg("js")
        .env("HOME", home.path())
        .env("TURBOFIG_MCP_PORT", mcp_port.to_string())
        .env("TURBOFIG_WS_PORT", ws_port.to_string())
        .env("TURBOFIG_BRIDGE_DIR", home.path())
        .env("TURBOFIG_MAX_INFLIGHT", max_inflight.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = child.spawn().expect("spawn turbofig js");
    {
        use std::io::Write;
        let mut si = child.stdin.take().unwrap();
        si.write_all(b"{\"op\":\"status\"}").unwrap();
    }
    assert!(child.wait().unwrap().success(), "daemon start via js");
    let guard = DaemonGuard::new(home.path(), mcp_port);
    wait_for_health(&client, mcp_port).await;
    let mut guard = guard;
    guard.refresh_pid().await;
    let token = std::fs::read_to_string(home.path().join("token")).expect("token");
    let mut ws = connect_plugin(ws_port, token.trim()).await;
    ws.send(Tt::Text(
        json!({"type": "FILE_INFO", "fileKey": "LoadFile", "name": "Load"}).to_string(),
    ))
    .await
    .expect("send FILE_INFO");
    let plugin = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws.next().await {
            let text = match msg {
                Tt::Text(text) => text,
                // Answer the keepalive ping at once (a read-only loop never flushes
                // the queued pong, and the daemon drops a connection with no pong).
                Tt::Ping(p) => {
                    let _ = ws.send(Tt::Pong(p)).await;
                    continue;
                }
                _ => continue,
            };
            let Ok(frame) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            let Some(id) = frame.get("requestId").and_then(Value::as_u64) else {
                continue;
            };
            let reply = match frame.get("type").and_then(Value::as_str) {
                Some("EXECUTE") => {
                    let code = frame["code"].as_str().unwrap_or("");
                    json!({"type": "RESULT", "requestId": id, "ok": true,
                           "result": {"marker": marker_of(code), "id": "12:34"}})
                }
                Some("SCREENSHOT") => json!({"type": "RESULT", "requestId": id, "ok": true,
                           "png": TINY_PNG_B64, "w": 1, "h": 1}),
                _ => continue,
            };
            if ws.send(Tt::Text(reply.to_string())).await.is_err() {
                break;
            }
        }
    });
    let rig = Rig {
        home,
        mcp_port,
        ws_port,
        _plugin: AbortOnDrop(plugin),
        _guard: guard,
    };
    // Wait until the plugin is routable (FILE_INFO processed).
    let t = Instant::now();
    loop {
        let (st, out, _) = run_js(&rig, &[], "return \"warmup\";").await;
        if st.success() && out.contains("\"warmup\"") {
            break;
        }
        assert!(
            t.elapsed().as_secs() < 15,
            "plugin never became routable: {out}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    rig
}

fn assert_marker_reply(out: &str, marker: &str) {
    let v: Value = serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("{e}: {out}"));
    assert_eq!(v["ok"], json!(true), "{out}");
    assert_eq!(v["result"]["marker"], json!(marker), "wrong answer: {out}");
    assert_eq!(out.trim().lines().count(), 1, "exactly one line: {out}");
}

/// `inbox` and `outbox` entries, ignoring dotfiles and `.png` result files.
fn leftovers(home: &Path) -> Vec<String> {
    ["inbox", "outbox"]
        .iter()
        .flat_map(|d| std::fs::read_dir(home.join(d)).unwrap().flatten())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.') && !n.ends_with(".png"))
        .collect()
}

async fn sequential(n: usize) {
    let _serial = common::serial_process_test().await;
    let rig = rig(32).await;
    let t = Instant::now();
    for i in 0..n {
        let marker = format!("seq-{i}-{:08x}", i.wrapping_mul(2654435761));
        let (st, out, err) = run_js(&rig, &[], &format!("return \"{marker}\";")).await;
        assert!(st.success(), "call {i} failed: {out} {err}");
        assert_marker_reply(&out, &marker);
    }
    println!("LOAD {n} sequential calls ok in {:?}", t.elapsed());
    assert!(
        leftovers(rig.home.path()).is_empty(),
        "{:?}",
        leftovers(rig.home.path())
    );
    rig.close().await;
}

async fn parallel(n: usize) {
    let _serial = common::serial_process_test().await;
    let rig = std::sync::Arc::new(rig(32).await);
    let t = Instant::now();
    let mut tasks = Vec::new();
    for i in 0..n {
        let rig = rig.clone();
        tasks.push(tokio::spawn(async move {
            let marker = format!("par-{i}-{:08x}", i.wrapping_mul(40503));
            let (st, out, err) = run_js(&rig, &[], &format!("return \"{marker}\";")).await;
            assert!(st.success(), "call {i} failed: {out} {err}");
            assert_marker_reply(&out, &marker);
        }));
    }
    for t in tasks {
        t.await.expect("parallel call");
    }
    println!("LOAD {n} parallel calls ok in {:?}", t.elapsed());
    let rig = std::sync::Arc::try_unwrap(rig)
        .ok()
        .expect("all calls finished");
    assert!(
        leftovers(rig.home.path()).is_empty(),
        "{:?}",
        leftovers(rig.home.path())
    );
    rig.close().await;
}

#[tokio::test]
async fn load_100_sequential_calls_each_get_their_own_answer() {
    sequential(100).await;
}

#[tokio::test]
async fn load_20_parallel_calls_each_get_their_own_answer() {
    parallel(20).await;
}

#[tokio::test]
async fn shot_end_to_end_writes_a_real_png() {
    let _serial = common::serial_process_test().await;
    let rig = rig(32).await;
    let (st, out, err) = run_js(&rig, &["--shot"], "return \"shot-marker\";").await;
    assert!(st.success(), "{out} {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "two lines: {out}");
    let first: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first["result"]["id"], json!("12:34"), "{out}");
    assert_eq!(first["result"]["marker"], json!("shot-marker"), "{out}");
    let second: Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(second["ok"], json!(true), "{out}");
    let path = second["path"].as_str().expect("path in line 2");
    let bytes = std::fs::read(path).expect("PNG file exists");
    assert!(bytes.starts_with(&PNG_SIGNATURE), "PNG signature");
    assert!(base64::engine::general_purpose::STANDARD
        .decode(TINY_PNG_B64)
        .is_ok());
    assert!(
        leftovers(rig.home.path()).is_empty(),
        "{:?}",
        leftovers(rig.home.path())
    );
    rig.close().await;
}

#[tokio::test]
#[ignore = "heavy: 500 sequential and 50 parallel calls"]
async fn heavy_500_sequential_calls() {
    sequential(500).await;
}

#[tokio::test]
#[ignore = "heavy: 500 sequential and 50 parallel calls"]
async fn heavy_50_parallel_calls() {
    parallel(50).await;
}

/// With the default admission limit (4), a burst of 20 never loses or mixes an
/// answer: each call exits 0 and prints its own marker or a `busy` result.
#[tokio::test]
async fn burst_at_the_default_limit_never_gives_a_wrong_answer() {
    let _serial = common::serial_process_test().await;
    let rig = std::sync::Arc::new(rig(4).await);
    let mut tasks = Vec::new();
    for i in 0..20 {
        let rig = rig.clone();
        tasks.push(tokio::spawn(async move {
            let marker = format!("burst-{i}");
            let (st, out, err) = run_js(&rig, &[], &format!("return \"{marker}\";")).await;
            assert!(st.success(), "call {i}: {out} {err}");
            let v: Value = serde_json::from_str(out.trim()).expect("one JSON line");
            if v["ok"] == json!(true) {
                assert_eq!(v["result"]["marker"], json!(marker), "wrong answer: {out}");
                true
            } else {
                assert_eq!(v["code"], json!("busy"), "{out}");
                false
            }
        }));
    }
    let mut ok = 0;
    for t in tasks {
        ok += usize::from(t.await.expect("burst call"));
    }
    println!(
        "LOAD burst of 20 at limit 4: {ok} answered, {} busy",
        20 - ok
    );
    assert!(ok >= 1);
    let rig = std::sync::Arc::try_unwrap(rig)
        .ok()
        .expect("all calls finished");
    rig.close().await;
}
