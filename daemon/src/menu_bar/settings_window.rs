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
use crate::plugin_files::ConnectionMode;
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
    /// The daemon's HTTP port, for the verified `/control` call that sets
    /// Connection.
    pub mcp_port: u16,
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

/// Reads Connection (default File bridge) from the marker in the turbofig
/// home. An odd marker counts as File bridge, like the daemon's gate.
fn connection_checked(home: &std::path::Path) -> ConnectionMode {
    crate::plugin_files::connection_mode(home)
}

/// Renders the page: the version, the "Start at login" switch, and the
/// "Connection" segmented control. Each is plain state text plus the link
/// that changes it (there is no JS to drive a live control).
fn render_settings_html(version: &str, start_at_login_on: bool, mode: ConnectionMode) -> String {
    let (state_text, toggle_action, checked) = if start_at_login_on {
        ("On", "start-at-login-off", "true")
    } else {
        ("Off", "start-at-login-on", "false")
    };
    let (file_state, file_checked, mcp_state, mcp_checked) = match mode {
        ConnectionMode::File => ("On", "true", "Off", "false"),
        ConnectionMode::Mcp => ("Off", "false", "On", "true"),
    };
    SETTINGS_HTML_TEMPLATE
        .replace("__VERSION__", version)
        .replace("__CONNECTION_FILE_STATE__", file_state)
        .replace("__CONNECTION_FILE_CHECKED__", file_checked)
        .replace("__CONNECTION_MCP_STATE__", mcp_state)
        .replace("__CONNECTION_MCP_CHECKED__", mcp_checked)
        .replace("__START_AT_LOGIN_STATE__", state_text)
        .replace("__START_AT_LOGIN_TOGGLE_ACTION__", toggle_action)
        .replace("__START_AT_LOGIN_CHECKED__", checked)
}

/// Sets Connection. First asks the running daemon over the verified
/// `/control` path, so the daemon writes the marker and broadcasts to every
/// plugin. When that fails (the daemon is not running, or it did not
/// verify), the app writes the marker itself with the same safe write.
pub(crate) async fn apply_connection_mode(
    home: &std::path::Path,
    mcp_port: u16,
    mode: ConnectionMode,
) -> std::io::Result<()> {
    if let Some(token) = crate::read_token_file(home).await {
        match crate::daemon_client::send_connection_mode(mcp_port, &token, mode.as_str()).await {
            Ok(status) if status.is_success() => return Ok(()),
            Ok(status) => {
                eprintln!("turbofig: the daemon refused Connection ({status}); writing it here")
            }
            Err(_) => {}
        }
    }
    crate::plugin_files::set_connection_mode(home, mode)
}

/// Builds the Settings window: a 420x320, non-resizable, titled "turbofig
/// settings" window hosting a webview over the rendered page. Activates the
/// app and focuses the window once built; enables the Web Inspector in
/// debug builds.
pub fn create_settings_window<T: 'static>(
    target: &EventLoopWindowTarget<T>,
    ctx: SettingsWindowContext,
) -> Result<SettingsWindowHandle, String> {
    let window = WindowBuilder::new()
        .with_title("turbofig settings")
        .with_inner_size(LogicalSize::new(420.0, 320.0))
        .with_resizable(false)
        .build(target)
        .map_err(|e| format!("could not create the window: {e}"))?;

    let navigation_ctx = ctx.clone();

    let webview = WebViewBuilder::new()
        .with_html(render_settings_html(
            env!("CARGO_PKG_VERSION"),
            start_at_login_checked(),
            connection_checked(&ctx.home),
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
        SettingsAction::ConnectionFile | SettingsAction::ConnectionMcp => {
            let mode = if action == SettingsAction::ConnectionMcp {
                ConnectionMode::Mcp
            } else {
                ConnectionMode::File
            };
            // The click handler runs on the event loop thread, which may sit
            // inside the app's async runtime: a plain thread with its own
            // small runtime is safe either way, and it keeps the window live.
            let home = ctx.home.clone();
            let port = ctx.mcp_port;
            let reload = ctx.reload.clone();
            std::thread::spawn(move || {
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(std::io::Error::other)
                    .and_then(|rt| rt.block_on(apply_connection_mode(&home, port, mode)));
                if let Err(e) = result {
                    eprintln!("turbofig: Connection -> {} failed: {e}", mode.as_str());
                }
                reload();
            });
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
    fn the_connection_row_renders_both_modes_and_leaves_no_placeholder() {
        let file = render_settings_html("1.2.3", false, ConnectionMode::File);
        let mcp = render_settings_html("1.2.3", true, ConnectionMode::Mcp);
        for html in [&file, &mcp] {
            assert!(!html.contains("__"));
            assert!(html.contains("turbofig-action://connection-file"));
            assert!(html.contains("turbofig-action://connection-mcp"));
        }
        assert!(file.contains(
            "aria-checked=\"true\" data-state=\"On\" href=\"turbofig-action://connection-file\""
        ));
        assert!(file.contains(
            "aria-checked=\"false\" data-state=\"Off\" href=\"turbofig-action://connection-mcp\""
        ));
        assert!(mcp.contains(
            "aria-checked=\"true\" data-state=\"On\" href=\"turbofig-action://connection-mcp\""
        ));
        assert!(mcp.contains(
            "aria-checked=\"false\" data-state=\"Off\" href=\"turbofig-action://connection-file\""
        ));
    }

    #[test]
    fn the_settings_page_shows_the_crate_version() {
        let html = render_settings_html(env!("CARGO_PKG_VERSION"), false, ConnectionMode::File);
        assert!(html.contains(&format!("v{}", env!("CARGO_PKG_VERSION"))));
    }

    #[test]
    fn connection_checked_follows_the_marker() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(connection_checked(tmp.path()), ConnectionMode::File);
        crate::plugin_files::set_connection_mode(tmp.path(), ConnectionMode::Mcp).expect("mcp");
        assert_eq!(connection_checked(tmp.path()), ConnectionMode::Mcp);
    }

    #[tokio::test]
    async fn apply_connection_mode_writes_the_marker_itself_when_no_daemon_runs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            l.local_addr().expect("addr").port()
        };
        apply_connection_mode(tmp.path(), port, ConnectionMode::Mcp)
            .await
            .expect("mcp");
        assert_eq!(connection_checked(tmp.path()), ConnectionMode::Mcp);
        apply_connection_mode(tmp.path(), port, ConnectionMode::File)
            .await
            .expect("file");
        assert_eq!(connection_checked(tmp.path()), ConnectionMode::File);
    }
}
