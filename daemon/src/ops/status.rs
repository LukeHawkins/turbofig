//! `turbofig_status`: daemon and plugin liveness.

use crate::plugin_call::{call_plugin, CallOutcome};
use crate::routing::{resolve_route_graced, RouteError, COLD_START_POLL, COLD_START_WAIT};
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
///
/// On a young daemon with an empty registry it first waits, for at most
/// `COLD_START_WAIT`, for the first file: a plugin that was open before the
/// daemon started needs up to ~2 s to reconnect. An older daemon never waits, and
/// neither does a draining one.
pub async fn run_files(state: &Arc<AppState>) -> Value {
    let deadline = std::time::Instant::now() + COLD_START_WAIT;
    while state.in_cold_start_grace()
        && !state.is_draining()
        && std::time::Instant::now() < deadline
    {
        tokio::time::sleep(COLD_START_POLL).await;
    }
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
    let (conn_id, tx, fk, name) = match resolve_route_graced(state, session_id, file_key).await {
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

    #[tokio::test]
    async fn files_does_not_wait_in_the_cold_start_window_while_draining() {
        let state = Arc::new(AppState::with_timeout(Duration::from_millis(500)));
        state.set_draining(true);
        let t = std::time::Instant::now();
        assert_eq!(run_files(&state).await, json!({"ok": true, "plugins": []}));
        assert!(t.elapsed() < COLD_START_WAIT - Duration::from_secs(1));
    }

    #[tokio::test]
    async fn files_waits_in_the_cold_start_window_for_the_first_file() {
        let state = Arc::new(AppState::with_timeout(Duration::from_millis(500)));
        let s2 = state.clone();
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let id = s2.add_connection(tx);
            s2.set_connection_info(id, "fk1".to_owned(), "File 1".to_owned());
        });
        let v = run_files(&state).await;
        assert_eq!(v["plugins"][0]["fileKey"], json!("fk1"));
    }

    #[tokio::test]
    async fn files_never_waits_once_the_cold_start_grace_is_off() {
        let state = Arc::new(AppState::with_timeout(Duration::from_millis(500)));
        state.skip_cold_start_grace();
        let t = std::time::Instant::now();
        assert_eq!(run_files(&state).await, json!({"ok": true, "plugins": []}));
        // A grace wait would last `COLD_START_WAIT` (3 s).
        assert!(t.elapsed() < Duration::from_millis(2_000));
    }

    #[tokio::test]
    async fn files_lists_the_registry_without_any_plugin_round_trip() {
        let state = Arc::new(AppState::with_timeout(Duration::from_millis(500)));
        state.skip_cold_start_grace();
        assert_eq!(run_files(&state).await, json!({"ok": true, "plugins": []}));
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
            run_files(&state).await,
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
