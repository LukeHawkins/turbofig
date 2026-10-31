//! `turbofig_screenshot`: capture a PNG screenshot of a Figma node.

use crate::image;
use crate::ops::budget::{inline_screenshot_warning, with_warning};
use crate::plugin_call::{
    busy_json, call_plugin, idempotency_error_json, plugin_disconnected_json, CallOutcome,
};
use crate::routing::{resolve_route, route_error_to_json};
use crate::state::{try_admit, AppState};
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

/// Capture a PNG screenshot of a Figma node and return a JSON value.
///
/// No plugin connected -> `{"ok":false,"code":"plugin_disconnected","error":"..."}`.
/// Plugin replies with ok:false -> `{"ok":false,"code":"...","error":"..."}`.
/// Timeout or mid-call disconnect -> `code":"not_started"`/`"started_unknown"`
///   (see `plugin_call::idempotency_error_json`).
/// Lane already at the admission-control cap -> `code":"busy"` (see
///   `state::try_admit`), without ever reaching the plugin.
/// On success with return_mode "inline": returns `{"ok":true,"w":w,"h":h,"png":<base64>}`.
/// On success with return_mode "file": decodes base64, writes to
///   `output_dir/<requestId>-<base36 millis>.png`, returns `{"ok":true,"path":"...","w":w,"h":h}`.
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

    let _admit = match try_admit(state, conn_id) {
        Ok(guard) => guard,
        Err(busy) => return busy_json(busy.queue_depth, busy.retry_after_ms),
    };

    let scale = scale.clamp(MIN_SCALE, MAX_SCALE);
    let timeout_ms = state.request_timeout.as_millis().min(u128::from(u64::MAX)) as u64;
    let max_dim = max_dim.max(1);
    let request = screenshot_frame(scale, node_id, session_id, timeout_ms, max_dim, full_res);

    let (id, outcome) = call_plugin(state, conn_id, &tx, request, state.request_timeout).await;

    let reply = match outcome {
        CallOutcome::Reply(reply) => reply,
        CallOutcome::Disconnected(started) => return idempotency_error_json(started),
        CallOutcome::NotConnected => return plugin_disconnected_json(),
        CallOutcome::TimedOut(started) => return idempotency_error_json(started),
    };

    if !reply.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        let error = reply
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("screenshot failed")
            .to_owned();
        let code = reply
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or("script_error")
            .to_owned();
        return json!({"ok": false, "error": error, "code": code});
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

    // Peek the header only; a full decode is needed only when a resize is
    // actually required.
    let probed = image::probe_dims(&raw_bytes);
    let needs_resize = !full_res && probed.map(|(w, h)| w.max(h) > max_dim).unwrap_or(false);

    // `resized` is true only when the bytes in `final_bytes` differ from the
    // plugin's original PNG, so the inline path below knows whether it can
    // reuse the plugin's own base64 verbatim or must re-encode.
    let (final_bytes, w, h, resized): (Vec<u8>, Value, Value, bool) = if needs_resize {
        // The expensive path: full pixel decode + resize + PNG re-encode.
        // Run it on a blocking thread so it never stalls the async runtime.
        // The bytes move in and come back, so there is no multi-MB clone.
        match tokio::task::spawn_blocking(move || {
            let (out, rw, rh) = image::resize_png(&raw_bytes, max_dim);
            (out, rw, rh, raw_bytes)
        })
        .await
        {
            Ok((resized_bytes, rw, rh, _)) if !(rw == 0 && rh == 0) => {
                (resized_bytes, json!(rw), json!(rh), true)
            }
            // resize_png failed to decode: keep the untouched input and the
            // plugin's own reported dims.
            Ok((_, _, _, orig)) => (orig, json!(plugin_w), json!(plugin_h), false),
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
        // base36 millis, never by a restart-local counter alone. A short
        // suffix is added when the name is taken, so calls in the same
        // millisecond and daemon restarts never overwrite an old file.
        let stem = screenshot_stem(id);
        let path = match write_unique(dir, &stem, &final_bytes).await {
            Ok(p) => p,
            Err(e) => return json!({"ok": false, "error": format!("write failed: {e}")}),
        };
        json!({"ok": true, "path": path.to_string_lossy(), "w": w, "h": h})
    }
}

/// Build the outbound SCREENSHOT frame. `maxDim` goes to the plugin only when
/// the consumer did not ask for full resolution, so the plugin can cap its
/// export scale. The daemon still downscales as a fallback for old plugins.
fn screenshot_frame(
    scale: f64,
    node_id: Option<&str>,
    session_id: Option<&str>,
    timeout_ms: u64,
    max_dim: u32,
    full_res: bool,
) -> Value {
    let mut frame = json!({
        "type": "SCREENSHOT",
        "scale": scale,
        "nodeId": node_id,
        "sessionId": session_id.unwrap_or(""),
        "timeoutMs": timeout_ms
    });
    if !full_res {
        frame["maxDim"] = json!(max_dim);
    }
    frame
}

/// `<id>-<base36 millis>`: short, and unique across restarts.
fn screenshot_stem(id: impl std::fmt::Display) -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{id}-{}", base36(millis))
}

fn base36(mut n: u128) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".to_owned();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// Write `bytes` to `dir/<stem>.png`. If the name exists, try `<stem>-1.png`,
/// `<stem>-2.png`, and so on. Never overwrites a file.
async fn write_unique(
    dir: &std::path::Path,
    stem: &str,
    bytes: &[u8],
) -> std::io::Result<std::path::PathBuf> {
    use tokio::io::AsyncWriteExt;
    for n in 0..1000u32 {
        let name = if n == 0 {
            format!("{stem}.png")
        } else {
            format!("{stem}-{}.png", base36(u128::from(n)))
        };
        let path = dir.join(name);
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
        {
            Ok(mut f) => {
                if let Err(e) = f.write_all(bytes).await {
                    let _ = tokio::fs::remove_file(&path).await;
                    return Err(e);
                }
                if let Err(e) = f.flush().await {
                    let _ = tokio::fs::remove_file(&path).await;
                    return Err(e);
                }
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "no free screenshot file name",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_carries_max_dim_by_default() {
        let f = screenshot_frame(2.0, Some("1:2"), None, 5000, 1200, false);
        assert_eq!(f["maxDim"], json!(1200));
        assert_eq!(f["type"], json!("SCREENSHOT"));
    }

    #[test]
    fn frame_omits_max_dim_with_full_res() {
        let f = screenshot_frame(2.0, None, None, 5000, 1200, true);
        assert!(f.get("maxDim").is_none(), "{f}");
    }

    #[test]
    fn base36_encodes() {
        assert_eq!(base36(0), "0");
        assert_eq!(base36(35), "z");
        assert_eq!(base36(36), "10");
    }

    #[tokio::test]
    async fn same_stem_twice_gets_distinct_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let stem = "7-abc";
        let a = write_unique(dir.path(), stem, b"one").await.expect("a");
        let b = write_unique(dir.path(), stem, b"two").await.expect("b");
        assert_ne!(a, b);
        assert_eq!(b.file_name().and_then(|n| n.to_str()), Some("7-abc-1.png"));
        assert_eq!(std::fs::read(&a).expect("read a"), b"one");
        assert_eq!(std::fs::read(&b).expect("read b"), b"two");
    }

    #[test]
    fn stem_is_short() {
        let s = screenshot_stem(123456789u64);
        assert!(s.len() <= 9 + 1 + 9, "{s}");
    }
}
