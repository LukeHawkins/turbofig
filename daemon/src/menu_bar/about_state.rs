//! Pure logic for the About window: the fixed action set the webview's
//! plain `turbofig-action://` links may name, and the first-use rule that
//! decides whether to open it automatically on app start. No `wry`/`tao`
//! dependency: `about_window.rs` is the only caller, and the only place any
//! of this touches a real window or webview.
//!
//! The page has no `<script>` at all (see `about_window.rs`'s own doc
//! comment): every action is a plain `<a href="turbofig-action://...">`,
//! and `about_window::navigation_is_allowed` intercepts the click, runs the
//! action below, and cancels the navigation.

use std::path::Path;

/// The 3 actions the About window's action links may name. Anything else
/// in a `turbofig-action://` URL is rejected by `parse_action_link`, never
/// dispatched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AboutAction {
    /// "Copy path": copies the plugin manifest's absolute path.
    CopyPath,
    /// "Show plugin folder": reveals the plugin manifest in Finder.
    ShowPluginFolder,
    /// "Copy agent prompt": copies the current agent connect prompt.
    CopyAgentPrompt,
}

/// The scheme every action link uses, e.g. `turbofig-action://copy-path`.
pub const ACTION_SCHEME: &str = "turbofig-action://";

/// Parses a navigated-to URL into one of the 3 known About-window actions.
/// Returns `None` for anything else at all: a different scheme, an unknown
/// name, trailing text, or a different case. The caller treats `None` as
/// "not an action": it is never dispatched.
pub fn parse_action_link(url: &str) -> Option<AboutAction> {
    match url.strip_prefix(ACTION_SCHEME)? {
        "copy-path" => Some(AboutAction::CopyPath),
        "show-plugin-folder" => Some(AboutAction::ShowPluginFolder),
        "copy-agent-prompt" => Some(AboutAction::CopyAgentPrompt),
        _ => None,
    }
}

/// True when the About window should open automatically on app start:
/// `<home>/plugin-seen` does not exist yet, i.e. no Figma plugin has ever
/// connected (the same marker `first_run`/`ws.rs` use elsewhere).
pub fn should_auto_open_about_window(home: &Path) -> bool {
    !home.join("plugin-seen").exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_action_link_accepts_all_3_actions() {
        assert_eq!(
            parse_action_link("turbofig-action://copy-path"),
            Some(AboutAction::CopyPath)
        );
        assert_eq!(
            parse_action_link("turbofig-action://show-plugin-folder"),
            Some(AboutAction::ShowPluginFolder)
        );
        assert_eq!(
            parse_action_link("turbofig-action://copy-agent-prompt"),
            Some(AboutAction::CopyAgentPrompt)
        );
    }

    #[test]
    fn parse_action_link_rejects_actions_moved_elsewhere_or_removed() {
        assert_eq!(parse_action_link("turbofig-action://open-docs"), None);
        assert_eq!(parse_action_link("turbofig-action://copy-mcp-command"), None);
        assert_eq!(parse_action_link("turbofig-action://copy-mcp-json"), None);
        assert_eq!(parse_action_link("turbofig-action://quit"), None);
        assert_eq!(parse_action_link("turbofig-action://page-ready"), None);
    }

    #[test]
    fn parse_action_link_rejects_anything_else() {
        assert_eq!(parse_action_link(""), None);
        assert_eq!(parse_action_link("turbofig-action://"), None);
        assert_eq!(parse_action_link("turbofig-action://Copy-Path"), None);
        assert_eq!(parse_action_link("turbofig-action://copy-path extra"), None);
        assert_eq!(parse_action_link("https://example.com"), None);
        assert_eq!(parse_action_link("javascript:alert(1)"), None);
        assert_eq!(parse_action_link("TURBOFIG-ACTION://copy-path"), None);
    }

    #[test]
    fn first_use_true_when_plugin_seen_is_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(should_auto_open_about_window(dir.path()));
    }

    #[test]
    fn first_use_false_once_plugin_seen_exists() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("plugin-seen"), "").expect("write marker");
        assert!(!should_auto_open_about_window(dir.path()));
    }

    /// The same embedded template `about_window.rs` loads with
    /// `include_str!`, read again here so this pure-logic test needs no
    /// `wry`/`tao` import at all. Asserts there is no `<script>` anywhere,
    /// and no remote `src`/`href` except the 2 allowed external links
    /// (the README on GitHub, and the owner's website).
    const ABOUT_HTML: &str = include_str!("../../assets/about/about.html");
    const README_HREF: &str = "href=\"https://github.com/LukeHawkins/turbofig#readme\"";
    const WEBSITE_HREF: &str = "href=\"https://lukehawkins.eu\"";

    #[test]
    fn the_about_page_has_no_script_and_only_the_2_allowed_remote_links() {
        assert!(
            !ABOUT_HTML.to_lowercase().contains("<script"),
            "the page must have no <script> at all"
        );
        assert!(ABOUT_HTML.contains(README_HREF), "expected the README link");
        assert!(ABOUT_HTML.contains(WEBSITE_HREF), "expected the website link");
        let without_allowed = ABOUT_HTML.replacen(README_HREF, "", 1).replacen(WEBSITE_HREF, "", 1);
        assert!(
            !without_allowed.contains("src=\"http"),
            "no src attribute may point at a remote URL"
        );
        assert!(
            !without_allowed.contains("href=\"http"),
            "no other href may point at a remote URL"
        );
    }
}
