//! The one send/await/timeout/cancel path shared by all four plugin ops.
//!
//! `run_status`, `run_execute`, `run_get_selection`, and `run_screenshot` in
//! `ops/` all do the same round trip: register a pending request tied to a
//! connection, send a JSON frame, wait for the matching RESULT (or a
//! timeout), and clean up either way. This module is that one path, so the
//! pending-map bookkeeping lives in exactly one place.

use crate::state::AppState;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Outcome of a plugin round trip.
pub(crate) enum CallOutcome {
    /// The plugin replied before the timeout.
    Reply(Value),
    /// The plugin's connection closed before it replied.
    Disconnected,
    /// No reply arrived within the allotted wait.
    TimedOut,
    /// The connection closed between picking the route and registering the
    /// pending request (or the send itself failed).
    NotConnected,
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
    let Some((id, rx)) = state.register_pending_if_connected(conn_id) else {
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
            CallOutcome::Disconnected
        }
        Err(_elapsed) => CallOutcome::TimedOut, // guard cancels on drop below
    };
    (id, outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        assert!(matches!(outcome, CallOutcome::TimedOut));
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
}
