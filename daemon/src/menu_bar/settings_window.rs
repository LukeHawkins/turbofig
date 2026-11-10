//! The Settings window: a real `tao` window hosting one `wry` webview over
//! a page rendered from `daemon/assets/settings/settings.html`
//! (`include_str!`'d below, never loaded from a URL). Same approach as the
//! About window (`about_window.rs`): no `<script>` anywhere, every control
//! is a plain `turbofig-action://` link, and this file's navigation handler
//! intercepts the click, runs the action, and cancels the navigation. Wired
//! to `settings_state.rs`'s pure action parsing and to `mod.rs`'s existing
//! seams (`Clipboard`/`AppOpener`, `set_start_at_login`). Nothing in the
//! crate's test suite calls any function here that builds a real window.

use super::activate::activate_app_and_focus;
use super::link_policy::{decide_navigation, new_window_target, open_external, NavDecision};
use super::settings_state::{parse_action_link, SettingsAction};
use super::{clipboard_for_app, opener_for_app};
use crate::plugin_files::McpHttpState;
use std::path::PathBuf;
use tao::dpi::LogicalSize;
use tao::event_loop::EventLoopWindowTarget;
use tao::window::{Window, WindowBuilder, WindowId};
use wry::{NewWindowResponse, WebView, WebViewBuilder};

/// The one page template, rendered once at window creation by substituting
/// the "Start at login" state and the running app's version: there is no
/// JS to push a live update into.
const SETTINGS_HTML_TEMPLATE: &str = include_str!("../../assets/settings/settings.html");

/// Everything the navigation handler needs, owned by the closure
/// `create_settings_window` builds.
#[derive(Clone)]
pub struct SettingsWindowContext {
    pub home: PathBuf,
    /// Asks the event loop to re-render the window after "Start at login"
    /// changes, because the page has no JavaScript to update itself.
    pub reload: std::sync::Arc<dyn Fn() + Send + Sync>,
}

/// Holds the window and webview alive for as long as the Settings window
/// exists. Dropping this closes the window; `mod.rs` keeps it in an
/// `Option`, replaced with `None` once the window's `CloseRequested` event
/// fires, the same lifecycle `AboutWindowHandle` follows.
pub struct SettingsWindowHandle {
    window: Window,
    // Never read directly: its only job is to stay alive as long as the
    // window does (dropping it tears down the webview), since there is no
    // more `evaluate_script` push to call on it.
    #[allow(dead_code)]
    webview: WebView,
}

impl SettingsWindowHandle {
    /// Brings the window to the front. Activates the app first, for the
    /// same reason `AboutWindowHandle::focus` does (see `activate.rs`).
    pub fn focus(&self) {
        activate_app_and_focus(&self.window);
    }

    /// This window's id, so `mod.rs`'s event loop can match a
    /// `WindowEvent::CloseRequested` against it.
    pub fn id(&self) -> WindowId {
        self.window.id()
    }
}

/// Reads "Start at login"'s current, real state (never a cached belief: see
/// `cli::app_autostart_plist_exists`'s own doc comment).
fn start_at_login_checked() -> bool {
    crate::cli::app_autostart_plist_exists(&crate::launchd::launch_agents_dir_from_env())
}

/// Reads "MCP over HTTP" (default on) from the marker in the turbofig home.
fn mcp_http_checked(home: &std::path::Path) -> McpHttpState {
    crate::plugin_files::mcp_http_state(home)
}

/// Renders the page: the version, and the "Start at login" and "MCP over
/// HTTP" rows, each as plain state text plus the one link that flips it
/// (there is no JS to drive a live checkbox).
fn render_settings_html(version: &str, start_at_login_on: bool, mcp: &McpHttpState) -> String {
    let (state_text, toggle_action, toggle_label) = if start_at_login_on {
        ("On", "start-at-login-off", "Turn off")
    } else {
        ("Off", "start-at-login-on", "Turn on")
    };
    let (mcp_text, mcp_action, mcp_label) = match mcp {
        McpHttpState::On => ("On", "mcp-http-off", "Turn off"),
        McpHttpState::Off => ("Off", "mcp-http-on", "Turn on"),
        // The marker is odd: /mcp is closed. "Turn on" tries to fix it and
        // logs the reason when it cannot.
        McpHttpState::Unknown(_) => ("unknown (see log)", "mcp-http-on", "Turn on"),
    };
    SETTINGS_HTML_TEMPLATE
        .replace("__VERSION__", version)
        .replace("__MCP_HTTP_STATE__", mcp_text)
        .replace("__MCP_HTTP_TOGGLE_ACTION__", mcp_action)
        .replace("__MCP_HTTP_TOGGLE_LABEL__", mcp_label)
        .replace("__START_AT_LOGIN_STATE__", state_text)
        .replace("__START_AT_LOGIN_TOGGLE_ACTION__", toggle_action)
        .replace("__START_AT_LOGIN_TOGGLE_LABEL__", toggle_label)
}

/// Builds the Settings window: a 360x260, non-resizable, titled "turbofig
/// settings" window hosting a webview over the rendered page. Activates the
/// app and focuses the window once built; enables the Web Inspector in
/// debug builds.
pub fn create_settings_window<T: 'static>(
    target: &EventLoopWindowTarget<T>,
    ctx: SettingsWindowContext,
) -> Result<SettingsWindowHandle, String> {
    let window = WindowBuilder::new()
        .with_title("turbofig settings")
        .with_inner_size(LogicalSize::new(360.0, 260.0))
        .with_resizable(false)
        .build(target)
        .map_err(|e| format!("could not create the window: {e}"))?;

    let navigation_ctx = ctx.clone();

    let webview = WebViewBuilder::new()
        .with_html(render_settings_html(
            env!("CARGO_PKG_VERSION"),
            start_at_login_checked(),
            &mcp_http_checked(&ctx.home),
        ))
        .with_devtools(cfg!(debug_assertions))
        .with_navigation_handler(move |url: String| navigation_is_allowed(&url, &navigation_ctx))
        .with_new_window_req_handler(|url, _features| {
            if let Some(allowed) = new_window_target(&url) {
                open_external(opener_for_app().as_ref(), allowed);
            }
            NewWindowResponse::Deny
        })
        .build(&window)
        .map_err(|e| format!("could not create the webview: {e}"))?;

    let handle = SettingsWindowHandle { window, webview };
    handle.focus();

    Ok(handle)
}

/// Dispatches one parsed action link.
fn dispatch_settings_action(action: SettingsAction, ctx: &SettingsWindowContext) {
    match action {
        SettingsAction::StartAtLoginOn => {
            if let Err(e) = super::set_start_at_login(true, &ctx.home) {
                eprintln!("turbofig: Start at Login -> true failed: {e}");
            }
            (ctx.reload)();
        }
        SettingsAction::StartAtLoginOff => {
            if let Err(e) = super::set_start_at_login(false, &ctx.home) {
                eprintln!("turbofig: Start at Login -> false failed: {e}");
            }
            (ctx.reload)();
        }
        SettingsAction::McpHttpOn | SettingsAction::McpHttpOff => {
            let on = action == SettingsAction::McpHttpOn;
            if let Err(e) = crate::plugin_files::set_mcp_http_enabled(&ctx.home, on) {
                eprintln!("turbofig: MCP over HTTP -> {on} failed: {e}");
            }
            (ctx.reload)();
        }
        SettingsAction::CopyPath => {
            let manifest_path = ctx.home.join("figma-plugin").join("manifest.json");
            clipboard_for_app().copy(&manifest_path.display().to_string());
        }
        SettingsAction::ShowPluginFolder => {
            let manifest_path = ctx.home.join("figma-plugin").join("manifest.json");
            opener_for_app().reveal_in_finder(&manifest_path);
        }
    }
}

/// The navigation handler: blocks everything except the initial
/// `about:blank` load, and dispatches any known `turbofig-action://` link
/// (there is no IPC channel any more, and no external link on this page).
fn navigation_is_allowed(url: &str, ctx: &SettingsWindowContext) -> bool {
    match decide_navigation(url) {
        NavDecision::AllowInitialLoad => true,
        NavDecision::OpenExternal(allowed) => {
            open_external(opener_for_app().as_ref(), allowed);
            false
        }
        NavDecision::Action => {
            if let Some(action) = parse_action_link(url) {
                dispatch_settings_action(action, ctx);
            }
            false
        }
        NavDecision::Block => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mcp_http_row_renders_both_states_and_leaves_no_placeholder() {
        let on = render_settings_html("1.2.3", false, &McpHttpState::On);
        assert!(on.contains("MCP over HTTP: On"));
        assert!(on.contains("turbofig-action://mcp-http-off"));
        let off = render_settings_html("1.2.3", true, &McpHttpState::Off);
        assert!(off.contains("MCP over HTTP: Off"));
        assert!(off.contains("turbofig-action://mcp-http-on"));
        let unknown = render_settings_html("1.2.3", true, &McpHttpState::Unknown("x".into()));
        assert!(unknown.contains("MCP over HTTP: unknown (see log)"));
        for html in [on, off, unknown] {
            assert!(!html.contains("__"));
        }
    }

    #[test]
    fn the_settings_page_shows_the_crate_version() {
        let html = render_settings_html(env!("CARGO_PKG_VERSION"), false, &McpHttpState::On);
        assert!(html.contains(&format!("v{}", env!("CARGO_PKG_VERSION"))));
    }

    #[test]
    fn mcp_http_checked_follows_the_marker() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(mcp_http_checked(tmp.path()), McpHttpState::On);
        crate::plugin_files::set_mcp_http_enabled(tmp.path(), false).expect("off");
        assert_eq!(mcp_http_checked(tmp.path()), McpHttpState::Off);
    }
}
