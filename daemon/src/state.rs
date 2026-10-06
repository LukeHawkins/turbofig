//! Shared daemon state: the connection registry, session pairing, and the
//! pending-request map that ties a tool call to the plugin reply that
//! resolves it.

use crate::config::{bridge_dir_from_env, request_timeout_from_env};
use crate::token::random_token_hex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU64, AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

/// Longest a plugin-supplied display name may be after sanitizing.
/// A long or control-character-laden name must never reach status JSON
/// unbounded: the plugin side is untrusted input.
const MAX_NAME_LEN: usize = 200;

/// Returns the reopen-the-plugin warning text when `plugin_version` is
/// non-empty and differs from the daemon's own `CARGO_PKG_VERSION`. An empty
/// `plugin_version` (not yet announced, or an older plugin build that never
/// sent `pluginVersion`) never warns: there is nothing to compare against.
/// Shared by `turbofig_status` (via `named_connections_json`) and `/health`,
/// so the two surfaces never drift on wording.
pub(crate) fn version_mismatch_warning(plugin_version: &str) -> Option<String> {
    if plugin_version.is_empty() || plugin_version == env!("CARGO_PKG_VERSION") {
        return None;
    }
    Some("reopen the turbofig plugin in Figma".to_owned())
}

/// Strip control characters and cap length on a plugin-supplied name.
fn sanitize_name(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_LEN)
        .collect()
}

/// How long a session-to-file pairing may live before it is pruned.
const SESSION_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Maximum number of session pairings kept at once. Past this, the oldest
/// pairings are evicted on the next insert. Unbounded growth here would be a
/// slow memory leak across a long-running daemon with many MCP sessions.
const SESSION_CAP: usize = 1000;

/// Pick a request-id counter start in `[1, 2^40)`.
///
/// A fixed start of 1 on every daemon restart lets a late RESULT frame from a
/// previous run (or a leftover bridge outbox file) collide with a freshly
/// issued id. A random start makes that collision astronomically unlikely.
/// `2^40` keeps every id a JS-safe integer even after billions of requests.
/// `RandomState`'s SipHash keys are seeded from the OS CSPRNG at process
/// start, so hashing a fixed, empty input still yields an unpredictable
/// `u64` without adding a `rand` dependency.
fn random_counter_start() -> u64 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let r = RandomState::new().build_hasher().finish();
    1 + (r % ((1u64 << 40) - 1))
}

/// An active plugin connection.
/// Holds the file key, the document name, and a sender for outbound JSON messages.
pub struct PluginConn {
    pub file_key: String,
    pub name: String,
    pub tx: mpsc::UnboundedSender<String>,
    /// The plugin's own reported version (from FILE_INFO's `pluginVersion`).
    /// Empty when not yet announced, or when an older plugin build never
    /// sent it. Used only to flag a version mismatch in status output; see
    /// `version_mismatch_warning`.
    pub plugin_version: String,
}

/// Shared daemon state passed to the MCP HTTP server, the WS server, and the bridge.
pub struct AppState {
    /// Registry of all active WebSocket connections.
    /// One entry per connected plugin. The file_key is empty until FILE_INFO
    /// arrives. Two live connections may hold the same non-empty file_key at
    /// once (a reconnect, or a second window on the same file): neither is
    /// evicted, so neither one's in-flight jobs are ever cancelled by the
    /// other connecting. `connections_named` is the routing-facing view that
    /// dedupes a shared file_key down to the newest (highest conn_id) entry.
    connections: Mutex<HashMap<u64, PluginConn>>,
    /// Allocates stable connection IDs.
    conn_counter: AtomicU64,
    /// MCP session -> (file_key, last-touched) pairing recorded by resolve_route.
    /// Pruned by age (SESSION_TTL) and by count (SESSION_CAP) on insert.
    sessions: Mutex<HashMap<String, (String, Instant)>>,
    /// Pending tool-call requests waiting for a RESULT frame from the plugin.
    /// Value is (conn_id, oneshot sender). The conn_id lets one file closing
    /// cancel only its own in-flight requests, and lets `resolve` refuse a
    /// RESULT whose connection does not own the pending entry.
    pending: Mutex<HashMap<u64, (u64, oneshot::Sender<Value>)>>,
    /// Monotonically increasing request-ID counter, started at a random value.
    counter: AtomicU64,
    /// How long to wait for a plugin reply before returning a timeout response.
    pub request_timeout: Duration,
    /// Directory where screenshot PNGs are written in file mode, if configured.
    screenshot_dir: Option<std::path::PathBuf>,
    /// The WebSocket pairing token. The WS upgrade in `ws.rs` requires a
    /// matching `token` query parameter: see `token.rs` for why. `new`/
    /// `with_timeout` generate this in memory via `random_token_hex`, so
    /// building an `AppState` in a test never touches `~/.turbofig/token`.
    /// Only `main.rs` loads the real, persisted token (via `token::ensure_token`)
    /// and passes it to `new_with_token`.
    token: String,
    /// When this `AppState` was constructed, i.e. daemon startup. Used only
    /// by `/health` and `turbofig_status`'s uptime figure.
    started_at: Instant,
    /// Set by the supervised-restart loop (`main.rs`, `TURBOFIG_SUPERVISED=1`)
    /// once it detects a Homebrew upgrade. While true, `resolve_route`
    /// refuses every new job with `RouteError::Draining`, so the old process
    /// accepts no more work while it waits for in-flight jobs to finish
    /// before exiting for launchd to start the new binary.
    draining: std::sync::atomic::AtomicBool,
    /// Set once a `stop` has been requested, by `/control`'s `stop` action
    /// (`control.rs`). A `stop` always overrides a restart already in
    /// progress: both the `/control` background exit task and the
    /// supervised-restart loop (`main.rs`) read this right before they call
    /// `std::process::exit`, and exit 0 whenever it is set, even if a
    /// restart (not a stop) is the one that is actually draining. Without
    /// this, a `stop` that lands while a restart's drain is already under
    /// way would still exit with the restart's non-zero code, so launchd
    /// would restart the daemon straight back up even though `turbofig
    /// stop` reported success.
    stop_requested: std::sync::atomic::AtomicBool,
    /// Count of whole tool calls (MCP and bridge) currently in progress, from
    /// entry to the final response (MCP) or result write (bridge). See
    /// `begin_job`/`JobGuard`. This is intentionally not the `pending` map's
    /// length: `pending` only covers the span waiting for a plugin reply, so
    /// it misses work after the reply arrives (a screenshot resize, a file
    /// write, building the HTTP response) and any job between `resolve_route`
    /// and the `pending` insert.
    job_counter: Arc<AtomicUsize>,
    /// Path to the `plugin-seen` marker file (see `plugin_files::mark_plugin_seen`),
    /// set only by `new_with_token`, the constructor the real daemon binary
    /// uses. `None` for every other constructor (`new`, `with_timeout`), so
    /// an in-process test building an `AppState` directly never writes to a
    /// real `~/.turbofig` on a successful plugin WS authentication.
    plugin_seen_path: Option<std::path::PathBuf>,
}

/// RAII guard returned by `AppState::begin_job`. Increments the shared job
/// counter on creation, decrements it on drop (including an early return or
/// a panic unwind), so a whole tool call is counted for its entire lifetime
/// without any call site having to remember to decrement by hand.
pub struct JobGuard {
    counter: Arc<AtomicUsize>,
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

impl AppState {
    /// Private constructor. All public constructors delegate here.
    fn build(
        timeout: Duration,
        screenshot_dir: Option<std::path::PathBuf>,
        token: String,
        plugin_seen_path: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            connections: Mutex::new(HashMap::new()),
            conn_counter: AtomicU64::new(1),
            sessions: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            counter: AtomicU64::new(random_counter_start()),
            request_timeout: timeout,
            screenshot_dir,
            token,
            started_at: Instant::now(),
            draining: std::sync::atomic::AtomicBool::new(false),
            stop_requested: std::sync::atomic::AtomicBool::new(false),
            job_counter: Arc::new(AtomicUsize::new(0)),
            plugin_seen_path,
        }
    }

    /// Create a new AppState. Reads the timeout from TURBOFIG_REQUEST_TIMEOUT_MS.
    /// Sets screenshot_dir to `~/.turbofig/outbox`.
    /// Generates a random in-memory pairing token: never touches disk. The
    /// daemon binary uses `new_with_token` instead, with the real persisted
    /// token, so the production WS upgrade check matches the one file-bridge
    /// clients and the plugin UI read from `~/.turbofig/token`.
    pub fn new() -> Self {
        Self::build(
            request_timeout_from_env(),
            Some(bridge_dir_from_env().join("outbox")),
            random_token_hex(),
            None,
        )
    }

    /// Create a new AppState with an explicit request timeout.
    /// Use this in tests to set a short timeout without touching global env.
    /// Sets screenshot_dir to None. Generates a random in-memory pairing token.
    pub fn with_timeout(d: Duration) -> Self {
        Self::build(d, None, random_token_hex(), None)
    }

    /// Create a new AppState exactly like `new()`, but with an explicit
    /// pairing token rather than a freshly generated one, and `home`, the
    /// real bridge directory. The daemon binary uses this with the token
    /// `token::ensure_token` persisted to `~/.turbofig/token`, so the WS
    /// upgrade check matches what a client reads from that file, and so the
    /// first successful plugin authentication writes `<home>/plugin-seen`
    /// (see `plugin_seen_path` and `ws.rs`). No other constructor sets
    /// `plugin_seen_path`, so a test building an `AppState` directly never
    /// writes that marker to a real home directory.
    pub fn new_with_token(token: String, home: &std::path::Path) -> Self {
        Self::build(
            request_timeout_from_env(),
            Some(bridge_dir_from_env().join("outbox")),
            token,
            Some(home.join("plugin-seen")),
        )
    }

    /// Return a clone of the screenshot output directory, if configured.
    pub fn screenshot_dir(&self) -> Option<std::path::PathBuf> {
        self.screenshot_dir.clone()
    }

    /// Returns the WebSocket pairing token. Never logged, never exposed in
    /// status output: only compared, in constant time, against a connecting
    /// client's `token` query parameter (`ws.rs`).
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Seconds since this `AppState` was constructed, i.e. daemon uptime.
    pub fn uptime_seconds(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// Writes the `plugin-seen` marker on the first successful plugin WS
    /// authentication, if this `AppState` was built with a real home
    /// directory (`new_with_token`). A no-op for every other constructor, so
    /// an in-process test never touches a real `~/.turbofig`. Logs a warning
    /// to stderr on a write failure rather than failing the connection: a
    /// plugin must still be able to connect even if the marker cannot be
    /// written.
    pub(crate) fn mark_plugin_seen(&self) {
        let Some(path) = self.plugin_seen_path.as_deref() else {
            return;
        };
        let home = match path.parent() {
            Some(home) => home,
            None => return,
        };
        if let Err(e) = crate::plugin_files::mark_plugin_seen(home) {
            eprintln!("Turbofig daemon: failed to write the plugin-seen marker: {e}");
        }
    }

    /// Marks the daemon as draining (true) or accepting work again (false).
    /// See the `draining` field doc for who sets this and why.
    pub fn set_draining(&self, value: bool) {
        self.draining.store(value, Ordering::SeqCst);
    }

    /// True once `set_draining(true)` has been called. `resolve_route`
    /// checks this before resolving any new job.
    pub fn is_draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }

    /// Atomically transitions draining from false to true. Returns true only
    /// for the caller that actually made the transition; a caller that finds
    /// draining already true gets false back and must not start a second
    /// drain-then-exit sequence of its own.
    ///
    /// `/control` (`control.rs`) uses this so two concurrent restart/stop
    /// requests (e.g. two proxies racing a version-handoff restart) never
    /// both run `wait_for_drain` and schedule their own exit: exactly one
    /// request does the real work, and the other's response just reports
    /// that a restart was already in progress.
    pub(crate) fn try_begin_draining(&self) -> bool {
        self.draining
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Records that a `stop` has been requested. Idempotent and independent
    /// of `try_begin_draining`: a `stop` that arrives while a restart is
    /// already draining still calls this, so the pending exit (whoever
    /// scheduled it) picks up the override. See the `stop_requested` field
    /// doc for why this must win over a restart's exit code.
    pub fn request_stop(&self) {
        self.stop_requested.store(true, Ordering::SeqCst);
    }

    /// True once `request_stop` has been called. Checked right before every
    /// exit point that would otherwise use a restart's exit code
    /// (`/control`'s background task and the supervised-restart loop).
    pub fn stop_requested(&self) -> bool {
        self.stop_requested.load(Ordering::SeqCst)
    }

    /// Number of whole tool calls (MCP and bridge) currently in progress.
    /// Used by the supervised-restart loop to know when it is safe to exit.
    pub fn jobs_in_flight(&self) -> usize {
        self.job_counter.load(Ordering::SeqCst)
    }

    /// Marks the start of one whole tool call (an MCP tool invocation or a
    /// bridge job). Call this at the top of the call, before routing; hold
    /// the returned guard until the final response or result write is done,
    /// then let it drop. See `job_counter`'s field doc for why this counts
    /// more than `pending`.
    pub fn begin_job(&self) -> JobGuard {
        self.job_counter.fetch_add(1, Ordering::SeqCst);
        JobGuard {
            counter: self.job_counter.clone(),
        }
    }

    /// Returns true when `conn_id` is still a live, registered connection.
    /// Call this before acting on any inbound frame tagged with a conn_id:
    /// a message that arrives after its connection has already closed must
    /// be ignored, not applied to a stale or reused id.
    pub(crate) fn connection_exists(&self, conn_id: u64) -> bool {
        self.connections
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&conn_id)
    }

    /// Register a new WebSocket connection. Returns a stable connection ID.
    /// The file_key and name start empty and are set when FILE_INFO arrives.
    pub(crate) fn add_connection(&self, tx: mpsc::UnboundedSender<String>) -> u64 {
        let conn_id = self.conn_counter.fetch_add(1, Ordering::Relaxed);
        // Recover the guard when a previous holder panicked. The map is still
        // usable. A poisoned mutex must not cascade into routing failures for
        // all connected files.
        let mut guard = self.connections.lock().unwrap_or_else(|e| e.into_inner());
        guard.insert(
            conn_id,
            PluginConn {
                file_key: String::new(),
                name: String::new(),
                tx,
                plugin_version: String::new(),
            },
        );
        conn_id
    }

    /// Update the file_key and name for an existing connection.
    /// Call this when FILE_INFO arrives.
    ///
    /// Sanitizes the plugin-supplied name (strips control characters, caps
    /// length) before it is ever stored or returned in status output: the
    /// plugin side is untrusted input.
    ///
    /// Two live connections may hold the same non-empty file_key at once (a
    /// reconnect, or a second window on the same file re-announcing after a
    /// rename): neither evicts the other. `connections_named` resolves which
    /// one routing prefers. A conn_id no longer in the registry (the socket
    /// already closed) is a silent no-op: a message from a dead connection
    /// must never resurrect an entry.
    #[cfg(test)]
    pub(crate) fn set_connection_info(&self, conn_id: u64, file_key: String, name: String) {
        self.set_connection_info_with_version(conn_id, file_key, name, String::new());
    }

    /// Same as `set_connection_info`, plus the plugin's self-reported
    /// version from FILE_INFO's `pluginVersion` field. The real WS dispatch
    /// path (`ws.rs`) calls this one; `set_connection_info` stays as a
    /// plain two-field convenience for the many existing tests that do not
    /// care about plugin version.
    pub(crate) fn set_connection_info_with_version(
        &self,
        conn_id: u64,
        file_key: String,
        name: String,
        plugin_version: String,
    ) {
        let name = sanitize_name(&name);
        let mut guard = self.connections.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(conn) = guard.get_mut(&conn_id) {
            conn.file_key = file_key;
            conn.name = name;
            conn.plugin_version = plugin_version;
        }
    }

    /// Remove a connection from the registry. Call this when the socket closes.
    pub(crate) fn remove_connection(&self, conn_id: u64) {
        let mut guard = self.connections.lock().unwrap_or_else(|e| e.into_inner());
        guard.remove(&conn_id);
    }

    /// Return all connections as (conn_id, file_key, name) tuples.
    pub fn list_connections(&self) -> Vec<(u64, String, String)> {
        let guard = self.connections.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .iter()
            .map(|(id, c)| (*id, c.file_key.clone(), c.name.clone()))
            .collect()
    }

    /// Return the sole connection's (file_key, name) when exactly one exists.
    /// Returns None when there are zero or more than one connections.
    /// Kept for existing single-plugin WebSocket tests.
    pub fn plugin_snapshot(&self) -> Option<(String, String)> {
        let guard = self.connections.lock().unwrap_or_else(|e| e.into_inner());
        if guard.len() == 1 {
            guard
                .values()
                .next()
                .map(|c| (c.file_key.clone(), c.name.clone()))
        } else {
            None
        }
    }

    /// Return named connections (non-empty file_key only), deduped by
    /// file_key: when two live connections share a key (a reconnect, or a
    /// second window on the same file), only the newest (highest conn_id)
    /// survives into this list, so routing and the Ambiguous/status lists
    /// never show the same file twice. Sorted by (file_key, conn_id) for
    /// deterministic output.
    pub(crate) fn connections_named(
        &self,
    ) -> Vec<(u64, mpsc::UnboundedSender<String>, String, String, String)> {
        let connections = self.connections.lock().unwrap_or_else(|e| e.into_inner());
        let mut newest: HashMap<String, (u64, mpsc::UnboundedSender<String>, String, String)> =
            HashMap::new();
        for (id, c) in connections.iter() {
            if c.file_key.is_empty() {
                continue;
            }
            let keep = match newest.get(&c.file_key) {
                Some((existing_id, _, _, _)) => *id > *existing_id,
                None => true,
            };
            if keep {
                newest.insert(
                    c.file_key.clone(),
                    (*id, c.tx.clone(), c.name.clone(), c.plugin_version.clone()),
                );
            }
        }
        let mut v: Vec<_> = newest
            .into_iter()
            .map(|(fk, (id, tx, name, plugin_version))| (id, tx, fk, name, plugin_version))
            .collect();
        v.sort_by(|a, b| a.2.cmp(&b.2).then(a.0.cmp(&b.0)));
        v
    }

    /// Return all named connections as JSON objects for status and error
    /// responses. Dedupes a shared file_key the same way `connections_named`
    /// does, so status output never lists the same file twice. Each entry
    /// carries `pluginVersion` (empty string when not yet announced) and, if
    /// it differs from the daemon's own `CARGO_PKG_VERSION`, a `warning`
    /// telling the caller to reopen the plugin. Never includes the pairing
    /// token: nothing here reads `AppState::token()`.
    pub(crate) fn named_connections_json(&self) -> Vec<Value> {
        self.connections_named()
            .into_iter()
            .map(|(_, _, fk, name, plugin_version)| {
                let mut obj = serde_json::json!({
                    "fileKey": fk,
                    "name": name,
                    "pluginVersion": plugin_version,
                });
                if let Some(warning) = version_mismatch_warning(&plugin_version) {
                    obj["warning"] = Value::String(warning);
                }
                obj
            })
            .collect()
    }

    /// Look up the file key paired to an MCP session, if any and not expired.
    /// An expired pairing is pruned on read. Public so an integration test can
    /// poll for "the pairing landed" instead of a fixed sleep.
    pub fn session_lookup(&self, sid: &str) -> Option<String> {
        let mut guard = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        match guard.get(sid) {
            Some((fk, at)) if at.elapsed() < SESSION_TTL => Some(fk.clone()),
            Some(_) => {
                guard.remove(sid);
                None
            }
            None => None,
        }
    }

    /// Record (or refresh) a session-to-file pairing.
    /// Prunes expired pairings, then evicts the oldest over SESSION_CAP.
    pub(crate) fn session_insert(&self, sid: &str, file_key: &str) {
        let mut guard = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        guard.retain(|_, (_, at)| now.duration_since(*at) < SESSION_TTL);
        if guard.len() >= SESSION_CAP && !guard.contains_key(sid) {
            if let Some(oldest_key) = guard
                .iter()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(k, _)| k.clone())
            {
                guard.remove(&oldest_key);
            }
        }
        guard.insert(sid.to_owned(), (file_key.to_owned(), now));
    }

    /// Remove a session pairing, e.g. when its target file key no longer resolves.
    pub(crate) fn session_remove(&self, sid: &str) {
        let mut guard = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        guard.remove(sid);
    }

    /// Register a pending request tagged with the owning connection, but only
    /// if `conn_id` is still present in the registry.
    ///
    /// Holds the connections lock for the whole check-then-register sequence,
    /// so a socket close racing this call cannot land between "the connection
    /// looked live" and "the pending entry was registered": `remove_connection`
    /// takes the same lock, so it either removed the connection before we
    /// checked (we return None) or must wait for us to finish (and then
    /// `cancel_pending_for_conn` correctly cancels the entry we just made).
    ///
    /// Returns the allocated request id and a receiver that resolves when the
    /// plugin replies, or `None` if the connection is already gone.
    pub(crate) fn register_pending_if_connected(
        &self,
        conn_id: u64,
    ) -> Option<(u64, oneshot::Receiver<Value>)> {
        let connections = self.connections.lock().unwrap_or_else(|e| e.into_inner());
        if !connections.contains_key(&conn_id) {
            return None;
        }
        let id = self.counter.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending.insert(id, (conn_id, tx));
        Some((id, rx))
    }

    /// Resolve a pending request with the plugin's response value.
    ///
    /// Only resolves when `conn_id` matches the connection the request was
    /// registered against. A RESULT frame claiming an id owned by a different
    /// connection is dropped silently: without this check, any connection
    /// (including a null-origin WebSocket from an untrusted web page) could
    /// guess or brute-force a live request id and forge its result.
    /// Silently ignores unknown request IDs.
    pub(crate) fn resolve(&self, id: u64, conn_id: u64, value: Value) {
        let mut guard = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if let std::collections::hash_map::Entry::Occupied(entry) = guard.entry(id) {
            if entry.get().0 == conn_id {
                let (_, tx) = entry.remove();
                let _ = tx.send(value);
            }
            // Owned by a different connection: leave it in place for its
            // rightful owner, and drop this forged reply.
        }
    }

    /// Remove and drop the oneshot sender for `id`.
    /// Call this when a request times out to prevent a pending-map leak.
    /// Silently ignores unknown IDs.
    pub(crate) fn cancel_pending(&self, id: u64) {
        let mut guard = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        guard.remove(&id);
    }

    /// Drop every pending sender whose conn_id matches.
    /// Call this when a socket closes so only that file's in-flight requests fail.
    /// Other files' pending requests are not affected.
    pub(crate) fn cancel_pending_for_conn(&self, conn_id: u64) {
        let mut guard = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        guard.retain(|_, (cid, _)| *cid != conn_id);
    }

    /// Number of currently pending requests. Lets a test observe that a
    /// cancelled or dropped call actually cleared its pending entry, or that
    /// an in-flight request has been registered, without the pending map
    /// itself being part of the day-to-day public API. Public (not
    /// `cfg(test)`-gated) so an integration test in `daemon/tests/` can poll
    /// it too, not only a unit test compiled inside this crate.
    pub fn pending_len(&self) -> usize {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn connections_lock_recovers_from_poison() {
        let state = Arc::new(AppState::with_timeout(Duration::from_millis(100)));
        let state_for_thread = state.clone();

        let handle = std::thread::spawn(move || {
            let _guard = state_for_thread
                .connections
                .lock()
                .expect("initial lock in poison thread");
            panic!("intentional poison for test");
        });
        let _ = handle.join();

        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let conn_id = state.add_connection(tx);
        assert!(conn_id > 0, "add_connection must succeed after poison");

        let conns = state.list_connections();
        assert_eq!(
            conns.len(),
            1,
            "list_connections must see the added connection"
        );
    }

    #[tokio::test]
    async fn cancel_pending_for_conn_drops_only_that_conns_entries() {
        let state = Arc::new(AppState::with_timeout(Duration::from_millis(100)));
        let (tx1, _rx1) = mpsc::unbounded_channel::<String>();
        let (tx2, _rx2) = mpsc::unbounded_channel::<String>();
        let conn1 = state.add_connection(tx1);
        let conn2 = state.add_connection(tx2);
        state.set_connection_info(conn1, "fk1".to_owned(), "File 1".to_owned());
        state.set_connection_info(conn2, "fk2".to_owned(), "File 2".to_owned());

        let (id1, rx1) = state
            .register_pending_if_connected(conn1)
            .expect("conn1 live");
        let (id2, rx2) = state
            .register_pending_if_connected(conn2)
            .expect("conn2 live");

        state.cancel_pending_for_conn(conn1);

        assert!(rx1.await.is_err(), "rx1 must be cancelled");

        state.resolve(id2, conn2, json!({"ok": true}));
        let val = rx2.await.expect("rx2 must still resolve");
        assert_eq!(val["ok"], json!(true));
        let _ = id1;
    }

    #[test]
    fn two_connections_coexist_in_registry() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx1, _rx1) = mpsc::unbounded_channel::<String>();
        let (tx2, _rx2) = mpsc::unbounded_channel::<String>();
        let conn1 = state.add_connection(tx1);
        let conn2 = state.add_connection(tx2);
        state.set_connection_info(conn1, "fk1".to_owned(), "File 1".to_owned());
        state.set_connection_info(conn2, "fk2".to_owned(), "File 2".to_owned());

        let connections = state.list_connections();
        assert_eq!(connections.len(), 2, "both connections must be listed");
        let fks: Vec<&str> = connections.iter().map(|(_, fk, _)| fk.as_str()).collect();
        assert!(fks.contains(&"fk1") && fks.contains(&"fk2"));
    }

    #[tokio::test]
    async fn two_connections_on_the_same_file_key_both_stay_registered() {
        // Two windows on the same file, or a reconnect before the old socket
        // dies: neither connection evicts the other, so a later FILE_INFO
        // from either one (e.g. after both re-announce on a file rename)
        // never knocks the other out of the registry.
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx1, _rx1) = mpsc::unbounded_channel::<String>();
        let (tx2, _rx2) = mpsc::unbounded_channel::<String>();
        let conn1 = state.add_connection(tx1);
        state.set_connection_info(conn1, "dup".to_owned(), "First".to_owned());

        let (_id, mut pending_rx) = state
            .register_pending_if_connected(conn1)
            .expect("conn1 live");

        let conn2 = state.add_connection(tx2);
        state.set_connection_info(conn2, "dup".to_owned(), "Second".to_owned());

        let connections = state.list_connections();
        assert_eq!(
            connections.len(),
            2,
            "both connections must stay registered"
        );

        assert!(
            pending_rx.try_recv().is_err(),
            "conn1's in-flight request must not be resolved or cancelled yet"
        );
    }

    #[tokio::test]
    async fn connecting_a_second_window_on_the_same_file_key_does_not_cancel_the_olders_in_flight_jobs(
    ) {
        let state = AppState::with_timeout(Duration::from_millis(200));
        let (tx1, _rx1) = mpsc::unbounded_channel::<String>();
        let (tx2, _rx2) = mpsc::unbounded_channel::<String>();
        let conn1 = state.add_connection(tx1);
        state.set_connection_info(conn1, "dup".to_owned(), "First".to_owned());
        let (id, rx) = state
            .register_pending_if_connected(conn1)
            .expect("conn1 live");

        let conn2 = state.add_connection(tx2);
        state.set_connection_info(conn2, "dup".to_owned(), "Second".to_owned());

        // The older connection's in-flight job must still be answerable: no
        // eviction ever cancelled it.
        state.resolve(id, conn1, json!({"ok": true}));
        let val = rx
            .await
            .expect("conn1's in-flight job must not be cancelled");
        assert_eq!(val["ok"], json!(true));
    }

    #[test]
    fn connections_named_dedupes_by_file_key_keeping_the_newest_conn_id() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx1, _rx1) = mpsc::unbounded_channel::<String>();
        let (tx2, _rx2) = mpsc::unbounded_channel::<String>();
        let conn1 = state.add_connection(tx1);
        let conn2 = state.add_connection(tx2);
        // The older connection (lower conn_id) re-announces after the newer
        // one: call order must not matter, only conn_id.
        state.set_connection_info(conn2, "dup".to_owned(), "Second".to_owned());
        state.set_connection_info(conn1, "dup".to_owned(), "First".to_owned());

        let named = state.connections_named();
        assert_eq!(named.len(), 1, "a shared file_key must dedupe to one entry");
        assert_eq!(
            named[0].0, conn2,
            "the highest conn_id must win regardless of announce order"
        );
        assert_eq!(
            state.list_connections().len(),
            2,
            "both connections still coexist"
        );
    }

    #[test]
    fn connections_named_falls_back_to_the_older_connection_when_the_newest_closes() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx1, _rx1) = mpsc::unbounded_channel::<String>();
        let (tx2, _rx2) = mpsc::unbounded_channel::<String>();
        let conn1 = state.add_connection(tx1);
        let conn2 = state.add_connection(tx2);
        state.set_connection_info(conn1, "dup".to_owned(), "First".to_owned());
        state.set_connection_info(conn2, "dup".to_owned(), "Second".to_owned());

        state.remove_connection(conn2);

        let named = state.connections_named();
        assert_eq!(named.len(), 1);
        assert_eq!(
            named[0].0, conn1,
            "routing must fall back to the older, still-open window"
        );
    }

    #[test]
    fn named_connections_json_dedupes_by_file_key() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx1, _rx1) = mpsc::unbounded_channel::<String>();
        let (tx2, _rx2) = mpsc::unbounded_channel::<String>();
        let conn1 = state.add_connection(tx1);
        let conn2 = state.add_connection(tx2);
        state.set_connection_info(conn1, "dup".to_owned(), "First".to_owned());
        state.set_connection_info(conn2, "dup".to_owned(), "Second".to_owned());

        let json = state.named_connections_json();
        assert_eq!(
            json.len(),
            1,
            "status output must not list the same file key twice"
        );
    }

    #[test]
    fn version_mismatch_warning_is_none_for_empty_or_matching_version() {
        assert_eq!(version_mismatch_warning(""), None);
        assert_eq!(version_mismatch_warning(env!("CARGO_PKG_VERSION")), None);
    }

    #[test]
    fn version_mismatch_warning_fires_for_a_different_version() {
        let warning = version_mismatch_warning("0.0.1-not-the-daemon-version");
        assert_eq!(
            warning,
            Some("reopen the turbofig plugin in Figma".to_owned())
        );
    }

    #[test]
    fn named_connections_json_carries_plugin_version_and_no_warning_when_matching() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let conn = state.add_connection(tx);
        state.set_connection_info_with_version(
            conn,
            "fk1".to_owned(),
            "File 1".to_owned(),
            env!("CARGO_PKG_VERSION").to_owned(),
        );

        let json = state.named_connections_json();
        assert_eq!(json.len(), 1);
        assert_eq!(json[0]["pluginVersion"], env!("CARGO_PKG_VERSION"));
        assert!(json[0].get("warning").is_none());
    }

    #[test]
    fn named_connections_json_warns_on_a_mismatched_plugin_version() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let conn = state.add_connection(tx);
        state.set_connection_info_with_version(
            conn,
            "fk1".to_owned(),
            "File 1".to_owned(),
            "0.0.1-old".to_owned(),
        );

        let json = state.named_connections_json();
        assert_eq!(json[0]["warning"], "reopen the turbofig plugin in Figma");
    }

    #[test]
    fn named_connections_json_never_includes_the_token() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let conn = state.add_connection(tx);
        state.set_connection_info_with_version(
            conn,
            "fk1".to_owned(),
            "File 1".to_owned(),
            "x".to_owned(),
        );
        let json = state.named_connections_json();
        let serialized = serde_json::to_string(&json).expect("serialize");
        assert!(!serialized.contains(state.token()));
    }

    #[test]
    fn mark_plugin_seen_is_a_no_op_without_a_real_home() {
        // with_timeout never sets plugin_seen_path; calling mark_plugin_seen
        // must never touch any real directory.
        let state = AppState::with_timeout(Duration::from_millis(100));
        state.mark_plugin_seen();
    }

    #[test]
    fn mark_plugin_seen_writes_the_marker_under_a_new_with_token_home() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = AppState::new_with_token("tok".to_owned(), tmp.path());
        state.mark_plugin_seen();
        assert!(tmp.path().join("plugin-seen").exists());
    }

    #[test]
    fn uptime_seconds_starts_at_zero_and_is_never_negative() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        // Can only assert it is a small, sane value right after construction;
        // elapsed() can never be negative by construction (Instant-based).
        assert!(state.uptime_seconds() < 5);
    }

    #[test]
    fn jobs_in_flight_is_zero_with_no_guards_held() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        assert_eq!(state.jobs_in_flight(), 0);
    }

    #[test]
    fn try_begin_draining_only_lets_the_first_caller_through() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        assert!(!state.is_draining());
        assert!(
            state.try_begin_draining(),
            "the first caller must make the transition"
        );
        assert!(state.is_draining());
        assert!(
            !state.try_begin_draining(),
            "a second caller must see draining already true and get false back"
        );
        assert!(
            !state.try_begin_draining(),
            "repeated calls after the first must keep returning false"
        );
    }

    #[test]
    fn stop_requested_is_set_independently_of_try_begin_draining() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        assert!(!state.stop_requested());
        // A restart already began draining; a later stop must still record
        // itself even though it loses the try_begin_draining race.
        assert!(state.try_begin_draining());
        assert!(!state.try_begin_draining());
        state.request_stop();
        assert!(state.stop_requested());
    }

    #[test]
    fn begin_job_counts_a_job_until_its_guard_drops() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let guard = state.begin_job();
        assert_eq!(state.jobs_in_flight(), 1);
        drop(guard);
        assert_eq!(state.jobs_in_flight(), 0);
    }

    #[test]
    fn begin_job_counts_each_concurrent_job_independently() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let first = state.begin_job();
        let second = state.begin_job();
        assert_eq!(state.jobs_in_flight(), 2);
        drop(first);
        assert_eq!(state.jobs_in_flight(), 1);
        drop(second);
        assert_eq!(state.jobs_in_flight(), 0);
    }

    #[test]
    fn set_connection_info_on_an_unregistered_conn_id_is_a_no_op() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        // No add_connection call: 42 is never registered.
        state.set_connection_info(42, "fk1".to_owned(), "Ghost".to_owned());
        assert!(
            state.list_connections().is_empty(),
            "a message from an unregistered conn_id must not create an entry"
        );
    }

    #[test]
    fn resolve_ignores_a_result_from_a_connection_that_does_not_own_it() {
        // A RESULT frame claiming an id that belongs to a different
        // connection must be dropped, not resolved: otherwise a second
        // connection (e.g. a forged null-origin WebSocket) could answer on
        // another file's behalf.
        let state = AppState::with_timeout(Duration::from_millis(200));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let conn_id = state.add_connection(tx);
        let (id, mut rx) = state
            .register_pending_if_connected(conn_id)
            .expect("conn live");

        // Attacker claims a different conn_id (e.g. its own connection's id).
        state.resolve(id, conn_id + 999, json!({"ok": true, "forged": true}));
        assert!(
            rx.try_recv().is_err(),
            "a forged RESULT from another connection must not resolve the request"
        );

        // The rightful connection can still resolve it afterwards.
        state.resolve(id, conn_id, json!({"ok": true, "forged": false}));
        let val = rx.try_recv().expect("the rightful RESULT must resolve");
        assert_eq!(val["forged"], json!(false));
    }

    #[test]
    fn register_pending_if_connected_returns_none_for_a_closed_connection() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        assert!(
            state.register_pending_if_connected(999).is_none(),
            "an unknown conn_id must not register a pending entry"
        );
    }

    #[test]
    fn sanitize_name_strips_control_characters_and_caps_length() {
        let raw = format!("a\u{0007}b\nc{}", "x".repeat(300));
        let cleaned = sanitize_name(&raw);
        assert!(!cleaned.contains('\u{0007}'));
        assert!(!cleaned.contains('\n'));
        assert!(cleaned.len() <= MAX_NAME_LEN);
    }

    #[test]
    fn set_connection_info_sanitizes_the_stored_name() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let conn_id = state.add_connection(tx);
        state.set_connection_info(conn_id, "fk".to_owned(), "bad\u{0007}name".to_owned());
        let (_, _, name) = state
            .list_connections()
            .into_iter()
            .find(|(id, _, _)| *id == conn_id)
            .expect("connection present");
        assert_eq!(name, "badname");
    }

    #[test]
    fn session_pairing_round_trips() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        assert_eq!(state.session_lookup("s1"), None);
        state.session_insert("s1", "fk1");
        assert_eq!(state.session_lookup("s1"), Some("fk1".to_owned()));
        state.session_remove("s1");
        assert_eq!(state.session_lookup("s1"), None);
    }

    #[test]
    fn session_insert_evicts_oldest_past_cap() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        for i in 0..SESSION_CAP {
            state.session_insert(&format!("s{i}"), "fk");
        }
        // One more insert must evict the oldest (s0) rather than grow unbounded.
        state.session_insert("s-new", "fk");
        assert_eq!(
            state.session_lookup("s0"),
            None,
            "oldest session must be evicted"
        );
        assert_eq!(state.session_lookup("s-new"), Some("fk".to_owned()));
    }
}
