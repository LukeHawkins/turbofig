use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use turbofig::cli::{
    carried_over_env_message, format_health, non_cellar_binary_warning, run_setup, run_uninstall,
    setup_steps_text, status_unreachable_message, uninstall_kept_home_message, Cli, Command,
};
use turbofig::launchd::{current_uid, RealLaunchctl};
use turbofig::supervisor::{
    installed_target, should_log_binary_gone, upgrade_detected, wait_for_drain,
};
use turbofig::AppState;

/// How often the supervised-restart loop checks whether the stable binary
/// path now resolves somewhere else (a Homebrew upgrade landed).
const SUPERVISOR_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);
/// Longest the supervised-restart loop waits for in-flight jobs to finish
/// before exiting anyway.
const SUPERVISOR_DRAIN_MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(60);
const SUPERVISOR_DRAIN_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
/// Grace wait after the job count reaches 0, before the process actually
/// exits. `jobs_in_flight` reaching 0 means the daemon has finished writing
/// its own in-memory result, but an outbound HTTP response or a bridge
/// result-file rename can still be a few scheduler ticks from landing;
/// this gives those a moment to flush before launchd restarts the process.
const SUPERVISOR_EXIT_GRACE: std::time::Duration = std::time::Duration::from_millis(250);

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

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if cli.check_embedded {
        cmd_check_embedded();
    }
    match cli.command {
        None | Some(Command::Serve) => run_daemon().await,
        Some(Command::Setup) => cmd_setup(),
        Some(Command::Uninstall { purge }) => cmd_uninstall(purge),
        Some(Command::Status) => cmd_status().await,
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

/// Best-effort copy of `text` to the macOS clipboard via `pbcopy`. Returns
/// true on success. Figma's "Import plugin from manifest" file picker hides
/// `~/.turbofig` (a dotfile), so a copy-pasteable manifest path is the
/// practical way in. A failure here (no `pbcopy`, a non-interactive
/// session) must never stop `setup`: the printed path is still correct on
/// its own, just not pre-copied.
fn copy_to_clipboard(text: &str) -> bool {
    use std::io::Write;
    let mut child = match std::process::Command::new("pbcopy")
        .stdin(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let Some(mut stdin) = child.stdin.take() else {
        return false;
    };
    if stdin.write_all(text.as_bytes()).is_err() {
        return false;
    }
    drop(stdin);
    child.wait().map(|status| status.success()).unwrap_or(false)
}

fn cmd_setup() {
    let home = turbofig::bridge_dir_from_env();
    let agents_dir = launch_agents_dir();
    let launchctl = RealLaunchctl;
    let uid = match current_uid() {
        Ok(u) => u,
        Err(e) => {
            eprintln!("turbofig setup: failed to determine the current user id: {e}");
            std::process::exit(1);
        }
    };
    let current_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("turbofig setup: failed to determine the running binary's path: {e}");
            std::process::exit(1);
        }
    };

    match run_setup(&home, &agents_dir, &launchctl, &uid, &current_exe) {
        Ok(outcome) => {
            println!(
                "turbofig: installed and started (plist: {})",
                outcome.plist_path.display()
            );
            print!(
                "{}",
                non_cellar_binary_warning(outcome.binary_outside_homebrew_cellar)
            );
            print!("{}", carried_over_env_message(&outcome.carried_over_env));
            let clipboard_copied = copy_to_clipboard(&outcome.manifest_path.to_string_lossy());
            print!(
                "{}",
                setup_steps_text(
                    &outcome.manifest_path,
                    turbofig::port_from_env(),
                    clipboard_copied
                )
            );
        }
        Err(e) => {
            eprintln!("turbofig setup: {e}");
            std::process::exit(1);
        }
    }
}

fn cmd_uninstall(purge: bool) {
    let home = turbofig::bridge_dir_from_env();
    let agents_dir = launch_agents_dir();
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
    // The daemon is always local (127.0.0.1); a corporate proxy env var
    // (HTTP_PROXY/HTTPS_PROXY) must never be allowed to intercept or break
    // this request, so build a client that ignores proxy env settings
    // instead of using reqwest::get's default client.
    let client = match reqwest::Client::builder().no_proxy().build() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("turbofig status: could not build the HTTP client: {e}");
            std::process::exit(1);
        }
    };
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

/// Runs the daemon exactly as `turbofig` with no arguments always has: binds
/// both ports, ensures the pairing token, refreshes a stale on-disk plugin
/// copy, and runs the three servers until one of them dies. Shared by the
/// bare `turbofig` invocation and `turbofig serve`.
async fn run_daemon() {
    let mcp_port = turbofig::port_from_env();
    let ws_port = turbofig::ws_port_from_env();
    let bridge_dir = turbofig::bridge_dir_from_env();

    let mcp_addr = format!("127.0.0.1:{mcp_port}");
    let ws_addr = format!("127.0.0.1:{ws_port}");

    let mcp_listener = match tokio::net::TcpListener::bind(&mcp_addr).await {
        Ok(l) => l,
        Err(e) => {
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

    // If `turbofig setup` already wrote the plugin files out once, and this
    // binary embeds a newer version (or the token rotated), refresh them now
    // so a Homebrew upgrade's new plugin reaches disk without a manual
    // `turbofig setup` re-run. A fresh install (no figma-plugin/ yet) is left
    // to `turbofig setup`, not written implicitly here.
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

    // Under launchd supervision (set by the plist `turbofig setup` writes),
    // watch for a Homebrew upgrade and hand off cleanly instead of letting
    // KeepAlive kill an in-flight job. See supervisor.rs and ARCHITECTURE.md.
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
