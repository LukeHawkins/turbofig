//! Call routing: pick which connected plugin a tool call targets.

use crate::state::AppState;
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// The daemon counts as "just started" for this long after startup.
pub(crate) const COLD_START_WINDOW: Duration = Duration::from_secs(5);
/// Longest a job waits for the first plugin connection on a young daemon.
pub(crate) const COLD_START_WAIT: Duration = Duration::from_secs(3);
pub(crate) const COLD_START_POLL: Duration = Duration::from_millis(50);
/// After the first file announces itself on a young daemon, a job with no
/// target waits this long before it auto-picks a single file: other open
/// files may still be reconnecting.
pub(crate) const COLD_START_SETTLE: Duration = Duration::from_secs(1);

/// Routing error returned by resolve_route.
#[derive(Debug)]
pub(crate) enum RouteError {
    /// No plugin is connected.
    NoPlugin,
    /// More than one file is connected and no explicit target was given.
    Ambiguous(Vec<String>),
    /// The requested file key is not connected.
    NotFound(String, Vec<String>),
    /// The daemon is draining for a supervised restart (a Homebrew upgrade
    /// was detected) and refuses every new job until the new binary starts.
    Draining,
}

/// Convert a RouteError into the caller-facing JSON shape.
pub(crate) fn route_error_to_json(e: RouteError) -> Value {
    match e {
        RouteError::NoPlugin => json!({
            "ok": false,
            "code": "plugin_disconnected",
            "error": "no plugin connected"
        }),
        RouteError::Ambiguous(fks) => json!({
            "ok": false,
            "code": "ambiguous_target",
            "error": "multiple files connected; specify fileKey",
            "files": fks
        }),
        RouteError::NotFound(fk, available) => json!({
            "ok": false,
            "code": "file_not_connected",
            "error": format!("file not connected: {fk}"),
            "files": available
        }),
        RouteError::Draining => json!({
            "ok": false,
            "code": "draining",
            "error": "daemon is restarting after an upgrade; retry shortly"
        }),
    }
}

/// Resolve which connection a call targets.
///
/// session_id: the mcp-session-id (None for the bridge or when absent).
/// explicit:   an explicit target fileKey from a tool param or bridge job.
///
/// Returns the resolved connection details or a RouteError.
/// Only connections whose file_key is non-empty count as valid targets.
/// A just-connected socket with no FILE_INFO is not a valid target.
///
/// Two live connections may share a file_key (a reconnect, or a second
/// window on the same file): `state.connections_named()` already dedupes
/// that down to the newest (highest conn_id) entry, so this never needs to
/// break a tie itself, and a closed newest connection naturally falls back
/// to an older one still open.
pub(crate) fn resolve_route(
    state: &AppState,
    session_id: Option<&str>,
    explicit: Option<&str>,
) -> Result<(u64, mpsc::UnboundedSender<String>, String, String), RouteError> {
    // A supervised restart in progress refuses every new job, regardless of
    // target, so the old process can drain and exit cleanly for launchd to
    // start the upgraded binary. See AppState::is_draining.
    if state.is_draining() {
        return Err(RouteError::Draining);
    }

    // Normalize an empty explicit fileKey to no target, the same as an
    // empty session id. An empty string can never name a real file, so it
    // must fall through to the session pairing or the sole connection
    // instead of always failing with "file not connected".
    let explicit = explicit.filter(|s| !s.is_empty());

    // Step 1: Determine the desired file_key.
    let desired: Option<String> = if let Some(fk) = explicit {
        if let Some(sid) = session_id {
            state.session_insert(sid, fk);
        }
        Some(fk.to_owned())
    } else if let Some(sid) = session_id {
        state.session_lookup(sid)
    } else {
        None
    };

    // Step 2: Collect named connections (non-empty file_key only).
    let named = state.connections_named();

    if let Some(ref fk) = desired {
        let found = named
            .iter()
            .find(|(_, _, fk2, _, _)| fk2 == fk)
            .map(|(id, tx, fk2, nm, _)| (*id, tx.clone(), fk2.clone(), nm.clone()));

        if let Some(result) = found {
            return Ok(result);
        }

        // Not found: clear any stale pairing for this session.
        if let Some(sid) = session_id {
            state.session_remove(sid);
        }
        let available: Vec<String> = named.into_iter().map(|(_, _, fk2, _, _)| fk2).collect();
        // No plugin at all is one state with one code, whether or not a
        // fileKey was requested.
        if available.is_empty() {
            return Err(RouteError::NoPlugin);
        }
        return Err(RouteError::NotFound(fk.clone(), available));
    }

    // No desired file_key: auto-pick from named connections.
    match named.len() {
        0 => Err(RouteError::NoPlugin),
        1 => {
            let (conn_id, tx, fk, nm, _) = named.into_iter().next().unwrap();
            if let Some(sid) = session_id {
                state.session_insert(sid, &fk);
            }
            Ok((conn_id, tx, fk, nm))
        }
        _ => {
            let fks: Vec<String> = named.into_iter().map(|(_, _, fk, _, _)| fk).collect();
            Err(RouteError::Ambiguous(fks))
        }
    }
}

/// `resolve_route`, plus a cold-start grace. While the daemon is young:
/// - a missing plugin is retried until the first file announces itself;
/// - a named file that is not connected yet is retried for as long as the
///   daemon is young, because other files may reconnect after the first;
/// - a job with no target waits `COLD_START_SETTLE` after the first file
///   before it auto-picks a single file.
///
/// No wait lasts longer than `COLD_START_WAIT`. An older daemon never waits.
pub(crate) async fn resolve_route_graced(
    state: &AppState,
    session_id: Option<&str>,
    explicit: Option<&str>,
) -> Result<(u64, mpsc::UnboundedSender<String>, String, String), RouteError> {
    let deadline = Instant::now() + COLD_START_WAIT;
    // Pin the target once: a failed lookup clears the session pairing, and a
    // retry must keep asking for the same file.
    let pinned: Option<String> = explicit
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| session_id.and_then(|sid| state.session_lookup(sid)));
    loop {
        // A draining daemon is about to exit: the settle wait only delays the
        // answer, so it is skipped.
        if pinned.is_none()
            && !state.is_draining()
            && state.in_cold_start_settle()
            && state.connections_named().len() == 1
            && Instant::now() < deadline
        {
            tokio::time::sleep(COLD_START_POLL).await;
            continue;
        }
        match resolve_route(state, session_id, pinned.as_deref()) {
            Err(RouteError::NoPlugin)
                if state.in_cold_start_grace() && Instant::now() < deadline =>
            {
                tokio::time::sleep(COLD_START_POLL).await;
            }
            Err(RouteError::NotFound(..))
                if state.daemon_is_young() && Instant::now() < deadline =>
            {
                tokio::time::sleep(COLD_START_POLL).await;
            }
            other => return other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn route_error_to_json_carries_a_machine_readable_code_for_every_variant() {
        assert_eq!(
            route_error_to_json(RouteError::NoPlugin)["code"],
            json!("plugin_disconnected")
        );
        assert_eq!(
            route_error_to_json(RouteError::Ambiguous(vec!["fk1".to_owned()]))["code"],
            json!("ambiguous_target")
        );
        assert_eq!(
            route_error_to_json(RouteError::NotFound("fk1".to_owned(), vec![]))["code"],
            json!("file_not_connected")
        );
        assert_eq!(
            route_error_to_json(RouteError::Draining)["code"],
            json!("draining")
        );
    }

    #[test]
    fn resolve_route_explicit_key_with_no_plugin_returns_no_plugin() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        assert!(matches!(
            resolve_route(&state, None, Some("fk1")),
            Err(RouteError::NoPlugin)
        ));
        assert!(matches!(
            resolve_route(&state, Some("sess"), Some("fk1")),
            Err(RouteError::NoPlugin)
        ));
    }

    #[test]
    fn resolve_route_no_connections_returns_no_plugin() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        assert!(matches!(
            resolve_route(&state, None, None),
            Err(RouteError::NoPlugin)
        ));
    }

    #[test]
    fn resolve_route_one_named_no_session_returns_it() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let conn_id = state.add_connection(tx);
        state.set_connection_info(conn_id, "fk1".to_owned(), "File 1".to_owned());

        let result = resolve_route(&state, None, None);
        assert!(result.is_ok(), "expected Ok");
        let (got_conn, _, got_fk, got_name) = result.unwrap();
        assert_eq!(got_conn, conn_id);
        assert_eq!(got_fk, "fk1");
        assert_eq!(got_name, "File 1");
    }

    #[test]
    fn resolve_route_one_named_session_records_pairing_and_re_routes() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let conn_id = state.add_connection(tx);
        state.set_connection_info(conn_id, "fk1".to_owned(), "File 1".to_owned());

        let result = resolve_route(&state, Some("session-a"), None);
        assert!(result.is_ok(), "first call must succeed");
        let (_, _, fk, _) = result.unwrap();
        assert_eq!(fk, "fk1");

        let result2 = resolve_route(&state, Some("session-a"), None);
        assert!(result2.is_ok(), "second call must succeed via pairing");
        let (_, _, fk2, _) = result2.unwrap();
        assert_eq!(fk2, "fk1");
    }

    #[test]
    fn resolve_route_two_named_no_session_returns_ambiguous() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx1, _rx1) = mpsc::unbounded_channel::<String>();
        let (tx2, _rx2) = mpsc::unbounded_channel::<String>();
        let conn1 = state.add_connection(tx1);
        let conn2 = state.add_connection(tx2);
        state.set_connection_info(conn1, "fk1".to_owned(), "File 1".to_owned());
        state.set_connection_info(conn2, "fk2".to_owned(), "File 2".to_owned());

        match resolve_route(&state, None, None) {
            Err(RouteError::Ambiguous(fks)) => {
                assert!(fks.contains(&"fk1".to_owned()), "fk1 must be in the list");
                assert!(fks.contains(&"fk2".to_owned()), "fk2 must be in the list");
            }
            other => panic!("expected Ambiguous, got ok={}", other.is_ok()),
        }
    }

    #[test]
    fn resolve_route_empty_explicit_file_key_falls_through_to_auto_pick() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx1, _rx1) = mpsc::unbounded_channel::<String>();
        let conn1 = state.add_connection(tx1);
        state.set_connection_info(conn1, "fk1".to_owned(), "File 1".to_owned());

        let (conn_id, _tx, fk, _nm) = resolve_route(&state, None, Some(""))
            .expect("empty explicit fileKey must auto-pick the sole plugin");
        assert_eq!(conn_id, conn1);
        assert_eq!(fk, "fk1");
    }

    #[test]
    fn resolve_route_explicit_fk2_returns_fk2_and_records_session_pairing() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx1, _rx1) = mpsc::unbounded_channel::<String>();
        let (tx2, _rx2) = mpsc::unbounded_channel::<String>();
        let conn1 = state.add_connection(tx1);
        let conn2 = state.add_connection(tx2);
        state.set_connection_info(conn1, "fk1".to_owned(), "File 1".to_owned());
        state.set_connection_info(conn2, "fk2".to_owned(), "File 2".to_owned());

        let result = resolve_route(&state, Some("session-x"), Some("fk2"));
        assert!(result.is_ok(), "explicit fk2 must resolve");
        let (_, _, got_fk, _) = result.unwrap();
        assert_eq!(got_fk, "fk2");

        let result2 = resolve_route(&state, Some("session-x"), None);
        assert!(result2.is_ok(), "paired session must resolve to fk2");
        let (_, _, got_fk2, _) = result2.unwrap();
        assert_eq!(got_fk2, "fk2");
    }

    #[test]
    fn resolve_route_explicit_unknown_returns_not_found_and_clears_stale_pairing() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let conn = state.add_connection(tx);
        state.set_connection_info(conn, "fk1".to_owned(), "File 1".to_owned());

        let _ = resolve_route(&state, Some("session-y"), Some("fk1"));

        match resolve_route(&state, Some("session-y"), Some("ghost")) {
            Err(RouteError::NotFound(fk, avail)) => {
                assert_eq!(fk, "ghost");
                assert!(avail.contains(&"fk1".to_owned()), "available must list fk1");
            }
            other => panic!("expected NotFound, got ok={}", other.is_ok()),
        }

        let result = resolve_route(&state, Some("session-y"), None);
        assert!(
            result.is_ok(),
            "after clearing stale pairing must auto-pick fk1"
        );
        let (_, _, fk, _) = result.unwrap();
        assert_eq!(fk, "fk1");
    }

    #[test]
    fn resolve_route_unnamed_connection_not_counted_for_auto_pick() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let _conn = state.add_connection(tx);

        assert!(matches!(
            resolve_route(&state, None, None),
            Err(RouteError::NoPlugin)
        ));
    }

    #[test]
    fn resolve_route_reconnect_same_file_key_auto_picks_the_newest() {
        // A reconnect (or a second window) on the same file does not evict
        // the older connection; state.rs dedupes connections_named() down to
        // the newest one, so routing lands on it without ever reporting
        // Ambiguous for a single logical file.
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx1, _rx1) = mpsc::unbounded_channel::<String>();
        let (tx2, _rx2) = mpsc::unbounded_channel::<String>();
        let _conn1 = state.add_connection(tx1);
        state.set_connection_info(_conn1, "dup".to_owned(), "First".to_owned());
        let conn2 = state.add_connection(tx2);
        state.set_connection_info(conn2, "dup".to_owned(), "Second".to_owned());

        let (conn_id, _tx, fk, nm) =
            resolve_route(&state, None, None).expect("single live file must auto-pick");
        assert_eq!(conn_id, conn2, "the newest connection must win");
        assert_eq!(fk, "dup");
        assert_eq!(nm, "Second");
    }

    #[test]
    fn resolve_route_falls_back_to_the_older_connection_when_the_newest_closes() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx1, _rx1) = mpsc::unbounded_channel::<String>();
        let (tx2, _rx2) = mpsc::unbounded_channel::<String>();
        let conn1 = state.add_connection(tx1);
        state.set_connection_info(conn1, "dup".to_owned(), "First".to_owned());
        let conn2 = state.add_connection(tx2);
        state.set_connection_info(conn2, "dup".to_owned(), "Second".to_owned());

        state.remove_connection(conn2);

        let (conn_id, _tx, fk, nm) = resolve_route(&state, None, None)
            .expect("the older connection must still be a valid route target");
        assert_eq!(conn_id, conn1);
        assert_eq!(fk, "dup");
        assert_eq!(nm, "First");
    }

    #[test]
    fn resolve_route_ambiguous_list_dedupes_repeated_file_keys() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx1, _rx1) = mpsc::unbounded_channel::<String>();
        let (tx2, _rx2) = mpsc::unbounded_channel::<String>();
        let (tx3, _rx3) = mpsc::unbounded_channel::<String>();
        let conn1 = state.add_connection(tx1);
        let conn2 = state.add_connection(tx2);
        let conn3 = state.add_connection(tx3);
        state.set_connection_info(conn1, "fk1".to_owned(), "A".to_owned());
        state.set_connection_info(conn2, "fk1".to_owned(), "B".to_owned());
        state.set_connection_info(conn3, "fk2".to_owned(), "C".to_owned());

        match resolve_route(&state, None, None) {
            Err(RouteError::Ambiguous(fks)) => {
                assert_eq!(
                    fks.len(),
                    2,
                    "two connections sharing fk1 must dedupe to one entry"
                );
                assert!(fks.contains(&"fk1".to_owned()));
                assert!(fks.contains(&"fk2".to_owned()));
            }
            other => panic!("expected Ambiguous, got ok={}", other.is_ok()),
        }
    }

    #[tokio::test]
    async fn graced_route_waits_for_a_plugin_that_connects_during_the_grace() {
        let state = std::sync::Arc::new(AppState::with_timeout(Duration::from_millis(100)));
        let s2 = state.clone();
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let id = s2.add_connection(tx);
            s2.set_connection_info(id, "fk1".to_owned(), "F".to_owned());
        });
        let got = resolve_route_graced(&state, None, None)
            .await
            .expect("must route once the plugin connects");
        assert_eq!(got.2, "fk1");
    }

    #[tokio::test]
    async fn graced_route_never_waits_after_a_plugin_has_connected() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let id = state.add_connection(tx);
        state.set_connection_info(id, "fk1".to_owned(), "F".to_owned());
        state.remove_connection(id);
        let t = Instant::now();
        assert!(matches!(
            resolve_route_graced(&state, None, None).await,
            Err(RouteError::NoPlugin)
        ));
        assert!(matches!(
            resolve_route_graced(&state, None, Some("fk1")).await,
            Err(RouteError::NoPlugin)
        ));
        assert!(t.elapsed() < Duration::from_millis(500));
    }

    #[tokio::test]
    async fn graced_route_gives_up_after_the_wait_when_no_plugin_comes() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let t = Instant::now();
        assert!(matches!(
            resolve_route_graced(&state, None, None).await,
            Err(RouteError::NoPlugin)
        ));
        let waited = t.elapsed();
        assert!(waited >= COLD_START_WAIT - Duration::from_millis(100));
        assert!(waited < COLD_START_WAIT + Duration::from_secs(1));
    }

    #[tokio::test]
    async fn graced_route_waits_for_file_b_when_file_a_reconnects_first() {
        let state = std::sync::Arc::new(AppState::with_timeout(Duration::from_millis(100)));
        let (txa, _rxa) = mpsc::unbounded_channel::<String>();
        let ida = state.add_connection(txa);
        state.set_connection_info(ida, "fkA".to_owned(), "A".to_owned());
        let s2 = state.clone();
        let (txb, _rxb) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let id = s2.add_connection(txb);
            s2.set_connection_info(id, "fkB".to_owned(), "B".to_owned());
        });
        let got = resolve_route_graced(&state, None, Some("fkB"))
            .await
            .expect("file B must route once it reconnects");
        assert_eq!(got.2, "fkB");
    }

    #[tokio::test]
    async fn graced_route_no_key_is_ambiguous_when_a_second_file_joins_in_the_settle_period() {
        let state = std::sync::Arc::new(AppState::with_timeout(Duration::from_millis(100)));
        let (txa, _rxa) = mpsc::unbounded_channel::<String>();
        let ida = state.add_connection(txa);
        state.set_connection_info(ida, "fkA".to_owned(), "A".to_owned());
        let s2 = state.clone();
        let (txb, _rxb) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let id = s2.add_connection(txb);
            s2.set_connection_info(id, "fkB".to_owned(), "B".to_owned());
        });
        assert!(matches!(
            resolve_route_graced(&state, None, None).await,
            Err(RouteError::Ambiguous(_))
        ));
    }

    #[tokio::test]
    async fn graced_route_picks_a_single_file_after_the_settle_period() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (txa, _rxa) = mpsc::unbounded_channel::<String>();
        let ida = state.add_connection(txa);
        state.set_connection_info(ida, "fkA".to_owned(), "A".to_owned());
        let t = Instant::now();
        let got = resolve_route_graced(&state, None, None)
            .await
            .expect("routes");
        assert_eq!(got.2, "fkA");
        assert!(t.elapsed() >= COLD_START_SETTLE - Duration::from_millis(100));
    }

    #[tokio::test]
    async fn graced_route_skips_the_settle_wait_while_draining() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        let (txa, _rxa) = mpsc::unbounded_channel::<String>();
        let ida = state.add_connection(txa);
        state.set_connection_info(ida, "fkA".to_owned(), "A".to_owned());
        state.set_draining(true);
        let t = Instant::now();
        let _ = resolve_route_graced(&state, None, None).await;
        assert!(t.elapsed() < COLD_START_SETTLE - Duration::from_millis(300));
    }

    #[tokio::test]
    async fn graced_route_never_waits_when_grace_is_skipped() {
        let state = AppState::with_timeout(Duration::from_millis(100));
        state.skip_cold_start_grace();
        let (txa, _rxa) = mpsc::unbounded_channel::<String>();
        let ida = state.add_connection(txa);
        state.set_connection_info(ida, "fkA".to_owned(), "A".to_owned());
        let t = Instant::now();
        assert!(resolve_route_graced(&state, None, None).await.is_ok());
        assert!(matches!(
            resolve_route_graced(&state, None, Some("fkB")).await,
            Err(RouteError::NotFound(..))
        ));
        assert!(t.elapsed() < Duration::from_millis(300));
    }
}
