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
//! event, and doubles as the tick that prunes old outbox files and malformed
//! inbox entries.
//!
//! Race-safe reads without a second file: a client writes the whole job in one
//! operation.  A wake that catches a half-written file fails to parse; the
//! scanner leaves that file and retries on the next wake.  A file that still
//! fails to parse (or even read) after a short grace window is given up on:
//! one error result (when it has a usable id) is written, or it is otherwise
//! just removed, and it is never retried or re-logged again while it keeps
//! failing the same way.

pub(crate) mod job;

use crate::state::AppState;
use job::Job;
use notify::{RecursiveMode, Watcher};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

/// Grace window for a job file that does not yet parse (or even read) as JSON.
/// Younger than this: assume a write still in flight and retry.
/// Older than this: give up permanently (see `given_up` below).
const PARSE_GRACE: Duration = Duration::from_millis(200);

/// Backstop poll interval. The `notify` watcher drives the fast path.
/// The backstop is a safety net for a dropped or delayed event, and the tick
/// that prunes stale outbox files. 500 ms still comfortably covers the
/// PARSE_GRACE window with room to spare, and does far less idle work than
/// the original 50 ms (20 wakeups/second) when the bridge is quiet.
const BACKSTOP: Duration = Duration::from_millis(500);

/// How long a result file (or a file-mode screenshot PNG) may sit in the
/// outbox before the backstop deletes it. The outbox is a drop box, not
/// storage: an unread result this old was never going to be read.
const OUTBOX_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

// ── Bridge server ─────────────────────────────────────────────────────────────

/// Serve the filesystem bridge.
///
/// Creates `<dir>/inbox/` and `<dir>/outbox/` on startup, then watches inbox
/// for filesystem events.  On each wake it scans inbox for `*.json` files.  For
/// each complete job file it:
///   1. Parses the job JSON into a typed `Job`.
///   2. Claims it (removes the inbox file).
///   3. Deletes any stale same-id outbox result, so a reused id never reads
///      an old answer.
///   4. Dispatches on the job variant in its own task.
///   5. Writes the result to `outbox/<id>.json` atomically.
///
/// Errors on individual jobs are captured in the result JSON; the loop never
/// panics.  Only `create_dir_all` and watcher-setup errors propagate.
///
/// `inbox/` and `outbox/` are set to mode 0700 (owner-only) by
/// [`prepare_dirs`], on both first creation and an existing install. `dir`
/// itself is also tightened to 0700 when it is the default home (no
/// `TURBOFIG_BRIDGE_DIR` override); see `prepare_dirs`.
pub async fn serve_bridge(state: Arc<AppState>, dir: PathBuf) -> std::io::Result<()> {
    let is_default_home = std::env::var("TURBOFIG_BRIDGE_DIR").is_err();
    let (inbox, outbox) = prepare_dirs(&dir, is_default_home).await?;

    // The watcher callback runs on its own thread. It signals the async loop
    // through an unbounded channel. A signal means "something changed, rescan".
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok() {
            let _ = tx.send(());
        }
    })
    .map_err(watcher_io_error)?;
    watcher
        .watch(&inbox, RecursiveMode::NonRecursive)
        .map_err(watcher_io_error)?;

    run_bridge_loop(rx, &inbox, &outbox, &state).await
}

/// Creates `<dir>/inbox` and `<dir>/outbox` and sets both to mode 0700, then
/// returns their paths. A job file can carry arbitrary eval code and a result
/// file can carry the response, so group- or world-readable bridge
/// directories would let another local user read or queue jobs.
///
/// Also tightens `dir` itself to 0700, but only when it is the default
/// `~/.turbofig` the daemon owns outright (i.e. `is_default_home` is true).
/// A custom `TURBOFIG_BRIDGE_DIR` can be an existing, shared path the user
/// pointed us at (`/tmp`, a project folder); the daemon does not own its
/// mode there and must never touch it or fail startup because it could not.
async fn prepare_dirs(dir: &Path, is_default_home: bool) -> std::io::Result<(PathBuf, PathBuf)> {
    let inbox = dir.join("inbox");
    let outbox = dir.join("outbox");

    tokio::fs::create_dir_all(dir).await?;
    tokio::fs::create_dir_all(&inbox).await?;
    tokio::fs::create_dir_all(&outbox).await?;

    // A failure to tighten any of these is logged, not fatal: a locked-down
    // bridge dir is a hardening goal, not a precondition for the daemon to
    // run at all.
    if is_default_home {
        if let Err(e) = set_owner_only(dir).await {
            eprintln!("Turbofig bridge: could not set the home dir to owner-only: {e}");
        }
    }
    if let Err(e) = set_owner_only(&inbox).await {
        eprintln!("Turbofig bridge: could not set inbox to owner-only: {e}");
    }
    if let Err(e) = set_owner_only(&outbox).await {
        eprintln!("Turbofig bridge: could not set outbox to owner-only: {e}");
    }

    Ok((inbox, outbox))
}

/// Sets `path` to mode 0700 (owner read/write/execute only, no group or world
/// access). Fixes the mode on every startup, so an existing install created
/// before this check (or loosened by an `umask`) gets corrected, not just a
/// fresh one.
#[cfg(unix)]
async fn set_owner_only(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await
}

/// No-op on non-Unix platforms: there is no POSIX mode to set.
#[cfg(not(unix))]
async fn set_owner_only(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Run the inbox event loop.
///
/// Receives filesystem-event signals on `rx`. Returns `Err` when the channel
/// closes (the watcher thread dropped the sender). A healthy loop runs forever.
///
/// Extracted from `serve_bridge` so the watcher-close error path can be tested
/// without a real `notify` watcher.
async fn run_bridge_loop(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<()>,
    inbox: &Path,
    outbox: &Path,
    state: &Arc<AppState>,
) -> std::io::Result<()> {
    let mut backstop = tokio::time::interval(BACKSTOP);
    let mut first_seen: HashMap<String, Instant> = HashMap::new();
    let mut given_up: HashSet<String> = HashSet::new();
    // Job ids currently claimed and running in their own tokio::spawn task.
    // Shared with those tasks (not just this loop) so a duplicate id written
    // while the first is still running is recognised even though the scan
    // itself never awaits the job.
    let in_flight: Arc<std::sync::Mutex<HashSet<String>>> =
        Arc::new(std::sync::Mutex::new(HashSet::new()));

    // Scan once at startup for any job left in inbox before the watch began.
    scan_and_service(
        inbox,
        outbox,
        state,
        &mut first_seen,
        &mut given_up,
        &in_flight,
    )
    .await;

    loop {
        // Wake on a filesystem event or the backstop tick, whichever is first.
        let mut is_backstop = false;
        tokio::select! {
            msg = rx.recv() => {
                if msg.is_none() {
                    // The watcher thread dropped the sender. The bridge is now
                    // deaf to inbox events. Return Err so the spawn wrapper in
                    // main calls process::exit and launchd KeepAlive restarts.
                    return Err(std::io::Error::other("bridge watcher channel closed"));
                }
            }
            _ = backstop.tick() => { is_backstop = true; }
        }
        // Coalesce a burst of events into one scan.
        while rx.try_recv().is_ok() {}
        if is_backstop {
            prune_old_outbox(outbox).await;
        }
        scan_and_service(
            inbox,
            outbox,
            state,
            &mut first_seen,
            &mut given_up,
            &in_flight,
        )
        .await;
    }
}

/// Convert a `notify` error into an `io::Error` so `serve_bridge` can use `?`.
fn watcher_io_error(e: notify::Error) -> std::io::Error {
    std::io::Error::other(format!("bridge watcher: {e}"))
}

/// Delete outbox entries (results and file-mode screenshot PNGs) older than
/// `OUTBOX_MAX_AGE`. The outbox is a drop box: a result nobody read this long
/// ago never will be, and an unbounded outbox is a slow disk leak on a
/// long-running daemon.
async fn prune_old_outbox(outbox: &Path) {
    let mut entries = match tokio::fs::read_dir(outbox).await {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Turbofig bridge: failed to read outbox for pruning: {e}");
            return;
        }
    };
    loop {
        match entries.next_entry().await {
            Ok(Some(entry)) => {
                let path = entry.path();
                let Ok(metadata) = entry.metadata().await else {
                    continue;
                };
                let age = metadata
                    .modified()
                    .ok()
                    .and_then(|m| SystemTime::now().duration_since(m).ok())
                    .unwrap_or_default();
                if age > OUTBOX_MAX_AGE {
                    let _ = tokio::fs::remove_file(&path).await;
                }
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
}

/// One malformed-entry outcome: either a usable job id (so an error result
/// can be written) or none (a non-UTF-8 name or an unreadable/non-file entry,
/// where the best we can do is remove it and stop looking at it).
enum GiveUp {
    WriteError(String, String),
    JustForget,
}

/// Decide what to do with an inbox entry that has failed to read or parse
/// for at least `PARSE_GRACE`. Removes the inbox entry where possible so it
/// is never retried, and returns what outcome to record.
async fn give_up_on(path: &Path, job_id: Option<&str>, reason: String) -> GiveUp {
    // Best-effort cleanup: a plain file removes with remove_file; a
    // directory (e.g. a directory accidentally named `x.json`) needs
    // remove_dir_all instead, since remove_file always fails on a directory.
    if tokio::fs::remove_file(path).await.is_err() {
        let _ = tokio::fs::remove_dir_all(path).await;
    }
    match job_id {
        Some(id) => GiveUp::WriteError(id.to_owned(), reason),
        None => {
            eprintln!("Turbofig bridge: giving up on unreadable inbox entry: {reason}");
            GiveUp::JustForget
        }
    }
}

/// Scan inbox/ once and service all *.json files found.
///
/// A file that parses as a `Job` is a complete job: claim it (remove the
/// inbox file) and run it in its own task, so one slow job never blocks the
/// others. A file that does not yet read or parse may be a write in flight:
/// leave it for the next wake. After `PARSE_GRACE` elapses since first-seen,
/// give up on it via `give_up_on` and record it in `given_up` so it is never
/// retried or re-logged again, even across many scans, as long as it keeps
/// failing. This scan runs serially, so a job is claimed by exactly one pass
/// and never processed twice.
async fn scan_and_service(
    inbox: &Path,
    outbox: &Path,
    state: &Arc<AppState>,
    first_seen: &mut HashMap<String, Instant>,
    given_up: &mut HashSet<String>,
    in_flight: &Arc<std::sync::Mutex<HashSet<String>>>,
) {
    let mut entries = match tokio::fs::read_dir(inbox).await {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Turbofig bridge: failed to read inbox: {e}");
            return;
        }
    };

    let mut seen_this_scan: HashSet<String> = HashSet::new();

    loop {
        match entries.next_entry().await {
            Ok(Some(entry)) => {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }

                // Track by the full path text (lossy for a non-UTF-8 name)
                // so every kind of malformed entry, not just a parse
                // failure, gets the same grace-then-give-up treatment.
                let key = path.to_string_lossy().into_owned();
                seen_this_scan.insert(key.clone());

                if given_up.contains(&key) {
                    continue;
                }

                let job_id = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned);

                let contents = match tokio::fs::read_to_string(&path).await {
                    Ok(s) => s,
                    Err(e) => {
                        handle_unready(
                            &path,
                            &key,
                            job_id.as_deref(),
                            format!("read failed: {e}"),
                            outbox,
                            first_seen,
                            given_up,
                        )
                        .await;
                        continue;
                    }
                };

                let raw: serde_json::Value = match serde_json::from_str(&contents) {
                    Ok(v) => v,
                    Err(e) => {
                        handle_unready(
                            &path,
                            &key,
                            job_id.as_deref(),
                            format!("malformed JSON: {e}"),
                            outbox,
                            first_seen,
                            given_up,
                        )
                        .await;
                        continue;
                    }
                };

                // The file read and parsed as JSON, so this is not a
                // half-written file: a schema-invalid Job (a bad field, an
                // unknown op) is a complete, final answer, not a "maybe
                // still being written" state. Fail it at once rather than
                // waiting out PARSE_GRACE plus the backstop: the grace
                // window exists only to tolerate a write still in flight.
                let job = match Job::parse(&raw) {
                    Ok(j) => j,
                    Err(e) => {
                        first_seen.remove(&key);
                        given_up.remove(&key);
                        if let Err(e) = tokio::fs::remove_file(&path).await {
                            if e.kind() != std::io::ErrorKind::NotFound {
                                eprintln!(
                                    "Turbofig bridge: could not remove invalid job {path:?}: {e}"
                                );
                            }
                        }
                        if let Some(id) = job_id.as_deref() {
                            delete_stale_result(outbox, id).await;
                            let result = serde_json::json!({"ok": false, "error": e});
                            write_result(outbox, id, result).await;
                        }
                        continue;
                    }
                };

                // Parsed successfully: no longer "unready".
                first_seen.remove(&key);

                let Some(job_id) = job_id else {
                    // A job with a non-UTF-8 filename parsed its contents
                    // fine, but there is no id to write a result under.
                    // This should not happen in practice (bridge clients
                    // choose the id), so just drop it.
                    let _ = tokio::fs::remove_file(&path).await;
                    continue;
                };

                // A client that reuses an id while the first job with that id
                // is still running must never have this scan touch the
                // in-flight job's .tmp file or its eventual result. The
                // simplest race-free answer: leave the duplicate in the
                // inbox untouched. It is claimed normally on a later scan,
                // once the in-flight id has been removed below.
                if in_flight
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains(&job_id)
                {
                    continue;
                }

                // Take the job-counter guard before the claim, not inside the
                // spawned task: `jobs_in_flight` must count this job for the
                // whole span from "claimed" onward, so the supervised-restart
                // drain wait never sees 0 while a just-claimed job has not
                // reached its own `tokio::spawn` yet.
                let job_guard = state.begin_job();

                // Claim the complete job by removing the inbox file.
                if let Err(e) = tokio::fs::remove_file(&path).await {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        // Another actor removed it. Skip silently.
                        continue;
                    }
                    let result = serde_json::json!({"ok": false, "error": format!("bridge could not claim job: {e}")});
                    write_result(outbox, &job_id, result).await;
                    continue;
                }

                // A reused id must never read a stale answer from a job that
                // used that id before: delete any old result before this job
                // writes its own.
                delete_stale_result(outbox, &job_id).await;

                in_flight
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(job_id.clone());

                // Run the claimed job in its own task so a slow job does not
                // stall the scan.
                let outbox_owned = outbox.to_path_buf();
                let state = state.clone();
                let in_flight = in_flight.clone();
                let in_flight_id = job_id.clone();
                tokio::spawn(async move {
                    let _job_guard = job_guard;
                    let result = process_job(job, &state, None, Some(&outbox_owned)).await;
                    write_result(&outbox_owned, &job_id, result).await;
                    in_flight
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&in_flight_id);
                });
            }
            Ok(None) => break,
            Err(e) => {
                eprintln!("Turbofig bridge: read_dir entry error: {e}");
                break;
            }
        }
    }

    // Prune first_seen/given_up entries for files that disappeared between
    // scans, so a later file reusing the same name starts fresh.
    first_seen.retain(|k, _| seen_this_scan.contains(k));
    given_up.retain(|k| seen_this_scan.contains(k));
}

/// Apply the first-seen/grace/give-up policy to one unready (unreadable or
/// unparseable) inbox entry.
#[allow(clippy::too_many_arguments)]
async fn handle_unready(
    path: &Path,
    key: &str,
    job_id: Option<&str>,
    reason: String,
    outbox: &Path,
    first_seen: &mut HashMap<String, Instant>,
    given_up: &mut HashSet<String>,
) {
    match first_seen.get(key) {
        Some(seen_at) if seen_at.elapsed() >= PARSE_GRACE => {
            first_seen.remove(key);
            match give_up_on(path, job_id, reason).await {
                GiveUp::WriteError(id, reason) => {
                    let result = serde_json::json!({"ok": false, "error": reason});
                    write_result(outbox, &id, result).await;
                }
                GiveUp::JustForget => {}
            }
            given_up.insert(key.to_owned());
        }
        Some(_) => {
            // Still within the grace window: a write may still be in flight.
        }
        None => {
            first_seen.insert(key.to_owned(), Instant::now());
        }
    }
}

/// Remove a stale `outbox/<id>.json` and its `.tmp` sibling, if present,
/// before a newly claimed job with that id writes its own result.
async fn delete_stale_result(outbox: &Path, job_id: &str) {
    let _ = tokio::fs::remove_file(outbox.join(format!("{job_id}.json"))).await;
    let _ = tokio::fs::remove_file(outbox.join(format!("{job_id}.json.tmp"))).await;
}

/// Run one parsed job against the shared daemon state.
///
/// `output_dir` is where a file-mode screenshot writes its PNG: the bridge
/// outbox for a bridge job, or the daemon's configured screenshot directory
/// for a job posted to `POST /job` (see `mcp::job_handler`). `None` disables
/// file-mode output entirely, matching an `AppState` with no screenshot
/// directory configured (e.g. `AppState::with_timeout` in a test).
///
/// `session_id` is the fileKey-pairing session to route this job under (see
/// `routing::resolve_route`): the filesystem bridge has no notion of a
/// session at all, so its one call site always passes `None`; `POST /job`
/// passes whatever `X-Turbofig-Session` header the caller sent (see
/// `mcp::job_session_id`), which is how the stdio MCP proxy (`proxy.rs`)
/// gets the same per-session fileKey pairing an HTTP MCP session gets from
/// its `mcp-session-id` header.
pub(crate) async fn process_job(
    job: Job,
    state: &Arc<AppState>,
    session_id: Option<&str>,
    output_dir: Option<&Path>,
) -> serde_json::Value {
    match job {
        Job::Status(p) => crate::ops::run_status(state, session_id, p.file_key.as_deref()).await,
        Job::Execute(p) => {
            crate::ops::run_execute(state, session_id, p.file_key.as_deref(), &p.code).await
        }
        Job::GetSelection(p) => {
            crate::ops::run_get_selection(
                state,
                session_id,
                p.file_key.as_deref(),
                p.fields.as_deref(),
                p.depth,
            )
            .await
        }
        Job::Screenshot(p) => {
            crate::ops::run_screenshot(
                state,
                session_id,
                p.file_key.as_deref(),
                p.scale,
                p.node_id.as_deref(),
                p.return_mode.as_str(),
                output_dir,
                p.max_dim,
                p.full_res,
            )
            .await
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Drop the sender before the loop starts. The loop must immediately return
    /// Err (not Ok) when it discovers the channel is closed on the first recv.
    #[tokio::test]
    async fn watcher_channel_close_returns_err() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let inbox = tmp.path().join("inbox");
        let outbox = tmp.path().join("outbox");
        tokio::fs::create_dir_all(&inbox)
            .await
            .expect("create inbox");
        tokio::fs::create_dir_all(&outbox)
            .await
            .expect("create outbox");

        let state = Arc::new(crate::state::AppState::with_timeout(Duration::from_millis(
            100,
        )));
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        drop(tx);

        let result = run_bridge_loop(rx, &inbox, &outbox, &state).await;
        assert!(result.is_err(), "closed channel must produce Err");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("bridge watcher channel closed"),
            "unexpected error: {msg}",
        );
    }

    #[cfg(unix)]
    fn mode_bits(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn prepare_dirs_sets_dirs_to_owner_only_on_first_create() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let dir = tmp.path().join("fresh");

        prepare_dirs(&dir, true).await.expect("prepare bridge dirs");

        assert_eq!(
            mode_bits(&dir),
            0o700,
            "the default home must be owner-only"
        );
        assert_eq!(
            mode_bits(&dir.join("inbox")),
            0o700,
            "inbox must be owner-only"
        );
        assert_eq!(
            mode_bits(&dir.join("outbox")),
            0o700,
            "outbox must be owner-only"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn prepare_dirs_fixes_mode_on_an_existing_too_open_install() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("temp dir");
        let dir = tmp.path().join("existing");
        let inbox = dir.join("inbox");
        let outbox = dir.join("outbox");
        tokio::fs::create_dir_all(&inbox).await.expect("inbox");
        tokio::fs::create_dir_all(&outbox).await.expect("outbox");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).expect("chmod dir");
        std::fs::set_permissions(&inbox, std::fs::Permissions::from_mode(0o777))
            .expect("chmod inbox");
        std::fs::set_permissions(&outbox, std::fs::Permissions::from_mode(0o777))
            .expect("chmod outbox");

        prepare_dirs(&dir, true).await.expect("prepare bridge dirs");

        assert_eq!(
            mode_bits(&dir),
            0o700,
            "an existing default home must be corrected"
        );
        assert_eq!(
            mode_bits(&inbox),
            0o700,
            "an existing inbox must be corrected"
        );
        assert_eq!(
            mode_bits(&outbox),
            0o700,
            "an existing outbox must be corrected"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn prepare_dirs_leaves_a_custom_bridge_dirs_own_mode_untouched() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("temp dir");
        let dir = tmp.path().join("custom");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("mkdir custom dir");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).expect("chmod dir");

        prepare_dirs(&dir, false)
            .await
            .expect("prepare bridge dirs");

        // A custom TURBOFIG_BRIDGE_DIR can be an existing, shared path the
        // user pointed us at; the daemon does not own its mode.
        assert_eq!(
            mode_bits(&dir),
            0o777,
            "a custom bridge dir's own mode must be left untouched"
        );
        assert_eq!(
            mode_bits(&dir.join("inbox")),
            0o700,
            "inbox must still be owner-only"
        );
        assert_eq!(
            mode_bits(&dir.join("outbox")),
            0o700,
            "outbox must still be owner-only"
        );
    }

    #[tokio::test]
    async fn malformed_job_gets_one_error_result_and_is_never_retried() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let inbox = tmp.path().join("inbox");
        let outbox = tmp.path().join("outbox");
        tokio::fs::create_dir_all(&inbox).await.unwrap();
        tokio::fs::create_dir_all(&outbox).await.unwrap();
        tokio::fs::write(inbox.join("bad.json"), b"not json")
            .await
            .unwrap();

        let state = Arc::new(crate::state::AppState::with_timeout(Duration::from_millis(
            100,
        )));
        let mut first_seen = HashMap::new();
        let mut given_up = HashSet::new();
        let in_flight = Arc::new(std::sync::Mutex::new(HashSet::new()));

        // First scan: within grace, nothing written yet, file still present.
        scan_and_service(
            &inbox,
            &outbox,
            &state,
            &mut first_seen,
            &mut given_up,
            &in_flight,
        )
        .await;
        assert!(
            inbox.join("bad.json").exists(),
            "must not claim within grace"
        );

        tokio::time::sleep(PARSE_GRACE + Duration::from_millis(50)).await;

        // Second scan: grace has elapsed. Must give up: write an error result,
        // remove the inbox file, and remember it in given_up.
        scan_and_service(
            &inbox,
            &outbox,
            &state,
            &mut first_seen,
            &mut given_up,
            &in_flight,
        )
        .await;
        assert!(
            !inbox.join("bad.json").exists(),
            "the bad file must be removed"
        );
        let result = tokio::fs::read_to_string(outbox.join("bad.json"))
            .await
            .expect("result written");
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["ok"], serde_json::json!(false));
        let key = inbox.join("bad.json").to_string_lossy().into_owned();
        assert!(
            given_up.contains(&key),
            "a given-up entry must be remembered so it is never retried"
        );

        // A third scan must not touch it again: no new write, no panic, no
        // re-insertion into first_seen.
        scan_and_service(
            &inbox,
            &outbox,
            &state,
            &mut first_seen,
            &mut given_up,
            &in_flight,
        )
        .await;
        assert!(!first_seen.contains_key(&key));
    }

    #[tokio::test]
    async fn a_schema_invalid_job_fails_at_once_without_waiting_out_the_parse_grace() {
        // Valid JSON, invalid Job schema (unknown op): must get one error
        // result on the very first scan, never the PARSE_GRACE wait that a
        // half-written file needs.
        let tmp = tempfile::tempdir().expect("temp dir");
        let inbox = tmp.path().join("inbox");
        let outbox = tmp.path().join("outbox");
        tokio::fs::create_dir_all(&inbox).await.unwrap();
        tokio::fs::create_dir_all(&outbox).await.unwrap();
        tokio::fs::write(inbox.join("job1.json"), br#"{"op":"delete_everything"}"#)
            .await
            .unwrap();

        let state = Arc::new(crate::state::AppState::with_timeout(Duration::from_millis(
            100,
        )));
        let mut first_seen = HashMap::new();
        let mut given_up = HashSet::new();
        let in_flight = Arc::new(std::sync::Mutex::new(HashSet::new()));

        scan_and_service(
            &inbox,
            &outbox,
            &state,
            &mut first_seen,
            &mut given_up,
            &in_flight,
        )
        .await;

        assert!(
            !inbox.join("job1.json").exists(),
            "a schema-invalid job must be claimed on the first scan"
        );
        let result = tokio::fs::read_to_string(outbox.join("job1.json"))
            .await
            .expect("error result written on the first scan");
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["ok"], serde_json::json!(false));
        assert!(
            first_seen.is_empty(),
            "a schema-invalid job must never enter the parse-grace bookkeeping"
        );
    }

    #[tokio::test]
    async fn a_duplicate_job_id_still_in_flight_is_left_in_the_inbox_untouched() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let inbox = tmp.path().join("inbox");
        let outbox = tmp.path().join("outbox");
        tokio::fs::create_dir_all(&inbox).await.unwrap();
        tokio::fs::create_dir_all(&outbox).await.unwrap();
        tokio::fs::write(inbox.join("dup.json"), br#"{"op":"status"}"#)
            .await
            .unwrap();

        let state = Arc::new(crate::state::AppState::with_timeout(Duration::from_millis(
            100,
        )));
        let mut first_seen = HashMap::new();
        let mut given_up = HashSet::new();
        let in_flight = Arc::new(std::sync::Mutex::new(HashSet::new()));
        // Simulate a first job with this id already running.
        in_flight.lock().unwrap().insert("dup".to_owned());

        scan_and_service(
            &inbox,
            &outbox,
            &state,
            &mut first_seen,
            &mut given_up,
            &in_flight,
        )
        .await;

        assert!(
            inbox.join("dup.json").exists(),
            "a duplicate id still in flight must be left in the inbox, not claimed or answered"
        );
        assert!(
            !outbox.join("dup.json").exists(),
            "a duplicate id still in flight must not get its own result written"
        );

        // The first job finishes; its id is no longer in flight.
        in_flight.lock().unwrap().remove("dup");
        scan_and_service(
            &inbox,
            &outbox,
            &state,
            &mut first_seen,
            &mut given_up,
            &in_flight,
        )
        .await;
        assert!(
            !inbox.join("dup.json").exists(),
            "once the id is free, a later scan must claim the duplicate normally"
        );
    }

    #[tokio::test]
    async fn claiming_a_reused_id_deletes_a_stale_outbox_result() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let outbox = tmp.path().join("outbox");
        tokio::fs::create_dir_all(&outbox).await.unwrap();
        tokio::fs::write(outbox.join("job1.json"), b"{\"ok\":true,\"stale\":true}")
            .await
            .unwrap();
        tokio::fs::write(outbox.join("job1.json.tmp"), b"leftover")
            .await
            .unwrap();

        delete_stale_result(&outbox, "job1").await;

        assert!(!outbox.join("job1.json").exists());
        assert!(!outbox.join("job1.json.tmp").exists());
    }

    #[tokio::test]
    async fn claiming_a_job_counts_it_in_flight_before_its_spawned_task_runs() {
        // The job-counter guard must be taken synchronously in the scan, not
        // inside the spawned task: otherwise a drain check racing right after
        // scan_and_service returns could see 0 in-flight jobs for a job that
        // was already claimed and is about to run.
        let tmp = tempfile::tempdir().expect("temp dir");
        let inbox = tmp.path().join("inbox");
        let outbox = tmp.path().join("outbox");
        tokio::fs::create_dir_all(&inbox).await.unwrap();
        tokio::fs::create_dir_all(&outbox).await.unwrap();
        tokio::fs::write(inbox.join("job1.json"), br#"{"op":"status"}"#)
            .await
            .unwrap();

        let state = Arc::new(crate::state::AppState::with_timeout(Duration::from_millis(
            100,
        )));
        let mut first_seen = HashMap::new();
        let mut given_up = HashSet::new();
        let in_flight = Arc::new(std::sync::Mutex::new(HashSet::new()));

        scan_and_service(
            &inbox,
            &outbox,
            &state,
            &mut first_seen,
            &mut given_up,
            &in_flight,
        )
        .await;

        assert_eq!(
            state.jobs_in_flight(),
            1,
            "the claimed job must already be counted before its spawned task is polled"
        );
    }

    #[tokio::test]
    async fn prune_old_outbox_removes_only_stale_files() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let outbox = tmp.path().join("outbox");
        tokio::fs::create_dir_all(&outbox).await.unwrap();
        tokio::fs::write(outbox.join("fresh.json"), b"{}")
            .await
            .unwrap();
        let old_path = outbox.join("old.json");
        tokio::fs::write(&old_path, b"{}").await.unwrap();

        // Backdate "old.json"'s mtime past OUTBOX_MAX_AGE.
        let old_time = SystemTime::now() - (OUTBOX_MAX_AGE + Duration::from_secs(60));
        let file = std::fs::File::open(&old_path).unwrap();
        file.set_modified(old_time).unwrap();

        prune_old_outbox(&outbox).await;

        assert!(
            outbox.join("fresh.json").exists(),
            "a fresh file must survive pruning"
        );
        assert!(!old_path.exists(), "a stale file must be pruned");
    }
}
