use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use turbofig::cli::{
    already_running_message, autostart_off_message, autostart_on_message, autostart_plist_exists,
    carried_over_env_message, format_health, non_cellar_binary_warning, run_autostart_off,
    run_autostart_on, run_uninstall, started_message, status_unreachable_message,
    stop_autostart_restart_hint, stop_nothing_running_message, stopped_message,
    uninstall_kept_home_message, AutostartState, Cli, Command,
};
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

/// Builds the HTTP client every command that talks to the daemon uses.
/// The daemon is always local (127.0.0.1); a corporate proxy env var
/// (HTTP_PROXY/HTTPS_PROXY) must never be allowed to intercept or break
/// this request, so this ignores proxy env settings rather than using
/// reqwest's default client. Exits 1 with a clear message on the (very
/// unlikely) failure to build a client at all.
fn build_http_client(context: &str) -> reqwest::Client {
    match reqwest::Client::builder().no_proxy().build() {
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

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if cli.check_embedded {
        cmd_check_embedded();
    }
    match cli.command {
        None | Some(Command::Serve) => run_daemon().await,
        Some(Command::Start) => cmd_start().await,
        Some(Command::Stop) => cmd_stop().await,
        Some(Command::Status) => cmd_status().await,
        Some(Command::Autostart { state }) => cmd_autostart(state),
        Some(Command::Uninstall { purge }) => cmd_uninstall(purge).await,
        Some(Command::Mcp) => cmd_mcp().await,
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

fn cmd_autostart(state: AutostartState) {
    match state {
        AutostartState::On => cmd_autostart_on(),
        AutostartState::Off => cmd_autostart_off(),
    }
}

fn cmd_autostart_on() {
    let home = turbofig::bridge_dir_from_env();
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

    match run_autostart_on(&agents_dir, &launchctl, &uid, &current_exe, &home) {
        Ok(outcome) => {
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
        Ok(plist_path) => println!("{}", autostart_off_message(&plist_path)),
        Err(e) => {
            eprintln!("turbofig autostart: {e}");
            std::process::exit(1);
        }
    }
}

async fn cmd_uninstall(purge: bool) {
    let home = turbofig::bridge_dir_from_env();
    let agents_dir = launch_agents_dir();
    let mcp_port = turbofig::port_from_env();
    let client = build_http_client("turbofig uninstall");

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

    match run_uninstall(&home, &agents_dir, &launchctl, &uid, purge) {
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

/// Starts the daemon detached if it is not already running, waits for
/// `/health`, then prints the version and both ports. If a daemon is
/// already running, prints that and exits 0: `start` is idempotent, safe to
/// run any number of times.
async fn cmd_start() {
    let mcp_port = turbofig::port_from_env();
    let ws_port = turbofig::ws_port_from_env();
    let home = turbofig::bridge_dir_from_env();
    let client = build_http_client("turbofig start");

    if let Some(health) = turbofig::spawn::fetch_health(&client, mcp_port).await {
        println!(
            "{}",
            already_running_message(health_version(&health), mcp_port)
        );
        return;
    }

    let turbofig_binary = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("turbofig start: failed to determine the running binary's path: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = turbofig::spawn::spawn_detached_daemon(&turbofig_binary, &home) {
        eprintln!("turbofig start: could not start the daemon: {e}");
        std::process::exit(1);
    }
    if let Err(e) = turbofig::spawn::wait_for_health(&client, mcp_port).await {
        eprintln!("turbofig start: {e}");
        std::process::exit(1);
    }

    let version = match turbofig::spawn::fetch_health(&client, mcp_port).await {
        Some(h) => health_version(&h).to_owned(),
        None => "unknown".to_owned(),
    };
    println!("{}", started_message(&version, mcp_port, ws_port));
}

/// Stops the running daemon via the authenticated `/control` path, then
/// waits for it to actually go away. Idempotent: stopping an already-
/// stopped daemon is success, not an error. Warns when the autostart plist
/// is present, since launchd's `KeepAlive` will otherwise restart the
/// daemon this command just stopped.
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
/// running (no token file, or the request could not even connect: a stale
/// token file from a daemon that is already gone), and `Err` with a clear
/// reason for any other failure (the daemon refused the request, or never
/// actually went away).
async fn stop_running_daemon(
    client: &reqwest::Client,
    mcp_port: u16,
    home: &std::path::Path,
) -> Result<bool, String> {
    let Ok(token) = tokio::fs::read_to_string(home.join("token")).await else {
        return Ok(false);
    };

    let resp = client
        .post(format!("http://127.0.0.1:{mcp_port}/control"))
        .bearer_auth(token.trim())
        .json(&serde_json::json!({"action": "stop"}))
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => {}
        Ok(r) => {
            return Err(format!(
                "the daemon refused the stop request ({})",
                r.status()
            ))
        }
        Err(_) => return Ok(false), // nothing answered: a stale token file
    }

    if !turbofig::spawn::wait_for_unreachable(client, mcp_port, STOP_UNREACHABLE_DEADLINE).await {
        return Err(format!(
            "the daemon on port {mcp_port} was still answering after the stop request"
        ));
    }
    Ok(true)
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
    let url = format!("http://127.0.0.1:{mcp_port}/health");
    let client = build_http_client("turbofig status");
    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<serde_json::Value>().await {
            Ok(body) => print!("{}", format_health(&body)),
            Err(e) => {
                eprintln!("turbofig status: could not parse the daemon's /health response: {e}");
                std::process::exit(1);
            }
        },
        Ok(resp) => {
            eprintln!("turbofig status: daemon responded with {}", resp.status());
            std::process::exit(1);
        }
        Err(_) => {
            println!("{}", status_unreachable_message(mcp_port));
            std::process::exit(1);
        }
    }
}

/// Returns `/health`'s body from an already-running daemon on `mcp_port`,
/// or `None` if none is reachable. Builds its own short-lived client: called
/// before `run_daemon` has any other state to reuse one from.
async fn already_running_health(mcp_port: u16) -> Option<serde_json::Value> {
    let client = reqwest::Client::builder().no_proxy().build().ok()?;
    turbofig::spawn::fetch_health(&client, mcp_port).await
}

/// Runs the daemon exactly as `turbofig` with no arguments always has: binds
/// both ports, ensures the pairing token, refreshes a stale on-disk plugin
/// copy, and runs the three servers until one of them dies. Shared by the
/// bare `turbofig` invocation and `turbofig serve`.
async fn run_daemon() {
    let mcp_port = turbofig::port_from_env();
    let ws_port = turbofig::ws_port_from_env();
    let bridge_dir = turbofig::bridge_dir_from_env();

    // Check before any other side effect (reading the token, binding a
    // port): a second `turbofig serve` while one is already healthy on this
    // port must exit at once with a clear message, not silently fail later.
    if let Some(health) = already_running_health(mcp_port).await {
        eprintln!(
            "{}",
            already_running_message(health_version(&health), mcp_port)
        );
        std::process::exit(1);
    }

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
                if let Some(health) = already_running_health(mcp_port).await {
                    eprintln!(
                        "{}",
                        already_running_message(health_version(&health), mcp_port)
                    );
                    std::process::exit(1);
                }
            }
            eprintln!("Turbofig daemon: failed to bind MCP port {mcp_addr}: {e}");
            std::process::exit(1);
        }
    };

    let ws_listener = match tokio::net::TcpListener::bind(&ws_addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Turbofig daemon: failed to bind WS port {ws_addr}: {e}");
            std::process::exit(1);
        }
    };

    println!(
        "Turbofig MCP listening on {}",
        mcp_listener.local_addr().expect("local addr after bind")
    );
    println!(
        "Turbofig WS  listening on {}",
        ws_listener.local_addr().expect("local addr after bind")
    );
    println!("Turbofig bridge dir: {}", bridge_dir.display());

    // Ensure the pairing token exists (created on first run, never overwritten
    // on a later one) before any server starts accepting connections. See
    // token.rs: this is the one path that reads or writes the real
    // ~/.turbofig/token; AppState::new() alone would generate an in-memory
    // token instead, which is what every test uses.
    let token = match turbofig::ensure_token(&bridge_dir) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Turbofig daemon: failed to create or read the pairing token: {e}");
            std::process::exit(1);
        }
    };

    // If a plugin copy already exists on disk, and this binary embeds a
    // newer version (or the token rotated), refresh it now so a Homebrew
    // upgrade's new plugin reaches disk without manual intervention. A
    // fresh install with no figma-plugin/ yet is left alone here; a later
    // step makes this write unconditional, matching an always-on first run.
    let figma_plugin_dir = bridge_dir.join("figma-plugin");
    if figma_plugin_dir.exists() && turbofig::plugin_files_outdated(&bridge_dir, &token) {
        match turbofig::write_plugin_files(&bridge_dir, &token) {
            Ok(_) => println!("Turbofig daemon: refreshed the on-disk Figma plugin files"),
            Err(e) => eprintln!("Turbofig daemon: failed to refresh the Figma plugin files: {e}"),
        }
    }

    let state = Arc::new(AppState::new_with_token(token));

    let mcp_state = state.clone();
    let mcp_handle = tokio::spawn(async move {
        if let Err(e) = turbofig::serve_with_state(mcp_listener, mcp_state).await {
            eprintln!("Turbofig daemon: MCP server error: {e}");
            std::process::exit(1);
        }
    });

    let ws_state = state.clone();
    let ws_handle = tokio::spawn(async move {
        if let Err(e) = turbofig::serve_ws(ws_listener, ws_state).await {
            eprintln!("Turbofig daemon: WS server error: {e}");
            std::process::exit(1);
        }
    });

    let bridge_state = state.clone();
    let bridge_handle = tokio::spawn(async move {
        if let Err(e) = turbofig::serve_bridge(bridge_state, bridge_dir).await {
            eprintln!("Turbofig daemon: bridge error: {e}");
            std::process::exit(1);
        }
    });

    // Under launchd supervision (set by the plist `turbofig autostart on`
    // writes), watch for a Homebrew upgrade and hand off cleanly instead of
    // letting KeepAlive kill an in-flight job. See supervisor.rs and
    // ARCHITECTURE.md.
    if std::env::var("TURBOFIG_SUPERVISED").as_deref() == Ok("1") {
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
                Ok(()) => eprintln!("Turbofig daemon: MCP server task ended unexpectedly"),
                Err(e) => eprintln!("Turbofig daemon: MCP server task panicked: {e}"),
            }
        }
        res = ws_handle => {
            match res {
                Ok(()) => eprintln!("Turbofig daemon: WS server task ended unexpectedly"),
                Err(e) => eprintln!("Turbofig daemon: WS server task panicked: {e}"),
            }
        }
        res = bridge_handle => {
            match res {
                Ok(()) => eprintln!("Turbofig daemon: bridge task ended unexpectedly"),
                Err(e) => eprintln!("Turbofig daemon: bridge task panicked: {e}"),
            }
        }
    }
    std::process::exit(1);
}

/// The supervised-restart loop: every `SUPERVISOR_CHECK_INTERVAL`, resolve
/// the stable binary path and compare it to the one captured at startup. On
/// a change, stop accepting new jobs, wait up to `SUPERVISOR_DRAIN_MAX_WAIT`
/// for in-flight jobs to finish, then exit 0 so launchd starts the new
/// binary. Never returns.
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
                    "Turbofig daemon: the turbofig binary is gone; run `turbofig uninstall` or reinstall"
                );
            }
            continue;
        }
        consecutive_unresolved = 0;
        if upgrade_detected(&baseline, current.as_deref()) {
            let current = current.expect("checked is_none above");
            println!(
                "Turbofig daemon: detected an upgrade ({} -> {}); draining and restarting",
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
                    "Turbofig daemon: {} job(s) still in flight after {:?}; restarting anyway",
                    state.jobs_in_flight(),
                    SUPERVISOR_DRAIN_MAX_WAIT
                );
            }
            // Give a just-finished job's HTTP response or bridge result-file
            // rename a moment to flush before the process actually exits.
            tokio::time::sleep(SUPERVISOR_EXIT_GRACE).await;
            std::process::exit(0);
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
