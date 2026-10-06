//! Environment-driven daemon configuration: ports, request timeout, bridge dir.
//!
//! Every setting here has a safe default and is overridable by one env var.
//! A missing, unparsable, or zero value always falls back to the default;
//! none of these settings can be "turned off" by a bad env var.

use std::path::PathBuf;
use std::time::Duration;

/// Parse a `u16` env value, falling back to `default` when `raw` is absent,
/// fails to parse, or is zero. Zero means an OS-assigned ephemeral port, never
/// a meaningful daemon port, so it is rejected like a parse failure.
fn port_or(raw: Option<&str>, default: u16) -> u16 {
    raw.and_then(|v| v.parse::<u16>().ok())
        .filter(|&p| p != 0)
        .unwrap_or(default)
}

/// Read the MCP HTTP port from `TURBOFIG_MCP_PORT`. Default is 18846.
pub fn port_from_env() -> u16 {
    port_or(std::env::var("TURBOFIG_MCP_PORT").ok().as_deref(), 18846)
}

/// Read the plugin WebSocket port from `TURBOFIG_WS_PORT`. Default is 18847.
pub fn ws_port_from_env() -> u16 {
    port_or(std::env::var("TURBOFIG_WS_PORT").ok().as_deref(), 18847)
}

/// Parse a request timeout in milliseconds, falling back to `default_ms` when
/// `raw` is absent, fails to parse, or is zero.
fn request_timeout_or(raw: Option<&str>, default_ms: u64) -> Duration {
    raw.and_then(|v| v.parse::<u64>().ok())
        .filter(|&ms| ms != 0)
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(default_ms))
}

/// Largest request timeout the daemon ever honours: 10 minutes.
/// `timeoutMs` is forwarded to the plugin, which passes it to a JS
/// `setTimeout`; a value above `2^31 - 1` ms overflows that call and fires
/// almost immediately instead of waiting. 600000 ms is far below that limit
/// and is already an unreasonably long single-call wait, so this clamp can
/// never be the thing standing between a real caller and a real answer.
const MAX_REQUEST_TIMEOUT_MS: u64 = 600_000;

/// Clamp a request timeout to `MAX_REQUEST_TIMEOUT_MS`, logging a warning
/// when the clamp actually changes the value. Split out from
/// `request_timeout_from_env` so it is testable without touching the
/// process environment.
fn clamp_request_timeout(d: Duration) -> Duration {
    let max = Duration::from_millis(MAX_REQUEST_TIMEOUT_MS);
    if d > max {
        eprintln!(
            "turbofig: TURBOFIG_REQUEST_TIMEOUT_MS ({}ms) exceeds the {}ms maximum; clamping",
            d.as_millis(),
            MAX_REQUEST_TIMEOUT_MS
        );
        max
    } else {
        d
    }
}

/// Read the plugin reply timeout from `TURBOFIG_REQUEST_TIMEOUT_MS`.
/// Default is 30000 ms. Clamped to `MAX_REQUEST_TIMEOUT_MS`.
pub fn request_timeout_from_env() -> Duration {
    clamp_request_timeout(request_timeout_or(
        std::env::var("TURBOFIG_REQUEST_TIMEOUT_MS").ok().as_deref(),
        30_000,
    ))
}

/// Default per-connection admission limit: how many EXECUTE/SCREENSHOT jobs
/// the daemon lets run at once against one plugin connection before it starts
/// answering `busy` instead of forwarding more work. See `state::try_admit`.
const DEFAULT_MAX_INFLIGHT: usize = 4;

/// Narrowest and widest a caller may set `TURBOFIG_MAX_INFLIGHT` to.
const MIN_MAX_INFLIGHT: usize = 1;
const MAX_MAX_INFLIGHT: usize = 32;

/// Parse a `usize` env value, falling back to `default` when `raw` is absent
/// or fails to parse, then clamps to `[MIN_MAX_INFLIGHT, MAX_MAX_INFLIGHT]`.
/// The clamp always applies, even to the default and to a parsed value: a
/// missing, unparsable, or out-of-range value never disables admission
/// control (0) and never lets one runaway caller starve every other
/// connection (an unbounded value).
fn max_inflight_or(raw: Option<&str>, default: usize) -> usize {
    raw.and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default)
        .clamp(MIN_MAX_INFLIGHT, MAX_MAX_INFLIGHT)
}

/// Read the per-connection admission limit from `TURBOFIG_MAX_INFLIGHT`.
/// Default 4. See `max_inflight_or` for the clamp.
pub fn max_inflight_from_env() -> usize {
    max_inflight_or(
        std::env::var("TURBOFIG_MAX_INFLIGHT").ok().as_deref(),
        DEFAULT_MAX_INFLIGHT,
    )
}

/// Resolve the bridge directory from optional env string values.
///
/// - If `bridge_dir_val` is `Some(path)`, use it directly.
/// - Else if `home_val` is `Some(home)`, use `<home>/.turbofig`.
/// - Else fall back to `./.turbofig`.
fn bridge_dir_or(bridge_dir_val: Option<&str>, home_val: Option<&str>) -> PathBuf {
    if let Some(val) = bridge_dir_val {
        return PathBuf::from(val);
    }
    match home_val {
        Some(home) => PathBuf::from(home).join(".turbofig"),
        None => PathBuf::from(".turbofig"),
    }
}

/// Read the bridge directory from `TURBOFIG_BRIDGE_DIR`.
/// Default: `~/.turbofig` (falls back to `./.turbofig` if `HOME` is unset).
pub fn bridge_dir_from_env() -> PathBuf {
    bridge_dir_or(
        std::env::var("TURBOFIG_BRIDGE_DIR").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

/// A display form of the bridge directory for the plugin panel's copy-prompt
/// (see `ws.rs`'s `welcome_message`): the real `$HOME` prefix shown as `~`
/// when the bridge dir lives under it, so a user running the default install
/// never sees their own home directory's absolute path in a prompt meant to
/// be pasted elsewhere. Falls back to the plain path when the bridge dir is
/// not under `$HOME` (a custom `TURBOFIG_BRIDGE_DIR`) or `$HOME` is unset.
pub fn bridge_dir_display() -> String {
    bridge_dir_display_for(
        &bridge_dir_from_env(),
        std::env::var("HOME").ok().as_deref(),
    )
}

/// The pure, testable half of `bridge_dir_display`.
fn bridge_dir_display_for(dir: &std::path::Path, home: Option<&str>) -> String {
    if let Some(home) = home {
        if let Ok(rel) = dir.strip_prefix(home) {
            return if rel.as_os_str().is_empty() {
                "~".to_owned()
            } else {
                PathBuf::from("~").join(rel).to_string_lossy().into_owned()
            };
        }
    }
    dir.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_defaults_when_unset() {
        assert_eq!(port_or(None, 18846), 18846);
    }

    #[test]
    fn port_parses_valid_number() {
        assert_eq!(port_or(Some("9000"), 18846), 9000);
    }

    #[test]
    fn port_falls_back_on_garbage_input() {
        assert_eq!(port_or(Some("notaport"), 18846), 18846);
    }

    #[test]
    fn port_rejects_zero_and_falls_back() {
        assert_eq!(port_or(Some("0"), 18846), 18846);
    }

    #[test]
    fn ws_port_defaults_to_18847_when_unset() {
        assert_eq!(port_or(None, 18847), 18847);
    }

    #[test]
    fn request_timeout_defaults_to_30s_when_unset() {
        assert_eq!(
            request_timeout_or(None, 30_000),
            Duration::from_millis(30_000)
        );
    }

    #[test]
    fn request_timeout_parses_valid_number() {
        assert_eq!(
            request_timeout_or(Some("5000"), 30_000),
            Duration::from_millis(5_000)
        );
    }

    #[test]
    fn request_timeout_falls_back_on_garbage_input() {
        assert_eq!(
            request_timeout_or(Some("notanumber"), 30_000),
            Duration::from_millis(30_000)
        );
    }

    #[test]
    fn request_timeout_rejects_zero_and_falls_back() {
        assert_eq!(
            request_timeout_or(Some("0"), 30_000),
            Duration::from_millis(30_000)
        );
    }

    #[test]
    fn clamp_request_timeout_leaves_a_value_at_or_under_the_max_unchanged() {
        assert_eq!(
            clamp_request_timeout(Duration::from_millis(MAX_REQUEST_TIMEOUT_MS)),
            Duration::from_millis(MAX_REQUEST_TIMEOUT_MS)
        );
        assert_eq!(
            clamp_request_timeout(Duration::from_millis(5_000)),
            Duration::from_millis(5_000)
        );
    }

    #[test]
    fn clamp_request_timeout_clamps_a_value_above_the_max() {
        assert_eq!(
            clamp_request_timeout(Duration::from_millis(MAX_REQUEST_TIMEOUT_MS + 1)),
            Duration::from_millis(MAX_REQUEST_TIMEOUT_MS)
        );
        assert_eq!(
            clamp_request_timeout(Duration::from_secs(u64::MAX / 2000)),
            Duration::from_millis(MAX_REQUEST_TIMEOUT_MS)
        );
    }

    #[test]
    fn bridge_dir_uses_explicit_value_when_set() {
        let path = bridge_dir_or(Some("/custom/dir"), None);
        assert_eq!(path, PathBuf::from("/custom/dir"));
    }

    #[test]
    fn bridge_dir_uses_home_turbofig_when_unset() {
        let path = bridge_dir_or(None, Some("/home/alice"));
        assert_eq!(path, PathBuf::from("/home/alice/.turbofig"));
    }

    #[test]
    fn bridge_dir_falls_back_to_relative_when_home_also_unset() {
        let path = bridge_dir_or(None, None);
        assert_eq!(path, PathBuf::from(".turbofig"));
    }

    #[test]
    fn bridge_dir_explicit_value_wins_over_home() {
        let path = bridge_dir_or(Some("/override"), Some("/home/alice"));
        assert_eq!(path, PathBuf::from("/override"));
    }

    #[test]
    fn bridge_dir_display_shows_tilde_for_the_default_under_home() {
        let display =
            bridge_dir_display_for(&PathBuf::from("/home/alice/.turbofig"), Some("/home/alice"));
        assert_eq!(display, "~/.turbofig");
    }

    #[test]
    fn bridge_dir_display_shows_plain_tilde_when_the_dir_is_home_itself() {
        let display = bridge_dir_display_for(&PathBuf::from("/home/alice"), Some("/home/alice"));
        assert_eq!(display, "~");
    }

    #[test]
    fn bridge_dir_display_shows_the_plain_path_for_a_custom_dir_outside_home() {
        let display =
            bridge_dir_display_for(&PathBuf::from("/tmp/custom-bridge"), Some("/home/alice"));
        assert_eq!(display, "/tmp/custom-bridge");
    }

    #[test]
    fn bridge_dir_display_shows_the_plain_path_when_home_is_unset() {
        let display = bridge_dir_display_for(&PathBuf::from("/home/alice/.turbofig"), None);
        assert_eq!(display, "/home/alice/.turbofig");
    }

    #[test]
    fn max_inflight_defaults_to_4_when_unset() {
        assert_eq!(max_inflight_or(None, DEFAULT_MAX_INFLIGHT), 4);
    }

    #[test]
    fn max_inflight_parses_a_valid_value() {
        assert_eq!(max_inflight_or(Some("8"), DEFAULT_MAX_INFLIGHT), 8);
    }

    #[test]
    fn max_inflight_falls_back_on_garbage_input() {
        assert_eq!(max_inflight_or(Some("nope"), DEFAULT_MAX_INFLIGHT), 4);
    }

    #[test]
    fn max_inflight_clamps_below_the_minimum() {
        assert_eq!(max_inflight_or(Some("0"), DEFAULT_MAX_INFLIGHT), 1);
    }

    #[test]
    fn max_inflight_clamps_above_the_maximum() {
        assert_eq!(max_inflight_or(Some("999"), DEFAULT_MAX_INFLIGHT), 32);
    }
}
