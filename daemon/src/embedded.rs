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

/// Literal placeholder `write_plugin_files` looks for and replaces with the
/// real pairing token. Must match `plugin_files::TOKEN_PLACEHOLDER`.
const TOKEN_PLACEHOLDER: &str = "__TURBOFIG_PAIRING_TOKEN__";

/// True when `ui_html` carries the pairing-token placeholder.
/// `--check-embedded` uses this to fail a release build whose `ui.html`
/// lost the placeholder (see `build.rs`'s `normalize_ui_html`), which would
/// make `write_plugin_files` unable to ever inject a real token.
pub fn ui_html_has_placeholder(ui_html: &str) -> bool {
    ui_html.contains(TOKEN_PLACEHOLDER)
}

/// True when `ui_html` contains a run of 64+ contiguous lowercase hex
/// characters, i.e. what a real pairing token looks like (see
/// `token::TOKEN_BYTES`). The placeholder itself is not hex (it contains
/// underscores and uppercase letters), so this never false-positives on it.
pub fn ui_html_contains_a_real_token(ui_html: &str) -> bool {
    let is_lowercase_hex_digit = |b: &u8| b.is_ascii_digit() || matches!(b, b'a'..=b'f');
    ui_html
        .as_bytes()
        .split(|b| !is_lowercase_hex_digit(b))
        .any(|run| run.len() >= 64)
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
            ui_html_has_placeholder(plugin.ui_html),
            "embedded ui.html must contain the pairing-token placeholder"
        );
        assert!(
            !ui_html_contains_a_real_token(plugin.ui_html),
            "embedded ui.html must never contain a real 64-hex-char pairing token"
        );
    }

    #[test]
    fn ui_html_has_placeholder_is_false_without_it() {
        assert!(!ui_html_has_placeholder("<html>no token here</html>"));
    }

    #[test]
    fn ui_html_contains_a_real_token_detects_64_lowercase_hex_chars() {
        let token = "a".repeat(64);
        let html = format!("<script>var t = \"{token}\";</script>");
        assert!(ui_html_contains_a_real_token(&html));
    }

    #[test]
    fn ui_html_contains_a_real_token_is_false_for_the_placeholder_alone() {
        assert!(!ui_html_contains_a_real_token(
            "<script>var t = \"__TURBOFIG_PAIRING_TOKEN__\";</script>"
        ));
    }

    #[test]
    fn ui_html_contains_a_real_token_is_false_for_short_hex_runs() {
        let short = "a".repeat(63);
        let html = format!("<script>var t = \"{short}\";</script>");
        assert!(!ui_html_contains_a_real_token(&html));
    }
}
