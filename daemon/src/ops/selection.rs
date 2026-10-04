//! `turbofig_get_selection`: return the current Figma selection, shaped.

use crate::ops::budget::{read_budget_warning, with_warning};
use crate::plugin_call::{call_plugin, CallOutcome};
use crate::routing::{resolve_route, route_error_to_json};
use crate::state::AppState;
use serde_json::{json, Value};
use std::sync::Arc;

/// Send GET_SELECTION to the target plugin and return the result as a JSON value.
///
/// No plugin connected -> `{"ok":false,"error":"no plugin connected"}`.
/// Plugin replies with ok:true -> `{"ok":true,"selection":[...]}`.
/// Plugin replies with ok:false -> `{"ok":false,"error":"..."}`.
/// Recv error -> `{"ok":false,"error":"plugin disconnected"}`.
/// Timeout -> `{"ok":false,"error":"plugin timed out (requestId N); ..."}`.
///
/// `fields` adds named node properties to each item beyond the base seven.
/// `depth` controls child traversal (0 = top-level only, clamped to 5).
/// Omit both for the compact default shape. Depth is clamped daemon-side
/// before the request reaches the plugin.
///
/// Reused by both `turbofig_get_selection` (MCP) and the filesystem bridge.
pub async fn run_get_selection(
    state: &Arc<AppState>,
    session_id: Option<&str>,
    file_key: Option<&str>,
    fields: Option<&[String]>,
    depth: Option<u32>,
) -> Value {
    let (conn_id, tx, _, _) = match resolve_route(state, session_id, file_key) {
        Ok(r) => r,
        Err(e) => return route_error_to_json(e),
    };

    let mut request = json!({"type": "GET_SELECTION", "sessionId": session_id.unwrap_or("")});
    if let Some(f) = fields {
        request["fields"] = json!(f);
    }
    if let Some(d) = depth {
        request["depth"] = json!(d.min(5));
    }

    let (id, outcome) = call_plugin(state, conn_id, &tx, request, state.request_timeout).await;

    match outcome {
        CallOutcome::Reply(reply) => {
            if reply.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                let selection = reply
                    .get("selection")
                    .cloned()
                    .unwrap_or_else(|| Value::Array(vec![]));
                let len = serde_json::to_string(&selection)
                    .map(|s| s.len())
                    .unwrap_or(0);
                let resp = json!({"ok": true, "selection": selection});
                with_warning(resp, read_budget_warning(len))
            } else {
                let error = reply
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("get_selection failed")
                    .to_owned();
                json!({"ok": false, "error": error})
            }
        }
        CallOutcome::Disconnected => {
            json!({"ok": false, "error": "plugin disconnected"})
        }
        CallOutcome::NotConnected => {
            json!({"ok": false, "error": "plugin send failed"})
        }
        CallOutcome::TimedOut => {
            json!({
                "ok": false,
                "error": format!(
                    "plugin timed out (requestId {id}); the job may still be running in Figma, \
                     a retry is not idempotent"
                )
            })
        }
    }
}
