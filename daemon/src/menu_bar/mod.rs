//! The menu-bar app: tray icon, menu, status polling, the Quit sequence,
//! and the About window. Steps 2 and 3 of the macOS app bundle (step 1:
//! `app_bundle.rs`; step 4: first-run, login item, docs).
//!
//! Every piece of actual logic lives in a sibling module with no
//! `tray-icon`/`tao`/`wry` dependency at all (`state`: the health-to-
//! `MenuState` translation; `icon`: PNG decoding; `lock`: the single-instance
//! guard; `quit`: the stop-then-confirm sequence; `about_state`: the About
//! window's IPC parsing, chip mapping, and first-use rule; `second_instance`:
//! the cross-process open-about signal), so it is unit-tested with no
//! window, no tray icon, no webview, and no event loop ever created. This
//! file and `about_window.rs` are the only places that build a real tray
//! icon, window, webview, or event loop; nothing in the crate's test suite
//! calls `run_menu_bar_app` or `create_about_window`. This file's own
//! `#[cfg(test)]` block only ever exercises `quit_running_app_and_wait_for_exit`,
//! which composes `lock`/`second_instance` with no tray/window/webview
//! involved either; `about_window.rs` still carries none of its own.

mod about_state;
mod about_window;
mod activate;
mod icon;
mod lock;
mod quit;
mod second_instance;
mod self_update;
mod settings_state;
mod settings_window;
mod state;

pub use lock::{try_acquire, AppLock};
pub use quit::{quit_sequence, DaemonStopper};
pub use state::{build_menu_state, ConnectedFileInfo, IconState, MenuState};

/// Signals a running app instance (if any) to quit, over `<home>/app.sock`,
/// the same socket a second launch uses to ask for the About window.
/// `turbofig uninstall` calls this before its own steps, so the app is
/// never left running (and holding the daemon up) under a home directory
/// `uninstall` is about to tear down. `Ok(true)`: an app was listening and
/// was told to quit. `Ok(false)`: no app was running (the ordinary case
/// when the user never installed, or already quit, the menu-bar app).
pub fn signal_quit_running_app(home: &Path) -> std::io::Result<bool> {
    second_instance::send(&home.join("app.sock"), second_instance::SignalMessage::Quit)
}

/// Asks a running app instance (if any) to quit, then waits until
/// `<home>/app.lock` is free or `deadline` elapses. Used by the bare
/// `turbofig` command after it refreshes an outdated `turbofig.app`
/// (`main.rs`'s `try_app_first_run`): the old instance is still running the
/// stale binary, so it must exit before the freshly installed bundle is
/// reopened, rather than the open just re-activating the stale one.
///
/// Returns `false` at once, with no signal sent, when no instance is
/// running (`<home>/app.lock` was free): nothing to restart. Returns `true`
/// once an instance is found and asked to quit, whether or not it actually
/// released the lock before `deadline`: the caller proceeds to reopen the
/// bundle either way, and `true` is what tells it to print its own
/// "restarted" line.
///
/// `sleep` is an explicit seam (the same pattern `cli.rs`'s
/// `bootstrap_with_retry` uses), so a test can exercise the poll loop with
/// no real wait.
pub fn quit_running_app_and_wait_for_exit(
    home: &Path,
    deadline: Duration,
    poll_interval: Duration,
    sleep: &dyn Fn(Duration),
) -> bool {
    let lock_path = home.join("app.lock");
    match lock::try_acquire(&lock_path) {
        Ok(Some(_lock)) => return false, // nothing was running; lock released on drop
        Ok(None) => {}                   // an instance holds the lock; ask it to quit below
        Err(_) => return false,          // can't tell; don't block the refresh on this
    }

    let _ = signal_quit_running_app(home);

    let start = std::time::Instant::now();
    loop {
        if let Ok(Some(_lock)) = lock::try_acquire(&lock_path) {
            return true;
        }
        if start.elapsed() >= deadline {
            return true; // gave it our best shot; the caller reopens anyway
        }
        sleep(poll_interval);
    }
}

use crate::first_run::{AppOpener, Clipboard};
#[cfg(debug_assertions)]
use crate::first_run::{FakeClipboard, FakeOpener, NullClipboard};
use about_window::{create_about_window, AboutWindowContext, AboutWindowHandle};
use settings_window::{create_settings_window, SettingsWindowContext, SettingsWindowHandle};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tao::event::{ElementState, Event, KeyEvent, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopWindowTarget};
use tao::keyboard::{KeyCode, ModifiersState};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::TrayIconBuilder;

/// How often the background poller refetches `/health`.
const HEALTH_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// How long "Quit turbofig" waits for `/health` to go unreachable before
/// exiting anyway. Shorter than `STOP_UNREACHABLE_DEADLINE` in `main.rs`
/// (65s): a stuck quit must never leave the user staring at a tray icon
/// that refuses to go away.
const QUIT_WAIT_DEADLINE: Duration = Duration::from_secs(10);

/// True when a debug build may touch the real desktop (clipboard, Figma,
/// Console). Mirrors `main.rs`'s own guard of the same name.
#[cfg(debug_assertions)]
fn debug_real_desktop_allowed() -> bool {
    std::env::var("TURBOFIG_DEV_REAL_DESKTOP").as_deref() == Ok("1")
}

fn clipboard_for_app() -> Box<dyn Clipboard> {
    #[cfg(debug_assertions)]
    {
        if let Ok(path) = std::env::var("TURBOFIG_TEST_FAKE_CLIPBOARD") {
            return Box::new(FakeClipboard::new(PathBuf::from(path)));
        }
        if !debug_real_desktop_allowed() {
            return Box::new(NullClipboard);
        }
    }
    Box::new(crate::first_run::RealClipboard)
}

fn opener_for_app() -> Box<dyn AppOpener> {
    #[cfg(debug_assertions)]
    {
        if let Ok(v) = std::env::var("TURBOFIG_TEST_FAKE_OPENER") {
            return Box::new(FakeOpener::new(v == "success"));
        }
        if !debug_real_desktop_allowed() {
            return Box::new(FakeOpener::new(false));
        }
    }
    Box::new(crate::first_run::RealAppOpener)
}

/// The real `DaemonStopper`: `POST /control` with the pairing token, then
/// polls `/health` until it stops answering or `QUIT_WAIT_DEADLINE` elapses.
/// Owns a small single-thread `tokio` runtime so `quit_sequence`'s sync
/// trait methods can drive the existing async `spawn`/`reqwest` helpers
/// without the caller (the tao event loop, itself not async) needing one.
struct RealDaemonStopper {
    rt: tokio::runtime::Runtime,
    client: reqwest::Client,
    mcp_port: u16,
    home: PathBuf,
}

impl RealDaemonStopper {
    /// Builds a `RealDaemonStopper` for `home`/`mcp_port`: its own small
    /// single-thread `tokio` runtime and HTTP client, ready for `stop`/
    /// `wait_unreachable`. Shared by the tray menu's "Quit turbofig" and the
    /// About window's `quit` IPC command, so both go through one
    /// construction path.
    fn new(home: PathBuf, mcp_port: u16) -> Self {
        Self {
            rt: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build a tokio runtime for the quit sequence"),
            client: crate::spawn::build_admin_client().unwrap_or_else(|_| reqwest::Client::new()),
            mcp_port,
            home,
        }
    }
}

impl DaemonStopper for RealDaemonStopper {
    fn stop(&self) -> bool {
        self.rt.block_on(async {
            let Some(token) = crate::read_token_file(&self.home).await else {
                return false;
            };
            self.client
                .post(format!("http://127.0.0.1:{}/control", self.mcp_port))
                .bearer_auth(&token)
                .json(&serde_json::json!({"action": "stop"}))
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false)
        })
    }

    fn wait_unreachable(&self) -> bool {
        self.rt.block_on(crate::spawn::wait_for_unreachable(
            &self.client,
            self.mcp_port,
            QUIT_WAIT_DEADLINE,
        ))
    }
}

/// `UserEvent`: the only thing crossing from a background thread (the
/// health poller, or the second-instance signal listener) into the tao
/// event loop's handler.
enum UserEvent {
    Health(Option<serde_json::Value>),
    /// A second instance signalled this one over `<home>/app.sock`
    /// (`second_instance`): open the About window, or focus it if it is
    /// already open.
    OpenAboutWindow,
    /// `turbofig uninstall` signalled this one over `<home>/app.sock` to
    /// quit, the same as its own "Quit turbofig" menu item.
    QuitRequested,
    /// "Start at login" changed in the Settings window: re-render it so its
    /// text shows the new state (the page has no JavaScript).
    ReloadSettings,
}

/// Builds the static part of the menu (every item, in the exact documented
/// order) and returns handles to the ones that change or that `mod.rs`
/// dispatches clicks against: the status line, the tray icon's menu itself
/// (so `mod.rs` can hand it to the tray builder), and the item ids.
///
/// The menu is exactly: the disabled status line, a separator, "About
/// turbofig…", "Settings…", a separator, "Quit turbofig". Every other
/// action (copying the agent prompt or the manifest path, revealing the
/// plugin in Finder, opening Figma, Start at Login, Open Log) moved into
/// the About or Settings window; "Open Figma" was removed entirely. See
/// `ARCHITECTURE.md`'s "Menu-bar app" section.
struct MenuHandles {
    menu: Menu,
    status_item: MenuItem,
    about_item: MenuItem,
    settings_item: MenuItem,
    quit_item: MenuItem,
}

fn build_menu(status_text: &str) -> MenuHandles {
    let menu = Menu::new();
    let status_item = MenuItem::new(status_text, false, None);
    let about_item = MenuItem::new("About turbofig\u{2026}", true, None);
    let settings_item = MenuItem::new("Settings\u{2026}", true, None);
    let quit_item = MenuItem::new("Quit turbofig", true, None);

    let _ = menu.append_items(&[
        &status_item,
        &PredefinedMenuItem::separator(),
        &about_item,
        &settings_item,
        &PredefinedMenuItem::separator(),
        &quit_item,
    ]);

    MenuHandles {
        menu,
        status_item,
        about_item,
        settings_item,
        quit_item,
    }
}

/// Opens the About window, or brings it to the front if it is already open.
/// Shared by "About turbofig…", the first-use auto-open, and a second
/// instance's signal, so all 3 paths behave identically.
fn open_or_focus_about_window(
    target: &EventLoopWindowTarget<UserEvent>,
    handle_cell: &Rc<RefCell<Option<AboutWindowHandle>>>,
    ctx: &AboutWindowContext,
) {
    let mut guard = handle_cell.borrow_mut();
    if let Some(handle) = guard.as_ref() {
        handle.focus();
        return;
    }
    match create_about_window(target, ctx.clone()) {
        Ok(handle) => {
            *guard = Some(handle);
        }
        Err(e) => {
            eprintln!("turbofig: could not open the About window: {e}");
        }
    }
}

/// Opens the Settings window, or brings it to the front if it is already
/// open. A second "Settings…" click just focuses the existing window, the
/// same rule `open_or_focus_about_window` follows for About.
fn open_or_focus_settings_window(
    target: &EventLoopWindowTarget<UserEvent>,
    handle_cell: &Rc<RefCell<Option<SettingsWindowHandle>>>,
    ctx: &SettingsWindowContext,
) {
    let mut guard = handle_cell.borrow_mut();
    if let Some(handle) = guard.as_ref() {
        handle.focus();
        return;
    }
    match create_settings_window(target, ctx.clone()) {
        Ok(handle) => *guard = Some(handle),
        Err(e) => {
            eprintln!("turbofig: could not open the Settings window: {e}");
        }
    }
}

/// Turns the app autostart LaunchAgent on or off (`cli::run_autostart_on_app`/
/// `run_autostart_off`), shared by the tray menu's "Start at Login" checkbox
/// and the About window's footer checkbox. Both read their initial/current
/// checked state from `cli::app_autostart_plist_exists`, never their own
/// cached belief, so the 2 checkboxes (and a plain `turbofig autostart`
/// CLI run) can never silently disagree with the real plist on disk.
fn set_start_at_login(enabled: bool, home: &Path) -> Result<(), String> {
    let agents_dir = crate::launchd::launch_agents_dir_from_env();
    let launchctl = crate::launchd::RealLaunchctl;
    let uid = crate::launchd::current_uid().map_err(|e| e.to_string())?;
    if enabled {
        let applications_dir = crate::app_bundle::applications_dir_from_env();
        let own_exe = std::env::current_exe().map_err(|e| e.to_string())?;
        crate::cli::run_autostart_on_app(
            &agents_dir,
            &applications_dir,
            &launchctl,
            &uid,
            &own_exe,
            home,
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    } else {
        crate::cli::run_autostart_off(&agents_dir, &launchctl, &uid)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// Runs the menu-bar app: acquires the single-instance lock, ensures the
/// daemon is running, builds the tray icon and menu, starts the background
/// health poller, then runs the main-thread event loop forever. Never
/// returns; every exit path goes through `std::process::exit`.
///
/// Only ever reached from `running_inside_app_bundle` (a real `turbofig.app`
/// launch) or the hidden `turbofig app run` dev command, both already
/// gated; this function itself does not re-check either.
pub async fn run_menu_bar_app() {
    let home = crate::bridge_dir_from_env();
    let mcp_port = crate::port_from_env();

    let lock_path = home.join("app.lock");
    let app_lock: Rc<RefCell<Option<AppLock>>> = Rc::new(RefCell::new(None));
    match try_acquire(&lock_path) {
        Ok(Some(lock)) => {
            // Held for the rest of the process's life, unless
            // `relaunch_for_upgrade` explicitly drops it first (see
            // `self_update`): dropping it releases the flock.
            *app_lock.borrow_mut() = Some(lock);
        }
        Ok(None) => {
            let socket_path = home.join("app.sock");
            match second_instance::send(&socket_path, second_instance::SignalMessage::OpenAbout) {
                Ok(true) => eprintln!("turbofig: another instance is already running; told it to open the About window"),
                Ok(false) => eprintln!("turbofig: another instance is already running (no listener found); exiting"),
                Err(e) => eprintln!("turbofig: another instance is already running; could not signal it: {e}"),
            }
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("turbofig: could not acquire the single-instance lock: {e}");
            std::process::exit(1);
        }
    }

    let client = match crate::spawn::build_admin_client() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("turbofig: could not build the HTTP client: {e}");
            std::process::exit(1);
        }
    };

    if crate::spawn::fetch_health(&client, mcp_port)
        .await
        .is_none()
    {
        let turbofig_binary = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("turbofig: failed to determine the running binary's path: {e}");
                std::process::exit(1);
            }
        };
        if let Err(e) = crate::spawn::spawn_detached_daemon(&turbofig_binary, &home) {
            eprintln!("turbofig: could not start the daemon: {e}");
            std::process::exit(1);
        }
        if let Err(e) = crate::spawn::wait_for_health(&client, mcp_port).await {
            eprintln!("turbofig: {e}");
            std::process::exit(1);
        }
    }

    let initial_health = crate::spawn::fetch_health_with_token(
        &client,
        mcp_port,
        read_token(&home).await.as_deref(),
    )
    .await;
    let initial_state = build_menu_state(
        initial_health.as_ref(),
        &home.display().to_string(),
        mcp_port,
    );

    let MenuHandles {
        menu,
        status_item,
        about_item,
        settings_item,
        quit_item,
    } = build_menu(&initial_state.status_text);

    let current_state = Arc::new(Mutex::new(initial_state.clone()));
    let about_window_handle: Rc<RefCell<Option<AboutWindowHandle>>> = Rc::new(RefCell::new(None));
    let settings_window_handle: Rc<RefCell<Option<SettingsWindowHandle>>> =
        Rc::new(RefCell::new(None));
    let should_auto_open_about = about_state::should_auto_open_about_window(&home);
    let about_ctx = AboutWindowContext {
        home: home.clone(),
        current_state: current_state.clone(),
    };
    // Tracks the last-seen keyboard modifiers, so the Cmd+W handler below
    // can tell a plain "w" from Cmd+W without its own event-loop state.
    let current_modifiers: Rc<Cell<ModifiersState>> = Rc::new(Cell::new(ModifiersState::empty()));

    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    event_loop.set_activation_policy(ActivationPolicy::Accessory);
    let proxy = event_loop.create_proxy();
    let reload_proxy = std::sync::Mutex::new(proxy.clone());
    let settings_ctx = SettingsWindowContext {
        home: home.clone(),
        reload: std::sync::Arc::new(move || {
            if let Ok(p) = reload_proxy.lock() {
                let _ = p.send_event(UserEvent::ReloadSettings);
            }
        }),
    };

    // The background health poller: its own small tokio runtime on its own
    // thread, so the main thread stays free for the tao event loop (macOS
    // requires the event loop to run on the main thread).
    let poll_home = home.clone();
    let poll_proxy = proxy.clone();
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("turbofig: menu bar: could not start the health poller: {e}");
                return;
            }
        };
        let client = match crate::spawn::build_admin_client() {
            Ok(c) => c,
            Err(_) => return,
        };
        loop {
            let health = rt.block_on(async {
                let token = read_token(&poll_home).await;
                crate::spawn::fetch_health_with_token(&client, mcp_port, token.as_deref()).await
            });
            if poll_proxy.send_event(UserEvent::Health(health)).is_err() {
                return; // the event loop is gone; stop polling.
            }
            std::thread::sleep(HEALTH_POLL_INTERVAL);
        }
    });

    // The second-instance signal listener: binds <home>/app.sock and, for
    // every connection, forwards its message (open-about or quit) into the
    // event loop. A bind failure here is a warning, not fatal: the app
    // still works, a second launch (or `turbofig uninstall`) just will not
    // be able to signal this one (it logs its own warning and exits).
    let socket_path = home.join("app.sock");
    match second_instance::bind(&socket_path) {
        Ok(listener) => {
            let about_proxy = proxy.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let event = match second_instance::read(stream) {
                        Some(second_instance::SignalMessage::OpenAbout) => {
                            Some(UserEvent::OpenAboutWindow)
                        }
                        Some(second_instance::SignalMessage::Quit) => {
                            Some(UserEvent::QuitRequested)
                        }
                        None => None,
                    };
                    if let Some(event) = event {
                        if about_proxy.send_event(event).is_err() {
                            return; // the event loop is gone; stop listening.
                        }
                    }
                }
            });
        }
        Err(e) => {
            eprintln!("turbofig: could not bind the second-instance socket: {e}");
        }
    }

    let bridge_dir_display = home.display().to_string();
    let tray_icon = icon::icon_for_state(initial_state.icon_state);
    let mut tray_builder = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("turbofig");
    if let Some(icon) = tray_icon {
        tray_builder = tray_builder.with_icon_templated(icon);
    }
    let tray = match tray_builder.build() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("turbofig: could not create the tray icon: {e}");
            std::process::exit(1);
        }
    };

    let about_id = about_item.id().clone();
    let settings_id = settings_item.id().clone();
    let quit_id = quit_item.id().clone();

    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::UserEvent(UserEvent::Health(health)) => {
                let new_state = build_menu_state(health.as_ref(), &bridge_dir_display, mcp_port);
                status_item.set_text(&new_state.status_text);
                if let Some(icon) = icon::icon_for_state(new_state.icon_state) {
                    let _ = tray.set_icon_templated(Some(icon));
                }
                let daemon_version = health
                    .as_ref()
                    .and_then(|h| h.get("version"))
                    .and_then(|v| v.as_str())
                    .map(str::to_owned);
                if let Ok(mut guard) = current_state.lock() {
                    *guard = new_state;
                }
                if let Some(daemon_version) = daemon_version {
                    maybe_relaunch_for_upgrade(&home, &app_lock, &daemon_version);
                }
            }
            Event::UserEvent(UserEvent::OpenAboutWindow) => {
                open_or_focus_about_window(target, &about_window_handle, &about_ctx);
            }
            Event::UserEvent(UserEvent::QuitRequested) => {
                perform_quit(&home, mcp_port);
            }
            Event::UserEvent(UserEvent::ReloadSettings) => {
                settings_window_handle.borrow_mut().take();
                open_or_focus_settings_window(target, &settings_window_handle, &settings_ctx);
            }
            Event::NewEvents(StartCause::Init) => {
                if should_auto_open_about {
                    open_or_focus_about_window(target, &about_window_handle, &about_ctx);
                }
            }
            Event::WindowEvent {
                window_id, event, ..
            } => {
                handle_window_event(
                    window_id,
                    &event,
                    &about_window_handle,
                    &settings_window_handle,
                    &current_modifiers,
                );
            }
            _ => {}
        }

        if let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == about_id {
                open_or_focus_about_window(target, &about_window_handle, &about_ctx);
            } else if event.id == settings_id {
                open_or_focus_settings_window(target, &settings_window_handle, &settings_ctx);
            } else if event.id == quit_id {
                perform_quit(&home, mcp_port);
            }
        }
    });
}

/// Handles one `WindowEvent` against either the About or the Settings
/// window: `CloseRequested` drops the window and webview and clears the
/// handle, so the next menu click opens a fresh one (fixes the Settings
/// window not being closable); a Cmd+W keypress while a window is focused
/// does the same, since neither window installs a native menu bar with a
/// "Close Window" key equivalent (`ActivationPolicy::Accessory` apps have
/// none by default).
fn handle_window_event(
    window_id: tao::window::WindowId,
    event: &WindowEvent,
    about_window_handle: &Rc<RefCell<Option<AboutWindowHandle>>>,
    settings_window_handle: &Rc<RefCell<Option<SettingsWindowHandle>>>,
    current_modifiers: &Rc<Cell<ModifiersState>>,
) {
    match event {
        WindowEvent::ModifiersChanged(modifiers) => {
            current_modifiers.set(*modifiers);
        }
        WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    physical_key: KeyCode::KeyW,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } if current_modifiers.get().contains(ModifiersState::SUPER) => {
            close_window_if_match(window_id, about_window_handle, settings_window_handle);
        }
        WindowEvent::CloseRequested => {
            close_window_if_match(window_id, about_window_handle, settings_window_handle);
        }
        _ => {}
    }
}

/// Drops whichever of the About/Settings window handles matches
/// `window_id`, closing that window and its webview.
fn close_window_if_match(
    window_id: tao::window::WindowId,
    about_window_handle: &Rc<RefCell<Option<AboutWindowHandle>>>,
    settings_window_handle: &Rc<RefCell<Option<SettingsWindowHandle>>>,
) {
    let mut about = about_window_handle.borrow_mut();
    if about.as_ref().is_some_and(|h| h.id() == window_id) {
        *about = None;
        return;
    }
    drop(about);
    let mut settings = settings_window_handle.borrow_mut();
    if settings.as_ref().is_some_and(|h| h.id() == window_id) {
        *settings = None;
    }
}

/// Reads the pairing token from `<home>/token`, if present. A thin wrapper
/// so both the startup health check and the background poller read it the
/// same way.
async fn read_token(home: &Path) -> Option<String> {
    crate::read_token_file(home).await
}

/// Stops the daemon and confirms it is gone, then exits. Shared by "Quit
/// turbofig" (the tray menu, and the About window's `quit` IPC command via
/// `handle_ipc_message`) and `UserEvent::QuitRequested` (a
/// `turbofig uninstall` signal over `<home>/app.sock`), so all 3 paths quit
/// identically. Never returns.
fn perform_quit(home: &Path, mcp_port: u16) -> ! {
    let stopper = RealDaemonStopper::new(home.to_path_buf(), mcp_port);
    if !quit_sequence(&stopper) {
        eprintln!("turbofig: the daemon was still answering after Quit's stop request");
    }
    std::process::exit(0);
}

/// Checks whether the daemon (`daemon_version`, from `/health`) is newer
/// than this app binary's own version, and relaunches the bundle once per
/// daemon version if so (`self_update::should_relaunch_for_upgrade`).
/// Releases `app_lock` first (so the new instance's own `lock::try_acquire`
/// can succeed), records the daemon version it relaunched for, reopens the
/// bundle as a new instance (`open -n`, through the `AppOpener` seam), then
/// exits. A version string that fails to parse, an unwritable state file,
/// or a failed reopen are all handled by logging and simply not relaunching
/// (or, having already released the lock, exiting anyway so the user is not
/// left with a locked-out tray icon): see the inline comments.
fn maybe_relaunch_for_upgrade(
    home: &Path,
    app_lock: &Rc<RefCell<Option<AppLock>>>,
    daemon_version: &str,
) {
    let app_version = env!("CARGO_PKG_VERSION");
    let last_relaunched_for = self_update::read_last_relaunched_version(home);
    if !self_update::should_relaunch_for_upgrade(
        app_version,
        daemon_version,
        last_relaunched_for.as_deref(),
    ) {
        return;
    }

    if let Err(e) = self_update::write_last_relaunched_version(home, daemon_version) {
        eprintln!("turbofig: could not record the self-update state file, relaunching anyway: {e}");
    }

    // Release the flock before reopening: the new instance's own
    // `lock::try_acquire` must succeed, not find this (about to exit) one
    // still holding it.
    app_lock.borrow_mut().take();

    let applications_dir = crate::app_bundle::applications_dir_from_env();
    let bundle_path = crate::app_bundle::app_bundle_path(&applications_dir);
    if !opener_for_app().open_new_instance(&bundle_path) {
        eprintln!(
            "turbofig: could not reopen {} after an upgrade",
            bundle_path.display()
        );
    }
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_false_at_once_when_no_instance_is_running() {
        let dir = tempfile::tempdir().expect("tempdir");
        let restarted = quit_running_app_and_wait_for_exit(
            dir.path(),
            Duration::from_millis(50),
            Duration::from_millis(5),
            &|_| panic!("must not sleep when nothing is running"),
        );
        assert!(!restarted);
    }

    #[test]
    fn returns_true_and_stops_waiting_as_soon_as_the_other_instance_releases_the_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().to_path_buf();
        let lock_path = home.join("app.lock");

        // Simulate a running instance: hold the lock, listen for the quit
        // signal, and release the lock once it arrives.
        let socket_path = home.join("app.sock");
        let listener = second_instance::bind(&socket_path).expect("bind app.sock");
        let held_lock = lock::try_acquire(&lock_path)
            .expect("acquire")
            .expect("lock must be free at the start of the test");
        let other_instance = std::thread::spawn(move || {
            let (stream, _addr) = listener.accept().expect("accept");
            let message = second_instance::read(stream);
            assert_eq!(message, Some(second_instance::SignalMessage::Quit));
            drop(held_lock); // release the lock, as the real app would on quit
        });

        let restarted = quit_running_app_and_wait_for_exit(
            &home,
            Duration::from_secs(5),
            Duration::from_millis(5),
            &|d| std::thread::sleep(d),
        );

        other_instance
            .join()
            .expect("join the fake instance thread");
        assert!(restarted);
        // The lock must actually be free now, not just reported so.
        assert!(lock::try_acquire(&lock_path).expect("acquire").is_some());
    }

    #[test]
    fn returns_true_after_the_deadline_when_the_lock_is_never_released() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().to_path_buf();
        let lock_path = home.join("app.lock");

        // Nothing is listening on app.sock, and the lock is held forever:
        // the function must still give up at the deadline and report true
        // (an instance was found), rather than hang or report false.
        let held_lock = lock::try_acquire(&lock_path)
            .expect("acquire")
            .expect("lock must be free at the start of the test");

        let slept = std::cell::Cell::new(Duration::ZERO);
        let restarted = quit_running_app_and_wait_for_exit(
            &home,
            Duration::from_millis(30),
            Duration::from_millis(10),
            &|d| slept.set(slept.get() + d),
        );

        assert!(restarted);
        assert!(slept.get() >= Duration::from_millis(30));
        drop(held_lock);
    }
}
