//! The About window: a real `tao` window hosting one `wry` webview that
//! shows exactly 1 embedded page (`daemon/assets/about/about.html`,
//! `include_str!`'d below, never loaded from a URL). Every piece of logic a
//! test can reach without a window lives in `about_state.rs`; this file is
//! the thin glue that builds the real window/webview and wires their
//! callbacks to that logic and to `mod.rs`'s existing seams
//! (`Clipboard`/`AppOpener`, `quit_sequence`). Nothing in the crate's test
//! suite calls any function here that builds a real window.

use super::about_state::{chips_from_connected_files, parse_ipc_command, IpcCommand};
use super::{clipboard_for_app, opener_for_app, quit_sequence, MenuState, RealDaemonStopper};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tao::dpi::LogicalSize;
use tao::event_loop::EventLoopWindowTarget;
use tao::window::{Window, WindowBuilder};
use wry::{WebView, WebViewBuilder};

/// The one embedded page. No remote URLs anywhere in it except the "Docs"
/// link's own `href` (see `about_state`'s html-scan test, which asserts
/// exactly that).
const ABOUT_HTML: &str = include_str!("../../assets/about/about.html");

/// The GitHub repo, also the docs destination for "Docs" and the IPC
/// `open_docs` command.
const DOCS_URL: &str = "https://github.com/LukeHawkins/turbofig";

/// Everything the IPC handler and the navigation handler need, owned by the
/// closures `create_about_window` builds. Cloned (cheaply: an `Arc`/`PathBuf`
/// each) into both.
#[derive(Clone)]
pub struct AboutWindowContext {
    pub home: PathBuf,
    pub mcp_port: u16,
    pub current_state: Arc<Mutex<MenuState>>,
}

/// Holds the window and webview alive for as long as the About window
/// exists. Dropping this closes the window; `mod.rs` keeps it in an
/// `Option` for the app's whole lifetime, replacing it with `None` only if
/// the window is ever closed by the user (not implemented as a separate
/// close handler today: closing the window just drops focus, the handle
/// stays, "About Turbofig…" or the second-instance signal refocuses it).
pub struct AboutWindowHandle {
    window: Window,
    webview: WebView,
}

impl AboutWindowHandle {
    /// Brings the window to the front, e.g. when "About Turbofig…" is
    /// clicked again, or a second instance signals this one.
    pub fn focus(&self) {
        self.window.set_visible(true);
        self.window.set_focus();
    }

    /// Pushes a fresh status update into the page (`window.turbofigSetStatus`):
    /// the 2 chips and whether the step-2 checkmark should show.
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

/// Builds the About window: a 420x520, non-resizable, titled "Turbofig"
/// window hosting a webview over `ABOUT_HTML`, wired to the given IPC
/// command set and a navigation handler that blocks every navigation away
/// from the embedded page.
pub fn create_about_window<T: 'static>(
    target: &EventLoopWindowTarget<T>,
    ctx: AboutWindowContext,
) -> Result<AboutWindowHandle, String> {
    let window = WindowBuilder::new()
        .with_title("Turbofig")
        .with_inner_size(LogicalSize::new(420.0, 520.0))
        .with_resizable(false)
        .build(target)
        .map_err(|e| format!("could not create the window: {e}"))?;

    let ipc_ctx = ctx.clone();
    let navigation_ctx = ctx.clone();

    let webview = WebViewBuilder::new()
        .with_html(ABOUT_HTML)
        .with_ipc_handler(move |req: wry::http::Request<String>| {
            handle_ipc_message(req.body(), &ipc_ctx);
        })
        .with_navigation_handler(move |url: String| navigation_is_allowed(&url, &navigation_ctx))
        .build(&window)
        .map_err(|e| format!("could not create the webview: {e}"))?;

    let handle = AboutWindowHandle { window, webview };

    let version = env!("CARGO_PKG_VERSION");
    let script = format!(
        "window.turbofigSetStatic && window.turbofigSetStatic({}, {});",
        serde_json::to_string(version).unwrap_or_else(|_| "\"\"".to_owned()),
        serde_json::to_string(&mcp_json()).unwrap_or_else(|_| "\"\"".to_owned()),
    );
    let _ = handle.webview.evaluate_script(&script);

    Ok(handle)
}

/// Dispatches one parsed IPC command. An unparseable message is dropped
/// silently (see `about_state::parse_ipc_command`): the embedded page only
/// ever sends the 7 known commands, so anything else getting through would
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
        IpcCommand::OpenFigma => {
            opener_for_app().open_figma();
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
        IpcCommand::Quit => {
            let stopper = RealDaemonStopper::new(ctx.home.clone(), ctx.mcp_port);
            quit_sequence(&stopper);
            std::process::exit(0);
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
