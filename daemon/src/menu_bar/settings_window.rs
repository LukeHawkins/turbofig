//! The Settings window: a real `tao` window hosting one `wry` webview over
//! exactly 1 embedded page (`daemon/assets/settings/settings.html`,
//! `include_str!`'d below, never loaded from a URL). Same approach as the
//! About window (`about_window.rs`): the thin glue here builds the real
//! window/webview and wires its callbacks to `settings_state.rs`'s pure IPC
//! parsing and to `mod.rs`'s existing seams (`Clipboard`/`AppOpener`,
//! `set_start_at_login`). Nothing in the crate's test suite calls any
//! function here that builds a real window.

use super::activate::activate_app_and_focus;
use super::settings_state::{parse_ipc_command, IpcCommand};
use super::{clipboard_for_app, opener_for_app};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use tao::dpi::LogicalSize;
use tao::event_loop::EventLoopWindowTarget;
use tao::window::{Window, WindowBuilder};
use wry::{WebView, WebViewBuilder};

/// The one embedded page.
const SETTINGS_HTML: &str = include_str!("../../assets/settings/settings.html");

/// The same tiny pending-state shim `about_window.rs` installs, for the
/// same reason: see that file's own doc comment on `PENDING_STATE_SHIM`.
const PENDING_STATE_SHIM: &str = r#"
window.__tfPending = { status: null, static: null };
window.turbofigSetStatic = function () {
  window.__tfPending.static = Array.prototype.slice.call(arguments);
};
"#;

/// Everything the IPC handler needs, owned by the closure
/// `create_settings_window` builds.
#[derive(Clone)]
pub struct SettingsWindowContext {
    pub home: PathBuf,
    /// The open Settings window, if any, set by `mod.rs` right after
    /// `create_settings_window` returns. The `PageReady` IPC arm reads
    /// through this the same way `AboutWindowContext::handle` does.
    pub handle: Rc<RefCell<Option<SettingsWindowHandle>>>,
}

/// Holds the window and webview alive for as long as the Settings window
/// exists. Dropping this closes the window; `mod.rs` keeps it in an
/// `Option` for the app's whole lifetime, the same lifecycle as
/// `AboutWindowHandle`.
pub struct SettingsWindowHandle {
    window: Window,
    webview: WebView,
}

impl SettingsWindowHandle {
    /// Brings the window to the front. Activates the app first, for the
    /// same reason `AboutWindowHandle::focus` does (see `activate.rs`).
    pub fn focus(&self) {
        activate_app_and_focus(&self.window);
    }

    /// Pushes the version and the current "Start at login" state.
    fn push_static(&self, version: &str, start_at_login_checked: bool) {
        let script = format!(
            "window.turbofigSetStatic && window.turbofigSetStatic({}, {});",
            serde_json::to_string(version).unwrap_or_else(|_| "\"\"".to_owned()),
            start_at_login_checked,
        );
        let _ = self.webview.evaluate_script(&script);
    }
}

/// Reads "Start at login"'s current, real state (never a cached belief: see
/// `cli::app_autostart_plist_exists`'s own doc comment).
fn start_at_login_checked() -> bool {
    crate::cli::app_autostart_plist_exists(&crate::launchd::launch_agents_dir_from_env())
}

/// Builds the Settings window: a 360x260, non-resizable, titled "Turbofig
/// Settings" window hosting a webview over `SETTINGS_HTML`. Activates the
/// app and focuses the window once built; enables the Web Inspector in
/// debug builds.
pub fn create_settings_window<T: 'static>(
    target: &EventLoopWindowTarget<T>,
    ctx: SettingsWindowContext,
) -> Result<SettingsWindowHandle, String> {
    let window = WindowBuilder::new()
        .with_title("Turbofig Settings")
        .with_inner_size(LogicalSize::new(360.0, 260.0))
        .with_resizable(false)
        .build(target)
        .map_err(|e| format!("could not create the window: {e}"))?;

    let ipc_ctx = ctx.clone();

    let webview = WebViewBuilder::new()
        .with_html(SETTINGS_HTML)
        .with_initialization_script(PENDING_STATE_SHIM)
        .with_devtools(cfg!(debug_assertions))
        .with_ipc_handler(move |req: wry::http::Request<String>| {
            handle_ipc_message(req.body(), &ipc_ctx);
        })
        .with_navigation_handler(|url: String| url == "about:blank" || url.starts_with("about:"))
        .build(&window)
        .map_err(|e| format!("could not create the webview: {e}"))?;

    let handle = SettingsWindowHandle { window, webview };
    handle.push_static(env!("CARGO_PKG_VERSION"), start_at_login_checked());
    handle.focus();

    Ok(handle)
}

/// Dispatches one parsed IPC command. An unparseable message is dropped
/// silently, the same rule `about_window`'s handler follows: the embedded
/// page only ever sends the 6 known commands.
fn handle_ipc_message(raw: &str, ctx: &SettingsWindowContext) {
    let Some(command) = parse_ipc_command(raw) else {
        return;
    };
    match command {
        IpcCommand::StartAtLoginOn => {
            if let Err(e) = super::set_start_at_login(true, &ctx.home) {
                eprintln!("turbofig: Start at Login -> true failed: {e}");
            }
        }
        IpcCommand::StartAtLoginOff => {
            if let Err(e) = super::set_start_at_login(false, &ctx.home) {
                eprintln!("turbofig: Start at Login -> false failed: {e}");
            }
        }
        IpcCommand::CopyManifestPath => {
            let manifest_path = ctx.home.join("figma-plugin").join("manifest.json");
            clipboard_for_app().copy(&manifest_path.display().to_string());
        }
        IpcCommand::OpenPluginFolder => {
            let manifest_path = ctx.home.join("figma-plugin").join("manifest.json");
            opener_for_app().reveal_in_finder(&manifest_path);
        }
        IpcCommand::OpenLog => {
            let log_path = ctx.home.join("daemon.log");
            opener_for_app().open_app_with_path("Console", &log_path);
        }
        IpcCommand::PageReady => {
            if let Some(handle) = ctx.handle.borrow().as_ref() {
                handle.push_static(env!("CARGO_PKG_VERSION"), start_at_login_checked());
            }
        }
    }
}
