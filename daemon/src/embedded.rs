//! The Figma plugin, embedded into the daemon binary at compile time.
//!
//! `build.rs` generates `OUT_DIR/embedded_plugin_data.rs`, which embeds
//! `plugin/manifest.json`, `plugin/dist/code.js`, and `plugin/dist/ui.html`
//! via `include_str!` when they exist at build time, or a stub that embeds
//! nothing otherwise (a Rust-only build with no `plugin/dist`). This module
//! wraps that generated function in a typed, documented API.

include!(concat!(env!("OUT_DIR"), "/embedded_plugin_data.rs"));

/// The three files that make up one built Figma plugin.
#[derive(Debug, Clone, Copy)]
pub struct EmbeddedPlugin {
    /// Contents of `plugin/manifest.json`.
    pub manifest: &'static str,
    /// Contents of `plugin/dist/code.js`, the plugin main-thread bundle.
    pub code_js: &'static str,
    /// Contents of `plugin/dist/ui.html`, the plugin UI bundle. Carries the
    /// `__TURBOFIG_PAIRING_TOKEN__` placeholder in place of a real token; see
    /// `plugin_files::write_plugin_files`.
    pub ui_html: &'static str,
}

/// Returns the embedded plugin, or `None` when the daemon was built without
/// `plugin/dist` present (a Rust-only build). A caller that needs the plugin
/// (e.g. a future `turbofig setup` command) must handle `None` by telling the
/// user to install a release build, or to run `cd plugin && bun run build`
/// first in a source checkout.
pub fn embedded_plugin() -> Option<EmbeddedPlugin> {
    raw_embedded_plugin().map(|(manifest, code_js, ui_html)| EmbeddedPlugin {
        manifest,
        code_js,
        ui_html,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This repo's `plugin/dist` is built, so in this dev/test environment the
    /// embedded plugin must be present and non-empty. A release build without
    /// `plugin/dist` is covered by `raw_embedded_plugin`'s stub path, which
    /// `build.rs` exercises directly (no unit test can toggle that at test time,
    /// since embedding happens at compile time).
    #[test]
    fn embedded_plugin_is_present_when_plugin_dist_was_built() {
        let plugin = embedded_plugin().expect(
            "plugin/dist must exist in this checkout; run `cd plugin && bun run build` first",
        );
        assert!(plugin.manifest.contains("\"name\""));
        assert!(!plugin.code_js.is_empty());
        assert!(!plugin.ui_html.is_empty());
    }

    /// `build.rs` must always normalize the pairing-token slot back to the
    /// `__TURBOFIG_PAIRING_TOKEN__` placeholder before embedding, even when a
    /// contributor's local `dist/ui.html` carries a real token read from
    /// their own `~/.turbofig/token` (`plugin/build-ui.ts`'s
    /// `readLocalToken`). A real 64-hex-char token baked into the binary can
    /// never be replaced later by `write_plugin_files`, since the placeholder
    /// it looks for is gone. See `build.rs`'s `normalize_ui_html`.
    #[test]
    fn embedded_ui_html_always_carries_the_placeholder_never_a_real_token() {
        let plugin = embedded_plugin().expect("plugin/dist must exist in this checkout");
        assert!(
            plugin.ui_html.contains("__TURBOFIG_PAIRING_TOKEN__"),
            "embedded ui.html must contain the pairing-token placeholder"
        );

        // A real token is 64 lowercase hex characters (see token::TOKEN_BYTES).
        // Scan for any run of 64+ contiguous lowercase-hex characters; the
        // placeholder itself is not hex (it contains underscores and
        // uppercase letters), so this can never false-positive on it.
        let is_lowercase_hex_digit = |b: &u8| b.is_ascii_digit() || matches!(b, b'a'..=b'f');
        let hex_run_found = plugin
            .ui_html
            .as_bytes()
            .split(|b| !is_lowercase_hex_digit(b))
            .any(|run| run.len() >= 64);
        assert!(
            !hex_run_found,
            "embedded ui.html must never contain a real 64-hex-char pairing token"
        );
    }
}
