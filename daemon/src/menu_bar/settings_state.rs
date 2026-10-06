//! Pure logic for the Settings window: the fixed action set the
//! `daemon/assets/settings/settings.html` page's `turbofig-action://` links
//! may name. No `wry`/`tao` dependency, the same split `about_state.rs`
//! uses for the About window: `settings_window.rs` is the only caller, and
//! the only place any of this touches a real window or webview.
//!
//! The page has no `<script>` at all, the same rule `about_state.rs`
//! follows: every control is a plain action link, intercepted by
//! `settings_window::navigation_is_allowed`.

/// The 4 actions the Settings window's action links may name. Anything else
/// in a `turbofig-action://` URL is rejected by `parse_action_link`, never
/// dispatched. "Start at login" is 2 actions (on/off), not a live checkbox,
/// since there is no JS to read a checkbox's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsAction {
    /// The "Turn on" link: turns Start at Login on.
    StartAtLoginOn,
    /// The "Turn off" link: turns Start at Login off.
    StartAtLoginOff,
    CopyPath,
    /// "Open plugin folder": reveals the plugin manifest (`open -R`), the
    /// same action the About window's "Show plugin folder" link runs.
    ShowPluginFolder,
}

/// The scheme every action link uses, e.g.
/// `turbofig-action://start-at-login-on`.
pub const ACTION_SCHEME: &str = "turbofig-action://";

/// Parses a navigated-to URL into one of the 4 known Settings-window
/// actions. Returns `None` for anything else at all: a different scheme, an
/// unknown name, trailing text, or a different case.
pub fn parse_action_link(url: &str) -> Option<SettingsAction> {
    match url.strip_prefix(ACTION_SCHEME)? {
        "start-at-login-on" => Some(SettingsAction::StartAtLoginOn),
        "start-at-login-off" => Some(SettingsAction::StartAtLoginOff),
        "copy-path" => Some(SettingsAction::CopyPath),
        "show-plugin-folder" => Some(SettingsAction::ShowPluginFolder),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_action_link_accepts_all_4_actions() {
        assert_eq!(
            parse_action_link("turbofig-action://start-at-login-on"),
            Some(SettingsAction::StartAtLoginOn)
        );
        assert_eq!(
            parse_action_link("turbofig-action://start-at-login-off"),
            Some(SettingsAction::StartAtLoginOff)
        );
        assert_eq!(
            parse_action_link("turbofig-action://copy-path"),
            Some(SettingsAction::CopyPath)
        );
        assert_eq!(
            parse_action_link("turbofig-action://show-plugin-folder"),
            Some(SettingsAction::ShowPluginFolder)
        );
    }

    #[test]
    fn parse_action_link_rejects_actions_removed_or_moved_elsewhere() {
        assert_eq!(parse_action_link("turbofig-action://open-log"), None);
        assert_eq!(parse_action_link("turbofig-action://page-ready"), None);
        assert_eq!(parse_action_link("turbofig-action://quit"), None);
    }

    #[test]
    fn parse_action_link_rejects_anything_else() {
        assert_eq!(parse_action_link(""), None);
        assert_eq!(parse_action_link("turbofig-action://"), None);
        assert_eq!(
            parse_action_link("turbofig-action://Start-At-Login-On"),
            None
        );
        assert_eq!(parse_action_link("turbofig-action://copy-path "), None);
        assert_eq!(parse_action_link("https://example.com"), None);
    }

    /// The embedded settings page (`settings_window.rs`'s own `include_str!`
    /// copy), scanned the same way `about_state`'s html-scan test checks
    /// `about.html`: no `<script>` anywhere, and no remote URL at all, since
    /// this page has no external link of its own.
    const SETTINGS_HTML: &str = include_str!("../../assets/settings/settings.html");

    #[test]
    fn the_settings_page_has_no_script_and_no_remote_urls() {
        assert!(!SETTINGS_HTML.to_lowercase().contains("<script"));
        assert!(!SETTINGS_HTML.contains("src=\"http"));
        assert!(!SETTINGS_HTML.contains("href=\"http"));
    }
}
