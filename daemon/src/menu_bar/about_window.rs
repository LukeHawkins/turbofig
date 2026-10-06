//! The About window: a real `tao` window hosting one `wry` webview that
//! shows exactly 1 embedded page (`daemon/assets/about/about.html`,
//! `include_str!`'d below, never loaded from a URL). Every piece of logic a
//! test can reach without a window lives in `about_state.rs`; this file is
//! the thin glue that builds the real window/webview and wires their
//! callbacks to that logic and to `mod.rs`'s existing seams
//! (`Clipboard`/`AppOpener`, `quit_sequence`). Nothing in the crate's test
//! suite calls any function here that builds a real window.

use super::about_state::{chips_from_connected_files, parse_ipc_command, IpcCommand};
use super::activate::activate_app_and_focus;
use super::{clipboard_for_app, opener_for_app, MenuState};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use tao::dpi::LogicalSize;
use tao::event_loop::EventLoopWindowTarget;
use tao::window::{Window, WindowBuilder};
use wry::{WebView, WebViewBuilder};

/// The one embedded page. No remote URLs anywhere in it except the "Docs"
/// link's own `href` (see `about_state`'s html-scan test, which asserts
/// exactly that).
const ABOUT_HTML: &str = include_str!("../../assets/about/about.html");

/// Installed with `with_initialization_script`, so it runs before the
/// page's own `<script>` tag: a tiny pending-state shim. If Rust calls
/// `evaluate_script("window.turbofigSetStatus(...)")` before the page's own
/// script has replaced these stubs with the real DOM-touching versions (a
/// real race: the page is a same-process `include_str!`, not a network
/// load, but WebKit still parses/runs it asynchronously relative to
/// `WebView::evaluate_script`), the call is recorded in `__tfPending`
/// instead of throwing on an undefined function and being lost. The page's
/// own script drains `__tfPending` once, the first time it runs (see
/// `about.html`); `handle_ipc_message`'s `PageReady` arm also re-pushes
/// Rust's own latest status as a second, independent safety net.
const PENDING_STATE_SHIM: &str = r#"
window.__tfPending = { status: null, static: null };
window.turbofigSetStatus = function () {
  window.__tfPending.status = Array.prototype.slice.call(arguments);
};
window.turbofigSetStatic = function () {
  window.__tfPending.static = Array.prototype.slice.call(arguments);
};
"#;

/// The GitHub repo, also the docs destination for "Docs" and the IPC
/// `open_docs` command.
const DOCS_URL: &str = "https://github.com/LukeHawkins/turbofig";

/// Everything the IPC handler and the navigation handler need, owned by the
/// closures `create_about_window` builds. Cloned (cheaply: an `Arc`/`Rc`/
/// `PathBuf` each) into both.
#[derive(Clone)]
pub struct AboutWindowContext {
    pub home: PathBuf,
    pub current_state: Arc<Mutex<MenuState>>,
    /// The latest `/health` body the background poller saw (`None` if the
    /// daemon has never answered), kept by `mod.rs`. Read by the
    /// `PageReady` IPC arm to re-push the real status after a possible lost
    /// race (see `PENDING_STATE_SHIM`).
    pub last_health: Rc<RefCell<Option<serde_json::Value>>>,
    /// The open About window, if any, set by `mod.rs` right after
    /// `create_about_window` returns. The `PageReady` IPC arm reads through
    /// this (rather than closing over a `webview` directly) since the
    /// window does not exist yet at the moment this context's closures are
    /// first built.
    pub handle: Rc<RefCell<Option<AboutWindowHandle>>>,
}

/// Holds the window and webview alive for as long as the About window
/// exists. Dropping this closes the window; `mod.rs` keeps it in an
/// `Option` for the app's whole lifetime, replacing it with `None` only if
/// the window is ever closed by the user (not implemented as a separate
/// close handler today: closing the window just drops focus, the handle
/// stays, "About turbofig…" or the second-instance signal refocuses it).
pub struct AboutWindowHandle {
    window: Window,
    webview: WebView,
}

impl AboutWindowHandle {
    /// Brings the window to the front, e.g. when "About turbofig…" is
    /// clicked again, or a second instance signals this one. Activates the
    /// app first: under `ActivationPolicy::Accessory` a plain
    /// `set_focus()` alone can leave the window non-key, so the Figma-side
    /// symptom this fixes is a tab (or any other) click silently doing
    /// nothing the first time the window is shown (see `activate.rs`).
    pub fn focus(&self) {
        activate_app_and_focus(&self.window);
    }

    /// Pushes a fresh status update into the page (`window.turbofigSetStatus`):
    /// the 2 status texts and whether the step-2 checkmark should show.
    pub fn push_status(&self, bridge_reachable: bool, connected_file_names: &[String]) {
        let chips = chips_from_connected_files(bridge_reachable, connected_file_names);
        let figma_connected = !connected_file_names.is_empty();
        let script = format!(
            "window.turbofigSetStatus && window.turbofigSetStatus({}, {}, {});",
            bridge_reachable,
            serde_json::to_string(&chips.figma_chip).unwrap_or_else(|_| "\"waiting\"".to_owned()),
            figma_connected,
        );
        let _ = self.webview.evaluate_script(&script);
    }

    /// Pushes the static (version + MCP json) fields. Split out of window
    /// creation so the `PageReady` IPC arm can re-push it under the same
    /// rule `push_status` already follows.
    fn push_static(&self, version: &str, mcp_json: &str) {
        let script = format!(
            "window.turbofigSetStatic && window.turbofigSetStatic({}, {});",
            serde_json::to_string(version).unwrap_or_else(|_| "\"\"".to_owned()),
            serde_json::to_string(mcp_json).unwrap_or_else(|_| "\"\"".to_owned()),
        );
        let _ = self.webview.evaluate_script(&script);
    }
}

/// The stable binary path (the Homebrew `<prefix>/bin/turbofig` symlink when
/// running from a Cellar, otherwise the running binary's own path), the same
/// rule `main.rs`'s `stable_path_for_running_binary` applies, duplicated
/// here (it is 3 lines over an already-public, already-tested
/// `launchd::stable_binary_path`) since that helper lives in the `turbofig`
/// binary crate, not the library.
fn stable_binary_path_display() -> String {
    let current_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("turbofig"));
    let canonical = current_exe
        .canonicalize()
        .unwrap_or_else(|_| current_exe.clone());
    crate::launchd::stable_binary_path(&canonical)
        .display()
        .to_string()
}

/// The "Other MCP clients" JSON line, built with the stable binary path.
fn mcp_json() -> String {
    format!(
        "{{\"command\": \"{}\", \"args\": [\"mcp\"]}}",
        stable_binary_path_display()
    )
}

/// Builds the About window: a 420x520, non-resizable, titled "turbofig"
/// window hosting a webview over `ABOUT_HTML`, wired to the given IPC
/// command set and a navigation handler that blocks every navigation away
/// from the embedded page. Activates the app and focuses the window once
/// built (see `AboutWindowHandle::focus`), and enables the Web Inspector in
/// debug builds so the owner can debug the embedded page directly.
pub fn create_about_window<T: 'static>(
    target: &EventLoopWindowTarget<T>,
    ctx: AboutWindowContext,
) -> Result<AboutWindowHandle, String> {
    let window = WindowBuilder::new()
        .with_title("turbofig")
        .with_inner_size(LogicalSize::new(420.0, 520.0))
        .with_resizable(false)
        .build(target)
        .map_err(|e| format!("could not create the window: {e}"))?;

    let ipc_ctx = ctx.clone();
    let navigation_ctx = ctx.clone();

    let webview = WebViewBuilder::new()
        .with_html(ABOUT_HTML)
        .with_initialization_script(PENDING_STATE_SHIM)
        .with_devtools(cfg!(debug_assertions))
        .with_ipc_handler(move |req: wry::http::Request<String>| {
            handle_ipc_message(req.body(), &ipc_ctx);
        })
        .with_navigation_handler(move |url: String| navigation_is_allowed(&url, &navigation_ctx))
        .build(&window)
        .map_err(|e| format!("could not create the webview: {e}"))?;

    let handle = AboutWindowHandle { window, webview };
    handle.push_static(env!("CARGO_PKG_VERSION"), &mcp_json());
    handle.focus();

    Ok(handle)
}

/// Dispatches one parsed IPC command. An unparseable message is dropped
/// silently (see `about_state::parse_ipc_command`): the embedded page only
/// ever sends the 6 known commands, so anything else getting through would
/// mean the page itself was tampered with, not a case worth acting on.
fn handle_ipc_message(raw: &str, ctx: &AboutWindowContext) {
    let Some(command) = parse_ipc_command(raw) else {
        return;
    };
    match command {
        IpcCommand::CopyManifestPath => {
            let manifest_path = ctx.home.join("figma-plugin").join("manifest.json");
            clipboard_for_app().copy(&manifest_path.display().to_string());
        }
        IpcCommand::RevealManifest => {
            let manifest_path = ctx.home.join("figma-plugin").join("manifest.json");
            opener_for_app().reveal_in_finder(&manifest_path);
        }
        IpcCommand::CopyAgentPrompt => {
            if let Ok(guard) = ctx.current_state.lock() {
                clipboard_for_app().copy(&guard.agent_prompt);
            }
        }
        IpcCommand::CopyMcpCommand => {
            clipboard_for_app().copy("claude mcp add turbofig -- turbofig mcp");
        }
        IpcCommand::CopyMcpJson => {
            clipboard_for_app().copy(&mcp_json());
        }
        IpcCommand::OpenDocs => {
            opener_for_app().open_url(DOCS_URL);
        }
        IpcCommand::PageReady => {
            // Belt and braces alongside `PENDING_STATE_SHIM`: re-push
            // whatever Rust currently holds, in case the very first push
            // (at window creation, or an in-flight health poll) raced the
            // page load badly enough that even the JS-side shim missed it
            // (e.g. `evaluate_script` ran before the webview had any
            // document at all to run it against).
            if let Some(handle) = ctx.handle.borrow().as_ref() {
                let health = ctx.last_health.borrow();
                let names = super::state::connected_file_names_from_health(health.as_ref());
                handle.push_status(health.is_some(), &names);
                handle.push_static(env!("CARGO_PKG_VERSION"), &mcp_json());
            }
        }
    }
}

/// The navigation handler: a hard backstop against ever showing remote
/// content in this webview. Allows only the initial `about:blank` load that
/// `WebViewBuilder::with_html` itself performs (`loadHTMLString:baseURL:`
/// with no base URL resolves to that pseudo-URL on macOS); for the "Docs"
/// link (intercepted by the page's own `onclick` first, this is a backstop
/// in case that JS ever fails), opens it externally through the opener seam
/// and still cancels the in-webview navigation; blocks everything else.
fn navigation_is_allowed(url: &str, ctx: &AboutWindowContext) -> bool {
    let _ = ctx;
    if url == "about:blank" || url.starts_with("about:") {
        return true;
    }
    if url == DOCS_URL {
        opener_for_app().open_url(url);
    }
    false
}
