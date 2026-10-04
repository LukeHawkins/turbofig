//! `turbofig_screenshot`: capture a PNG screenshot of a Figma node.

use crate::image;
use crate::ops::budget::{inline_screenshot_warning, with_warning};
use crate::plugin_call::{call_plugin, CallOutcome};
use crate::routing::{resolve_route, route_error_to_json};
use crate::state::AppState;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde_json::{json, Value};
use std::sync::Arc;

/// Documented valid range for the `scale` export factor. Figma itself only
/// supports this range; anything outside it is clamped rather than sent on
/// to the plugin, in both the MCP and the filesystem-bridge paths (they both
/// call this one function).
const MIN_SCALE: f64 = 0.1;
const MAX_SCALE: f64 = 4.0;

/// Build a timeout error message that names the request id and warns that a
/// retry is not safe.
fn timeout_error(id: u64) -> String {
    format!(
        "plugin timed out (requestId {id}); the job may still be running in Figma, \
         a retry is not idempotent"
    )
}

/// Capture a PNG screenshot of a Figma node and return a JSON value.
///
/// No plugin connected -> `{"ok":false,"error":"no plugin connected"}`.
/// Plugin replies with ok:false -> `{"ok":false,"error":"..."}`.
/// Timeout -> `{"ok":false,"error":"plugin timed out (requestId N); ..."}`.
/// On success with return_mode "inline": returns `{"ok":true,"w":w,"h":h,"png":<base64>}`.
/// On success with return_mode "file": decodes base64, writes to
///   `output_dir/<requestId>-<nanos>.png`, returns `{"ok":true,"path":"...","w":w,"h":h}`.
///
/// Reused by both `turbofig_screenshot` (MCP) and the filesystem bridge.
/// `scale` is clamped to `[0.1, 4.0]` before it ever reaches the plugin.
#[allow(clippy::too_many_arguments)]
pub async fn run_screenshot(
    state: &Arc<AppState>,
    session_id: Option<&str>,
    file_key: Option<&str>,
    scale: f64,
    node_id: Option<&str>,
    return_mode: &str,
    output_dir: Option<&std::path::Path>,
    max_dim: u32,
    full_res: bool,
) -> Value {
    let (conn_id, tx, _, _) = match resolve_route(state, session_id, file_key) {
        Ok(r) => r,
        Err(e) => return route_error_to_json(e),
    };

    let scale = scale.clamp(MIN_SCALE, MAX_SCALE);
    let request = json!({
        "type": "SCREENSHOT",
        "scale": scale,
        "nodeId": node_id,
        "sessionId": session_id.unwrap_or("")
    });

    let (id, outcome) = call_plugin(state, conn_id, &tx, request, state.request_timeout).await;

    let reply = match outcome {
        CallOutcome::Reply(reply) => reply,
        CallOutcome::Disconnected => return json!({"ok": false, "error": "plugin disconnected"}),
        CallOutcome::NotConnected => return json!({"ok": false, "error": "plugin send failed"}),
        CallOutcome::TimedOut => return json!({"ok": false, "error": timeout_error(id)}),
    };

    if !reply.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        let error = reply
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("screenshot failed")
            .to_owned();
        return json!({"ok": false, "error": error});
    }
    let Some(png_b64) = reply.get("png").and_then(|v| v.as_str()) else {
        return json!({"ok": false, "error": "screenshot failed"});
    };
    // Read plugin-reported dims as f64: node dimensions can be fractional,
    // and as_u64 would silently truncate a fractional value to 0.
    let plugin_w = reply.get("w").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let plugin_h = reply.get("h").and_then(|v| v.as_f64()).unwrap_or(0.0);

    let raw_bytes = match B64.decode(png_b64) {
        Ok(b) => b,
        Err(_) => return json!({"ok": false, "error": "invalid base64 png"}),
    };

    let max_dim = max_dim.max(1);
    // Peek the header only; a full decode is needed only when a resize is
    // actually required.
    let probed = image::probe_dims(&raw_bytes);
    let needs_resize = !full_res && probed.map(|(w, h)| w.max(h) > max_dim).unwrap_or(false);

    // `resized` is true only when the bytes in `final_bytes` differ from the
    // plugin's original PNG, so the inline path below knows whether it can
    // reuse the plugin's own base64 verbatim or must re-encode.
    let (final_bytes, w, h, resized): (Vec<u8>, Value, Value, bool) = if needs_resize {
        // The expensive path: full pixel decode + Lanczos3 resize + PNG
        // re-encode. Run it on a blocking thread so it never stalls the
        // async runtime. Clone only here, on the path that already pays for
        // a full image decode; the common unscaled path below never clones.
        let input = raw_bytes.clone();
        match tokio::task::spawn_blocking(move || image::resize_png(&input, max_dim)).await {
            Ok((resized_bytes, rw, rh)) if !(rw == 0 && rh == 0) => {
                (resized_bytes, json!(rw), json!(rh), true)
            }
            // resize_png failed to decode: fall back to the untouched bytes
            // and the plugin's own reported dims.
            Ok(_) => (raw_bytes, json!(plugin_w), json!(plugin_h), false),
            Err(_) => return json!({"ok": false, "error": "screenshot resize task failed"}),
        }
    } else {
        // No resize needed: pass the bytes through untouched. Dims come from
        // the cheap header probe, falling back to the plugin's own report
        // when the payload was not a decodable PNG (test stubs).
        match probed {
            Some((w, h)) => (raw_bytes, json!(w), json!(h), false),
            None => (raw_bytes, json!(plugin_w), json!(plugin_h), false),
        }
    };

    if return_mode == "inline" {
        let (out_b64, len) = if resized {
            let s = B64.encode(&final_bytes);
            let len = s.len();
            (s, len)
        } else {
            // Reuse the plugin's own base64 verbatim: no decode, no
            // re-encode, no extra allocation beyond this one Value::String.
            (png_b64.to_owned(), png_b64.len())
        };
        let resp = json!({"ok": true, "w": w, "h": h, "png": out_b64});
        with_warning(resp, inline_screenshot_warning(len))
    } else {
        let Some(dir) = output_dir else {
            return json!({"ok": false, "error": "file mode needs an output dir"});
        };
        if let Err(e) = tokio::fs::create_dir_all(dir).await {
            return json!({"ok": false, "error": format!("write failed: {e}")});
        }
        // Name the file by the (randomly started, monotonic) request id plus
        // a nanosecond suffix, never by a restart-local counter: a counter
        // that resets to 1 on every daemon restart would let a new
        // screenshot silently overwrite an old one with the same name.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = dir.join(format!("{id}-{nanos}.png"));
        if let Err(e) = tokio::fs::write(&path, &final_bytes).await {
            return json!({"ok": false, "error": format!("write failed: {e}")});
        }
        json!({"ok": true, "path": path.to_string_lossy(), "w": w, "h": h})
    }
}
