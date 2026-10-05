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
use crate::token::constant_time_eq;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
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

/// Extracts the bearer token from `Authorization: Bearer <token>`.
/// Returns `None` for a missing header, a non-UTF-8 header, or a header that
/// does not carry the `Bearer ` prefix.
fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

/// `POST /control` handler.
///
/// Requires `Authorization: Bearer <pairing token>`, checked with
/// `constant_time_eq`; a missing or wrong token gives 401 before anything
/// else happens. On success, drains in-flight jobs via the same
/// `wait_for_drain` logic the supervised-restart loop uses, then schedules
/// the process to exit 0 shortly after the response is sent. Never logs the
/// token, win or lose.
pub(crate) async fn control_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ControlRequest>,
) -> impl IntoResponse {
    let Some(candidate) = bearer_token(&headers) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"ok": false, "error": "missing bearer token"})),
        );
    };
    if !constant_time_eq(candidate, state.token()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"ok": false, "error": "invalid token"})),
        );
    }

    state.set_draining(true);
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

    #[test]
    fn bearer_token_reads_the_prefixed_value() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer abc123".parse().unwrap(),
        );
        assert_eq!(bearer_token(&headers), Some("abc123"));
    }

    #[test]
    fn bearer_token_is_none_without_the_prefix() {
        let mut headers = HeaderMap::new();
        headers.insert(axum::http::header::AUTHORIZATION, "abc123".parse().unwrap());
        assert_eq!(bearer_token(&headers), None);
    }

    #[test]
    fn bearer_token_is_none_when_absent() {
        let headers = HeaderMap::new();
        assert_eq!(bearer_token(&headers), None);
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
