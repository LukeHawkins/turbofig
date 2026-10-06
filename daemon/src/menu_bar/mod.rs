//! The menu-bar app: tray icon, menu, status polling, and the Quit
//! sequence. Step 2 of the macOS app bundle (step 1: `app_bundle.rs`; step
//! 3: the setup window; step 4: first-run, login item, docs).
//!
//! Every piece of actual logic lives in a sibling module with no
//! `tray-icon`/`tao` dependency at all (`state`: the health-to-`MenuState`
//! translation; `icon`: PNG decoding; `lock`: the single-instance guard;
//! `quit`: the stop-then-confirm sequence), so it is unit-tested with no
//! window, no tray icon, and no event loop ever created. This file is the
//! only place that builds a real tray icon or runs a real event loop;
//! nothing in the crate's test suite calls `run_menu_bar_app`, and this
//! module carries no `#[cfg(test)]` block of its own.

mod icon;
mod lock;
mod quit;
mod state;

pub use lock::{try_acquire, AppLock};
pub use quit::{quit_sequence, DaemonStopper};
pub use state::{build_menu_state, ConnectedFileInfo, IconState, MenuState};

use crate::first_run::{AppOpener, Clipboard, FakeClipboard, FakeOpener, NullClipboard};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::TrayIconBuilder;

/// How often the background poller refetches `/health`.
const HEALTH_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// How long "Quit Turbofig" waits for `/health` to go unreachable before
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

/// `UserEvent`: the only thing crossing from the background health poller
/// thread into the tao event loop's handler.
enum UserEvent {
    Health(Option<serde_json::Value>),
}

/// Builds the static part of the menu (every item, in the exact documented
/// order) and returns handles to the 3 that change after construction: the
/// status line, the tray icon's menu itself (so `mod.rs` can hand it to the
/// tray builder), and the ids needed to dispatch clicks.
struct MenuHandles {
    menu: Menu,
    status_item: MenuItem,
    copy_prompt_item: MenuItem,
    copy_manifest_item: MenuItem,
    open_figma_item: MenuItem,
    open_setup_item: MenuItem,
    start_at_login_item: CheckMenuItem,
    open_log_item: MenuItem,
    quit_item: MenuItem,
}

fn build_menu(header: &str, status_text: &str) -> MenuHandles {
    let menu = Menu::new();
    let header_item = MenuItem::new(header, false, None);
    let status_item = MenuItem::new(status_text, false, None);
    let copy_prompt_item = MenuItem::new("Copy Agent Prompt", true, None);
    let copy_manifest_item = MenuItem::new("Copy Plugin Manifest Path", true, None);
    let open_figma_item = MenuItem::new("Open Figma", true, None);
    let open_setup_item = MenuItem::new("Open Setup\u{2026}", true, None);
    let start_at_login_item = CheckMenuItem::new("Start at Login", true, false, None);
    let open_log_item = MenuItem::new("Open Log", true, None);
    let quit_item = MenuItem::new("Quit Turbofig", true, None);

    let _ = menu.append_items(&[
        &header_item,
        &status_item,
        &PredefinedMenuItem::separator(),
        &copy_prompt_item,
        &copy_manifest_item,
        &open_figma_item,
        &open_setup_item,
        &PredefinedMenuItem::separator(),
        &start_at_login_item,
        &open_log_item,
        &PredefinedMenuItem::separator(),
        &quit_item,
    ]);

    MenuHandles {
        menu,
        status_item,
        copy_prompt_item,
        copy_manifest_item,
        open_figma_item,
        open_setup_item,
        start_at_login_item,
        open_log_item,
        quit_item,
    }
}

/// Step 3 implements the real setup window. For now this only logs, so the
/// menu item is already wired end to end and step 3 only needs to replace
/// this function's body.
fn open_setup_window() {
    eprintln!("turbofig: Open Setup… is not implemented yet (step 3)");
}

/// Step 4 implements persisting this as a real login item. For now this
/// only logs; the checkbox itself still toggles in the menu so the item is
/// already wired end to end.
fn set_start_at_login_stub(checked: bool) {
    eprintln!("turbofig: Start at Login -> {checked} is not implemented yet (step 4)");
}

/// Runs the menu-bar app: acquires the single-instance lock, ensures the
/// daemon is running, builds the tray icon and menu, starts the background
/// health poller, then runs the main-thread event loop forever. Never
/// returns; every exit path goes through `std::process::exit`.
///
/// Only ever reached from `running_inside_app_bundle` (a real `Turbofig.app`
/// launch) or the hidden `turbofig app run` dev command, both already
/// gated; this function itself does not re-check either.
pub async fn run_menu_bar_app() {
    let home = crate::bridge_dir_from_env();
    let mcp_port = crate::port_from_env();

    let lock_path = home.join("app.lock");
    match try_acquire(&lock_path) {
        Ok(Some(lock)) => {
            // Held for the rest of the process's life; dropping it (on
            // exit) releases the flock. Leak it deliberately: this process
            // never releases the lock early on purpose.
            std::mem::forget(lock);
        }
        Ok(None) => {
            eprintln!("turbofig: another instance is already running; exiting");
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

    let manifest_path = home.join("figma-plugin").join("manifest.json");
    let log_path = home.join("daemon.log");

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
        copy_prompt_item,
        copy_manifest_item,
        open_figma_item,
        open_setup_item,
        start_at_login_item,
        open_log_item,
        quit_item,
    } = build_menu(&initial_state.header, &initial_state.status_text);

    let current_state = Arc::new(Mutex::new(initial_state.clone()));

    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    event_loop.set_activation_policy(ActivationPolicy::Accessory);
    let proxy = event_loop.create_proxy();

    // The background health poller: its own small tokio runtime on its own
    // thread, so the main thread stays free for the tao event loop (macOS
    // requires the event loop to run on the main thread).
    let poll_home = home.clone();
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
            if proxy.send_event(UserEvent::Health(health)).is_err() {
                return; // the event loop is gone; stop polling.
            }
            std::thread::sleep(HEALTH_POLL_INTERVAL);
        }
    });

    let bridge_dir_display = home.display().to_string();
    let tray_icon = icon::icon_for_state(initial_state.icon_state);
    let mut tray_builder = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Turbofig");
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

    let copy_prompt_id = copy_prompt_item.id().clone();
    let copy_manifest_id = copy_manifest_item.id().clone();
    let open_figma_id = open_figma_item.id().clone();
    let open_setup_id = open_setup_item.id().clone();
    let start_at_login_id = start_at_login_item.id().clone();
    let open_log_id = open_log_item.id().clone();
    let quit_id = quit_item.id().clone();

    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::UserEvent(UserEvent::Health(health)) => {
                let new_state = build_menu_state(health.as_ref(), &bridge_dir_display, mcp_port);
                status_item.set_text(&new_state.status_text);
                if let Some(icon) = icon::icon_for_state(new_state.icon_state) {
                    let _ = tray.set_icon_templated(Some(icon));
                }
                if let Ok(mut guard) = current_state.lock() {
                    *guard = new_state;
                }
            }
            Event::NewEvents(StartCause::Init) => {
                // The tray icon and menu are already built above; nothing
                // else to do on init. tao requires the event loop to have
                // started before some platform calls are safe, which is
                // why the tray itself is built just before `run` rather
                // than here.
            }
            _ => {}
        }

        if let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == copy_prompt_id {
                if let Ok(guard) = current_state.lock() {
                    clipboard_for_app().copy(&guard.agent_prompt);
                }
            } else if event.id == copy_manifest_id {
                clipboard_for_app().copy(&manifest_path.display().to_string());
            } else if event.id == open_figma_id {
                opener_for_app().open_figma();
            } else if event.id == open_setup_id {
                open_setup_window();
            } else if event.id == start_at_login_id {
                let checked = start_at_login_item.is_checked();
                set_start_at_login_stub(checked);
            } else if event.id == open_log_id {
                opener_for_app().open_app_with_path("Console", &log_path);
            } else if event.id == quit_id {
                let stopper = RealDaemonStopper {
                    rt: tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("build a tokio runtime for the quit sequence"),
                    client: crate::spawn::build_admin_client()
                        .unwrap_or_else(|_| reqwest::Client::new()),
                    mcp_port,
                    home: home.clone(),
                };
                if !quit_sequence(&stopper) {
                    eprintln!("turbofig: the daemon was still answering after Quit's stop request");
                }
                std::process::exit(0);
            }
        }
    });
}

/// Reads the pairing token from `<home>/token`, if present. A thin wrapper
/// so both the startup health check and the background poller read it the
/// same way.
async fn read_token(home: &Path) -> Option<String> {
    crate::read_token_file(home).await
}
