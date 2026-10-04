//! Shared daemon state: the connection registry, session pairing, and the
//! pending-request map that ties a tool call to the plugin reply that
//! resolves it.

use crate::config::{bridge_dir_from_env, request_timeout_from_env};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

/// Longest a plugin-supplied display name may be after sanitizing.
/// A long or control-character-laden name must never reach status JSON
/// unbounded: the plugin side is untrusted input.
const MAX_NAME_LEN: usize = 200;

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
}

impl AppState {
    /// Private constructor. All public constructors delegate here.
    fn build(timeout: Duration, screenshot_dir: Option<std::path::PathBuf>) -> Self {
        Self {
            connections: Mutex::new(HashMap::new()),
            conn_counter: AtomicU64::new(1),
            sessions: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            counter: AtomicU64::new(random_counter_start()),
            request_timeout: timeout,
            screenshot_dir,
        }
    }

    /// Create a new AppState. Reads the timeout from TURBOFIG_REQUEST_TIMEOUT_MS.
    /// Sets screenshot_dir to `~/.turbofig/outbox`.
    pub fn new() -> Self {
        Self::build(
            request_timeout_from_env(),
            Some(bridge_dir_from_env().join("outbox")),
        )
    }

    /// Create a new AppState with an explicit request timeout.
    /// Use this in tests to set a short timeout without touching global env.
    /// Sets screenshot_dir to None.
    pub fn with_timeout(d: Duration) -> Self {
        Self::build(d, None)
    }

    /// Return a clone of the screenshot output directory, if configured.
    pub fn screenshot_dir(&self) -> Option<std::path::PathBuf> {
        self.screenshot_dir.clone()
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
    pub(crate) fn set_connection_info(&self, conn_id: u64, file_key: String, name: String) {
        let name = sanitize_name(&name);
        let mut guard = self.connections.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(conn) = guard.get_mut(&conn_id) {
            conn.file_key = file_key;
            conn.name = name;
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
    ) -> Vec<(u64, mpsc::UnboundedSender<String>, String, String)> {
        let connections = self.connections.lock().unwrap_or_else(|e| e.into_inner());
        let mut newest: HashMap<String, (u64, mpsc::UnboundedSender<String>, String)> =
            HashMap::new();
        for (id, c) in connections.iter() {
            if c.file_key.is_empty() {
                continue;
            }
            let keep = match newest.get(&c.file_key) {
                Some((existing_id, _, _)) => *id > *existing_id,
                None => true,
            };
            if keep {
                newest.insert(c.file_key.clone(), (*id, c.tx.clone(), c.name.clone()));
            }
        }
        let mut v: Vec<_> = newest
            .into_iter()
            .map(|(fk, (id, tx, name))| (id, tx, fk, name))
            .collect();
        v.sort_by(|a, b| a.2.cmp(&b.2).then(a.0.cmp(&b.0)));
        v
    }

    /// Return all named connections as JSON objects for status and error
    /// responses. Dedupes a shared file_key the same way `connections_named`
    /// does, so status output never lists the same file twice.
    pub(crate) fn named_connections_json(&self) -> Vec<Value> {
        self.connections_named()
            .into_iter()
            .map(|(_, _, fk, name)| serde_json::json!({"fileKey": fk, "name": name}))
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
