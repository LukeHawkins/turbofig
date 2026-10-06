//! The authenticated local control path.
//!
//! `POST /control` lets a caller holding the pairing token ask the daemon to
//! drain its in-flight jobs and exit. `turbofig mcp` is the first client: it
//! calls this when `/health` reports a daemon version that differs from its
//! own (an upgrade swapped the binary on disk), so the old process clears
//! out before the proxy starts the new one. Both actions always drain the
//! same way; they differ only in the exit code under launchd supervision
//! (`supervisor::is_supervised`): `stop` always exits 0, so the plist's
//! `KeepAlive: {SuccessfulExit: false}` (`launchd.rs`) leaves the daemon
//! stopped until the next login; a supervised `restart` exits
//! `supervisor::SUPERVISED_RESTART_EXIT_CODE` instead, so the same
//! `KeepAlive` rule restarts it with the (by then upgraded) binary.
//! Unsupervised (no launchd watching), both exit 0: it is the caller's job
//! to decide whether to start a new binary itself, as `turbofig mcp`'s
//! `restart_for_upgrade` does.
//!
//! A `stop` always overrides a restart already draining. If a restart's
//! drain is under way and a `stop` call arrives before the process exits,
//! the exit code changes to 0, so launchd leaves the daemon stopped instead
//! of restarting it, matching the `202` reply `turbofig stop` already got.
//! See `AppState::request_stop`/`stop_requested`.
//!
//! The handler itself never blocks on the drain: it replies `202` at once
//! with `{"ok":true,"action":...,"draining":true}`, then drains and exits in
//! a background task. A drain can take up to `CONTROL_DRAIN_MAX_WAIT` (60s);
//! holding the HTTP response open that whole time (the old behaviour) ties
//! up a connection and a caller's timeout budget for no reason, since every
//! real caller (`turbofig stop`, the version-handoff restart, `autostart
//! on`'s pre-stop) already has to separately poll `/health` until it stops
//! answering to know the daemon is actually gone; the response itself was
//! never the signal that mattered.
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

/// The exit grace in effect. A debug build honours
/// `TURBOFIG_TEST_CONTROL_EXIT_GRACE_MS`, so a test can hold the window open
/// long enough for a slow CI runner. Release builds always use
/// `CONTROL_EXIT_GRACE`.
fn control_exit_grace() -> Duration {
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("TURBOFIG_TEST_CONTROL_EXIT_GRACE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        return Duration::from_millis(ms);
    }
    CONTROL_EXIT_GRACE
}

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
/// else happens, including before the body is parsed (a malformed body from
/// an unauthenticated caller must never leak a 400/422 instead of 401). On
/// success, replies `202` with `{"ok":true,"action":...,
/// "draining":true}` at once, then drains in-flight jobs (the same
/// `wait_for_drain` logic the supervised-restart loop uses) and exits in a
/// background task. Never logs the token, win or lose.
///
/// Race-safe against two concurrent callers (e.g. two proxies both deciding
/// the daemon needs a version-handoff restart): `AppState::try_begin_draining`
/// is a single atomic compare-exchange, so only the first caller through
/// actually drains and schedules the exit. A caller that loses the race gets
/// back the exact same `202 {"ok":true,"action":...,"draining":true}` at
/// once, changing nothing: a restart already being under way is success from
/// this caller's point of view too, and there is nothing left to report that
/// the first caller's response did not already say.
pub(crate) async fn control_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> (StatusCode, Json<Value>) {
    if !bearer_token_matches(&headers, state.token()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"ok": false, "error": "missing or invalid bearer token"})),
        );
    }

    // The body is parsed only after the token check: an unauthenticated
    // caller must never learn whether its body was well-formed (400/422
    // instead of 401 would do that), so the request is taken as raw bytes
    // here instead of through axum's `Json` extractor, which runs before any
    // handler code and would parse (and reject) the body first.
    let req: ControlRequest = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "error": format!("invalid control request: {e}")})),
            );
        }
    };

    let action = req.action;
    if action == ControlAction::Stop {
        // Record the stop even if a restart is already draining (the branch
        // below then returns false for this caller): a stop must always win
        // over a restart's exit code. See `AppState::request_stop`.
        state.request_stop();
    }
    if state.try_begin_draining() {
        // Drain and exit in the background: the caller never waits on this,
        // only on /health going unreachable (see this module's doc comment).
        tokio::spawn(async move {
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
                    action,
                );
            }
            // Give a just-sent response (this one, or a concurrent repeat
            // caller's) a moment to flush before the process actually exits.
            // See `exit_code_for` for which code each action uses and why,
            // and `AppState::stop_requested` for why a stop that arrived
            // after this task started still forces exit 0.
            tokio::time::sleep(control_exit_grace()).await;
            let code = if state.stop_requested() {
                0
            } else {
                exit_code_for(action)
            };
            std::process::exit(code);
        });
    }

    (
        StatusCode::ACCEPTED,
        Json(json!({"ok": true, "action": action, "draining": true})),
    )
}

/// The exit code `/control` uses for `action`, once it is ready to exit.
///
/// `stop` always exits 0: under launchd supervision, the plist's `KeepAlive:
/// {SuccessfulExit: false}` (`launchd.rs`) then leaves the daemon stopped
/// until the next login, exactly what `turbofig stop` promises. A
/// supervised `restart` instead exits `SUPERVISED_RESTART_EXIT_CODE`, a
/// non-zero code, so that same `KeepAlive` rule restarts it at once with the
/// binary that triggered the restart (an upgrade). Unsupervised, `restart`
/// exits 0 too: nothing is watching to restart it, so the caller
/// (`turbofig mcp`'s `restart_for_upgrade`) is the one that starts the new
/// process.
fn exit_code_for(action: ControlAction) -> i32 {
    match action {
        ControlAction::Stop => 0,
        ControlAction::Restart if crate::supervisor::is_supervised() => {
            crate::supervisor::SUPERVISED_RESTART_EXIT_CODE
        }
        ControlAction::Restart => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exit_code_for_stop_is_always_zero() {
        let _guard = crate::supervisor::SUPERVISED_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // SAFETY: test-only env mutation, guarded by ENV_TEST_LOCK.
        unsafe {
            std::env::remove_var("TURBOFIG_SUPERVISED");
        }
        assert_eq!(exit_code_for(ControlAction::Stop), 0);
        unsafe {
            std::env::set_var("TURBOFIG_SUPERVISED", "1");
        }
        assert_eq!(
            exit_code_for(ControlAction::Stop),
            0,
            "stop must exit 0 even under supervision, so KeepAlive leaves it stopped"
        );
        unsafe {
            std::env::remove_var("TURBOFIG_SUPERVISED");
        }
    }

    #[test]
    fn exit_code_for_restart_depends_on_supervision() {
        let _guard = crate::supervisor::SUPERVISED_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // SAFETY: test-only env mutation, guarded by ENV_TEST_LOCK.
        unsafe {
            std::env::remove_var("TURBOFIG_SUPERVISED");
        }
        assert_eq!(
            exit_code_for(ControlAction::Restart),
            0,
            "unsupervised, the caller starts the new binary, so 0 is fine"
        );
        unsafe {
            std::env::set_var("TURBOFIG_SUPERVISED", "1");
        }
        assert_eq!(
            exit_code_for(ControlAction::Restart),
            crate::supervisor::SUPERVISED_RESTART_EXIT_CODE,
            "supervised, launchd must see a non-zero exit to restart the daemon"
        );
        unsafe {
            std::env::remove_var("TURBOFIG_SUPERVISED");
        }
    }

    #[tokio::test]
    async fn a_second_concurrent_control_call_reports_draining_without_a_second_drain_wait() {
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
            axum::body::Bytes::from(json!({"action": "restart"}).to_string()),
        )
        .await;

        // A repeated call during an ongoing drain gets the exact same 202
        // shape as the first caller: nothing distinguishes it, and it must
        // never start a second drain wait or a second exit timer.
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["action"], json!("restart"));
        assert_eq!(body["draining"], json!(true));
    }

    // The first-caller path (a fresh `try_begin_draining` success) always
    // schedules a real `std::process::exit` in the background, so it is
    // never exercised in-process here: doing so would eventually kill this
    // whole test binary, not just a child. See `daemon/tests/control.rs`'s
    // `assert_control_drains_and_exits`, which spawns the real compiled
    // daemon as a child process instead, precisely so that exit only ever
    // ends the child; it also asserts the 202-at-once reply added here.

    #[tokio::test]
    async fn an_authenticated_malformed_body_is_400_not_401() {
        let state = std::sync::Arc::new(AppState::with_timeout(std::time::Duration::from_millis(
            100,
        )));
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {}", state.token()).parse().unwrap(),
        );
        let (status, Json(body)) = control_handler(
            State(state),
            headers,
            axum::body::Bytes::from("this is not json"),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["ok"], json!(false));
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
