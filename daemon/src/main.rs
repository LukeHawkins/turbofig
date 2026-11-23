use clap::Parser;
#[cfg(target_os = "macos")]
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use turbofig::cli::{
    already_running_message, autostart_off_message, autostart_on_message, autostart_plist_exists,
    carried_over_env_message, format_health, non_cellar_binary_warning, run_autostart_off,
    run_autostart_on_after_stopping_existing, run_uninstall, started_message,
    status_unreachable_message, stop_autostart_restart_hint, stop_nothing_running_message,
    stopped_message, uninstall_kept_home_message, AutostartState, Cli, Command,
};
#[cfg(target_os = "macos")]
use turbofig::cli::{run_autostart_on_app_after_stopping_existing, AppAction};
use turbofig::launchd::{current_uid, RealLaunchctl};
use turbofig::supervisor::{
    installed_target, should_log_binary_gone, upgrade_detected, wait_for_drain,
};
use turbofig::AppState;

/// How often the supervised-restart loop checks whether the stable binary
/// path now resolves somewhere else (a Homebrew upgrade landed).
const SUPERVISOR_CHECK_INTERVAL: Duration = Duration::from_secs(30);
/// Longest the supervised-restart loop waits for in-flight jobs to finish
/// before exiting anyway.
const SUPERVISOR_DRAIN_MAX_WAIT: Duration = Duration::from_secs(60);
const SUPERVISOR_DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Grace wait after the job count reaches 0, before the process actually
/// exits. `jobs_in_flight` reaching 0 means the daemon has finished writing
/// its own in-memory result, but an outbound HTTP response or a bridge
/// result-file rename can still be a few scheduler ticks from landing;
/// this gives those a moment to flush before launchd restarts the process.
const SUPERVISOR_EXIT_GRACE: Duration = Duration::from_millis(250);
/// Longest `turbofig stop` (and `uninstall`'s best-effort stop) waits for
/// `/health` to go unreachable after an authenticated restart request. The
/// daemon's own drain wait is up to 60s; this comfortably outlasts that.
const STOP_UNREACHABLE_DEADLINE: Duration = Duration::from_secs(65);

/// The real `~/Library/LaunchAgents` directory, unless overridden.
///
/// Kept behind `TURBOFIG_LAUNCH_AGENTS_DIR` so a test never writes to the
/// real user's LaunchAgents folder: a test sets this to a temp dir instead.
fn launch_agents_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("TURBOFIG_LAUNCH_AGENTS_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
    PathBuf::from(home).join("Library/LaunchAgents")
}

/// Builds the HTTP client every command that talks to the daemon's admin
/// surface (`/health`, `/control`) uses: `spawn::build_admin_client`'s 2s
/// connect and overall timeout, so a wedged daemon or an unrelated foreign
/// process answering on the port can never hang a CLI command forever. The
/// daemon is always local (127.0.0.1); a corporate proxy env var
/// (HTTP_PROXY/HTTPS_PROXY) must never be allowed to intercept or break this
/// request either, hence `no_proxy` inside `build_admin_client` rather than
/// reqwest's default client. Exits 1 with a clear message on the (very
/// unlikely) failure to build a client at all.
fn build_http_client(context: &str) -> reqwest::Client {
    match turbofig::spawn::build_admin_client() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{context}: could not build the HTTP client: {e}");
            std::process::exit(1);
        }
    }
}

/// Extracts `/health`'s `version` field, or `"unknown"` if absent.
fn health_version(health: &serde_json::Value) -> &str {
    health
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
}

/// Makes sure a VERIFIED daemon runs on `mcp_port`, starting one if nothing
/// listens. Returns its `/health` body and whether it was already running.
/// A listener that is not the daemon is never "running": this prints the
/// error, which names the port, and exits 1. Exits 1 on any start failure.
async fn ensure_verified_daemon(
    context: &str,
    mcp_port: u16,
    home: &Path,
    log_hint: bool,
) -> (serde_json::Value, bool) {
    let log_path = home.join("daemon.log");
    let fail = |msg: String| -> ! {
        eprintln!("{context}: {msg}");
        if log_hint {
            eprintln!("{context}: see {} for details", log_path.display());
        }
        std::process::exit(1);
    };
    let mut first = turbofig::daemon_client::verify_allowing_legacy_in_home(mcp_port, home)
        .await
        .map(|(health, kind)| {
            (
                health,
                kind == turbofig::daemon_client::Listener::LegacySameUser,
            )
        });
    // An older same-user daemon is restarted, then a VERIFIED one must answer.
    if matches!(first, Ok((_, true))) {
        if let Err(e) = turbofig::proxy::hand_off_legacy_daemon(mcp_port, home).await {
            fail(e);
        }
        first = turbofig::daemon_client::verify_daemon_in_home(mcp_port, home)
            .await
            .map(|h| (h, false));
    }
    let first = first.map(|(h, _)| h);
    match startup_step(first) {
        Startup::Running(health) => (health, true),
        Startup::Start => {
            let turbofig_binary = match std::env::current_exe() {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{context}: failed to determine the running binary's path: {e}");
                    std::process::exit(1);
                }
            };
            if let Err(e) = turbofig::spawn::spawn_detached_daemon(&turbofig_binary, home) {
                fail(format!("could not start the daemon: {e}"));
            }
            match turbofig::daemon_client::wait_for_verified_daemon(
                mcp_port,
                home,
                turbofig::spawn::HEALTH_DEADLINE,
            )
            .await
            {
                Ok(health) => (health, false),
                Err(e) => fail(e),
            }
        }
        Startup::Fail(msg) => {
            eprintln!("{context}: {msg}");
            std::process::exit(1);
        }
    }
}

/// What `ensure_verified_daemon` does after its first check.
#[derive(Debug, PartialEq)]
enum Startup {
    /// A verified daemon runs.
    Running(serde_json::Value),
    /// Nothing listens: start a daemon.
    Start,
    /// Something else holds the port: print this and exit 1.
    Fail(String),
}

/// Only a verified daemon counts as running. A listener that is not the
/// daemon is a failure, never a reason to start a second daemon.
fn startup_step(first: Result<serde_json::Value, turbofig::daemon_client::VerifyError>) -> Startup {
    use turbofig::daemon_client::VerifyError;
    match first {
        Ok(health) => Startup::Running(health),
        Err(VerifyError::Unreachable) => Startup::Start,
        Err(e) => Startup::Fail(e.to_string()),
    }
}

/// Raises the open-file soft limit to the hard limit, so many plugin
/// connections cannot run the daemon out of descriptors. On macOS, a hard
/// limit of "infinity" is capped at `OPEN_MAX` (10240), the most
/// `setrlimit` accepts there. Logs and goes on if it cannot.
fn raise_nofile_limit() {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `lim` is a valid, writable `rlimit` for the whole call.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
        eprintln!(
            "turbofig daemon: could not read the open-file limit: {}",
            std::io::Error::last_os_error()
        );
        return;
    }
    let target = nofile_target(lim.rlim_cur, lim.rlim_max);
    if target <= lim.rlim_cur {
        return;
    }
    let new = libc::rlimit {
        rlim_cur: target,
        rlim_max: lim.rlim_max,
    };
    // SAFETY: `new` is a valid `rlimit` for the whole call.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &new) } != 0 {
        eprintln!(
            "turbofig daemon: could not raise the open-file limit to {target}: {}",
            std::io::Error::last_os_error()
        );
    } else {
        println!(
            "turbofig open-file limit raised from {} to {target}",
            lim.rlim_cur
        );
    }
}

/// The soft limit to ask for: the hard limit, capped at 10240 on macOS when
/// the hard limit is infinity (or larger than the kernel accepts).
fn nofile_target(soft: libc::rlim_t, hard: libc::rlim_t) -> libc::rlim_t {
    #[cfg(target_os = "macos")]
    let hard = hard.min(10240);
    hard.max(soft)
}

fn main() {
    let cli = Cli::parse();
    if cli.check_embedded {
        cmd_check_embedded();
    }
    // `js` runs before any async runtime or other start-up: it must stay fast.
    if let Some(Command::Js { file, shot, dir }) = &cli.command {
        std::process::exit(turbofig::js_cmd::run(
            file.as_deref(),
            *shot,
            dir.as_deref(),
        ));
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build the tokio runtime")
        .block_on(async_main(cli));
}

async fn async_main(cli: Cli) {
    match cli.command {
        None => cmd_run_or_app_mode().await,
        Some(Command::Serve) => run_daemon().await,
        Some(Command::Start) => cmd_start().await,
        Some(Command::Stop) => cmd_stop().await,
        Some(Command::Status) => cmd_status().await,
        Some(Command::Autostart { state, headless }) => cmd_autostart(state, headless).await,
        Some(Command::Uninstall { purge }) => cmd_uninstall(purge).await,
        Some(Command::Mcp) => cmd_mcp().await,
        Some(Command::Js { .. }) => unreachable!("handled in main"),
        #[cfg(target_os = "macos")]
        Some(Command::App {
            action: AppAction::Install,
        }) => cmd_app_install().await,
        #[cfg(target_os = "macos")]
        Some(Command::App {
            action: AppAction::Run,
        }) => cmd_app_run().await,
    }
}

/// Dispatches the bare `turbofig` invocation (no subcommand): into the
/// menu-bar app stub when this process was launched from inside
/// `turbofig.app` with no extra CLI args, otherwise the ordinary first-run/
/// status flow (`cmd_run`). macOS-only check; every other OS always runs
/// `cmd_run`.
async fn cmd_run_or_app_mode() {
    #[cfg(target_os = "macos")]
    {
        if turbofig::app_bundle::running_inside_app_bundle() && std::env::args().count() == 1 {
            run_menu_bar_app().await;
            return;
        }
    }
    cmd_run().await
}

/// Dispatches into the real menu-bar app (`turbofig::menu_bar`, step 2).
/// Never reached unless `running_inside_app_bundle` is true, so this never
/// runs without a bundle already in place.
#[cfg(target_os = "macos")]
async fn run_menu_bar_app() {
    turbofig::menu_bar::run_menu_bar_app().await;
}

/// `turbofig app run`: a manual-testing surface for the menu-bar app with no
/// app bundle in place. Hidden; a debug build refuses unless
/// `TURBOFIG_DEV_REAL_DESKTOP=1` is set, since this shows a real tray icon
/// and menu. A release build (what Homebrew installs, and what
/// `turbofig.app` actually launches) always runs it.
#[cfg(target_os = "macos")]
async fn cmd_app_run() {
    #[cfg(debug_assertions)]
    if !debug_real_desktop_allowed() {
        eprintln!(
            "turbofig app run: refusing in a debug build; this shows a real tray icon and menu. \
             Set TURBOFIG_DEV_REAL_DESKTOP=1 to override (never in a test)."
        );
        std::process::exit(1);
    }
    turbofig::menu_bar::run_menu_bar_app().await;
}

/// `turbofig app install`: a manual-testing surface for step 4. Assembles
/// (or refreshes) `turbofig.app` and prints its path.
#[cfg(target_os = "macos")]
async fn cmd_app_install() {
    let applications_dir = turbofig::app_bundle::applications_dir_from_env();
    let own_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("turbofig app install: failed to determine the running binary's path: {e}");
            std::process::exit(1);
        }
    };
    match turbofig::app_bundle::install_app_bundle(&applications_dir, &own_exe) {
        Ok(path) => println!("{}", path.display()),
        Err(e) => {
            eprintln!("turbofig app install: {e}");
            std::process::exit(1);
        }
    }
}

/// Release-pipeline check: exits 0 when this binary embeds the Figma plugin,
/// 1 when it does not. `.github/workflows/release.yml` calls it on every
/// built binary, so a release can never ship without the plugin.
fn cmd_check_embedded() -> ! {
    let Some(plugin) = turbofig::embedded_plugin() else {
        eprintln!(
            "turbofig: this binary has no embedded Figma plugin (build plugin/ before cargo)"
        );
        std::process::exit(1);
    };

    if !turbofig::ui_html_has_placeholder(plugin.ui_html) {
        eprintln!(
            "turbofig: embedded ui.html has no pairing-token placeholder; it may carry a real \
             token baked in (see build.rs's normalize_ui_html)"
        );
        std::process::exit(1);
    }
    if turbofig::ui_html_contains_a_real_token(plugin.ui_html) {
        eprintln!("turbofig: embedded ui.html appears to contain a real pairing token");
        std::process::exit(1);
    }

    println!("turbofig: embedded Figma plugin present");
    std::process::exit(0);
}

async fn cmd_autostart(state: AutostartState, headless: bool) {
    match state {
        AutostartState::On => cmd_autostart_on(headless).await,
        AutostartState::Off => cmd_autostart_off(),
    }
}

/// `turbofig autostart on`: installs the app autostart service by default
/// (the menu-bar app itself), or the headless, daemon-only one with
/// `--headless`. Either way, a daemon already running ad hoc is stopped
/// first (best-effort; see `run_autostart_on_after_stopping_existing`'s
/// doc), so launchd's own supervised instance can take over cleanly.
async fn cmd_autostart_on(headless: bool) {
    let home = turbofig::bridge_dir_from_env();
    let mcp_port = turbofig::port_from_env();
    let agents_dir = launch_agents_dir();
    let launchctl = RealLaunchctl;
    let uid = match current_uid() {
        Ok(u) => u,
        Err(e) => {
            eprintln!("turbofig autostart: failed to determine the current user id: {e}");
            std::process::exit(1);
        }
    };
    let current_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("turbofig autostart: failed to determine the running binary's path: {e}");
            std::process::exit(1);
        }
    };

    // A daemon already running (started by `turbofig start`/`turbofig`, not
    // by launchd) must be stopped before the plist is bootstrapped: once
    // launchd's own `serve` starts, finding a daemon already healthy on this
    // port makes it exit at once and (per `KeepAlive: {SuccessfulExit:
    // false}`, see `launchd.rs`) stay stopped until the next login, rather
    // than ever taking over as the supervised instance. Best-effort: a
    // failure here is a warning, not a reason to abandon `autostart on`; see
    // `run_autostart_on_after_stopping_existing`.
    let client = build_http_client("turbofig autostart");
    let stop_result = stop_running_daemon(&client, mcp_port, &home).await;

    let result = if headless {
        run_autostart_on_after_stopping_existing(
            stop_result,
            &agents_dir,
            &launchctl,
            &uid,
            &current_exe,
            &home,
        )
    } else {
        #[cfg(target_os = "macos")]
        {
            let applications_dir = turbofig::app_bundle::applications_dir_from_env();
            run_autostart_on_app_after_stopping_existing(
                stop_result,
                &agents_dir,
                &applications_dir,
                &launchctl,
                &uid,
                &current_exe,
                &home,
            )
        }
        #[cfg(not(target_os = "macos"))]
        {
            eprintln!("turbofig autostart: the app service needs macOS; use --headless on this OS");
            std::process::exit(1);
        }
    };

    match result {
        Ok((outcome, stop_warning)) => {
            if let Some(w) = stop_warning {
                eprintln!(
                    "turbofig autostart: warning: could not stop the already-running daemon first: {w}. \
                     launchd's own daemon exits 0 at once when it finds a verified or older turbofig \
                     daemon on the port, so the old daemon stays in charge, unsupervised. If the \
                     listener is not a turbofig daemon, launchd's daemon exits 1 and launchd retries."
                );
            }
            println!("{}", autostart_on_message(&outcome.plist_path));
            print!(
                "{}",
                non_cellar_binary_warning(outcome.binary_outside_homebrew_cellar)
            );
            print!("{}", carried_over_env_message(&outcome.carried_over_env));
        }
        Err(e) => {
            eprintln!("turbofig autostart: {e}");
            std::process::exit(1);
        }
    }
}

fn cmd_autostart_off() {
    let agents_dir = launch_agents_dir();
    let launchctl = RealLaunchctl;
    let uid = match current_uid() {
        Ok(u) => u,
        Err(e) => {
            eprintln!("turbofig autostart: failed to determine the current user id: {e}");
            std::process::exit(1);
        }
    };

    match run_autostart_off(&agents_dir, &launchctl, &uid) {
        Ok(removed) => println!("{}", autostart_off_message(&removed)),
        Err(e) => {
            eprintln!("turbofig autostart: {e}");
            std::process::exit(1);
        }
    }
}

/// The directory `uninstall` removes `turbofig.app` from, on macOS. An
/// unused empty path on every other OS: `run_uninstall`'s own
/// `remove_app_bundle_best_effort` is a no-op there, so the value is never
/// read.
#[cfg(target_os = "macos")]
fn applications_dir_for_uninstall() -> PathBuf {
    turbofig::app_bundle::applications_dir_from_env()
}

#[cfg(not(target_os = "macos"))]
fn applications_dir_for_uninstall() -> PathBuf {
    PathBuf::new()
}

async fn cmd_uninstall(purge: bool) {
    let home = turbofig::bridge_dir_from_env();
    let agents_dir = launch_agents_dir();
    let applications_dir = applications_dir_for_uninstall();
    let mcp_port = turbofig::port_from_env();
    let client = build_http_client("turbofig uninstall");

    // Quit a running menu-bar app first (best-effort: no app running at all
    // is the ordinary case, not a failure), so it is never left behind,
    // holding the daemon up, under a home directory uninstall is about to
    // tear down.
    #[cfg(target_os = "macos")]
    if let Err(e) = turbofig::menu_bar::signal_quit_running_app(&home) {
        eprintln!("turbofig uninstall: warning: could not signal a running app to quit: {e}");
    }

    // Best-effort: uninstall must still succeed when nothing was running, or
    // when the stop request itself fails for some other reason. The plist
    // removal and purge below are what uninstall is really responsible for.
    let _ = stop_running_daemon(&client, mcp_port, &home).await;

    let launchctl = RealLaunchctl;
    let uid = match current_uid() {
        Ok(u) => u,
        Err(e) => {
            eprintln!("turbofig uninstall: failed to determine the current user id: {e}");
            std::process::exit(1);
        }
    };

    match run_uninstall(
        &home,
        &agents_dir,
        &applications_dir,
        &launchctl,
        &uid,
        purge,
    ) {
        Ok(outcome) => {
            println!("turbofig: uninstalled the launchd service");
            if !outcome.purged {
                println!("{}", uninstall_kept_home_message(&outcome.home));
            } else {
                println!("turbofig: removed {}", outcome.home.display());
            }
        }
        Err(e) => {
            eprintln!("turbofig uninstall: {e}");
            std::process::exit(1);
        }
    }
}

/// True when a debug build may touch the real desktop (clipboard, Figma).
///
/// Debug builds are what `cargo test` and `cargo run` produce. A test that
/// ran the bare binary once brought Figma to the front and overwrote the
/// developer's clipboard on every test run, so a debug build now leaves the
/// desktop alone unless `TURBOFIG_DEV_REAL_DESKTOP=1` is set. Release builds
/// (what Homebrew installs) always use the real clipboard and opener.
#[cfg(debug_assertions)]
fn debug_real_desktop_allowed() -> bool {
    std::env::var("TURBOFIG_DEV_REAL_DESKTOP").as_deref() == Ok("1")
}

/// Returns the real clipboard. In a debug build, `TURBOFIG_TEST_FAKE_CLIPBOARD`
/// names a file where a fake records the copied text instead, and without
/// `TURBOFIG_DEV_REAL_DESKTOP=1` the copy goes nowhere and reports failure
/// (`NullClipboard`), so the caller prints "copy the path" rather than
/// falsely claiming the text is on the clipboard. Neither escape hatch
/// exists in a release binary.
fn clipboard_for_run() -> Box<dyn turbofig::first_run::Clipboard> {
    #[cfg(debug_assertions)]
    {
        if let Ok(path) = std::env::var("TURBOFIG_TEST_FAKE_CLIPBOARD") {
            return Box::new(turbofig::first_run::FakeClipboard::new(PathBuf::from(path)));
        }
        if !debug_real_desktop_allowed() {
            return Box::new(turbofig::first_run::NullClipboard);
        }
    }
    Box::new(turbofig::first_run::RealClipboard)
}

/// Returns the real Figma-Desktop opener. In a debug build,
/// `TURBOFIG_TEST_FAKE_OPENER` (`"success"` or `"fail"`) selects a fake with
/// that fixed result, and without `TURBOFIG_DEV_REAL_DESKTOP=1` the opener
/// never runs `open` and reports a failure. Neither escape hatch exists in a
/// release binary.
fn opener_for_run() -> Box<dyn turbofig::first_run::AppOpener> {
    #[cfg(debug_assertions)]
    {
        if let Ok(v) = std::env::var("TURBOFIG_TEST_FAKE_OPENER") {
            return Box::new(turbofig::first_run::FakeOpener::new(v == "success"));
        }
        if !debug_real_desktop_allowed() {
            return Box::new(turbofig::first_run::FakeOpener::new(false));
        }
    }
    Box::new(turbofig::first_run::RealAppOpener)
}

/// Runs the bare `turbofig` command (no subcommand): starts the daemon
/// detached if it is not already running, then prints either the first-run
/// walkthrough (no `<home>/plugin-seen` marker yet: add the plugin, connect
/// an agent) or a short status (the marker already exists: a later run).
/// Safe to repeat any number of times. Exits 0 on success; exits 1, naming
/// the daemon log path, if the daemon could not be started or never became
/// healthy.
async fn cmd_run() {
    let mcp_port = turbofig::port_from_env();
    let ws_port = turbofig::ws_port_from_env();
    let home = turbofig::bridge_dir_from_env();
    ensure_verified_daemon("turbofig", mcp_port, &home, true).await;

    // On macOS, the app (its tray icon and About window) is now the
    // onboarding surface: install/refresh turbofig.app, open it, and point
    // the terminal at it, falling back to the ordinary text walkthrough
    // below only if either step fails.
    #[cfg(target_os = "macos")]
    if let Some(text) = try_app_first_run() {
        print!("{text}");
        return;
    }

    // The connected-file names below need /health's full, authenticated
    // payload; the token is already on disk by now (the daemon writes it
    // before either listener binds).
    let token = turbofig::read_token_file(&home).await;
    let health = match token.as_deref() {
        Some(t) => turbofig::daemon_client::verified_full_health(mcp_port, t)
            .await
            .ok(),
        None => None,
    };
    let version = health
        .as_ref()
        .map(|h| health_version(h).to_owned())
        .unwrap_or_else(|| "unknown".to_owned());
    let manifest_path = home.join("figma-plugin").join("manifest.json");

    if home.join("plugin-seen").exists() {
        let connected_file_names: Vec<String> = health
            .as_ref()
            .and_then(|h| h.get("connectedFiles"))
            .and_then(|v| v.as_array())
            .map(|files| {
                files
                    .iter()
                    .filter_map(|f| f.get("name").and_then(|n| n.as_str()).map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        print!(
            "{}",
            turbofig::first_run::status_text(
                &version,
                mcp_port,
                ws_port,
                &connected_file_names,
                &manifest_path,
            )
        );
        return;
    }

    let clipboard = clipboard_for_run();
    let opener = opener_for_run();
    let clipboard_copied = clipboard.copy(&manifest_path.display().to_string());
    let figma_opened = opener.open_figma();
    print!(
        "{}",
        turbofig::first_run::first_run_text(
            &version,
            mcp_port,
            ws_port,
            &manifest_path,
            figma_opened,
            clipboard_copied,
        )
    );
}

/// Longest the bare `turbofig` command waits, after asking a running app
/// instance to quit, for `<home>/app.lock` to be released before reopening
/// a just-refreshed `turbofig.app` anyway. See `try_app_first_run`.
#[cfg(target_os = "macos")]
const APP_REFRESH_RESTART_DEADLINE: Duration = Duration::from_secs(5);
/// How often that wait re-checks the lock.
#[cfg(target_os = "macos")]
const APP_REFRESH_RESTART_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Moves an existing `~/Applications/turbofig.app` to `/Applications` when
/// `/Applications` has become writable (the user gained admin rights, or
/// this is simply the first run after an install that predates this
/// migration) and nothing is already there. A no-op in every other case:
/// nothing in `/Applications` already, `/Applications` not writable, or no
/// bundle in `~/Applications` to move.
///
/// Called once per refresh (`run_daemon`'s outdated check, and the bare
/// command's `try_app_first_run`), before either resolves
/// `applications_dir_from_env`: a successful move changes what that
/// resolves to from here on, since it checks for an existing bundle in
/// `/Applications` first. Updates the app LaunchAgent's `ProgramArguments`
/// to the new location too, if one is installed; a failure at any step is a
/// warning, never fatal, so the ordinary refresh/open flow still proceeds
/// against whatever `applications_dir_from_env` resolves to next.
#[cfg(target_os = "macos")]
fn migrate_app_bundle_to_global_if_possible(own_exe: &Path, agents_dir: &Path) {
    rename_legacy_bundle_name_if_needed(agents_dir);

    if std::env::var("TURBOFIG_APPLICATIONS_DIR").is_ok() {
        return; // the override always wins; never migrate under it
    }
    let global = turbofig::app_bundle::global_applications_dir();
    let home = turbofig::app_bundle::home_applications_dir();
    if turbofig::app_bundle::has_our_bundle(&global)
        || !turbofig::app_bundle::has_our_bundle(&home)
        || !turbofig::app_bundle::dir_is_writable(&global)
    {
        return;
    }

    let from = turbofig::app_bundle::app_bundle_path(&home);
    let to = turbofig::app_bundle::app_bundle_path(&global);
    if let Err(e) = std::fs::rename(&from, &to) {
        eprintln!(
            "turbofig: warning: could not move turbofig.app to {}: {e}",
            global.display()
        );
        return;
    }
    println!(
        "turbofig: moved turbofig.app to your Applications folder ({})",
        global.display()
    );

    if agents_dir
        .join(turbofig::launchd::app_plist_file_name())
        .exists()
    {
        let uid = match current_uid() {
            Ok(u) => u,
            Err(e) => {
                eprintln!(
                    "turbofig: warning: could not update the app LaunchAgent after moving \
                     turbofig.app: failed to determine the current user id: {e}"
                );
                return;
            }
        };
        let launchctl = RealLaunchctl;
        if let Err(e) = turbofig::cli::run_autostart_on_app(
            agents_dir,
            &global,
            &launchctl,
            &uid,
            own_exe,
            &turbofig::bridge_dir_from_env(),
        ) {
            eprintln!(
                "turbofig: warning: could not update the app LaunchAgent after moving \
                 turbofig.app: {e}"
            );
        }
    }
}

/// Renames an on-disk `turbofig.app` to `turbofig.app` wherever
/// `applications_dir_from_env` resolves (the lowercase brand rename,
/// 2026-10): an owner upgrading from a pre-rename install keeps a bundle at
/// its old, capitalized name until this runs once. Also rewrites the app
/// LaunchAgent's on-disk `ProgramArguments`, if one exists, so an existing
/// autostart entry keeps pointing at a real file.
///
/// Called before `migrate_app_bundle_to_global_if_possible` resolves
/// anything: a no-op when no bundle is there, it already carries the
/// lowercase name, or it belongs to someone else. A failure at any step is
/// only ever a warning, never fatal.
#[cfg(target_os = "macos")]
fn rename_legacy_bundle_name_if_needed(agents_dir: &Path) {
    let applications_dir = turbofig::app_bundle::applications_dir_from_env();
    let renamed = match turbofig::app_bundle::migrate_legacy_bundle_name(
        &applications_dir,
        &turbofig::app_bundle::RealLaunchServicesRegistrar,
    ) {
        Ok(renamed) => renamed,
        Err(e) => {
            eprintln!("turbofig: warning: could not rename turbofig.app to turbofig.app: {e}");
            return;
        }
    };
    if !renamed {
        return;
    }
    println!("turbofig: renamed turbofig.app to turbofig.app");

    match turbofig::launchd::migrate_app_plist_bundle_name(agents_dir) {
        Ok(true) => println!("turbofig: updated the app LaunchAgent for the renamed bundle"),
        Ok(false) => {}
        Err(e) => eprintln!(
            "turbofig: warning: could not update the app LaunchAgent after the lowercase \
             rename: {e}"
        ),
    }
}

/// Installs/refreshes `turbofig.app` and opens it (through the `AppOpener`
/// seam, `open <bundle path>`). `Some(text)`: both steps succeeded, print
/// `app_first_run_text()` instead of the ordinary walkthrough. `None`: one
/// of them failed; already printed a 1-line reason, the caller falls back
/// to the ordinary walkthrough.
///
/// When the install actually replaced an outdated bundle (not a fresh
/// install) and an app instance is currently running, asks it to quit and
/// waits (`menu_bar::quit_running_app_and_wait_for_exit`) before reopening,
/// so a rebuild with the same version (the owner's reported bug) actually
/// reaches a running app instead of just re-activating the stale one; see
/// `app_bundle::app_bundle_outdated`'s build-id comparison.
#[cfg(target_os = "macos")]
fn try_app_first_run() -> Option<String> {
    let own_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("turbofig: failed to determine the running binary's path: {e}; falling back");
            return None;
        }
    };
    migrate_app_bundle_to_global_if_possible(&own_exe, &launch_agents_dir());
    let applications_dir = turbofig::app_bundle::applications_dir_from_env();
    let used_home_fallback = applications_dir == turbofig::app_bundle::home_applications_dir();
    let home = turbofig::bridge_dir_from_env();

    let bundle_dir = turbofig::app_bundle::app_bundle_path(&applications_dir);
    let was_outdated = bundle_dir.exists()
        && turbofig::app_bundle::app_bundle_outdated(&applications_dir, &own_exe);

    let bundle_install_err = turbofig::app_bundle::install_app_bundle(&applications_dir, &own_exe)
        .err()
        .map(|e| e.to_string());

    if bundle_install_err.is_none() && was_outdated {
        let restarted = turbofig::menu_bar::quit_running_app_and_wait_for_exit(
            &home,
            APP_REFRESH_RESTART_DEADLINE,
            APP_REFRESH_RESTART_POLL_INTERVAL,
            &|d| std::thread::sleep(d),
        );
        if restarted {
            println!("Updated turbofig.app and restarted it.");
        }
    }

    let opened = if bundle_install_err.is_none() {
        opener_for_run().open_url(&bundle_dir.display().to_string())
    } else {
        false
    };
    match turbofig::first_run::app_first_run_outcome(bundle_install_err, opened, used_home_fallback)
    {
        Ok(text) => Some(text),
        Err(reason) => {
            eprintln!("turbofig: {reason}; falling back to the manual walkthrough");
            None
        }
    }
}

/// Starts the daemon detached if it is not already running, waits for
/// `/health`, then prints the version and both ports. If a daemon is
/// already running, prints that and exits 0: `start` is idempotent, safe to
/// run any number of times.
async fn cmd_start() {
    let mcp_port = turbofig::port_from_env();
    let ws_port = turbofig::ws_port_from_env();
    let home = turbofig::bridge_dir_from_env();
    let (health, was_running) =
        ensure_verified_daemon("turbofig start", mcp_port, &home, false).await;
    if was_running {
        println!(
            "{}",
            already_running_message(health_version(&health), mcp_port)
        );
        return;
    }
    let version = health_version(&health).to_owned();
    println!("{}", started_message(&version, mcp_port, ws_port));
}

/// Stops the running daemon via the authenticated `/control` path, then
/// waits for it to actually go away. Idempotent: stopping an already-
/// stopped daemon is success, not an error. Prints a hint when the autostart
/// plist is present: the daemon stays stopped until the next login, or until
/// `turbofig start` is run.
async fn cmd_stop() {
    let mcp_port = turbofig::port_from_env();
    let home = turbofig::bridge_dir_from_env();
    let agents_dir = launch_agents_dir();
    let client = build_http_client("turbofig stop");

    match stop_running_daemon(&client, mcp_port, &home).await {
        Ok(true) => {
            println!("{}", stopped_message());
            if autostart_plist_exists(&agents_dir) {
                println!("{}", stop_autostart_restart_hint());
            }
        }
        Ok(false) => println!("{}", stop_nothing_running_message()),
        Err(e) => {
            eprintln!("turbofig stop: {e}");
            std::process::exit(1);
        }
    }
}

/// Stops the daemon on `mcp_port` via an authenticated `POST /control`,
/// using the token at `<home>/token`, then waits for it to actually go
/// away. Shared by `cmd_stop` and `cmd_uninstall`'s best-effort stop.
///
/// Returns `Ok(true)` when a daemon was stopped, `Ok(false)` when none was
/// running at all (no token file and `/health` unreachable, or the request
/// could not even connect and `/health` agrees: a stale token file from a
/// daemon that is already gone), and `Err` with a clear reason for any other
/// failure: the daemon refused the request, never actually went away, or
/// (see `token_trouble_stop_message`) `/health` answers but the token is
/// missing or no longer matches, so this cannot authenticate `/control` at
/// all. That last case must never fall through to `Ok(false)`: the daemon is
/// still running, so "no daemon appears to be running" would be a lie.
async fn stop_running_daemon(
    client: &reqwest::Client,
    mcp_port: u16,
    home: &std::path::Path,
) -> Result<bool, String> {
    use turbofig::daemon_client::{ClientError, VerifyError};
    // The token goes only to a daemon that proves it holds the token. A
    // listener that is not the daemon is an error, never "nothing running".
    let foreign = foreign_stop_message;
    let Some(token) = turbofig::read_token_file(home).await else {
        return match turbofig::daemon_client::verify_daemon_in_home(mcp_port, home).await {
            Err(VerifyError::Unreachable) => Ok(false),
            _ => Err(turbofig::cli::token_trouble_stop_message().to_owned()),
        };
    };

    let sent = turbofig::daemon_client::send_control(mcp_port, &token, "stop").await;
    let sent = match sent {
        Ok(status) => Ok(status),
        Err(ClientError::Verify(e)) => Err(e),
        Err(ClientError::Http(_)) => {
            // The request failed after the daemon was verified. Check again,
            // so a daemon that is up is never reported as "nothing running".
            return match turbofig::daemon_client::verify_daemon_in_home(mcp_port, home).await {
                Err(VerifyError::Unreachable) => Ok(false),
                Err(e) => Err(foreign(&e)),
                Ok(_) => Err(turbofig::cli::token_trouble_stop_message().to_owned()),
            };
        }
    };
    match stop_step(sent) {
        StopStep::Stopped => {}
        StopStep::NothingRunning => return Ok(false),
        StopStep::Fail(msg) => return Err(msg),
    }

    if !turbofig::spawn::wait_for_unreachable(client, mcp_port, STOP_UNREACHABLE_DEADLINE).await {
        return Err(format!(
            "the daemon on port {mcp_port} was still answering after the stop request"
        ));
    }
    Ok(true)
}

/// The error `stop` prints for a listener that is not the daemon.
fn foreign_stop_message(e: &turbofig::daemon_client::VerifyError) -> String {
    format!(
        "{e}. If it is your daemon, the token file changed. {}",
        turbofig::cli::token_trouble_stop_message()
    )
}

/// What `stop` does with the result of its verified `/control` request.
#[derive(Debug, PartialEq)]
enum StopStep {
    /// The daemon accepted the stop: wait for it to go away.
    Stopped,
    /// Nothing listens: stop is a no-op success.
    NothingRunning,
    /// Print this and exit 1.
    Fail(String),
}

/// An unverified listener is a failure and never gets the token. Only an
/// unreachable port means "nothing running".
fn stop_step(sent: Result<reqwest::StatusCode, turbofig::daemon_client::VerifyError>) -> StopStep {
    use turbofig::daemon_client::VerifyError;
    match sent {
        Ok(status) if status.is_success() => StopStep::Stopped,
        Ok(status) if status == reqwest::StatusCode::UNAUTHORIZED => {
            StopStep::Fail(turbofig::cli::token_trouble_stop_message().to_owned())
        }
        Ok(status) => StopStep::Fail(format!("the daemon refused the stop request ({status})")),
        Err(VerifyError::Unreachable) => StopStep::NothingRunning,
        Err(e) => StopStep::Fail(foreign_stop_message(&e)),
    }
}

/// Runs `turbofig mcp`: the stdio MCP proxy. Never prints to stdout itself
/// (that channel is reserved for MCP frames); every diagnostic here goes to
/// stderr, including the final error on failure.
async fn cmd_mcp() {
    let mcp_port = turbofig::port_from_env();
    let home = turbofig::bridge_dir_from_env();
    let turbofig_binary = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("turbofig mcp: failed to determine the running binary's path: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = turbofig::proxy::run(mcp_port, home, turbofig_binary).await {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

async fn cmd_status() {
    let mcp_port = turbofig::port_from_env();
    let home = turbofig::bridge_dir_from_env();
    use turbofig::daemon_client::{ClientError, VerifyError};
    // Needs /health's full, authenticated payload to show connected files.
    // The token goes only to a verified daemon.
    let result = match turbofig::read_token_file(&home).await {
        Some(token) => turbofig::daemon_client::verified_full_health(mcp_port, &token).await,
        None => Err(ClientError::Verify(
            match turbofig::daemon_client::verify_daemon_in_home(mcp_port, &home).await {
                Err(e) => e,
                Ok(_) => VerifyError::Unreachable,
            },
        )),
    };
    match result {
        Ok(body) => print!("{}", format_health(&body)),
        Err(ClientError::Verify(VerifyError::Unreachable)) => {
            println!("{}", status_unreachable_message(mcp_port));
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("turbofig status: {e}");
            std::process::exit(1);
        }
    }
}

/// The `serve` startup check: is a VERIFIED daemon already on `mcp_port`?
/// Only a daemon that proves it holds the token counts as running. Called
/// before `run_daemon` has any other state to reuse.
async fn serve_startup_check(mcp_port: u16, home: &Path) -> turbofig::daemon_client::ServeCheck {
    let result = turbofig::daemon_client::verify_daemon_in_home(mcp_port, home).await;
    turbofig::daemon_client::serve_check(&result)
}

/// Acts on a `serve` startup check that found a listener: prints the
/// message and exits. A verified daemon gives `already_running_exit_code`.
/// A listener that is not the daemon always gives exit 1, so launchd
/// `KeepAlive` retries instead of treating it as success. Returns for
/// `Proceed`.
fn exit_for_serve_check(check: &turbofig::daemon_client::ServeCheck, mcp_port: u16) {
    if let Some((msg, code)) = serve_check_exit(check, mcp_port, already_running_exit_code()) {
        eprintln!("{msg}");
        std::process::exit(code);
    }
}

/// The message and exit code for a `serve` startup check, or `None` to go
/// on. `running_code` is the exit code for a verified daemon. An unverified
/// listener always gives 1, whatever `running_code` is.
fn serve_check_exit(
    check: &turbofig::daemon_client::ServeCheck,
    mcp_port: u16,
    running_code: i32,
) -> Option<(String, i32)> {
    use turbofig::daemon_client::ServeCheck;
    match check {
        ServeCheck::Proceed => None,
        ServeCheck::AlreadyRunning(version) => {
            Some((already_running_message(version, mcp_port), running_code))
        }
        ServeCheck::Foreign(msg) => Some((format!("turbofig daemon: {msg}"), 1)),
    }
}

/// Exit code `serve` uses when it finds a daemon already running on this
/// port. Under launchd supervision (`turbofig::supervisor::is_supervised`),
/// this must be 0: the plist's `KeepAlive: {SuccessfulExit: false}`
/// (`launchd.rs`) then leaves the daemon stopped, instead of the old exit 1
/// which, combined with `KeepAlive` being unconditional, restarted this same
/// "already running" `serve` about every 10s forever. Unsupervised (a stray
/// manual second `serve`), 1 is still the right, ordinary CLI error exit.
fn already_running_exit_code() -> i32 {
    if turbofig::supervisor::is_supervised() {
        0
    } else {
        1
    }
}

/// Exits after a startup error. A refusal from `secure_fs` under the launchd
/// service exits 0, so `KeepAlive {SuccessfulExit: false}` does not restart
/// the daemon every 10 s on a folder only the user can repair. Any other
/// error exits 1.
fn exit_for_startup_error(e: &std::io::Error) -> ! {
    let code = turbofig::exit_code_for_error(e);
    if code == 0 {
        eprintln!(
            "turbofig daemon: not restarting under launchd. Fix the problem above, then run `turbofig autostart on` or `turbofig serve`."
        );
    }
    std::process::exit(code)
}

/// Runs the daemon exactly as `turbofig` with no arguments always has: binds
/// both ports, ensures the pairing token, refreshes a stale on-disk plugin
/// copy, and runs the three servers until one of them dies. Shared by the
/// bare `turbofig` invocation and `turbofig serve`.
/// Takes the exclusive lock on the bridge folder for the life of this
/// process, so no 2 daemons ever serve one inbox. Exits non-zero (0 under
/// launchd for a folder refusal, like the other startup checks) when another
/// daemon holds it.
fn take_bridge_lock(bridge_dir: &Path) {
    let own_mode = std::env::var("TURBOFIG_BRIDGE_DIR").is_err();
    match turbofig::lock_bridge_dir(bridge_dir, own_mode, std::time::Duration::from_millis(300)) {
        Ok(turbofig::LockOutcome::Held) => {
            turbofig::write_lock_info(
                turbofig::request_timeout_from_env()
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64,
            );
        }
        Ok(turbofig::LockOutcome::Unsupported) => {
            eprintln!(
                "turbofig daemon: the filesystem of {} has no file locks; running without the bridge folder lock",
                bridge_dir.display()
            );
        }
        Ok(turbofig::LockOutcome::Busy) => {
            eprintln!(
                "turbofig daemon: another turbofig daemon is already using {}",
                bridge_dir.display()
            );
            std::process::exit(already_running_exit_code());
        }
        Err(e) => {
            eprintln!(
                "turbofig daemon: could not lock {}: {e}",
                bridge_dir.display()
            );
            exit_for_startup_error(&e);
        }
    }
}

async fn run_daemon() {
    let mcp_port = turbofig::port_from_env();
    let ws_port = turbofig::ws_port_from_env();
    let bridge_dir = turbofig::bridge_dir_from_env();

    // `spawn_detached_daemon`'s own log rotation only ever runs in the
    // process that (re)spawns a daemon, so a long-running launchd-supervised
    // daemon's log is otherwise never rotated across its lifetime. Give this
    // `serve` the same chance, before anything below writes a line to
    // stdout/stderr that a rotation-and-reopen must not lose. A no-op
    // outside launchd supervision (see the function's own doc comment).
    turbofig::spawn::rotate_daemon_log_and_reopen_std_streams(&bridge_dir);

    // Check before any other side effect (reading the token, binding a
    // port): a second `turbofig serve` while one is already healthy on this
    // port must exit at once with a clear message, not silently fail later.
    exit_for_serve_check(&serve_startup_check(mcp_port, &bridge_dir).await, mcp_port);
    take_bridge_lock(&bridge_dir);
    raise_nofile_limit();

    let mcp_addr = format!("127.0.0.1:{mcp_port}");
    let ws_addr = format!("127.0.0.1:{ws_port}");

    let mcp_listener = match tokio::net::TcpListener::bind(&mcp_addr).await {
        Ok(l) => l,
        Err(e) => {
            // The health check above can still lose a tight startup race
            // (two `serve`s launched within the same instant): if the bind
            // failed because something is already there, re-check /health
            // once more so this path reports the same clear message instead
            // of a raw "Address already in use".
            if e.kind() == std::io::ErrorKind::AddrInUse {
                exit_for_serve_check(&serve_startup_check(mcp_port, &bridge_dir).await, mcp_port);
            }
            eprintln!("turbofig daemon: failed to bind MCP port {mcp_addr}: {e}");
            std::process::exit(1);
        }
    };

    let ws_listener = match tokio::net::TcpListener::bind(&ws_addr).await {
        Ok(l) => l,
        Err(e) => {
            // The MCP port is already bound by this process, so no other
            // daemon can run: the WS port is taken by something else. Report
            // the WS port itself, not the MCP port.
            eprintln!("turbofig daemon: failed to bind WS port {ws_addr}: {e}");
            std::process::exit(1);
        }
    };

    println!(
        "turbofig MCP listening on {}",
        mcp_listener.local_addr().expect("local addr after bind")
    );
    println!(
        "turbofig WS  listening on {}",
        ws_listener.local_addr().expect("local addr after bind")
    );
    println!("turbofig bridge dir: {}", bridge_dir.display());

    // Ensure the pairing token exists (created on first run, never overwritten
    // on a later one) before any server starts accepting connections. See
    // token.rs: this is the one path that reads or writes the real
    // ~/.turbofig/token; AppState::new() alone would generate an in-memory
    // token instead, which is what every test uses.
    let token = match turbofig::ensure_token(&bridge_dir) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("turbofig daemon: failed to create or read the pairing token: {e}");
            exit_for_startup_error(&e);
        }
    };

    // Always ensure the on-disk Figma plugin copy exists and is current:
    // create it on a fresh install (no figma-plugin/ yet), and refresh it on
    // a later start when this binary embeds a newer version or the token
    // rotated. `plugin_files_outdated` already reports true for a missing
    // directory, so no separate existence check is needed here.
    let figma_plugin_existed = bridge_dir.join("figma-plugin").exists();
    if turbofig::plugin_files_outdated(&bridge_dir, &token) {
        match turbofig::write_plugin_files(&bridge_dir, &token) {
            Ok(_) => {
                let verb = if figma_plugin_existed {
                    "refreshed"
                } else {
                    "wrote"
                };
                println!("turbofig daemon: {verb} the on-disk Figma plugin files");
            }
            Err(e) => eprintln!("turbofig daemon: failed to write the Figma plugin files: {e}"),
        }
    }

    // Keep an existing app bundle current across a `brew upgrade`: never
    // create one here (step 4 decides when a bundle is first created), only
    // refresh one that is already there and out of date.
    #[cfg(target_os = "macos")]
    {
        let own_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("turbofig"));
        migrate_app_bundle_to_global_if_possible(&own_exe, &launch_agents_dir());
        let applications_dir = turbofig::app_bundle::applications_dir_from_env();
        if turbofig::app_bundle::app_bundle_outdated(&applications_dir, &own_exe) {
            match turbofig::app_bundle::install_app_bundle(&applications_dir, &own_exe) {
                Ok(path) => println!(
                    "turbofig daemon: reinstalled the outdated app bundle at {}",
                    path.display()
                ),
                Err(e) => {
                    eprintln!("turbofig daemon: failed to reinstall the outdated app bundle: {e}")
                }
            }
        }
    }

    let state = Arc::new(AppState::new_with_token(token, &bridge_dir));

    let mcp_state = state.clone();
    let mcp_handle = tokio::spawn(async move {
        if let Err(e) = turbofig::serve_with_state(mcp_listener, mcp_state).await {
            eprintln!("turbofig daemon: MCP server error: {e}");
            std::process::exit(1);
        }
    });

    let ws_state = state.clone();
    let ws_handle = tokio::spawn(async move {
        if let Err(e) = turbofig::serve_ws(ws_listener, ws_state).await {
            eprintln!("turbofig daemon: WS server error: {e}");
            std::process::exit(1);
        }
    });

    let bridge_state = state.clone();
    let bridge_handle = tokio::spawn(async move {
        if let Err(e) = turbofig::serve_bridge(bridge_state, bridge_dir).await {
            eprintln!("turbofig daemon: bridge error: {e}");
            exit_for_startup_error(&e);
        }
    });

    // Under launchd supervision (set by the plist `turbofig autostart on`
    // writes), watch for a Homebrew upgrade and hand off cleanly instead of
    // letting KeepAlive kill an in-flight job. See supervisor.rs and
    // ARCHITECTURE.md.
    if turbofig::supervisor::is_supervised() {
        let supervised_state = state.clone();
        tokio::spawn(async move {
            run_supervisor_loop(supervised_state).await;
        });
    }

    // A healthy daemon runs forever. Any handle that completes (whether by a
    // normal return, unexpected for a server; by an Err path that did not call
    // process::exit; or by a task panic surfacing as a JoinError) means a
    // subsystem is dead. Log the name and exit so launchd KeepAlive restarts.
    tokio::select! {
        res = mcp_handle => {
            match res {
                Ok(()) => eprintln!("turbofig daemon: MCP server task ended unexpectedly"),
                Err(e) => eprintln!("turbofig daemon: MCP server task panicked: {e}"),
            }
        }
        res = ws_handle => {
            match res {
                Ok(()) => eprintln!("turbofig daemon: WS server task ended unexpectedly"),
                Err(e) => eprintln!("turbofig daemon: WS server task panicked: {e}"),
            }
        }
        res = bridge_handle => {
            match res {
                Ok(()) => eprintln!("turbofig daemon: bridge task ended unexpectedly"),
                Err(e) => eprintln!("turbofig daemon: bridge task panicked: {e}"),
            }
        }
    }
    std::process::exit(1);
}

/// The supervised-restart loop: every `SUPERVISOR_CHECK_INTERVAL`, resolve
/// the stable binary path and compare it to the one captured at startup. On
/// a change, stop accepting new jobs, wait up to `SUPERVISOR_DRAIN_MAX_WAIT`
/// for in-flight jobs to finish, then exits
/// `supervisor::SUPERVISED_RESTART_EXIT_CODE` (non-zero) so launchd's
/// `KeepAlive: {SuccessfulExit: false}` restarts the new binary. Exits 0
/// instead if a `/control` stop landed during the drain (`state.
/// stop_requested`), so a stop always overrides a restart. Never returns.
async fn run_supervisor_loop(state: Arc<AppState>) -> ! {
    // The stable path (for example /opt/homebrew/bin/turbofig) never changes
    // across an upgrade. Its resolved target (the versioned Cellar binary) does.
    let stable = stable_path_for_running_binary();
    let baseline = match installed_target(&stable) {
        Some(target) => target,
        // No baseline could be resolved at startup either; fall back to the
        // stable path itself so later comparisons still have something to
        // compare against. Once the path resolves to a real target, that
        // target will differ from this unresolved baseline, so the first
        // resolution after startup fires one restart, not never.
        None => stable.clone(),
    };
    let mut interval = tokio::time::interval(SUPERVISOR_CHECK_INTERVAL);
    interval.tick().await; // first tick fires immediately; consume it
    let mut consecutive_unresolved: u32 = 0;

    loop {
        interval.tick().await;
        let current = installed_target(&stable);
        if current.is_none() {
            consecutive_unresolved += 1;
            if should_log_binary_gone(consecutive_unresolved) {
                eprintln!(
                    "turbofig daemon: the turbofig binary is gone; run `turbofig uninstall` or reinstall"
                );
            }
            continue;
        }
        consecutive_unresolved = 0;
        if upgrade_detected(&baseline, current.as_deref()) {
            let current = current.expect("checked is_none above");
            println!(
                "turbofig daemon: detected an upgrade ({} -> {}); draining and restarting",
                baseline.display(),
                current.display()
            );
            state.set_draining(true);
            let drained = wait_for_drain(
                || state.jobs_in_flight(),
                SUPERVISOR_DRAIN_MAX_WAIT,
                SUPERVISOR_DRAIN_POLL_INTERVAL,
            )
            .await;
            if !drained {
                eprintln!(
                    "turbofig daemon: {} job(s) still in flight after {:?}; restarting anyway",
                    state.jobs_in_flight(),
                    SUPERVISOR_DRAIN_MAX_WAIT
                );
            }
            // Give a just-finished job's HTTP response or bridge result-file
            // rename a moment to flush before the process actually exits.
            tokio::time::sleep(SUPERVISOR_EXIT_GRACE).await;
            // Non-zero, not 0: this loop only ever runs under supervision
            // (the `is_supervised()` check above it), and the plist's
            // `KeepAlive: {SuccessfulExit: false}` (`launchd.rs`) restarts
            // the daemon only on a non-zero exit, picking up the upgraded
            // binary. See `supervisor::SUPERVISED_RESTART_EXIT_CODE`.
            //
            // Unless a `/control` stop landed while this drain was under
            // way (`state.request_stop`, `control.rs`): a stop always
            // overrides a restart, so exit 0 instead and leave the daemon
            // stopped.
            let code = if state.stop_requested() {
                0
            } else {
                turbofig::supervisor::SUPERVISED_RESTART_EXIT_CODE
            };
            std::process::exit(code);
        }
    }
}

/// The stable binary path for the currently running process: the Homebrew
/// `<prefix>/bin/turbofig` symlink when running from the Cellar, otherwise
/// the running binary itself.
fn stable_path_for_running_binary() -> PathBuf {
    let current_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("turbofig"));
    let canonical = current_exe
        .canonicalize()
        .unwrap_or_else(|_| current_exe.clone());
    turbofig::launchd::stable_binary_path(&canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nofile_target_raises_the_soft_limit_to_the_hard_limit() {
        #[cfg(not(target_os = "macos"))]
        assert_eq!(nofile_target(1024, 65536), 65536);
        #[cfg(target_os = "macos")]
        assert_eq!(nofile_target(256, 4096), 4096);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn nofile_target_caps_an_infinite_hard_limit_on_macos() {
        assert_eq!(nofile_target(256, libc::rlim_t::MAX), 10240);
        assert_eq!(nofile_target(256, libc::RLIM_INFINITY), 10240);
    }

    #[test]
    fn nofile_target_never_lowers_the_soft_limit() {
        assert!(nofile_target(20000, 20000) >= 20000);
    }

    #[test]
    fn raise_nofile_limit_does_not_lower_the_limit() {
        let get = || {
            let mut l = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut l) };
            l.rlim_cur
        };
        let before = get();
        raise_nofile_limit();
        assert!(get() >= before);
    }

    #[test]
    fn serve_check_exit_codes() {
        use turbofig::daemon_client::ServeCheck;
        assert_eq!(serve_check_exit(&ServeCheck::Proceed, 1, 0), None);
        let (msg, code) =
            serve_check_exit(&ServeCheck::AlreadyRunning("1.2.3".into()), 18846, 0).unwrap();
        assert_eq!(code, 0);
        assert!(msg.contains("already running"));
        let (_, code) =
            serve_check_exit(&ServeCheck::AlreadyRunning("1.2.3".into()), 18846, 1).unwrap();
        assert_eq!(code, 1);
        // A foreign listener exits 1 even when "already running" would exit 0.
        let (msg, code) = serve_check_exit(
            &ServeCheck::Foreign("not your turbofig daemon".into()),
            1,
            0,
        )
        .unwrap();
        assert_eq!(code, 1);
        assert!(msg.contains("not your turbofig daemon"));
    }

    #[test]
    fn startup_step_counts_only_a_verified_daemon_as_running() {
        use turbofig::daemon_client::VerifyError;
        let health = serde_json::json!({"version": "1.0.0"});
        assert_eq!(startup_step(Ok(health.clone())), Startup::Running(health));
        assert_eq!(startup_step(Err(VerifyError::Unreachable)), Startup::Start);
        let foreign = startup_step(Err(VerifyError::Unverified {
            addr: "127.0.0.1:18846".into(),
        }));
        assert!(matches!(foreign, Startup::Fail(m) if m.contains("127.0.0.1:18846")));
        assert!(matches!(
            startup_step(Err(VerifyError::NoToken { addr: "x".into() })),
            Startup::Fail(_)
        ));
    }

    #[test]
    fn stop_step_never_calls_a_foreign_listener_nothing_running() {
        use turbofig::daemon_client::VerifyError;
        assert_eq!(
            stop_step(Ok(reqwest::StatusCode::ACCEPTED)),
            StopStep::Stopped
        );
        assert_eq!(
            stop_step(Err(VerifyError::Unreachable)),
            StopStep::NothingRunning
        );
        let step = stop_step(Err(VerifyError::Unverified {
            addr: "127.0.0.1:18846".into(),
        }));
        assert!(matches!(&step, StopStep::Fail(m)
            if m.contains("127.0.0.1:18846") && m.contains("not your turbofig daemon")));
        assert!(matches!(
            stop_step(Ok(reqwest::StatusCode::UNAUTHORIZED)),
            StopStep::Fail(_)
        ));
    }
}
