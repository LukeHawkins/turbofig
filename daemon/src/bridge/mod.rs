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

/// Longest job id (file stem) the bridge accepts. A longer id cannot name a
/// result file reliably (file names cap near 255 bytes).
const MAX_ID_BYTES: usize = 200;

/// Largest job file the bridge reads. A larger file gets an error result.
/// Just under the 32 MiB WebSocket frame cap, so every job that fits a frame still runs.
pub(crate) const MAX_JOB_BYTES: u64 = 30 * 1024 * 1024;

use crate::state::AppState;
use job::Job;
use notify::{RecursiveMode, Watcher};
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::OpenOptionsExt as _;
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
    if let Err(e) = write_helper_doc(&dir).await {
        eprintln!("turbofig bridge: could not write {HELPER_DOC_NAME}: {e}");
    }

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
/// A custom `TURBOFIG_BRIDGE_DIR` keeps its own mode: the daemon never
/// changes it. It still gets the symlink and owner checks, so a folder that
/// is a symlink or is owned by another user (`TURBOFIG_BRIDGE_DIR=/tmp`
/// itself, a root-owned symlink on macOS) is refused. A folder of the
/// user's own inside `/tmp` works.
async fn prepare_dirs(dir: &Path, is_default_home: bool) -> std::io::Result<(PathBuf, PathBuf)> {
    // Symlink, owner and chmod checks live in `secure_fs`. A failure is
    // fatal: the daemon must not run on a folder it cannot trust.
    crate::secure_fs::prepare_bridge_dirs(dir, is_default_home)
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

    // A job written while no daemon ran may be hours old: the Figma file can
    // have changed since. Answer it with not_started instead of running it.
    expire_stale_jobs(inbox, outbox, state.request_timeout).await;

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
            state.restore_token_if_missing();
            prune_outbox_if_safe(outbox).await;
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

/// Startup only. Any regular inbox job file older than `max_age` (by mtime)
/// is removed and answered with a `not_started` result that says it was too
/// old and never ran. A job file of another owner, or with an id too long to
/// name a result, is left to the normal scan.
async fn expire_stale_jobs(inbox: &Path, outbox: &Path, max_age: Duration) {
    use std::os::unix::fs::MetadataExt;
    let Ok(mut entries) = tokio::fs::read_dir(inbox).await else {
        return;
    };
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else {
            continue;
        };
        if id.len() > MAX_ID_BYTES {
            continue;
        }
        let Ok(meta) = tokio::fs::symlink_metadata(&path).await else {
            continue;
        };
        if !meta.is_file() || meta.uid() != euid {
            continue;
        }
        let age = meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .unwrap_or_default();
        if age <= max_age {
            continue;
        }
        if tokio::fs::remove_file(&path).await.is_err() {
            continue;
        }
        delete_stale_result(outbox, &id).await;
        write_result(outbox, &id, stale_job_result(age)).await;
    }
}

/// `not_started` result for an inbox job that waited longer than the request
/// timeout while no daemon ran.
fn stale_job_result(age: Duration) -> serde_json::Value {
    serde_json::json!({
        "ok": false,
        "code": "not_started",
        "error": format!(
            "the job was {} s old when the daemon started, so it was too old and never ran; resend it",
            age.as_secs()
        ),
    })
}

/// Convert a `notify` error into an `io::Error` so `serve_bridge` can use `?`.
fn watcher_io_error(e: notify::Error) -> std::io::Error {
    std::io::Error::other(format!("bridge watcher: {e}"))
}

/// Pure check for the prune guard. The outbox must be a real folder (not a
/// symlink), owned by `euid`, and hold the daemon's marker file.
fn outbox_prune_allowed(
    is_symlink: bool,
    is_dir: bool,
    owner_uid: u32,
    euid: u32,
    marked: bool,
) -> bool {
    !is_symlink && is_dir && owner_uid == euid && marked
}

/// Runs `prune_old_outbox` only when `outbox_prune_allowed` says so. The
/// check runs before every prune, because the folder can change after
/// startup. An outbox with no marker was not made by the daemon: it may hold
/// user files, so it is never pruned (one log line).
async fn prune_outbox_if_safe(outbox: &Path) {
    use std::os::unix::fs::MetadataExt;
    static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let Ok(meta) = tokio::fs::symlink_metadata(outbox).await else {
        return;
    };
    let marked = tokio::fs::symlink_metadata(outbox.join(crate::secure_fs::OUTBOX_MARKER))
        .await
        .is_ok_and(|m| m.is_file());
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    let ft = meta.file_type();
    if outbox_prune_allowed(ft.is_symlink(), ft.is_dir(), meta.uid(), euid, marked) {
        prune_old_outbox(outbox).await;
    } else if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        eprintln!(
            "turbofig bridge: not pruning {}: it is not a daemon-made folder owned by this user (no {} marker, a symlink, or a foreign owner)",
            outbox.display(),
            crate::secure_fs::OUTBOX_MARKER
        );
    }
}

/// Delete outbox entries (results and file-mode screenshot PNGs) older than
/// `OUTBOX_MAX_AGE`. The outbox is a drop box: a result nobody read this long
/// ago never will be, and an unbounded outbox is a slow disk leak on a
/// long-running daemon.
async fn prune_old_outbox(outbox: &Path) {
    let mut entries = match tokio::fs::read_dir(outbox).await {
        Ok(e) => e,
        Err(e) => {
            eprintln!("turbofig bridge: failed to read outbox for pruning: {e}");
            return;
        }
    };
    loop {
        match entries.next_entry().await {
            Ok(Some(entry)) => {
                let path = entry.path();
                // entry.metadata() does not follow symlinks (lstat).
                let Ok(metadata) = entry.metadata().await else {
                    continue;
                };
                // Delete only regular files that carry the daemon's own
                // name patterns. TURBOFIG_BRIDGE_DIR can point at a folder
                // that holds user files: those must never be pruned.
                let owned_name = entry
                    .file_name()
                    .to_str()
                    .is_some_and(is_daemon_outbox_name);
                if !metadata.is_file() || !owned_name {
                    continue;
                }
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
            eprintln!("turbofig bridge: giving up on unreadable inbox entry: {reason}");
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
            eprintln!("turbofig bridge: failed to read inbox: {e}");
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

                // An id too long to name a result file can never be answered
                // under its own name, and the claim would run the job with no
                // result. Never run it: remove the file and write an error
                // result under a shortened name (see `short_id`).
                if let Some(id) = job_id.as_deref() {
                    if id.len() > MAX_ID_BYTES {
                        // The error name could equal a real in-flight id:
                        // leave the entry and retry on a later scan.
                        let short = too_long_reply_id(id);
                        if in_flight
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .contains(&short)
                        {
                            continue;
                        }
                        first_seen.remove(&key);
                        let reason = format!("job id is longer than {MAX_ID_BYTES} bytes");
                        reject_entry(&path, &key, Some(&short), reason, outbox, given_up).await;
                        continue;
                    }

                    // A client that reuses an id while the first job with that
                    // id is still running must never have this scan touch the
                    // in-flight job's .tmp file or its eventual result. Check
                    // this before any parse or give-up path, so a malformed
                    // or invalid duplicate cannot bypass the hold. Leave the
                    // duplicate in the inbox untouched: it is handled on a
                    // later scan, once the in-flight id has been removed.
                    if in_flight
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .contains(id)
                    {
                        continue;
                    }
                }

                // Only a regular file is a job. file_type() does not follow
                // symlinks, so a symlink or a FIFO is skipped without being
                // opened (a FIFO open would block the bridge forever).
                match entry.file_type().await {
                    Ok(ft) if ft.is_file() => {}
                    Ok(_) => {
                        first_seen.remove(&key);
                        let reason = "inbox entry is not a regular file".to_owned();
                        reject_entry(&path, &key, job_id.as_deref(), reason, outbox, given_up)
                            .await;
                        continue;
                    }
                    Err(e) => {
                        handle_unready(
                            &path,
                            &key,
                            job_id.as_deref(),
                            format!("read failed: {e}"),
                            UnreadyKind::Io,
                            outbox,
                            first_seen,
                            given_up,
                        )
                        .await;
                        continue;
                    }
                }

                let bytes = match read_job_file(&path).await {
                    Ok(b) => b,
                    Err(ReadError::TooLarge(len)) => {
                        first_seen.remove(&key);
                        let reason =
                            format!("job file is {len} bytes, over the {MAX_JOB_BYTES} byte limit");
                        reject_entry(&path, &key, job_id.as_deref(), reason, outbox, given_up)
                            .await;
                        continue;
                    }
                    Err(ReadError::ForeignOwner(uid)) => {
                        first_seen.remove(&key);
                        let reason = format!("job file is owned by uid {uid}, not by this user");
                        eprintln!("turbofig bridge: {reason}: {path:?}");
                        reject_entry(&path, &key, job_id.as_deref(), reason, outbox, given_up)
                            .await;
                        continue;
                    }
                    Err(ReadError::Io(e)) => {
                        handle_unready(
                            &path,
                            &key,
                            job_id.as_deref(),
                            format!("read failed: {e}"),
                            UnreadyKind::Io,
                            outbox,
                            first_seen,
                            given_up,
                        )
                        .await;
                        continue;
                    }
                };

                let raw: serde_json::Value = match serde_json::from_slice(&bytes) {
                    Ok(v) => v,
                    Err(e) => {
                        handle_unready(
                            &path,
                            &key,
                            job_id.as_deref(),
                            format!("malformed JSON: {e}"),
                            UnreadyKind::Parse,
                            outbox,
                            first_seen,
                            given_up,
                        )
                        .await;
                        continue;
                    }
                };
                drop(bytes);

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
                                    "turbofig bridge: could not remove invalid job {path:?}: {e}"
                                );
                            }
                        }
                        if let Some(id) = job_id.as_deref() {
                            delete_stale_result(outbox, id).await;
                            write_result(outbox, id, invalid_job_result(&e)).await;
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
                eprintln!("turbofig bridge: read_dir entry error: {e}");
                break;
            }
        }
    }

    // Prune first_seen/given_up entries for files that disappeared between
    // scans, so a later file reusing the same name starts fresh.
    first_seen.retain(|k, _| seen_this_scan.contains(k));
    given_up.retain(|k| seen_this_scan.contains(k));
}

/// Why an inbox entry is unready: an I/O failure (a retry can succeed) or a
/// malformed file (the client must fix the job).
#[derive(Clone, Copy, PartialEq, Eq)]
enum UnreadyKind {
    Io,
    Parse,
}

/// Apply the first-seen/grace/give-up policy to one unready (unreadable or
/// unparseable) inbox entry.
#[allow(clippy::too_many_arguments)]
async fn handle_unready(
    path: &Path,
    key: &str,
    job_id: Option<&str>,
    reason: String,
    kind: UnreadyKind,
    outbox: &Path,
    first_seen: &mut HashMap<String, Instant>,
    given_up: &mut HashSet<String>,
) {
    match first_seen.get(key) {
        Some(seen_at) if seen_at.elapsed() >= PARSE_GRACE => {
            first_seen.remove(key);
            match give_up_on(path, job_id, reason).await {
                GiveUp::WriteError(id, reason) => {
                    let result = if kind == UnreadyKind::Io {
                        io_error_result(&reason)
                    } else {
                        invalid_job_result(&reason)
                    };
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
/// A stale `.tmp` (or a symlink planted at that name) is unlinked first, and
/// the new `.tmp` opens with `create_new` (O_EXCL, mode 0600), so the write
/// never goes through a symlink. A failure removes the leftover .tmp.
/// The whole sequence runs in one blocking-pool task (one hop, not five).
async fn write_result(outbox: &Path, job_id: &str, result: serde_json::Value) {
    let out_tmp = outbox.join(format!("{job_id}.json.tmp"));
    let out_final = outbox.join(format!("{job_id}.json"));
    let bytes = result.to_string().into_bytes();
    let outcome =
        tokio::task::spawn_blocking(move || write_result_blocking(&out_tmp, &out_final, &bytes))
            .await;
    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            eprintln!("turbofig bridge: write failed for {job_id}: {e}");
            write_failure_notice(outbox, job_id, &e).await;
        }
        Err(e) => eprintln!("turbofig bridge: write task failed for {job_id}: {e}"),
    }
}

/// The result could not be written (a full disk is the usual cause). Say so
/// in the log, then try one tiny error result: it can still fit where the
/// real result did not, and it turns a ~30 s wait for `started_unknown`
/// into an answer that names the cause.
async fn write_failure_notice(outbox: &Path, job_id: &str, e: &std::io::Error) {
    let full = e.raw_os_error() == Some(libc::ENOSPC);
    if full {
        eprintln!(
            "turbofig bridge: DISK FULL: cannot write results to {}. Free disk space.",
            outbox.display()
        );
    }
    let cause = if full {
        "the disk is full".to_owned()
    } else {
        e.to_string()
    };
    let notice = serde_json::json!({
        "ok": false,
        "code": "bridge_error",
        "error": format!(
            "the daemon could not write the result file ({cause}); the job may have run, check before resending"
        ),
    });
    let out_tmp = outbox.join(format!("{job_id}.json.tmp"));
    let out_final = outbox.join(format!("{job_id}.json"));
    let bytes = notice.to_string().into_bytes();
    let retried =
        tokio::task::spawn_blocking(move || write_result_blocking(&out_tmp, &out_final, &bytes))
            .await;
    if !matches!(retried, Ok(Ok(()))) {
        eprintln!("turbofig bridge: could not write an error result for {job_id} either");
    }
}

/// Create the `.tmp` with `create_new` and mode 0600. On `AlreadyExists`
/// (a directory at that name, or a racing writer) unlink and retry once.
fn create_tmp(out_tmp: &Path) -> std::io::Result<std::fs::File> {
    let open = || {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(out_tmp)
    };
    match open() {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            remove_any(out_tmp);
            open()
        }
        other => other,
    }
}

/// Remove a file, a symlink (the link itself) or an empty directory at `path`.
fn remove_any(path: &Path) {
    // remove_file unlinks a symlink itself, never its target. The normal path
    // is NotFound: no further syscall then.
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {
            // remove_dir removes an EMPTY directory only. A non-empty one
            // stays, and the write that follows fails and is logged.
            if std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
                let _ = std::fs::remove_dir(path);
            }
        }
    }
}

fn write_result_blocking(out_tmp: &Path, out_final: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    remove_any(out_tmp);
    let mut file = create_tmp(out_tmp)?;
    let written = file.write_all(bytes).and_then(|()| file.flush());
    drop(file);
    let outcome = written.and_then(|()| std::fs::rename(out_tmp, out_final));
    if outcome.is_err() {
        let _ = std::fs::remove_file(out_tmp);
    }
    outcome
}

/// File name of the helper doc inside the bridge dir.
const HELPER_DOC_NAME: &str = "tf-api.md";

/// The helper API doc, embedded so an installed user (no repo checkout) can
/// read it from the bridge dir.
const HELPER_DOC: &str = include_str!("../../../helpers/tf-api.md");

/// Write the embedded helper doc to `<dir>/tf-api.md` on every bridge start,
/// so the copy always matches the daemon version. Atomic: write a `.tmp`
/// with `create_new` (mode 0600), then rename over the old copy.
async fn write_helper_doc(dir: &Path) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let tmp = dir.join(format!("{HELPER_DOC_NAME}.tmp"));
    let dest = dir.join(HELPER_DOC_NAME);
    let _ = tokio::fs::remove_file(&tmp).await;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .await?;
    let written = match file.write_all(HELPER_DOC.as_bytes()).await {
        Ok(()) => file.flush().await,
        Err(e) => Err(e),
    };
    drop(file);
    let result = match written {
        Ok(()) => tokio::fs::rename(&tmp, &dest).await,
        Err(e) => Err(e),
    };
    if result.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
    }
    result
}

/// Error result for a job file the bridge rejects before it runs.
fn invalid_job_result(error: &str) -> serde_json::Value {
    serde_json::json!({"ok": false, "error": error, "code": "invalid_job"})
}

/// Error result for an I/O failure on a job file (EMFILE, EACCES): a retry
/// can succeed, unlike `invalid_job`.
fn io_error_result(error: &str) -> serde_json::Value {
    serde_json::json!({"ok": false, "error": error, "code": "io_error"})
}

/// Why a job file could not be read.
enum ReadError {
    TooLarge(u64),
    ForeignOwner(u32),
    Io(std::io::Error),
}

/// Read a job file in one blocking-pool task: open with O_NOFOLLOW and
/// O_NONBLOCK (a FIFO swapped in after the file-type check then cannot block
/// the open; regular files ignore the flag), check the size of the open
/// handle against `MAX_JOB_BYTES`, then read.
async fn read_job_file(path: &Path) -> Result<Vec<u8>, ReadError> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || read_job_file_blocking(&path))
        .await
        .unwrap_or_else(|e| Err(ReadError::Io(std::io::Error::other(e))))
}

/// Some(owner) when a job file belongs to another account. Only this user
/// may queue a job, so the bridge never runs such a file.
fn foreign_job_owner(owner_uid: u32, euid: u32) -> Option<u32> {
    (owner_uid != euid).then_some(owner_uid)
}

fn read_job_file_blocking(path: &Path) -> Result<Vec<u8>, ReadError> {
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(ReadError::Io)?;
    let meta = file.metadata().map_err(ReadError::Io)?;
    if !meta.is_file() {
        return Err(ReadError::Io(std::io::Error::other("not a regular file")));
    }
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    if let Some(uid) = foreign_job_owner(meta.uid(), euid) {
        return Err(ReadError::ForeignOwner(uid));
    }
    if meta.len() > MAX_JOB_BYTES {
        return Err(ReadError::TooLarge(meta.len()));
    }
    let mut buf = Vec::with_capacity(meta.len() as usize);
    // The cap also bounds a file that grows after the size check.
    (&mut file)
        .take(MAX_JOB_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(ReadError::Io)?;
    if buf.len() as u64 > MAX_JOB_BYTES {
        return Err(ReadError::TooLarge(buf.len() as u64));
    }
    Ok(buf)
}

/// Remove a rejected inbox entry without opening it, write one error result
/// when there is an id, and record the entry so it is never examined again.
async fn reject_entry(
    path: &Path,
    key: &str,
    id: Option<&str>,
    reason: String,
    outbox: &Path,
    given_up: &mut HashSet<String>,
) {
    // remove_file unlinks a symlink or FIFO itself. A real directory needs
    // remove_dir_all.
    if tokio::fs::remove_file(path).await.is_err() {
        let is_real_dir = tokio::fs::symlink_metadata(path)
            .await
            .is_ok_and(|m| m.is_dir());
        if is_real_dir {
            let _ = tokio::fs::remove_dir_all(path).await;
        }
    }
    match id {
        Some(id) => write_result(outbox, id, invalid_job_result(&reason)).await,
        None => eprintln!("turbofig bridge: rejected inbox entry: {reason}"),
    }
    given_up.insert(key.to_owned());
}

/// Reply id for an over-long job id: `<first 100 bytes>.idtoolong`. The
/// suffix keeps the reply from overwriting the result of another job whose
/// id equals the first 100 bytes. The prune still removes `<it>.json`.
fn too_long_reply_id(id: &str) -> String {
    format!("{}.idtoolong", short_id(id))
}

/// First 100 bytes of an over-long id (cut on a char boundary).
fn short_id(id: &str) -> String {
    let mut end = 100.min(id.len());
    while !id.is_char_boundary(end) {
        end -= 1;
    }
    id[..end].to_owned()
}

/// True for a name the daemon itself writes into the outbox: `<id>.json`,
/// `<id>.json.tmp`, or a screenshot `<requestId>-<base36>[-<base36>].png`.
fn is_daemon_outbox_name(name: &str) -> bool {
    let base36 = |p: &str| {
        !p.is_empty()
            && p.bytes()
                .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
    };
    if let Some(stem) = name
        .strip_suffix(".json.tmp")
        .or_else(|| name.strip_suffix(".json"))
    {
        return !stem.is_empty();
    }
    if let Some(stem) = name.strip_suffix(".png") {
        let parts: Vec<&str> = stem.split('-').collect();
        // <requestId: u64>-<base36 millis, 8 to 9 chars>[-<base36 suffix, 1 to 2 chars>]
        let len_ok = |p: &str, lo: usize, hi: usize| (lo..=hi).contains(&p.len()) && base36(p);
        return (parts.len() == 2 || parts.len() == 3)
            && parts[0].parse::<u64>().is_ok()
            && parts[0].bytes().all(|b| b.is_ascii_digit())
            && len_ok(parts[1], 8, 9)
            && parts.get(2).is_none_or(|p| len_ok(p, 1, 2));
    }
    false
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
        // 0o755: group and other can read but not write. A group- or
        // other-writable folder is refused (see secure_fs).
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod dir");

        prepare_dirs(&dir, false)
            .await
            .expect("prepare bridge dirs");

        // A custom TURBOFIG_BRIDGE_DIR can be an existing path the user
        // pointed us at; the daemon does not own its mode.
        assert_eq!(
            mode_bits(&dir),
            0o755,
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

    #[test]
    fn outbox_name_patterns_match_only_daemon_files() {
        assert!(is_daemon_outbox_name("job1.json"));
        assert!(is_daemon_outbox_name("job1.json.tmp"));
        assert!(is_daemon_outbox_name("12-lx3k9a0z.png"));
        assert!(is_daemon_outbox_name("12-lx3k9a0z-1.png"));
        assert!(!is_daemon_outbox_name("thesis.docx"));
        assert!(!is_daemon_outbox_name("photo.png"));
        assert!(!is_daemon_outbox_name("my-photo.png"));
        assert!(!is_daemon_outbox_name(".json"));
        assert!(!is_daemon_outbox_name("notes.txt"));
        assert!(!is_daemon_outbox_name("2024-10-31.png"));
        assert!(!is_daemon_outbox_name("2024-photo.png"));
    }

    #[cfg(unix)]
    fn age_file(path: &Path) {
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open for age");
        f.set_modified(SystemTime::now() - Duration::from_secs(48 * 3600))
            .expect("set mtime");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn prune_deletes_only_old_daemon_owned_regular_files() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let out = tmp.path().join("outbox");
        std::fs::create_dir_all(&out).expect("outbox");
        let out = out.as_path();
        let owned = ["a.json", "a.json.tmp", "5-abcdefgh.png"];
        for n in owned.iter().chain(["thesis.docx"].iter()) {
            std::fs::write(out.join(n), b"x").expect("write");
            age_file(&out.join(n));
        }
        std::fs::write(out.join("fresh.json"), b"x").expect("write fresh");
        // An old symlink with a daemon name must not be followed or removed.
        let target = tmp.path().join("target.txt");
        std::fs::write(&target, b"x").expect("target");
        std::os::unix::fs::symlink(&target, out.join("link.json")).expect("symlink");

        prune_old_outbox(out).await;

        assert!(
            out.join("thesis.docx").exists(),
            "foreign file must survive"
        );
        assert!(out.join("fresh.json").exists(), "fresh file must survive");
        assert!(out.join("link.json").exists(), "symlink must survive");
        assert!(target.exists());
        for n in owned {
            assert!(!out.join(n).exists(), "{n} must be pruned");
        }
    }

    #[test]
    fn prune_guard_needs_a_real_owned_marked_outbox() {
        assert!(outbox_prune_allowed(false, true, 5, 5, true));
        assert!(!outbox_prune_allowed(true, true, 5, 5, true));
        assert!(!outbox_prune_allowed(false, false, 5, 5, true));
        assert!(!outbox_prune_allowed(false, true, 4, 5, true));
        assert!(!outbox_prune_allowed(false, true, 5, 5, false));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn prune_if_safe_skips_unmarked_and_symlinked_outboxes() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let out = tmp.path().join("outbox");
        std::fs::create_dir_all(&out).expect("outbox");
        std::fs::write(out.join("package.json"), b"x").expect("write");
        age_file(&out.join("package.json"));
        prune_outbox_if_safe(&out).await;
        assert!(out.join("package.json").exists(), "no marker: no prune");

        std::fs::write(out.join(crate::secure_fs::OUTBOX_MARKER), b"").expect("marker");
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&out, &link).expect("symlink");
        prune_outbox_if_safe(&link).await;
        assert!(out.join("package.json").exists(), "symlink: no prune");

        prune_outbox_if_safe(&out).await;
        assert!(!out.join("package.json").exists(), "marked: pruned");
        assert!(out.join(crate::secure_fs::OUTBOX_MARKER).exists());
    }

    #[test]
    fn a_foreign_job_owner_is_detected() {
        assert_eq!(foreign_job_owner(501, 502), Some(501));
        assert_eq!(foreign_job_owner(502, 502), None);
    }

    #[test]
    fn too_long_reply_id_is_distinct_and_pruned() {
        let long = format!("{}{}", "b".repeat(100), "c".repeat(140));
        let reply = too_long_reply_id(&long);
        assert_eq!(reply, format!("{}.idtoolong", "b".repeat(100)));
        assert_ne!(reply, "b".repeat(100));
        assert!(is_daemon_outbox_name(&format!("{reply}.json")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_result_never_writes_through_a_planted_symlink() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let outbox = tmp.path().join("outbox");
        std::fs::create_dir_all(&outbox).expect("outbox");
        let victim = tmp.path().join("victim.txt");
        std::fs::write(&victim, b"original").expect("victim");
        std::os::unix::fs::symlink(&victim, outbox.join("j1.json.tmp")).expect("symlink");

        write_result(&outbox, "j1", serde_json::json!({"ok": true})).await;

        assert_eq!(std::fs::read(&victim).expect("read"), b"original");
        let result = std::fs::read_to_string(outbox.join("j1.json")).expect("result");
        assert!(result.contains("\"ok\":true"));
        assert_eq!(mode_bits(&outbox.join("j1.json")), 0o600);
    }

    #[test]
    fn short_id_cuts_on_a_char_boundary() {
        let id = "\u{e9}".repeat(120);
        let short = short_id(&id);
        assert!(short.len() <= 100 && !short.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn helper_doc_is_written_owner_only_and_overwrites_old_copy() {
        let tmp = tempfile::tempdir().expect("temp dir");
        std::fs::write(tmp.path().join("tf-api.md"), b"old version").expect("old");

        write_helper_doc(tmp.path()).await.expect("write doc");

        let doc = tmp.path().join("tf-api.md");
        assert_eq!(std::fs::read_to_string(&doc).expect("read"), HELPER_DOC);
        assert_eq!(mode_bits(&doc), 0o600);
        assert!(!tmp.path().join("tf-api.md.tmp").exists());
    }

    #[test]
    fn outbox_name_pattern_rejects_names_the_daemon_never_writes() {
        assert!(is_daemon_outbox_name("12-lx3k9a0z.png"));
        assert!(is_daemon_outbox_name("12-lx3k9a0z-1.png"));
        assert!(!is_daemon_outbox_name("2024-10-31.png"));
        assert!(!is_daemon_outbox_name("2024-photo.png"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_result_recovers_from_a_directory_at_the_tmp_name() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let outbox = tmp.path().join("outbox");
        std::fs::create_dir_all(outbox.join("j2.json.tmp")).expect("empty dir at tmp");

        write_result(&outbox, "j2", serde_json::json!({"ok": true})).await;

        assert!(outbox.join("j2.json").is_file(), "result must be written");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_result_never_deletes_a_non_empty_directory_at_the_tmp_name() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let outbox = tmp.path().join("outbox");
        std::fs::create_dir_all(outbox.join("j3.json.tmp")).expect("dir at tmp");
        std::fs::write(outbox.join("j3.json.tmp").join("f"), b"x").expect("file in dir");

        write_result(&outbox, "j3", serde_json::json!({"ok": true})).await;

        assert!(
            outbox.join("j3.json.tmp").join("f").is_file(),
            "must survive"
        );
        assert!(!outbox.join("j3.json").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn read_job_file_does_not_block_on_a_fifo() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let fifo = tmp.path().join("f.json");
        let c = std::ffi::CString::new(fifo.to_str().expect("utf8")).expect("cstring");
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let r = tokio::time::timeout(Duration::from_secs(5), read_job_file(&fifo))
            .await
            .expect("open of a FIFO must not block");
        assert!(r.is_err());
    }
}
