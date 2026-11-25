//! `turbofig_status`: daemon and plugin liveness.

use crate::plugin_call::{call_plugin, CallOutcome};
use crate::routing::{resolve_route, RouteError};
use crate::state::AppState;
use serde_json::{json, Value};
use std::sync::Arc;

/// Drop the fields that only say "nothing is happening": `saturated:false`
/// and an empty `plugins` list. The consumer reads every token of this reply.
fn shape(mut v: Value) -> Value {
    if let Some(obj) = v.as_object_mut() {
        if obj.get("saturated") == Some(&Value::Bool(false)) {
            obj.remove("saturated");
        }
        if obj
            .get("plugins")
            .and_then(|p| p.as_array())
            .is_some_and(|a| a.is_empty())
        {
            obj.remove("plugins");
        }
    }
    v
}

/// The connected files from the daemon's own registry. It sends nothing to
/// any plugin, so it never waits on a slow or silent one. `plugins` is always
/// present, empty when no file is connected.
pub fn run_files(state: &Arc<AppState>) -> Value {
    json!({"ok": true, "plugins": state.named_connections_json()})
}

/// Run the status check and return a JSON value.
///
/// Resolves the target connection via session_id and file_key.
/// - Resolved -> sends STATUS, awaits RESULT, returns the connected shape with
///   a `"plugins"` list. Returns `responsive:false` on timeout.
/// - NoPlugin -> `{"ok":true,"plugin":{"connected":false,"reason":...}}`.
/// - Ambiguous -> `{"ok":true,"plugin":{"connected":true},"plugins":[...]}`.
/// - NotFound -> `{"ok":true,"plugin":{"connected":false},"plugins":[...]}`.
///
/// `saturated:false` and an empty `plugins` list are omitted.
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
            // No plugin connected at all: name why, and how long ago one was
            // last seen, so a caller recovering from a Figma restart (the
            // plugin cannot be auto-launched; see skills/file-bridge.md)
            // knows what to tell the user instead of just retrying blind.
            let mut plugin = json!({
                "connected": false,
                "reason": "open the turbofig plugin in Figma"
            });
            if let Some(ago) = state.last_disconnect_ago_ms() {
                plugin["lastDisconnectAgoMs"] = json!(ago);
            }
            return shape(json!({
                "ok": true,
                "plugin": plugin
            }));
        }
        Err(RouteError::Ambiguous(_)) => {
            let plugins = state.named_connections_json();
            return shape(json!({
                "ok": true,
                "saturated": state.any_saturated(),
                "plugin": {"connected": true},
                "plugins": plugins
            }));
        }
        Err(RouteError::NotFound(_, _)) => {
            let plugins = state.named_connections_json();
            return shape(json!({
                "ok": true,
                "saturated": state.any_saturated(),
                "plugin": {"connected": false},
                "plugins": plugins
            }));
        }
        Err(RouteError::Draining) => {
            return shape(json!({
                "ok": true,
                "draining": true,
                "plugin": {"connected": false}
            }));
        }
    };

    let request = json!({"type": "STATUS", "sessionId": session_id.unwrap_or("")});
    let (id, outcome) = call_plugin(state, conn_id, &tx, request, state.request_timeout).await;

    shape(match outcome {
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
                "saturated": state.any_saturated(),
                "plugin": {
                    "connected": true,
                    "fileKey": fk,
                    "name": nm
                },
                "plugins": plugins
            })
        }
        CallOutcome::Disconnected(_) | CallOutcome::NotConnected => {
            let plugins = state.named_connections_json();
            json!({
                "ok": true,
                "saturated": state.any_saturated(),
                "plugin": {"connected": false},
                "plugins": plugins
            })
        }
        CallOutcome::TimedOut(_) => {
            let plugins = state.named_connections_json();
            // Name the unresponsive file so the caller knows which one is silent.
            json!({
                "ok": true,
                "saturated": state.any_saturated(),
                "plugin": {
                    "connected": true,
                    "responsive": false,
                    "code": "timeout",
                    "fileKey": fk,
                    "name": name,
                    "requestId": id
                },
                "plugins": plugins
            })
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::mpsc;

    /// One connected file whose plugin answers STATUS.
    async fn one_file_status(plugin_version: &str) -> Value {
        let state = Arc::new(AppState::with_timeout(Duration::from_millis(500)));
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let conn = state.add_connection(tx);
        state.set_connection_info_with_version(
            conn,
            "fk1".to_owned(),
            "File 1".to_owned(),
            plugin_version.to_owned(),
        );
        let state2 = state.clone();
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                let req: Value = serde_json::from_str(&msg).expect("frame");
                let id = req["requestId"].as_u64().expect("requestId");
                state2.resolve(id, conn, json!({"ok": true}));
            }
        });
        run_status(&state, None, None).await
    }

    #[test]
    fn files_lists_the_registry_without_any_plugin_round_trip() {
        let state = Arc::new(AppState::with_timeout(Duration::from_millis(500)));
        assert_eq!(run_files(&state), json!({"ok": true, "plugins": []}));
        // The plugin never reads its channel: a `status` call would time out.
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let conn = state.add_connection(tx);
        state.set_connection_info_with_version(
            conn,
            "fk1".to_owned(),
            "File 1".to_owned(),
            env!("CARGO_PKG_VERSION").to_owned(),
        );
        assert_eq!(
            run_files(&state),
            json!({"ok": true, "plugins": [{"fileKey": "fk1", "name": "File 1"}]})
        );
    }

    #[tokio::test]
    async fn idle_connected_file_status_omits_nothing_happening_fields() {
        let v = one_file_status(env!("CARGO_PKG_VERSION")).await;
        assert_eq!(
            v,
            json!({
                "ok": true,
                "plugin": {"connected": true, "fileKey": "fk1", "name": "File 1"},
                "plugins": [{"fileKey": "fk1", "name": "File 1"}]
            })
        );
    }

    #[tokio::test]
    async fn status_keeps_plugin_version_when_it_differs_from_the_daemon() {
        let v = one_file_status("0.0.1").await;
        assert_eq!(v["plugins"][0]["pluginVersion"], json!("0.0.1"));
        assert!(v["plugins"][0]["warning"].is_string());
    }

    #[tokio::test]
    async fn no_plugin_status_is_short_and_has_no_empty_fields() {
        let state = Arc::new(AppState::with_timeout(Duration::from_millis(50)));
        let v = run_status(&state, None, None).await;
        assert_eq!(
            v,
            json!({
                "ok": true,
                "plugin": {"connected": false, "reason": "open the turbofig plugin in Figma"}
            })
        );
    }
}
