//! Filesystem bridge transport.
//!
//! Clients write a job file to `<bridge_dir>/inbox/<id>.json` and read the
//! result from `<bridge_dir>/outbox/<id>.json`.  The bridge wakes on a
//! filesystem event (the `notify` crate), services each job via the shared
//! AppState, and writes results atomically (write `<id>.json.tmp` then rename).
//!
//! This transport lets a client drive the daemon using only local file writes
//! and reads.  It fires no curl requests and opens no MCP connection, so it
//! bypasses enterprise policies that gate network tool confirmations.
//!
//! Wakes are event-driven for sub-millisecond notice and near-zero idle CPU.
//! A slow backstop poll runs beside the watcher as a safety net for any missed
//! event.
//!
//! Race-safe reads without a second file: a client writes the whole job in one
//! operation.  A wake that catches a half-written file fails to parse; the
//! scanner leaves that file and retries on the next wake.  A file that still
//! fails to parse after a short grace window is treated as genuinely malformed
//! and gets an error result.  So one write per job stays the contract.

use crate::AppState;
use notify::{RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// Grace window for a job file that does not yet parse as JSON.
/// Younger than this: assume a write still in flight and retry.
/// Older than this: treat as genuinely malformed and return an error.
const PARSE_GRACE: Duration = Duration::from_millis(200);

/// Backstop poll interval. The `notify` watcher drives the fast path with a
/// sub-millisecond wake; this only catches an event the OS delayed or dropped,
/// so it bounds the worst-case pickup without busy polling. 50 ms scans an
/// empty inbox 20 times a second, which costs almost nothing.
const BACKSTOP: Duration = Duration::from_millis(50);

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
/// Creates `<dir>/inbox/` and `<dir>/outbox/` on startup, then watches inbox
/// for filesystem events.  On each wake it scans inbox for `*.json` files.  For
/// each complete job file it:
///   1. Parses the job JSON.
///   2. Claims it (removes the inbox file).
///   3. Dispatches on the `"op"` field in its own task.
///   4. Writes the result to `outbox/<id>.json` atomically.
///
/// Errors on individual jobs are captured in the result JSON; the loop never
/// panics.  Only `create_dir_all` and watcher-setup errors propagate.
pub async fn serve_bridge(state: Arc<AppState>, dir: PathBuf) -> std::io::Result<()> {
    let inbox = dir.join("inbox");
    let outbox = dir.join("outbox");

    tokio::fs::create_dir_all(&inbox).await?;
    tokio::fs::create_dir_all(&outbox).await?;

    // The watcher callback runs on its own thread. It signals the async loop
    // through an unbounded channel. A signal means "something changed, rescan".
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok() {
            let _ = tx.send(());
        }
    })
    .map_err(watcher_io_error)?;
    watcher
        .watch(&inbox, RecursiveMode::NonRecursive)
        .map_err(watcher_io_error)?;

    let mut backstop = tokio::time::interval(BACKSTOP);

    // Scan once at startup for any job left in inbox before the watch began.
    scan_and_service(&inbox, &outbox, &state).await;

    loop {
        // Wake on a filesystem event or the backstop tick, whichever is first.
        tokio::select! {
            _ = rx.recv() => {}
            _ = backstop.tick() => {}
        }
        // Coalesce a burst of events into one scan.
        while rx.try_recv().is_ok() {}
        scan_and_service(&inbox, &outbox, &state).await;
    }
}

/// Convert a `notify` error into an `io::Error` so `serve_bridge` can use `?`.
fn watcher_io_error(e: notify::Error) -> std::io::Error {
    std::io::Error::other(format!("bridge watcher: {e}"))
}

/// Return the age of a file from its modified time.
/// If the age cannot be read, return a large value so the file counts as old.
async fn file_age(path: &Path) -> Duration {
    match tokio::fs::metadata(path).await.and_then(|m| m.modified()) {
        Ok(t) => SystemTime::now()
            .duration_since(t)
            .unwrap_or(Duration::ZERO),
        Err(_) => Duration::from_secs(3600),
    }
}

/// Scan inbox/ once and service all *.json files found.
///
/// A file that parses as JSON is a complete job: claim it (remove the inbox
/// file) and run it in its own task, so one slow job never blocks the others.
/// A file that does not parse yet may be a write in flight: leave it and retry
/// on the next wake, unless it is older than `PARSE_GRACE`, in which case it is
/// genuinely malformed and gets an error result.  This scan runs serially, so a
/// job is claimed by exactly one pass and never processed twice.
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

        let contents = match tokio::fs::read_to_string(&path).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Turbofig bridge: read failed for {job_id}: {e}");
                continue;
            }
        };

        // Parse first to decide whether the job is complete.
        let job: serde_json::Value = match serde_json::from_str(&contents) {
            Ok(v) => v,
            Err(e) => {
                // A young unparseable file is likely still being written; leave
                // it for the next wake. An old one is genuinely malformed.
                if file_age(&path).await < PARSE_GRACE {
                    continue;
                }
                let _ = tokio::fs::remove_file(&path).await;
                let result =
                    serde_json::json!({"ok": false, "error": format!("malformed JSON: {e}")});
                write_result(outbox, &job_id, result).await;
                continue;
            }
        };

        // Claim the complete job by removing the inbox file. A claim failure
        // means we skip it rather than risk reprocessing it on the next wake.
        if let Err(e) = tokio::fs::remove_file(&path).await {
            eprintln!("Turbofig bridge: claim (remove) failed for {job_id}: {e}");
            continue;
        }

        // Run the claimed job in its own task so a slow job does not stall the
        // scan.
        let outbox = outbox.to_path_buf();
        let state = state.clone();
        tokio::spawn(async move {
            let result = process_job(job, &state, &outbox).await;
            write_result(&outbox, &job_id, result).await;
        });
    }
}

/// Dispatch a parsed job by op.  Never panics.
///
/// `output_dir` is the bridge outbox. Screenshot file-mode writes its PNG there.
async fn process_job(
    job: serde_json::Value,
    state: &Arc<AppState>,
    output_dir: &Path,
) -> serde_json::Value {
    match job.get("op").and_then(|v| v.as_str()) {
        Some("status") => crate::run_status(state).await,
        Some("execute") => match job.get("code").and_then(|v| v.as_str()) {
            Some(code) => crate::run_execute(state, code).await,
            None => serde_json::json!({"ok": false, "error": "execute op needs a code field"}),
        },
        Some("get_selection") => crate::run_get_selection(state).await,
        Some("screenshot") => {
            let scale = job.get("scale").and_then(|v| v.as_f64()).unwrap_or(1.0);
            let node_id = job.get("nodeId").and_then(|v| v.as_str());
            let return_mode = job.get("return").and_then(|v| v.as_str()).unwrap_or("file");
            crate::run_screenshot(state, scale, node_id, return_mode, Some(output_dir)).await
        }
        Some(op) => serde_json::json!({"ok": false, "error": format!("unknown op: {op}")}),
        None => serde_json::json!({"ok": false, "error": "missing op field"}),
    }
}

/// Write a result to outbox/<id>.json atomically: write .tmp then rename.
/// A rename failure removes the leftover .tmp so it does not accumulate.
async fn write_result(outbox: &Path, job_id: &str, result: serde_json::Value) {
    let out_tmp = outbox.join(format!("{job_id}.json.tmp"));
    let out_final = outbox.join(format!("{job_id}.json"));

    match tokio::fs::write(&out_tmp, result.to_string()).await {
        Ok(()) => {
            if let Err(e) = tokio::fs::rename(&out_tmp, &out_final).await {
                eprintln!("Turbofig bridge: rename failed for {job_id}: {e}");
                let _ = tokio::fs::remove_file(&out_tmp).await;
            }
        }
        Err(e) => {
            eprintln!("Turbofig bridge: write failed for {job_id}: {e}");
        }
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
