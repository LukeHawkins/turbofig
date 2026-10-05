//! Integration tests for the bare `turbofig` command (no subcommand): a
//! real first run (daemon not yet running, no `plugin-seen` marker) and a
//! real second run (daemon already running, `plugin-seen` marker faked in).
//!
//! Both runs use `TURBOFIG_TEST_FAKE_CLIPBOARD`/`TURBOFIG_TEST_FAKE_OPENER`
//! (debug-build-only escape hatches; see `main.rs`'s `clipboard_for_run`/
//! `opener_for_run`) so neither one ever shells out to the real `pbcopy` or
//! `open -a Figma`. Real process, test ports, a temp home; the daemon
//! started here is stopped at the end via an authenticated `POST /control
//! stop`.

mod common;

use common::{assert_safe_turbofig_spawn, free_port, stop_daemon, wait_for_health, BIN};
use std::time::Duration;
use tokio::process::Command;

/// Runs `turbofig` with no subcommand, pointed at `mcp_port`/`ws_port`/`home`,
/// faking the clipboard (recording the copied text to `clipboard_record`) and
/// the Figma-open result (`opener_succeeds`). Returns the exit status and
/// captured stdout.
///
/// This is the one place in the suite allowed to spawn a bare `turbofig`:
/// `assert_safe_turbofig_spawn` below only lets that through because both
/// fakes are always set here, never because the check was skipped.
async fn run_bare_turbofig(
    home: &std::path::Path,
    mcp_port: u16,
    ws_port: u16,
    clipboard_record: &std::path::Path,
    opener_succeeds: bool,
) -> (std::process::ExitStatus, String) {
    let opener_value = if opener_succeeds { "success" } else { "fail" };
    let clipboard_record_str = clipboard_record.display().to_string();
    assert_safe_turbofig_spawn(
        &[],
        &[
            (
                "TURBOFIG_TEST_FAKE_CLIPBOARD",
                clipboard_record_str.as_str(),
            ),
            ("TURBOFIG_TEST_FAKE_OPENER", opener_value),
        ],
    );
    let output = Command::new(BIN)
        .env("TURBOFIG_MCP_PORT", mcp_port.to_string())
        .env("TURBOFIG_WS_PORT", ws_port.to_string())
        .env("TURBOFIG_BRIDGE_DIR", home)
        .env("TURBOFIG_TEST_FAKE_CLIPBOARD", clipboard_record)
        .env("TURBOFIG_TEST_FAKE_OPENER", opener_value)
        .output()
        .await
        .expect("run bare turbofig");
    (
        output.status,
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

#[tokio::test]
async fn bare_turbofig_first_run_starts_the_daemon_and_prints_the_walkthrough() {
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let clipboard_record = home.path().join("clipboard-record.txt");
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    assert!(
        !home.path().join("plugin-seen").exists(),
        "a fresh temp home must start with no plugin-seen marker"
    );

    let (status, stdout) =
        run_bare_turbofig(home.path(), mcp_port, ws_port, &clipboard_record, true).await;

    assert!(status.success(), "a first run must exit 0: {stdout}");
    wait_for_health(&client, mcp_port).await;

    let manifest_path = home.path().join("figma-plugin/manifest.json");
    assert!(
        stdout.contains(&format!(
            "turbofig {} is running (MCP 127.0.0.1:{mcp_port}, plugin 127.0.0.1:{ws_port}).",
            env!("CARGO_PKG_VERSION")
        )),
        "stdout must report the running version and both ports: {stdout}"
    );
    assert!(
        stdout.contains("1. Add the Figma plugin (once). Figma is opening now."),
        "the opener succeeded, so stdout must say Figma is opening now: {stdout}"
    );
    assert!(
        stdout.contains(&manifest_path.display().to_string()),
        "stdout must name the real manifest path: {stdout}"
    );
    assert!(
        stdout.contains("2. Connect your agent (once):"),
        "stdout must carry step 2: {stdout}"
    );
    assert!(
        stdout.contains("claude mcp add turbofig -- turbofig mcp"),
        "stdout must carry the Claude Code connect command: {stdout}"
    );
    assert!(
        stdout.contains("3. MCP blocked on your machine?"),
        "stdout must carry step 3: {stdout}"
    );

    let recorded = tokio::fs::read_to_string(&clipboard_record)
        .await
        .expect("the fake clipboard must have recorded the copied text");
    assert_eq!(
        recorded,
        manifest_path.display().to_string(),
        "the fake clipboard must have recorded the manifest path"
    );

    assert!(
        !home.path().join("plugin-seen").exists(),
        "the bare command itself never writes plugin-seen: only a real plugin connect does"
    );

    stop_daemon(&client, mcp_port, home.path()).await;
}

#[tokio::test]
async fn bare_turbofig_second_run_with_an_existing_marker_prints_a_short_status() {
    let home = tempfile::tempdir().expect("temp home");
    let mcp_port = free_port();
    let ws_port = free_port();
    let clipboard_record = home.path().join("clipboard-record.txt");
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    // First run: start the daemon for real.
    let (status, _) =
        run_bare_turbofig(home.path(), mcp_port, ws_port, &clipboard_record, true).await;
    assert!(status.success());
    wait_for_health(&client, mcp_port).await;

    // Fake the marker a real plugin connect would have written.
    tokio::fs::write(home.path().join("plugin-seen"), "1700000000\n")
        .await
        .expect("fake the plugin-seen marker");

    let (status, stdout) =
        run_bare_turbofig(home.path(), mcp_port, ws_port, &clipboard_record, true).await;
    assert!(status.success(), "a second run must exit 0: {stdout}");

    let manifest_path = home.path().join("figma-plugin/manifest.json");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        3,
        "a later run's status must be exactly 3 lines: {stdout:?}"
    );
    assert!(lines[0].contains("is running"));
    assert_eq!(
        lines[1], "Connected files: none, open the turbofig plugin in Figma",
        "no plugin connected in this test, so the hint line must show"
    );
    assert_eq!(
        lines[2],
        format!("Plugin manifest: {}", manifest_path.display())
    );
    assert!(
        !stdout.contains("Add the Figma plugin"),
        "a later run must never repeat the first-run walkthrough: {stdout}"
    );

    stop_daemon(&client, mcp_port, home.path()).await;
    // Give the stop request a moment to land before the temp dir drops.
    tokio::time::sleep(Duration::from_millis(50)).await;
}
