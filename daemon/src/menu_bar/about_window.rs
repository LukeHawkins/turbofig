//! The About window: a real `tao` window hosting one `wry` webview that
//! shows exactly 1 page, rendered from `daemon/assets/about/about.html`
//! (`include_str!`'d below, never loaded from a URL). The page has no
//! `<script>` at all: every action is a plain `<a href="turbofig-action://
//! ...">`, and this file's `navigation_is_allowed` intercepts the click,
//! runs the action, and cancels the navigation (`with_navigation_handler`
//! never lets the webview actually follow one of these URLs). Every piece
//! of logic a test can reach without a window lives in `about_state.rs`;
//! this file is the thin glue that builds the real window/webview and
//! wires its navigation handler to that logic and to `mod.rs`'s existing
//! seams (`Clipboard`/`AppOpener`). Nothing in the crate's test suite calls
//! any function here that builds a real window.

use super::about_state::{parse_action_link, AboutAction};
use super::activate::activate_app_and_focus;
use super::{clipboard_for_app, opener_for_app, MenuState};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tao::dpi::LogicalSize;
use tao::event_loop::EventLoopWindowTarget;
use tao::window::{Window, WindowBuilder, WindowId};
use wry::{WebView, WebViewBuilder};

/// The one page template. Rendered once, at window creation, by substituting
/// `__VERSION__` for the running app's version: there is no JS to push a
/// live update into, so every value the page shows is baked in up front.
const ABOUT_HTML_TEMPLATE: &str = include_str!("../../assets/about/about.html");

/// The owner's website, the footer link's destination.
const WEBSITE_URL: &str = "https://lukehawkins.eu";

/// Everything the navigation handler needs, owned by the closure
/// `create_about_window` builds. Cloned (cheaply: an `Arc`/`PathBuf` each).
#[derive(Clone)]
pub struct AboutWindowContext {
    pub home: PathBuf,
    /// Read by the "Copy agent prompt" action for the current agent
    /// connect prompt text.
    pub current_state: Arc<Mutex<MenuState>>,
}

/// Holds the window and webview alive for as long as the About window
/// exists. Dropping this closes the window; `mod.rs` keeps it in an
/// `Option`, replaced with `None` once the window's `CloseRequested` event
/// fires (see `mod.rs`'s event loop), so the next menu click opens a fresh
/// one.
pub struct AboutWindowHandle {
    window: Window,
    // Never read directly: its only job is to stay alive as long as the
    // window does (dropping it tears down the webview), since there is no
    // more `evaluate_script` push to call on it.
    #[allow(dead_code)]
    webview: WebView,
}

impl AboutWindowHandle {
    /// Brings the window to the front, e.g. when "About turbofig…" is
    /// clicked again, or a second instance signals this one. Activates the
    /// app first: under `ActivationPolicy::Accessory` a plain
    /// `set_focus()` alone can leave the window non-key (see `activate.rs`).
    pub fn focus(&self) {
        activate_app_and_focus(&self.window);
    }

    /// This window's id, so `mod.rs`'s event loop can match a
    /// `WindowEvent::CloseRequested` against it.
    pub fn id(&self) -> WindowId {
        self.window.id()
    }
}

fn render_about_html(version: &str) -> String {
    ABOUT_HTML_TEMPLATE.replace("__VERSION__", version)
}

/// Builds the About window: a 420x480, non-resizable, titled "turbofig"
/// window hosting a webview over the rendered page, wired to a navigation
/// handler that intercepts every `turbofig-action://` link, opens the 1
/// allow-listed external link (the owner's website) through
/// the `AppOpener` seam, and blocks every other navigation away from the
/// embedded page. Activates the app and focuses the window once built (see
/// `AboutWindowHandle::focus`), and enables the Web Inspector in debug
/// builds so the owner can debug the embedded page directly.
pub fn create_about_window<T: 'static>(
    target: &EventLoopWindowTarget<T>,
    ctx: AboutWindowContext,
) -> Result<AboutWindowHandle, String> {
    let window = WindowBuilder::new()
        .with_title("turbofig")
        .with_inner_size(LogicalSize::new(420.0, 480.0))
        .with_resizable(false)
        .build(target)
        .map_err(|e| format!("could not create the window: {e}"))?;

    let navigation_ctx = ctx;

    let webview = WebViewBuilder::new()
        .with_html(render_about_html(env!("CARGO_PKG_VERSION")))
        .with_devtools(cfg!(debug_assertions))
        .with_navigation_handler(move |url: String| navigation_is_allowed(&url, &navigation_ctx))
        .build(&window)
        .map_err(|e| format!("could not create the webview: {e}"))?;

    let handle = AboutWindowHandle { window, webview };
    handle.focus();

    Ok(handle)
}

/// Dispatches one parsed action link.
fn dispatch_about_action(action: AboutAction, ctx: &AboutWindowContext) {
    match action {
        AboutAction::CopyPath => {
            let manifest_path = ctx.home.join("figma-plugin").join("manifest.json");
            clipboard_for_app().copy(&manifest_path.display().to_string());
        }
        AboutAction::ShowPluginFolder => {
            let manifest_path = ctx.home.join("figma-plugin").join("manifest.json");
            opener_for_app().reveal_in_finder(&manifest_path);
        }
        AboutAction::CopyAgentPrompt => {
            if let Ok(guard) = ctx.current_state.lock() {
                clipboard_for_app().copy(&guard.agent_prompt);
            }
        }
    }
}

/// The navigation handler: a hard backstop against ever showing remote
/// content in this webview, and the dispatch point for every
/// `turbofig-action://` link (there is no IPC channel any more: the page has
/// no `<script>` to send one). Allows only the initial `about:blank` load
/// that `WebViewBuilder::with_html` itself performs; for the 1 allow-listed
/// external link (the owner's website), opens it externally
/// through the opener seam and still cancels the in-webview navigation; for
/// a known action link, runs it and cancels the navigation; blocks
/// everything else outright.
fn navigation_is_allowed(url: &str, ctx: &AboutWindowContext) -> bool {
    if url == "about:blank" || url.starts_with("about:") {
        return true;
    }
    if url == WEBSITE_URL {
        opener_for_app().open_url(url);
        return false;
    }
    if let Some(action) = parse_action_link(url) {
        dispatch_about_action(action, ctx);
    }
    false
}
