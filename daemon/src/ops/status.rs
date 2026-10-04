//! `turbofig_status`: daemon and plugin liveness.

use crate::plugin_call::{call_plugin, CallOutcome};
use crate::routing::{resolve_route, RouteError};
use crate::state::AppState;
use serde_json::{json, Value};
use std::sync::Arc;

/// Run the status check and return a JSON value.
///
/// Resolves the target connection via session_id and file_key.
/// - Resolved -> sends STATUS, awaits RESULT, returns the connected shape with
///   a `"plugins"` list. Returns `responsive:false` on timeout.
/// - NoPlugin -> `{"ok":true,"plugin":{"connected":false},"plugins":[]}`.
/// - Ambiguous -> `{"ok":true,"plugin":{"connected":true},"plugins":[...]}`.
/// - NotFound -> `{"ok":true,"plugin":{"connected":false},"plugins":[...]}`.
///
/// `"ok":true` always means the daemon is alive regardless of plugin state.
/// Reused by both `turbofig_status` (MCP) and the filesystem bridge.
pub async fn run_status(
    state: &Arc<AppState>,
    session_id: Option<&str>,
    file_key: Option<&str>,
) -> Value {
    let (conn_id, tx, fk, name) = match resolve_route(state, session_id, file_key) {
        Ok(r) => r,
        Err(RouteError::NoPlugin) => {
            return json!({"ok": true, "plugin": {"connected": false}, "plugins": []});
        }
        Err(RouteError::Ambiguous(_)) => {
            let plugins = state.named_connections_json();
            return json!({"ok": true, "plugin": {"connected": true}, "plugins": plugins});
        }
        Err(RouteError::NotFound(_, _)) => {
            let plugins = state.named_connections_json();
            return json!({"ok": true, "plugin": {"connected": false}, "plugins": plugins});
        }
    };

    let request = json!({"type": "STATUS", "sessionId": session_id.unwrap_or("")});
    let (id, outcome) = call_plugin(state, conn_id, &tx, request, state.request_timeout).await;

    match outcome {
        CallOutcome::Reply(result) => {
            let fk = result
                .get("fileKey")
                .and_then(|v| v.as_str())
                .unwrap_or(&fk)
                .to_owned();
            let nm = result
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(&name)
                .to_owned();
            let plugins = state.named_connections_json();
            json!({
                "ok": true,
                "plugin": {
                    "connected": true,
                    "fileKey": fk,
                    "name": nm
                },
                "plugins": plugins
            })
        }
        CallOutcome::Disconnected | CallOutcome::NotConnected => {
            let plugins = state.named_connections_json();
            json!({"ok": true, "plugin": {"connected": false}, "plugins": plugins})
        }
        CallOutcome::TimedOut => {
            let plugins = state.named_connections_json();
            // Name the unresponsive file so the caller knows which one is silent.
            json!({
                "ok": true,
                "plugin": {
                    "connected": true,
                    "responsive": false,
                    "fileKey": fk,
                    "name": name,
                    "requestId": id
                },
                "plugins": plugins
            })
        }
    }
}
