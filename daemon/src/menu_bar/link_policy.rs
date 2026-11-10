//! The one link policy for the About and Settings webviews. Pure logic, no
//! window: `decide_navigation` maps a URL to a `NavDecision`, and
//! `open_external` sends an allowed URL to the default browser through the
//! `AppOpener` seam.
//!
//! WKWebView reports `https://lukehawkins.eu` as `https://lukehawkins.eu/`
//! (it adds the root slash). An exact string compare against the bare URL
//! never matched, so the website link was blocked. The compare here strips
//! one trailing `/` and nothing else, so no other URL can match.

use crate::first_run::AppOpener;

/// The only external pages a link may open: the owner's website and the
/// project's GitHub page.
pub const EXTERNAL_ALLOWLIST: [&str; 2] = [
    "https://lukehawkins.eu",
    "https://github.com/LukeHawkins/turbofig",
];

/// What the navigation handler must do for one URL.
#[derive(Debug, PartialEq, Eq)]
pub enum NavDecision {
    /// The initial `about:` load of the embedded page: let it through.
    AllowInitialLoad,
    /// An allow-listed external page: open this exact URL in the default
    /// browser, and cancel the in-webview navigation.
    OpenExternal(&'static str),
    /// A `turbofig-action://` link: the window parses and runs it, and the
    /// navigation is cancelled.
    Action,
    /// Anything else: cancel the navigation, do nothing.
    Block,
}

/// The scheme every action link uses.
const ACTION_PREFIX: &str = "turbofig-action://";

/// Decides what to do for `url`.
pub fn decide_navigation(url: &str) -> NavDecision {
    if url.starts_with("about:") {
        return NavDecision::AllowInitialLoad;
    }
    if url.starts_with(ACTION_PREFIX) {
        return NavDecision::Action;
    }
    let bare = url.strip_suffix('/').unwrap_or(url);
    for allowed in EXTERNAL_ALLOWLIST {
        if bare == allowed {
            return NavDecision::OpenExternal(allowed);
        }
    }
    NavDecision::Block
}

/// Opens `url` in the default browser, as one argument, only if it is on
/// the allowlist. Returns whether the opener ran and succeeded.
pub fn open_external(opener: &dyn AppOpener, url: &'static str) -> bool {
    EXTERNAL_ALLOWLIST.contains(&url) && opener.open_url(url)
}

/// The decision for a `window.open` / `target="_blank"` request. The page
/// never needs a new window: an allow-listed URL opens in the browser, and
/// the new window is always denied. Returns the URL to open, if any.
pub fn new_window_target(url: &str) -> Option<&'static str> {
    match decide_navigation(url) {
        NavDecision::OpenExternal(allowed) => Some(allowed),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::Path;

    #[derive(Default)]
    struct RecordingOpener {
        urls: RefCell<Vec<String>>,
    }

    impl AppOpener for RecordingOpener {
        fn open_figma(&self) -> bool {
            false
        }
        fn open_app_with_path(&self, _app: &str, _path: &Path) -> bool {
            false
        }
        fn open_url(&self, url: &str) -> bool {
            self.urls.borrow_mut().push(url.to_string());
            true
        }
        fn open_new_instance(&self, _path: &Path) -> bool {
            false
        }
        fn reveal_in_finder(&self, _path: &Path) -> bool {
            false
        }
    }

    #[test]
    fn allowlisted_https_opens_externally_with_or_without_root_slash() {
        assert_eq!(
            decide_navigation("https://lukehawkins.eu"),
            NavDecision::OpenExternal("https://lukehawkins.eu")
        );
        assert_eq!(
            decide_navigation("https://lukehawkins.eu/"),
            NavDecision::OpenExternal("https://lukehawkins.eu")
        );
        assert_eq!(
            decide_navigation("https://github.com/LukeHawkins/turbofig"),
            NavDecision::OpenExternal("https://github.com/LukeHawkins/turbofig")
        );
        assert_eq!(
            decide_navigation("https://github.com/LukeHawkins/turbofig/"),
            NavDecision::OpenExternal("https://github.com/LukeHawkins/turbofig")
        );
    }

    #[test]
    fn every_other_url_is_blocked() {
        for url in [
            "https://example.com",
            "http://lukehawkins.eu",
            "https://lukehawkins.eu.evil.com",
            "https://evil.com/https://lukehawkins.eu",
            "https://lukehawkins.eu@evil.com",
            "https://lukehawkins.eu/path",
            "https://lukehawkins.eu//",
            "https://github.com/LukeHawkins",
            "https://github.com/LukeHawkins/turbofig/issues",
            "https://github.com/lukehawkins/turbofig",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,hi",
            "",
        ] {
            assert_eq!(decide_navigation(url), NavDecision::Block, "{url}");
        }
    }

    #[test]
    fn action_links_are_handled_and_the_initial_load_passes() {
        assert_eq!(
            decide_navigation("turbofig-action://copy-path"),
            NavDecision::Action
        );
        assert_eq!(
            decide_navigation("about:blank"),
            NavDecision::AllowInitialLoad
        );
    }

    #[test]
    fn navigating_the_webview_away_from_the_page_is_never_allowed() {
        // Only `about:` may load in the webview itself.
        for url in ["https://lukehawkins.eu/", "https://example.com", "http://x"] {
            assert_ne!(decide_navigation(url), NavDecision::AllowInitialLoad);
        }
    }

    #[test]
    fn the_opener_gets_the_exact_url_as_one_argument() {
        let opener = RecordingOpener::default();
        assert!(open_external(&opener, "https://lukehawkins.eu"));
        assert!(open_external(
            &opener,
            "https://github.com/LukeHawkins/turbofig"
        ));
        assert_eq!(
            *opener.urls.borrow(),
            vec![
                "https://lukehawkins.eu".to_string(),
                "https://github.com/LukeHawkins/turbofig".to_string()
            ]
        );
    }

    #[test]
    fn the_opener_refuses_a_url_off_the_allowlist() {
        let opener = RecordingOpener::default();
        assert!(!open_external(&opener, "https://example.com"));
        assert!(opener.urls.borrow().is_empty());
    }

    #[test]
    fn new_window_requests_follow_the_same_allowlist() {
        assert_eq!(
            new_window_target("https://lukehawkins.eu/"),
            Some("https://lukehawkins.eu")
        );
        assert_eq!(new_window_target("https://example.com"), None);
        assert_eq!(new_window_target("turbofig-action://copy-path"), None);
    }
}
