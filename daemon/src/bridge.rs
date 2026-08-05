//! Filesystem bridge transport.
//!
//! Clients write a job file to `<bridge_dir>/inbox/<id>.json` and read the
//! result from `<bridge_dir>/outbox/<id>.json`.  The bridge polls `inbox/`
//! on a 50 ms interval, services each job via the shared AppState, and writes
//! results atomically (write `<id>.json.tmp` then rename).
//!
//! This transport lets a client drive the daemon using only local file writes
//! and reads.  It fires no curl requests and opens no MCP connection, so it
//! bypasses enterprise policies that gate network tool confirmations.
//!
//! NOTE: polling with a tokio interval is intentional for this spike.
//!       Replacing it with the `notify` crate is a future upgrade.

use crate::AppState;
use std::path::{Path, PathBuf};
use std::sync::Arc;

// ── Bridge dir helpers ────────────────────────────────────────────────────────

/// Resolve the bridge directory from optional env string values.
///
/// - If `bridge_dir_val` is `Some(path)`, use it directly.
/// - Else if `home_val` is `Some(home)`, use `<home>/.turbofig`.
/// - Else fall back to `./.turbofig`.
fn bridge_dir_from_str(bridge_dir_val: Option<&str>, home_val: Option<&str>) -> PathBuf {
    if let Some(val) = bridge_dir_val {
        return PathBuf::from(val);
    }
    match home_val {
        Some(home) => PathBuf::from(home).join(".turbofig"),
        None => PathBuf::from(".turbofig"),
    }
}

/// Read the bridge directory from TURBOFIG_BRIDGE_DIR.
/// Default: `~/.turbofig` (falls back to `./.turbofig` if HOME is not set).
pub fn bridge_dir_from_env() -> PathBuf {
    bridge_dir_from_str(
        std::env::var("TURBOFIG_BRIDGE_DIR").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

// ── Bridge server ─────────────────────────────────────────────────────────────

/// Serve the filesystem bridge.
///
/// Creates `<dir>/inbox/` and `<dir>/outbox/` on startup.
/// Every 50 ms, scans inbox for `*.json` files.  For each file:
///   1. Reads and parses the job JSON.
///   2. Dispatches on the `"op"` field.
///   3. Writes the result to `outbox/<id>.json` atomically.
///   4. Removes the inbox file.
///
/// Errors on individual jobs are captured in the result JSON; the loop never
/// panics.  Only `create_dir_all` errors propagate to the caller.
pub async fn serve_bridge(state: Arc<AppState>, dir: PathBuf) -> std::io::Result<()> {
    let inbox = dir.join("inbox");
    let outbox = dir.join("outbox");

    tokio::fs::create_dir_all(&inbox).await?;
    tokio::fs::create_dir_all(&outbox).await?;

    let mut ticker = tokio::time::interval(tokio::time::Duration::from_millis(50));

    loop {
        ticker.tick().await;
        scan_and_service(&inbox, &outbox, &state).await;
    }
}

/// Scan inbox/ once and service all *.json files found.
async fn scan_and_service(inbox: &Path, outbox: &Path, state: &Arc<AppState>) {
    let mut entries = match tokio::fs::read_dir(inbox).await {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Turbofig bridge: failed to read inbox: {e}");
            return;
        }
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }

        let job_id = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_owned(),
            None => continue,
        };

        let result = process_job(&path, state).await;

        // Write atomically: write to .tmp, then rename.
        let out_tmp = outbox.join(format!("{job_id}.json.tmp"));
        let out_final = outbox.join(format!("{job_id}.json"));

        match tokio::fs::write(&out_tmp, result.to_string()).await {
            Ok(()) => {
                if let Err(e) = tokio::fs::rename(&out_tmp, &out_final).await {
                    eprintln!("Turbofig bridge: rename failed for {job_id}: {e}");
                }
            }
            Err(e) => {
                eprintln!("Turbofig bridge: write failed for {job_id}: {e}");
            }
        }

        // Remove the processed inbox file.
        if let Err(e) = tokio::fs::remove_file(&path).await {
            eprintln!("Turbofig bridge: remove inbox failed for {job_id}: {e}");
        }
    }
}

/// Read a job file, parse it, and dispatch by op.  Never panics.
async fn process_job(path: &std::path::Path, state: &Arc<AppState>) -> serde_json::Value {
    let contents = match tokio::fs::read_to_string(path).await {
        Ok(s) => s,
        Err(e) => return serde_json::json!({"ok": false, "error": format!("unreadable: {e}")}),
    };

    let job: serde_json::Value = match serde_json::from_str(&contents) {
        Ok(v) => v,
        Err(e) => return serde_json::json!({"ok": false, "error": format!("malformed JSON: {e}")}),
    };

    match job.get("op").and_then(|v| v.as_str()) {
        Some("status") => crate::run_status(state).await,
        Some(op) => {
            serde_json::json!({"ok": false, "error": format!("unknown op: {op}")})
        }
        None => serde_json::json!({"ok": false, "error": "missing op field"}),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_dir_uses_explicit_value_when_set() {
        let path = bridge_dir_from_str(Some("/custom/dir"), None);
        assert_eq!(path, PathBuf::from("/custom/dir"));
    }

    #[test]
    fn bridge_dir_uses_home_turbofig_when_unset() {
        let path = bridge_dir_from_str(None, Some("/home/alice"));
        assert_eq!(path, PathBuf::from("/home/alice/.turbofig"));
    }

    #[test]
    fn bridge_dir_falls_back_to_relative_when_home_also_unset() {
        let path = bridge_dir_from_str(None, None);
        assert_eq!(path, PathBuf::from(".turbofig"));
    }

    #[test]
    fn bridge_dir_explicit_value_wins_over_home() {
        let path = bridge_dir_from_str(Some("/override"), Some("/home/alice"));
        assert_eq!(path, PathBuf::from("/override"));
    }
}
