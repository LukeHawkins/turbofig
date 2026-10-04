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

/// Read the plugin reply timeout from `TURBOFIG_REQUEST_TIMEOUT_MS`.
/// Default is 30000 ms.
pub fn request_timeout_from_env() -> Duration {
    request_timeout_or(
        std::env::var("TURBOFIG_REQUEST_TIMEOUT_MS").ok().as_deref(),
        30_000,
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
}
