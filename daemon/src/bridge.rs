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
//! A job may carry an optional `"fileKey"` field to target a specific open
//! Figma file.  When `fileKey` is omitted and exactly one plugin is connected,
//! the bridge selects that sole connection automatically.  When multiple plugins
//! are connected and no `fileKey` is given, the route resolver returns a clear
//! error listing the available file keys.
//!
//! Wakes are event-driven for sub-millisecond notice.
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
use std::time::{Duration, Instant};

/// Grace window for a job file that does not yet parse as JSON.
/// Younger than this: assume a write still in flight and retry.
/// Older than this: treat as genuinely malformed and return an error.
const PARSE_GRACE: Duration = Duration::from_millis(200);

/// Backstop poll interval. The `notify` watcher drives the fast path.
/// The backstop does a directory read about 20 times a second as a cheap
/// safety net for a dropped or delayed event.
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
    let mut first_seen: std::collections::HashMap<String, std::time::Instant> =
        std::collections::HashMap::new();

    // Scan once at startup for any job left in inbox before the watch began.
    scan_and_service(&inbox, &outbox, &state, &mut first_seen).await;

    loop {
        // Wake on a filesystem event or the backstop tick, whichever is first.
        tokio::select! {
            msg = rx.recv() => {
                if msg.is_none() {
                    eprintln!("Turbofig bridge: watcher channel closed");
                    break;
                }
            }
            _ = backstop.tick() => {}
        }
        // Coalesce a burst of events into one scan.
        while rx.try_recv().is_ok() {}
        scan_and_service(&inbox, &outbox, &state, &mut first_seen).await;
    }
    Ok(())
}

/// Convert a `notify` error into an `io::Error` so `serve_bridge` can use `?`.
fn watcher_io_error(e: notify::Error) -> std::io::Error {
    std::io::Error::other(format!("bridge watcher: {e}"))
}

/// Scan inbox/ once and service all *.json files found.
///
/// A file that parses as JSON is a complete job: claim it (remove the inbox
/// file) and run it in its own task, so one slow job never blocks the others.
/// A file that does not parse yet may be a write in flight: leave it for the
/// next wake. After `PARSE_GRACE` elapses since first-seen, treat it as
/// genuinely malformed and return an error result. This scan runs serially, so
/// a job is claimed by exactly one pass and never processed twice.
async fn scan_and_service(
    inbox: &Path,
    outbox: &Path,
    state: &Arc<AppState>,
    first_seen: &mut std::collections::HashMap<String, Instant>,
) {
    let mut entries = match tokio::fs::read_dir(inbox).await {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Turbofig bridge: failed to read inbox: {e}");
            return;
        }
    };

    let mut seen_this_scan: std::collections::HashSet<String> = std::collections::HashSet::new();

    loop {
        match entries.next_entry().await {
            Ok(Some(entry)) => {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }

                let job_id = match path.file_stem().and_then(|s| s.to_str()) {
                    Some(s) => s.to_owned(),
                    None => continue,
                };

                seen_this_scan.insert(job_id.clone());

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
                        // Use the first-seen map to decide whether to wait or
                        // treat the file as genuinely malformed.
                        if let Some(seen_at) = first_seen.get(&job_id) {
                            if seen_at.elapsed() >= PARSE_GRACE {
                                // Grace window expired. Report the error.
                                first_seen.remove(&job_id);
                                let _ = tokio::fs::remove_file(&path).await;
                                let result = serde_json::json!({"ok": false, "error": format!("malformed JSON: {e}")});
                                write_result(outbox, &job_id, result).await;
                            }
                            // Still within grace window or already handled.
                        } else {
                            // First time seeing this file. Record it and retry.
                            first_seen.insert(job_id.clone(), Instant::now());
                        }
                        continue;
                    }
                };

                // Remove the first-seen entry now that the file parsed.
                first_seen.remove(&job_id);

                // Claim the complete job by removing the inbox file.
                if let Err(e) = tokio::fs::remove_file(&path).await {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        // Another actor removed it. Skip silently.
                        first_seen.remove(&job_id);
                        continue;
                    }
                    // Claim failed for another reason. Unblock the client with an error result.
                    first_seen.remove(&job_id);
                    let result = serde_json::json!({"ok": false, "error": format!("bridge could not claim job: {e}")});
                    write_result(outbox, &job_id, result).await;
                    continue;
                }

                // Run the claimed job in its own task so a slow job does not
                // stall the scan.
                let outbox = outbox.to_path_buf();
                let state = state.clone();
                tokio::spawn(async move {
                    let result = process_job(job, &state, &outbox).await;
                    write_result(&outbox, &job_id, result).await;
                });
            }
            Ok(None) => break,
            Err(e) => {
                eprintln!("Turbofig bridge: read_dir entry error: {e}");
                break;
            }
        }
    }

    // Prune entries for files that disappeared between scans.
    first_seen.retain(|k, _| seen_this_scan.contains(k));
}

/// Dispatch a parsed job by op.  Never panics.
///
/// `output_dir` is the bridge outbox. Screenshot file-mode writes its PNG there.
///
/// Read the optional `fileKey` from the job and pass it to each run_* call.
/// The bridge has no MCP session id, so session_id is always None.
async fn process_job(
    job: serde_json::Value,
    state: &Arc<AppState>,
    output_dir: &Path,
) -> serde_json::Value {
    let file_key = job.get("fileKey").and_then(|v| v.as_str());
    match job.get("op").and_then(|v| v.as_str()) {
        Some("status") => crate::run_status(state, None, file_key).await,
        Some("execute") => match job.get("code").and_then(|v| v.as_str()) {
            Some(code) => crate::run_execute(state, None, file_key, code).await,
            None => serde_json::json!({"ok": false, "error": "execute op needs a code field"}),
        },
        Some("get_selection") => {
            // Read optional field list and depth for shaped returns.
            let fields: Option<Vec<String>> =
                job.get("fields").and_then(|v| v.as_array()).map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                });
            let depth: Option<u32> = job
                .get("depth")
                .and_then(|v| v.as_u64())
                .map(|d| d.min(5) as u32);
            crate::run_get_selection(state, None, file_key, fields.as_deref(), depth).await
        }
        Some("screenshot") => {
            let scale = job.get("scale").and_then(|v| v.as_f64()).unwrap_or(1.0);
            let node_id = job.get("nodeId").and_then(|v| v.as_str());
            let return_mode = job.get("return").and_then(|v| v.as_str()).unwrap_or("file");
            crate::run_screenshot(
                state,
                None,
                file_key,
                scale,
                node_id,
                return_mode,
                Some(output_dir),
            )
            .await
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
