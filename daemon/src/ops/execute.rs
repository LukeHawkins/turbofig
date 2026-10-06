//! `turbofig_execute`: run JavaScript in the Figma plugin context.

use crate::ops::budget::{read_budget_warning, with_warning};
use crate::plugin_call::{
    busy_json, call_plugin, idempotency_error_json, plugin_disconnected_json, CallOutcome,
};
use crate::routing::{resolve_route, route_error_to_json};
use crate::state::{try_admit, AppState};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

/// How much longer than the daemon's own request timeout to wait for an
/// EXECUTE reply. The plugin receives the daemon's timeout as `timeoutMs`
/// and is expected to give up and reply `ok:false` at that point itself; the
/// daemon waits a little past that so the plugin's own "I gave up" reply
/// (which is more informative than a bare daemon-side timeout) usually wins
/// the race.
const EXECUTE_GRACE: Duration = Duration::from_millis(1_000);

/// Send `code` to the target plugin and return the result as a JSON value.
///
/// No plugin connected -> `{"ok":false,"code":"plugin_disconnected","error":"no plugin connected"}`.
/// Plugin replies -> pass the RESULT through, plus the plugin's own `code`
///   when it is `ok:false` (default `"script_error"` if the plugin sent none).
/// Timeout or mid-call disconnect -> `code":"not_started"` when no STARTED
///   frame ever arrived for this job, `"started_unknown"` when one did (see
///   `plugin_call::idempotency_error_json`).
/// Lane already at the admission-control cap -> `code":"busy"` with a
///   `queueDepth` and a `retryAfterMs` hint, without ever reaching the
///   plugin (see `state::try_admit`).
///
/// Reused by both `turbofig_execute` (MCP) and the filesystem bridge.
pub async fn run_execute(
    state: &Arc<AppState>,
    session_id: Option<&str>,
    file_key: Option<&str>,
    code: &str,
) -> Value {
    let (conn_id, tx, _, _) = match resolve_route(state, session_id, file_key) {
        Ok(r) => r,
        Err(e) => return route_error_to_json(e),
    };

    let _admit = match try_admit(state, conn_id) {
        Ok(guard) => guard,
        Err(busy) => return busy_json(busy.queue_depth, busy.retry_after_ms),
    };

    let timeout_ms = state.request_timeout.as_millis().min(u128::from(u64::MAX)) as u64;
    let request = json!({
        "type": "EXECUTE",
        "code": code,
        "sessionId": session_id.unwrap_or(""),
        "timeoutMs": timeout_ms
    });
    let wait = state.request_timeout + EXECUTE_GRACE;
    let (id, outcome) = call_plugin(state, conn_id, &tx, request, wait).await;

    match outcome {
        CallOutcome::Reply(reply) => {
            if reply.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                let result = reply.get("result").cloned().unwrap_or(Value::Null);
                let len = serde_json::to_string(&result).map(|s| s.len()).unwrap_or(0);
                let resp = json!({"ok": true, "result": result});
                with_warning(resp, read_budget_warning(len))
            } else {
                let error = reply
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("eval failed")
                    .to_owned();
                let code = reply
                    .get("code")
                    .and_then(|v| v.as_str())
                    .unwrap_or("script_error")
                    .to_owned();
                json!({"ok": false, "error": error, "code": code})
            }
        }
        CallOutcome::Disconnected(started) => idempotency_error_json(id, started),
        CallOutcome::NotConnected => plugin_disconnected_json(),
        CallOutcome::TimedOut(started) => idempotency_error_json(id, started),
    }
}
