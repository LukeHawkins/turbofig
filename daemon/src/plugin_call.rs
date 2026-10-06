//! The one send/await/timeout/cancel path shared by all four plugin ops.
//!
//! `run_status`, `run_execute`, `run_get_selection`, and `run_screenshot` in
//! `ops/` all do the same round trip: register a pending request tied to a
//! connection, send a JSON frame, wait for the matching RESULT (or a
//! timeout), and clean up either way. This module is that one path, so the
//! pending-map bookkeeping lives in exactly one place.

use crate::state::AppState;
use serde_json::Value;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Outcome of a plugin round trip.
pub(crate) enum CallOutcome {
    /// The plugin replied before the timeout.
    Reply(Value),
    /// The plugin's connection closed before it replied. Carries whether a
    /// STARTED frame for this request had already arrived, so the caller can
    /// tell "did not start, safe to retry" from "started, may have run".
    Disconnected(bool),
    /// No reply arrived within the allotted wait. Carries the same started
    /// flag as `Disconnected`, for the same reason.
    TimedOut(bool),
    /// The connection closed between picking the route and registering the
    /// pending request (or the send itself failed). The job was never sent,
    /// so there is no idempotency question here: it is always safe to retry.
    NotConnected,
}

/// Builds the caller-facing JSON for a timed-out or disconnected call.
/// `id` is the request id (named in the message so a caller can correlate it
/// against a later STARTED/RESULT if one ever turns up); `started` is the
/// outcome's own flag (see `CallOutcome::TimedOut`/`Disconnected`).
///
/// This is the one place `not_started`/`started_unknown` text and codes are
/// built, shared by `ops::run_execute`, `ops::run_get_selection`, and
/// `ops::run_screenshot`, so a caller on any transport (MCP, `POST /job`, the
/// filesystem bridge) sees the same wording and the same machine-readable
/// `code` for the same situation.
pub(crate) fn idempotency_error_json(id: u64, started: bool) -> Value {
    if started {
        serde_json::json!({
            "ok": false,
            "code": "started_unknown",
            "error": format!(
                "requestId {id}: started, may have run, check before retrying"
            )
        })
    } else {
        serde_json::json!({
            "ok": false,
            "code": "not_started",
            "error": format!("requestId {id}: did not start, safe to retry")
        })
    }
}

/// Builds the caller-facing JSON for `CallOutcome::NotConnected`: the
/// connection closed before the request was ever sent, so there is no
/// idempotency question, only "nothing reached the plugin".
pub(crate) fn plugin_disconnected_json() -> Value {
    serde_json::json!({
        "ok": false,
        "code": "plugin_disconnected",
        "error": "plugin not connected"
    })
}

/// Builds the caller-facing JSON for an admission-control rejection (see
/// `state::try_admit`). `queue_depth` is how many jobs are already admitted
/// on the lane that rejected this one; `retry_after_ms` is the daemon's own
/// backoff hint, scaled by that depth.
pub(crate) fn busy_json(queue_depth: usize, retry_after_ms: u64) -> Value {
    serde_json::json!({
        "ok": false,
        "code": "busy",
        "error": format!(
            "too many jobs in flight on this connection ({queue_depth}); retry after {retry_after_ms}ms"
        ),
        "queueDepth": queue_depth,
        "retryAfterMs": retry_after_ms
    })
}

/// Removes a pending entry from `state` when dropped, unless it was already
/// resolved or cancelled. This cleans up the pending map promptly when the
/// calling future itself is dropped (e.g. an MCP client disconnects
/// mid-call), instead of waiting out the full request timeout.
struct PendingGuard {
    state: Arc<AppState>,
    id: u64,
    active: bool,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if self.active {
            self.state.cancel_pending(self.id);
        }
    }
}

/// Register a pending request for `conn_id`, send `request` (with
/// `"requestId"` inserted) over `tx`, then wait up to `wait` for the
/// plugin's reply.
///
/// Returns the allocated request id (callers that need it, e.g. to report a
/// timeout, read it off the id field) alongside the outcome.
pub(crate) async fn call_plugin(
    state: &Arc<AppState>,
    conn_id: u64,
    tx: &mpsc::UnboundedSender<String>,
    mut request: Value,
    wait: Duration,
) -> (u64, CallOutcome) {
    let Some((id, rx, started)) = state.register_pending_if_connected(conn_id) else {
        return (0, CallOutcome::NotConnected);
    };
    if let Some(obj) = request.as_object_mut() {
        obj.insert("requestId".to_owned(), Value::from(id));
    }

    let mut guard = PendingGuard {
        state: state.clone(),
        id,
        active: true,
    };

    if tx.send(request.to_string()).is_err() {
        // guard drops here and cancels the just-registered pending entry.
        return (id, CallOutcome::NotConnected);
    }

    let outcome = match tokio::time::timeout(wait, rx).await {
        Ok(Ok(reply)) => {
            guard.active = false; // resolve() already removed the entry
            CallOutcome::Reply(reply)
        }
        Ok(Err(_)) => {
            guard.active = false; // the sender side dropped; nothing left to cancel
            CallOutcome::Disconnected(started.load(Ordering::SeqCst))
        }
        Err(_elapsed) => CallOutcome::TimedOut(started.load(Ordering::SeqCst)), // guard cancels on drop below
    };
    (id, outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Poll `condition` until it returns true, or panic with `msg` once
    /// `deadline_ms` elapses. Replaces a fixed "give it a moment" sleep with
    /// a wait on the actual state change, so a slow CI runner gets more time
    /// but a fast one does not wait longer than it needs to.
    async fn wait_until<F: FnMut() -> bool>(mut condition: F, deadline_ms: u64, msg: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(deadline_ms);
        loop {
            if condition() {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("Timed out waiting for: {msg}");
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn call_plugin_reports_not_connected_for_a_closed_connection() {
        let state = Arc::new(AppState::with_timeout(Duration::from_millis(100)));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let (_id, outcome) = call_plugin(
            &state,
            999,
            &tx,
            json!({"type": "STATUS"}),
            state.request_timeout,
        )
        .await;
        assert!(matches!(outcome, CallOutcome::NotConnected));
    }

    #[tokio::test]
    async fn call_plugin_times_out_when_the_plugin_never_replies() {
        let state = Arc::new(AppState::with_timeout(Duration::from_millis(50)));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let conn_id = state.add_connection(tx.clone());
        let (_id, outcome) = call_plugin(
            &state,
            conn_id,
            &tx,
            json!({"type": "STATUS"}),
            Duration::from_millis(50),
        )
        .await;
        assert!(matches!(outcome, CallOutcome::TimedOut(false)));
    }

    #[tokio::test]
    async fn call_plugin_times_out_with_started_true_once_a_started_frame_arrived() {
        // A STARTED frame arriving before the daemon-side wait elapses must
        // flip the timeout outcome's started flag, so the caller can tell
        // "may have run" from "never ran" even though neither ever gets a
        // RESULT.
        let state = Arc::new(AppState::with_timeout(Duration::from_millis(200)));
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let conn_id = state.add_connection(tx.clone());

        let state2 = state.clone();
        tokio::spawn(async move {
            if let Some(msg) = rx.recv().await {
                let req: Value = serde_json::from_str(&msg).expect("parse frame");
                let id = req["requestId"].as_u64().expect("requestId");
                state2.mark_started(id, conn_id);
                // Never sends a RESULT: the call must time out.
            }
        });

        let (_id, outcome) = call_plugin(
            &state,
            conn_id,
            &tx,
            json!({"type": "EXECUTE"}),
            Duration::from_millis(100),
        )
        .await;
        assert!(matches!(outcome, CallOutcome::TimedOut(true)));
    }

    #[tokio::test]
    async fn dropping_the_calling_future_cancels_its_pending_entry() {
        // A caller that drops its own future mid-call (e.g. an MCP client
        // disconnect) must not leak a pending entry until the full timeout
        // elapses: PendingGuard must clean it up immediately on drop.
        let state = Arc::new(AppState::with_timeout(Duration::from_secs(30)));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let conn_id = state.add_connection(tx.clone());

        let state_for_task = state.clone();
        let handle = tokio::spawn(async move {
            // Never replied to, and the timeout is long: this only returns
            // if the task is aborted first.
            call_plugin(
                &state_for_task,
                conn_id,
                &tx,
                json!({"type": "STATUS"}),
                Duration::from_secs(30),
            )
            .await
        });

        // Wait for the task to register its pending entry, then abort it
        // before it ever gets a reply or times out.
        wait_until(
            || state.pending_len() == 1,
            5000,
            "the call to register a pending entry",
        )
        .await;
        handle.abort();
        let _ = handle.await;

        // Aborting drops the task's future, which must drop the PendingGuard.
        wait_until(
            || state.pending_len() == 0,
            5000,
            "the pending entry to be cancelled promptly, not left until the 30s timeout",
        )
        .await;
    }

    #[tokio::test]
    async fn call_plugin_returns_the_reply() {
        let state = Arc::new(AppState::with_timeout(Duration::from_millis(200)));
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let conn_id = state.add_connection(tx.clone());

        let state2 = state.clone();
        tokio::spawn(async move {
            if let Some(msg) = rx.recv().await {
                let req: Value = serde_json::from_str(&msg).expect("parse frame");
                let id = req["requestId"].as_u64().expect("requestId");
                state2.resolve(id, conn_id, json!({"ok": true}));
            }
        });

        let (_id, outcome) = call_plugin(
            &state,
            conn_id,
            &tx,
            json!({"type": "STATUS"}),
            Duration::from_millis(200),
        )
        .await;
        match outcome {
            CallOutcome::Reply(v) => assert_eq!(v["ok"], json!(true)),
            _ => panic!("expected a reply"),
        }
    }

    #[test]
    fn idempotency_error_json_not_started_carries_the_not_started_code() {
        let v = idempotency_error_json(42, false);
        assert_eq!(v["ok"], json!(false));
        assert_eq!(v["code"], json!("not_started"));
        assert!(v["error"].as_str().unwrap().contains("safe to retry"));
    }

    #[test]
    fn idempotency_error_json_started_carries_the_started_unknown_code() {
        let v = idempotency_error_json(42, true);
        assert_eq!(v["code"], json!("started_unknown"));
        assert!(v["error"].as_str().unwrap().contains("may have run"));
    }

    #[test]
    fn plugin_disconnected_json_carries_its_code() {
        let v = plugin_disconnected_json();
        assert_eq!(v["ok"], json!(false));
        assert_eq!(v["code"], json!("plugin_disconnected"));
    }

    #[test]
    fn busy_json_carries_queue_depth_and_retry_after() {
        let v = busy_json(3, 750);
        assert_eq!(v["ok"], json!(false));
        assert_eq!(v["code"], json!("busy"));
        assert_eq!(v["queueDepth"], json!(3));
        assert_eq!(v["retryAfterMs"], json!(750));
    }
}
