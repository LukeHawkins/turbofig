//! `turbofig js --guide`: a real daemon, mock plugins that never answer a
//! request, and the real binary. The guide must list the connected files
//! from the daemon's registry (no plugin round trip), print only the guide on
//! stdout, and exit 0 with 0 files.

mod common;

use common::{connect_plugin, free_port, spawn_daemon, wait_for_health, DaemonGuard, BIN};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::Command;
use tokio_tungstenite::tungstenite::Message as Tt;

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// `turbofig js [args]` against the test daemon. Stdin is closed: `--guide`
/// must not read it.
async fn run_guide(
    home: &std::path::Path,
    mcp_port: u16,
    ws_port: u16,
    args: &[&str],
) -> (std::process::ExitStatus, String, String) {
    let out = Command::new(BIN)
        .arg("js")
        .args(args)
        .env("HOME", home)
        .env("TURBOFIG_MCP_PORT", mcp_port.to_string())
        .env("TURBOFIG_WS_PORT", ws_port.to_string())
        .env("TURBOFIG_BRIDGE_DIR", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("run turbofig js --guide");
    (
        out.status,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A plugin that connects, announces one file and then answers keepalive
/// pings only: every request frame is ignored.
async fn silent_plugin(ws_port: u16, token: &str, key: &str, name: &str) -> AbortOnDrop {
    let mut ws = connect_plugin(ws_port, token).await;
    ws.send(Tt::Text(
        json!({"type": "FILE_INFO", "fileKey": key, "name": name}).to_string(),
    ))
    .await
    .expect("send FILE_INFO");
    AbortOnDrop(tokio::spawn(async move {
        while let Some(Ok(msg)) = ws.next().await {
            if let Tt::Ping(p) = msg {
                let _ = ws.send(Tt::Pong(p)).await;
            }
        }
    }))
}

#[tokio::test]
async fn guide_lists_0_then_2_files_fast_and_writes_guide_md() {
    let _serial = common::serial_process_test().await;
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let guard = DaemonGuard::new(home.path(), mcp_port);
    let _daemon = spawn_daemon(home.path(), mcp_port, ws_port, None);
    wait_for_health(&client, mcp_port).await;
    let mut guard = guard;
    guard.refresh_pid().await;
    let token = std::fs::read_to_string(home.path().join("token")).expect("token");

    // 0 files: exit 0, only the guide on stdout, nothing on stderr.
    let (st, out, err) = run_guide(home.path(), mcp_port, ws_port, &["--guide"]).await;
    assert!(st.success(), "{err}");
    assert_eq!(err, "");
    assert!(out.starts_with(&format!("turbofig {} guide\n", env!("CARGO_PKG_VERSION"))));
    assert!(out.contains("\nfiles: none\n"), "{out}");
    assert!(out.contains("ask user: Figma > Plugins > Development > turbofig"));
    assert!(out.contains(&format!("read {}/tf-api.md", home.path().display())));
    assert!(!out.contains("{{"));

    // 2 files whose plugins never answer a request.
    let _p1 = silent_plugin(ws_port, token.trim(), "AbC123", "Pricing").await;
    let _p2 = silent_plugin(ws_port, token.trim(), "XyZ789", "Deck").await;
    let t = Instant::now();
    let listing = loop {
        let (st, out, err) = run_guide(home.path(), mcp_port, ws_port, &["--guide"]).await;
        assert!(st.success(), "{err}");
        if out.contains("key=AbC123") && out.contains("key=XyZ789") {
            break out;
        }
        assert!(
            t.elapsed() < Duration::from_secs(10),
            "files never listed: {out}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(
        listing.contains("\nfiles: \"Pricing\" key=AbC123 | \"Deck\" key=XyZ789\n")
            || listing.contains("\nfiles: \"Deck\" key=XyZ789 | \"Pricing\" key=AbC123\n"),
        "{listing}"
    );

    // Never waits on a plugin round trip: far below the request timeout.
    let t = Instant::now();
    let (st, _, _) = run_guide(home.path(), mcp_port, ws_port, &["--guide"]).await;
    assert!(st.success());
    assert!(
        t.elapsed() < Duration::from_secs(2),
        "guide took {:?}",
        t.elapsed()
    );

    // guide.md: written by the daemon, owner-only, with this folder inside.
    let md_path = home.path().join("guide.md");
    let md = std::fs::read_to_string(&md_path).expect("guide.md");
    assert!(md.starts_with(&format!(
        "turbofig {} guide (file tools)\n",
        env!("CARGO_PKG_VERSION")
    )));
    assert!(md.contains(&format!("{}/inbox/<unique-id>.json", home.path().display())));
    assert_eq!(
        std::fs::metadata(&md_path).unwrap().permissions().mode() & 0o777,
        0o600
    );

    // --guide with --shot or --file is a usage error.
    let (st, _, err) = run_guide(home.path(), mcp_port, ws_port, &["--guide", "--shot"]).await;
    assert_eq!(st.code(), Some(2), "{err}");

    drop(_p1);
    drop(_p2);
    common::stop_daemon(&client, mcp_port, home.path()).await;
    drop(guard);
}
