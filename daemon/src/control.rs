//! The authenticated local control path.
//!
//! `POST /control` lets a caller holding the pairing token ask the daemon to
//! drain its in-flight jobs and exit. `turbofig mcp` is the first client: it
//! calls this when `/health` reports a daemon version that differs from its
//! own (an upgrade swapped the binary on disk), so the old process clears
//! out before the proxy starts the new one. The daemon side never tells
//! `restart` and `stop` apart beyond the response body: both drain and exit
//! 0, and it is the caller's job to decide whether to start a new binary
//! afterward.
//!
//! The pairing token is the same one the WebSocket upgrade and the Figma
//! plugin already use (`token.rs`), compared here in constant time. There is
//! no separate control token: the pairing token is already a local secret
//! only a paired client holds, and adding a second secret would just be
//! another file to keep in sync.

use crate::state::AppState;
use crate::supervisor::wait_for_drain;
use crate::token::bearer_token_matches;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

/// Longest the control path waits for in-flight jobs to finish before
/// exiting anyway. Mirrors `main.rs`'s supervised-restart drain wait.
const CONTROL_DRAIN_MAX_WAIT: Duration = Duration::from_secs(60);
const CONTROL_DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Delay between the HTTP response going out and the process actually
/// exiting, so the caller's response has time to flush before the process
/// dies. Mirrors `main.rs`'s `SUPERVISOR_EXIT_GRACE`.
const CONTROL_EXIT_GRACE: Duration = Duration::from_millis(250);

#[derive(Debug, Deserialize)]
pub(crate) struct ControlRequest {
    action: ControlAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ControlAction {
    Restart,
    Stop,
}

/// `POST /control` handler.
///
/// Requires `Authorization: Bearer <pairing token>`, checked with
/// `constant_time_eq`; a missing or wrong token gives 401 before anything
/// else happens. On success, drains in-flight jobs via the same
/// `wait_for_drain` logic the supervised-restart loop uses, then schedules
/// the process to exit 0 shortly after the response is sent. Never logs the
/// token, win or lose.
///
/// Race-safe against two concurrent callers (e.g. two proxies both deciding
/// the daemon needs a version-handoff restart): `AppState::try_begin_draining`
/// is a single atomic compare-exchange, so only the first caller through
/// actually drains and schedules the exit. A caller that loses the race gets
/// back `{"ok":true,"alreadyInProgress":true}` at once, with no second drain
/// wait and no second exit timer, never a 4xx/5xx: a restart already being
/// under way is success from this caller's point of view too.
pub(crate) async fn control_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ControlRequest>,
) -> (StatusCode, Json<Value>) {
    if !bearer_token_matches(&headers, state.token()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"ok": false, "error": "missing or invalid bearer token"})),
        );
    }

    if !state.try_begin_draining() {
        return (
            StatusCode::OK,
            Json(json!({"ok": true, "action": req.action, "alreadyInProgress": true})),
        );
    }

    let drained = wait_for_drain(
        || state.jobs_in_flight(),
        CONTROL_DRAIN_MAX_WAIT,
        CONTROL_DRAIN_POLL_INTERVAL,
    )
    .await;
    if !drained {
        eprintln!(
            "Turbofig daemon: {} job(s) still in flight after {:?}; exiting anyway for /control {:?}",
            state.jobs_in_flight(),
            CONTROL_DRAIN_MAX_WAIT,
            req.action,
        );
    }

    // Exit after a short grace delay so this response has time to reach the
    // caller before the process dies, rather than exiting from inside the
    // handler before axum can write the body.
    tokio::spawn(async move {
        tokio::time::sleep(CONTROL_EXIT_GRACE).await;
        std::process::exit(0);
    });

    (
        StatusCode::OK,
        Json(json!({"ok": true, "action": req.action})),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_second_concurrent_control_call_reports_already_in_progress_without_a_second_drain_wait(
    ) {
        let state = std::sync::Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        // Simulate the first caller already having won the race, exactly as
        // control_handler's own `try_begin_draining` call would have done.
        assert!(state.try_begin_draining());

        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {}", state.token()).parse().unwrap(),
        );
        let (status, Json(body)) = control_handler(
            State(state),
            headers,
            Json(ControlRequest {
                action: ControlAction::Restart,
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["alreadyInProgress"], json!(true));
    }

    #[test]
    fn control_action_serializes_to_snake_case_strings() {
        assert_eq!(
            serde_json::to_value(ControlAction::Restart).unwrap(),
            json!("restart")
        );
        assert_eq!(
            serde_json::to_value(ControlAction::Stop).unwrap(),
            json!("stop")
        );
    }
}
