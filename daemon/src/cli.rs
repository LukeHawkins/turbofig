//! The `turbofig` CLI: argument parsing (`clap`) and the pure/testable
//! halves of `autostart on`/`autostart off`/`uninstall`/`status`. `main.rs`
//! wires these to the real filesystem, `launchctl`, and HTTP client; tests
//! use the seams here (`Launchctl`, explicit `home`/`launch_agents_dir`/`uid`
//! arguments) instead.

use crate::launchd::{
    carry_over_turbofig_env, domain_target, is_in_homebrew_cellar, plist_contents, plist_file_name,
    service_target, stable_binary_path, Launchctl,
};
use clap::{Parser, Subcommand, ValueEnum};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(
    name = "turbofig",
    version,
    about = "Bridge any AI to Figma.",
    long_about = "turbofig: the always-on bridge from Figma to any AI agent.\n\nWith no subcommand, runs the daemon in the foreground (same as `turbofig serve`)."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Exit 0 if this binary embeds the Figma plugin, 1 if not. Used by the
    /// release workflow; hidden from `--help`.
    #[arg(long, hide = true)]
    pub check_embedded: bool,
}

#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Run the daemon in the foreground. Same as no subcommand.
    Serve,
    /// Start the daemon detached in the background, if it is not already
    /// running.
    Start,
    /// Stop the running daemon.
    Stop,
    /// Query the running daemon's `/health` endpoint.
    Status,
    /// Turn the launchd autostart service on or off.
    Autostart {
        #[arg(value_enum)]
        state: AutostartState,
    },
    /// Stop the daemon, turn autostart off, and remove its plist.
    Uninstall {
        /// Also delete the turbofig entries in the home directory (token,
        /// plugin files, bridge inbox/outbox, log), then the directory
        /// itself if left empty. Without this flag, the home directory is
        /// kept.
        #[arg(long)]
        purge: bool,
    },
    /// Run a stdio MCP server that forwards every tool call onto the
    /// daemon's `POST /job`, starting the daemon if it is not reachable.
    Mcp,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutostartState {
    On,
    Off,
}

/// Result of a successful `run_autostart_on` call.
#[derive(Debug, PartialEq, Eq)]
pub struct AutostartOnOutcome {
    pub plist_path: PathBuf,
    /// Names of the `TURBOFIG_*` environment variables that were set at
    /// `autostart on` time and carried into the plist's `EnvironmentVariables`,
    /// in the order written. Empty when none were set.
    pub carried_over_env: Vec<String>,
    /// True when the binary pinned into the plist is not inside a Homebrew
    /// Cellar (a source checkout's `target/release` or `target/debug`). The
    /// service breaks if that binary moves or is deleted.
    pub binary_outside_homebrew_cellar: bool,
}

/// Turns the launchd autostart service on: writes the plist pinning the
/// stable binary path (so a `brew upgrade` never invalidates it), then
/// `bootout`s (ignoring "not loaded") and `bootstrap`s it so the daemon
/// starts now and on every login.
///
/// Writes no token, no plugin files: the daemon writes those itself on its
/// own startup (`run_daemon` in `main.rs`), whether it was started by
/// launchd, `turbofig start`, or `turbofig serve` directly. This only ever
/// touches the plist.
///
/// Idempotent: the plist is byte-identical across runs given the same
/// binary path and home, and a `bootout` then `bootstrap` pair gives the
/// same end state whether or not the service was already loaded. Running
/// `turbofig autostart on` twice in a row is always safe.
pub fn run_autostart_on(
    launch_agents_dir: &Path,
    launchctl: &dyn Launchctl,
    uid: &str,
    current_exe: &Path,
    home: &Path,
) -> io::Result<AutostartOnOutcome> {
    run_autostart_on_with_sleep(launch_agents_dir, launchctl, uid, current_exe, home, &|d| {
        std::thread::sleep(d)
    })
}

/// The testable half of `run_autostart_on`: takes an explicit `sleep` so a
/// test can pass a no-op and exercise `bootstrap`'s retry loop without a
/// real wait.
fn run_autostart_on_with_sleep(
    launch_agents_dir: &Path,
    launchctl: &dyn Launchctl,
    uid: &str,
    current_exe: &Path,
    home: &Path,
    sleep: &dyn Fn(Duration),
) -> io::Result<AutostartOnOutcome> {
    std::fs::create_dir_all(launch_agents_dir)?;
    let canonical_exe = current_exe
        .canonicalize()
        .unwrap_or_else(|_| current_exe.to_path_buf());
    let program = stable_binary_path(&canonical_exe);
    let log_path = home.join("daemon.log");
    let plist_path = launch_agents_dir.join(plist_file_name());
    let extra_env = carry_over_turbofig_env();
    std::fs::write(&plist_path, plist_contents(&program, &log_path, &extra_env))?;

    launchctl.bootout(&service_target(uid));
    bootstrap_with_retry(launchctl, &domain_target(uid), &plist_path, sleep)?;

    Ok(AutostartOnOutcome {
        plist_path,
        carried_over_env: extra_env.into_iter().map(|(key, _)| key).collect(),
        binary_outside_homebrew_cellar: !is_in_homebrew_cellar(&canonical_exe),
    })
}

/// Message printed after `autostart on` when the pinned binary is not inside
/// a Homebrew Cellar. Empty when it is (the common case: a real install).
pub fn non_cellar_binary_warning(binary_outside_homebrew_cellar: bool) -> String {
    if !binary_outside_homebrew_cellar {
        return String::new();
    }
    "turbofig: warning: this binary is not a Homebrew install. The launchd \
     service will break if it moves or is deleted. Run \
     `brew install LukeHawkins/tap/turbofig` for a stable install.\n"
        .to_owned()
}

/// How many times to retry `launchctl bootstrap` after a failure.
const BOOTSTRAP_MAX_ATTEMPTS: u32 = 5;

/// Calls `launchctl.bootstrap`, retrying up to `BOOTSTRAP_MAX_ATTEMPTS` times
/// with a short backoff between attempts. `bootstrap` right after `bootout`
/// often fails on macOS with "Bootstrap failed: 5" while the old service
/// instance is still shutting down; retrying a few times usually succeeds
/// without the user having to re-run `autostart on` themselves.
fn bootstrap_with_retry(
    launchctl: &dyn Launchctl,
    domain_target: &str,
    plist_path: &Path,
    sleep: &dyn Fn(Duration),
) -> io::Result<()> {
    let mut attempt = 1;
    loop {
        match launchctl.bootstrap(domain_target, plist_path) {
            Ok(()) => return Ok(()),
            Err(e) if attempt >= BOOTSTRAP_MAX_ATTEMPTS => return Err(e),
            Err(_) => {
                sleep(Duration::from_millis(200 * u64::from(attempt)));
                attempt += 1;
            }
        }
    }
}

/// Message printed after `autostart on` when 1 or more `TURBOFIG_*`
/// variables were carried from the process's own environment into the
/// plist. Empty when `carried_over_env` is empty.
pub fn carried_over_env_message(carried_over_env: &[String]) -> String {
    if carried_over_env.is_empty() {
        return String::new();
    }
    format!(
        "turbofig: carried {} into the launchd service.\n",
        carried_over_env.join(", ")
    )
}

/// Message printed when `autostart on` installs and starts the service.
pub fn autostart_on_message(plist_path: &Path) -> String {
    format!("turbofig: autostart on (plist: {})", plist_path.display())
}

/// Message printed when `autostart off` unloads the service and removes the
/// plist.
pub fn autostart_off_message(plist_path: &Path) -> String {
    format!("turbofig: autostart off (removed {})", plist_path.display())
}

/// The exact 3 numbered steps plus the 1 optional MCP line the no-arg first
/// run prints (moved here from the retired `setup` command; wired up by a
/// later step).
///
/// `mcp_port` is the real port the daemon's MCP endpoint listens on
/// (`TURBOFIG_MCP_PORT`-overridable), not a hardcoded default: a custom
/// port must show up here too, or the printed command connects to nothing.
///
/// `clipboard_copied` is whether the caller already best-effort copied
/// `manifest_path` to the clipboard: Figma's file picker hides `~/.turbofig`,
/// so a copy-pasteable path is the practical way in. When true, an
/// unnumbered hint line is added after step 1, pointing at Figma's "go to
/// folder" shortcut; this never changes the count of numbered steps.
pub fn setup_steps_text(manifest_path: &Path, mcp_port: u16, clipboard_copied: bool) -> String {
    let clipboard_hint = if clipboard_copied {
        "   (Figma's file picker hides ~/.turbofig: press Cmd+Shift+G, paste the path \
         - it is on your clipboard - then press Return.)\n"
    } else {
        ""
    };
    format!(
        "1. In Figma Desktop: Plugins > Development > Import plugin from manifest, then pick {}.\n\
{clipboard_hint}\
2. Run the turbofig plugin in a file.\n\
3. Click the copy-prompt button in the plugin and paste it into your AI agent.\n\
\n\
Optional, for MCP clients: claude mcp add --transport http turbofig http://127.0.0.1:{mcp_port}/mcp\n",
        manifest_path.display()
    )
}

/// Result of a successful `run_uninstall` call.
#[derive(Debug, PartialEq, Eq)]
pub struct UninstallOutcome {
    pub home: PathBuf,
    pub purged: bool,
}

/// Turns autostart off: unloads the launchd service and removes its plist.
/// A missing plist is not an error: this is idempotent too. Returns the
/// plist path removed (or that would have been removed).
pub fn run_autostart_off(
    launch_agents_dir: &Path,
    launchctl: &dyn Launchctl,
    uid: &str,
) -> io::Result<PathBuf> {
    launchctl.bootout(&service_target(uid));
    let plist_path = launch_agents_dir.join(plist_file_name());
    ignore_not_found(std::fs::remove_file(&plist_path))?;
    Ok(plist_path)
}

/// Returns true when the autostart plist exists, i.e. `autostart on` has
/// been run (whether or not launchd currently has it loaded). Used only to
/// decide whether `turbofig stop` prints a hint that launchd will restart
/// the daemon it just stopped.
pub fn autostart_plist_exists(launch_agents_dir: &Path) -> bool {
    launch_agents_dir.join(plist_file_name()).exists()
}

/// Turns autostart off (see `run_autostart_off`), then, with `purge`, also
/// deletes the known turbofig entries inside `home` (see `purge_home`).
/// Stopping the running daemon itself is the caller's job (`main.rs`'s
/// `cmd_uninstall`): this function only ever touches the plist and the
/// home directory, never the network.
pub fn run_uninstall(
    home: &Path,
    launch_agents_dir: &Path,
    launchctl: &dyn Launchctl,
    uid: &str,
    purge: bool,
) -> io::Result<UninstallOutcome> {
    run_autostart_off(launch_agents_dir, launchctl, uid)?;

    if purge {
        purge_home(home)?;
    }

    Ok(UninstallOutcome {
        home: home.to_path_buf(),
        purged: purge,
    })
}

/// The exact entries `turbofig` writes directly under its home directory.
/// `--purge` removes only these, never the whole directory, so a `home` that
/// is a shared folder or `$HOME` is never wiped out from under the user.
const PURGE_ENTRIES: &[&str] = &["token", "figma-plugin", "inbox", "outbox", "daemon.log"];

/// Deletes the known turbofig entries inside `home`, then removes `home`
/// itself only if it is left empty. Anything else in `home` (a file the user
/// put there, or one a future version of turbofig did not know to list) is
/// left in place, and the directory is not removed with it still inside.
fn purge_home(home: &Path) -> io::Result<()> {
    for entry in PURGE_ENTRIES {
        let path = home.join(entry);
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        if metadata.is_dir() {
            ignore_not_found(std::fs::remove_dir_all(&path))?;
        } else {
            ignore_not_found(std::fs::remove_file(&path))?;
        }
    }

    match std::fs::read_dir(home) {
        Ok(mut entries) => {
            if entries.next().is_none() {
                ignore_not_found(std::fs::remove_dir(home))?;
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }

    Ok(())
}

/// Returns `Ok(())` for a successful result or a `NotFound` error; propagates
/// any other error.
fn ignore_not_found(result: io::Result<()>) -> io::Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Message printed after a non-purge uninstall, naming the kept directory.
pub fn uninstall_kept_home_message(home: &Path) -> String {
    format!(
        "turbofig: kept {} (token, plugin files, bridge inbox/outbox). Run with --purge to remove it.",
        home.display()
    )
}

/// Formats a `/health` JSON body into a readable report for `turbofig status`.
pub fn format_health(body: &serde_json::Value) -> String {
    let version = body
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let uptime = body
        .get("uptimeSeconds")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let mut out = format!("turbofig daemon: version {version}, up {uptime}s\n");

    let files = body
        .get("connectedFiles")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if files.is_empty() {
        out.push_str("No Figma file connected.\n");
        return out;
    }
    for f in &files {
        let name = f.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let file_key = f.get("fileKey").and_then(|v| v.as_str()).unwrap_or("");
        let plugin_version = f
            .get("pluginVersion")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        out.push_str(&format!("- {name} ({file_key}) plugin {plugin_version}"));
        if let Some(warning) = f.get("warning").and_then(|v| v.as_str()) {
            out.push_str(&format!(" -- {warning}"));
        }
        out.push('\n');
    }
    out
}

/// Message printed when the daemon cannot be reached on `mcp_port`.
pub fn status_unreachable_message(mcp_port: u16) -> String {
    format!(
        "turbofig: could not reach the daemon on port {mcp_port}.\nRun `turbofig start` to start it."
    )
}

/// Message for `turbofig serve` (or `turbofig start`, or a stray second
/// `serve`) finding a daemon already answering `/health` on this port.
/// `serve` prints this and exits 1 before any other side effect (binding a
/// port, touching the token); `start` prints the same text and exits 0.
pub fn already_running_message(version: &str, mcp_port: u16) -> String {
    format!("turbofig is already running (version {version}, port {mcp_port})")
}

/// Message `turbofig start` prints once it has confirmed the daemon it just
/// started (or found already running) answers `/health`.
pub fn started_message(version: &str, mcp_port: u16, ws_port: u16) -> String {
    format!("turbofig: started (version {version}, MCP port {mcp_port}, WS port {ws_port})")
}

/// Message `turbofig stop` prints once the daemon has actually gone away.
pub fn stopped_message() -> &'static str {
    "turbofig: stopped the daemon"
}

/// Message `turbofig stop` prints when no daemon was reachable to stop
/// (idempotent: this is success, not an error).
pub fn stop_nothing_running_message() -> &'static str {
    "turbofig: no daemon appears to be running"
}

/// Hint `turbofig stop` appends when the autostart plist is present:
/// launchd's `KeepAlive` will restart the daemon this command just stopped.
pub fn stop_autostart_restart_hint() -> &'static str {
    "turbofig: autostart is on, so launchd will restart it. Run `turbofig autostart off` to stop that."
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launchd::Launchctl;
    use serde_json::json;
    use std::cell::RefCell;
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// `run_autostart_on` reads the real process environment for
    /// `TURBOFIG_*` variables. Every test that touches one of those
    /// variables (directly, or indirectly by calling `run_autostart_on`)
    /// must hold this lock first, so two such tests never race on shared
    /// global state.
    static ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct FakeLaunchctl {
        calls: RefCell<Vec<String>>,
        /// Number of leading `bootstrap` calls that fail before one succeeds.
        bootstrap_failures_remaining: RefCell<u32>,
    }

    impl FakeLaunchctl {
        fn new() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                bootstrap_failures_remaining: RefCell::new(0),
            }
        }

        fn failing_bootstrap_times(n: u32) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                bootstrap_failures_remaining: RefCell::new(n),
            }
        }
    }

    impl Launchctl for FakeLaunchctl {
        fn bootout(&self, service_target: &str) {
            self.calls
                .borrow_mut()
                .push(format!("bootout {service_target}"));
        }
        fn bootstrap(&self, domain_target: &str, plist_path: &Path) -> io::Result<()> {
            self.calls.borrow_mut().push(format!(
                "bootstrap {domain_target} {}",
                plist_path.display()
            ));
            let mut remaining = self.bootstrap_failures_remaining.borrow_mut();
            if *remaining > 0 {
                *remaining -= 1;
                return Err(io::Error::other("fake bootstrap failure"));
            }
            Ok(())
        }
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir().join(format!("turbofig-cli-test-{label}-{nanos}"))
    }

    #[test]
    fn run_autostart_on_is_idempotent_across_two_runs() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = unique_temp_dir("home");
        let launch_agents_dir = unique_temp_dir("agents");
        let launchctl = FakeLaunchctl::new();
        let fake_exe = std::env::current_exe().expect("current_exe");

        let first = run_autostart_on(&launch_agents_dir, &launchctl, "501", &fake_exe, &home)
            .expect("first autostart on");
        let second = run_autostart_on(&launch_agents_dir, &launchctl, "501", &fake_exe, &home)
            .expect("second autostart on");

        assert_eq!(first, second);
        assert!(launch_agents_dir
            .join("eu.lukehawkins.turbofig.plist")
            .exists());
        // autostart on must never touch the home directory's own contents:
        // the daemon's own startup owns the token and plugin files.
        assert!(!home.join("token").exists());
        assert_eq!(
            *launchctl.calls.borrow(),
            vec![
                "bootout gui/501/eu.lukehawkins.turbofig".to_owned(),
                format!(
                    "bootstrap gui/501 {}",
                    launch_agents_dir
                        .join("eu.lukehawkins.turbofig.plist")
                        .display()
                ),
                "bootout gui/501/eu.lukehawkins.turbofig".to_owned(),
                format!(
                    "bootstrap gui/501 {}",
                    launch_agents_dir
                        .join("eu.lukehawkins.turbofig.plist")
                        .display()
                ),
            ]
        );

        std::fs::remove_dir_all(&home).ok();
        std::fs::remove_dir_all(&launch_agents_dir).ok();
    }

    #[test]
    fn run_autostart_on_carries_a_turbofig_env_var_into_the_plist() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = unique_temp_dir("home-env-carry");
        let launch_agents_dir = unique_temp_dir("agents-env-carry");
        let launchctl = FakeLaunchctl::new();
        let fake_exe = std::env::current_exe().expect("current_exe");

        // SAFETY: test-only env mutation; no other test in this module reads
        // TURBOFIG_MCP_PORT or TURBOFIG_LAUNCH_AGENTS_DIR.
        unsafe {
            std::env::set_var("TURBOFIG_MCP_PORT", "19999");
            std::env::set_var("TURBOFIG_LAUNCH_AGENTS_DIR", "/should/not/appear");
        }

        let outcome = run_autostart_on(&launch_agents_dir, &launchctl, "501", &fake_exe, &home);

        unsafe {
            std::env::remove_var("TURBOFIG_MCP_PORT");
            std::env::remove_var("TURBOFIG_LAUNCH_AGENTS_DIR");
        }

        let outcome = outcome.expect("autostart on");
        assert_eq!(
            outcome.carried_over_env,
            vec!["TURBOFIG_MCP_PORT".to_owned()]
        );

        let plist_text = std::fs::read_to_string(&outcome.plist_path).expect("read plist");
        assert!(plist_text.contains("<key>TURBOFIG_MCP_PORT</key>\n\t\t<string>19999</string>"));
        assert!(!plist_text.contains("TURBOFIG_LAUNCH_AGENTS_DIR"));

        std::fs::remove_dir_all(&home).ok();
        std::fs::remove_dir_all(&launch_agents_dir).ok();
    }

    #[test]
    fn run_autostart_on_retries_bootstrap_after_2_failures_with_no_real_wait() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = unique_temp_dir("home-bootstrap-retry");
        let launch_agents_dir = unique_temp_dir("agents-bootstrap-retry");
        let launchctl = FakeLaunchctl::failing_bootstrap_times(2);
        let fake_exe = std::env::current_exe().expect("current_exe");
        let sleeps: RefCell<Vec<Duration>> = RefCell::new(Vec::new());

        let outcome = run_autostart_on_with_sleep(
            &launch_agents_dir,
            &launchctl,
            "501",
            &fake_exe,
            &home,
            &|d| sleeps.borrow_mut().push(d),
        );

        assert!(
            outcome.is_ok(),
            "autostart on must succeed once bootstrap stops failing"
        );
        assert_eq!(
            sleeps.borrow().len(),
            2,
            "must sleep once per failed attempt before retrying"
        );
        assert_eq!(
            launchctl
                .calls
                .borrow()
                .iter()
                .filter(|c| c.starts_with("bootstrap"))
                .count(),
            3,
            "2 failures plus 1 success"
        );

        std::fs::remove_dir_all(&home).ok();
        std::fs::remove_dir_all(&launch_agents_dir).ok();
    }

    #[test]
    fn run_autostart_on_gives_up_after_the_max_bootstrap_attempts() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = unique_temp_dir("home-bootstrap-giveup");
        let launch_agents_dir = unique_temp_dir("agents-bootstrap-giveup");
        let launchctl = FakeLaunchctl::failing_bootstrap_times(BOOTSTRAP_MAX_ATTEMPTS);
        let fake_exe = std::env::current_exe().expect("current_exe");

        let outcome = run_autostart_on_with_sleep(
            &launch_agents_dir,
            &launchctl,
            "501",
            &fake_exe,
            &home,
            &|_| {},
        );

        assert!(outcome.is_err());

        std::fs::remove_dir_all(&home).ok();
        std::fs::remove_dir_all(&launch_agents_dir).ok();
    }

    #[test]
    fn carried_over_env_message_is_empty_with_nothing_carried() {
        assert_eq!(carried_over_env_message(&[]), "");
    }

    #[test]
    fn carried_over_env_message_names_the_carried_variables() {
        let msg = carried_over_env_message(&["TURBOFIG_MCP_PORT".to_owned()]);
        assert!(msg.contains("TURBOFIG_MCP_PORT"));
    }

    #[test]
    fn non_cellar_binary_warning_is_empty_for_a_homebrew_install() {
        assert_eq!(non_cellar_binary_warning(false), "");
    }

    #[test]
    fn non_cellar_binary_warning_warns_for_a_dev_checkout_binary() {
        let msg = non_cellar_binary_warning(true);
        assert!(msg.contains("not a Homebrew install"));
        assert!(
            msg.contains("brew install LukeHawkins/tap/turbofig"),
            "the formula lives only in the tap, not homebrew-core: {msg}"
        );
    }

    #[test]
    fn run_autostart_on_flags_a_dev_checkout_binary_as_outside_the_cellar() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = unique_temp_dir("home-cellar-flag");
        let launch_agents_dir = unique_temp_dir("agents-cellar-flag");
        let launchctl = FakeLaunchctl::new();
        // The test binary runs from target/{debug,release}, never a real
        // Homebrew Cellar, so this exercises the true branch for free.
        let fake_exe = std::env::current_exe().expect("current_exe");

        let outcome = run_autostart_on(&launch_agents_dir, &launchctl, "501", &fake_exe, &home)
            .expect("autostart on");

        assert!(outcome.binary_outside_homebrew_cellar);

        std::fs::remove_dir_all(&home).ok();
        std::fs::remove_dir_all(&launch_agents_dir).ok();
    }

    #[test]
    fn setup_steps_text_has_exactly_three_numbered_steps_and_one_optional_line() {
        let text = setup_steps_text(
            Path::new("/tmp/home/figma-plugin/manifest.json"),
            18846,
            false,
        );
        assert!(text.contains("1. In Figma Desktop"));
        assert!(text.contains("2. Run the turbofig plugin"));
        assert!(text.contains("3. Click the copy-prompt button"));
        assert!(text.contains("Optional, for MCP clients:"));
        assert!(text.contains("/tmp/home/figma-plugin/manifest.json"));
        assert_eq!(text.matches("claude mcp add").count(), 1);
    }

    #[test]
    fn setup_steps_text_names_a_custom_mcp_port() {
        let text = setup_steps_text(
            Path::new("/tmp/home/figma-plugin/manifest.json"),
            19999,
            false,
        );
        assert!(text.contains("http://127.0.0.1:19999/mcp"));
        assert!(!text.contains("18846"));
    }

    #[test]
    fn setup_steps_text_adds_a_clipboard_hint_only_when_copied() {
        let without = setup_steps_text(Path::new("/tmp/x/manifest.json"), 18846, false);
        assert!(!without.contains("Cmd+Shift+G"));

        let with = setup_steps_text(Path::new("/tmp/x/manifest.json"), 18846, true);
        assert!(with.contains("Cmd+Shift+G"));
        // The hint must never change the count of numbered steps.
        assert!(with.contains("1. In Figma Desktop"));
        assert!(with.contains("2. Run the turbofig plugin"));
        assert!(with.contains("3. Click the copy-prompt button"));
    }

    #[test]
    fn run_autostart_off_removes_the_plist() {
        let launch_agents_dir = unique_temp_dir("agents-autostart-off");
        std::fs::create_dir_all(&launch_agents_dir).expect("mkdir agents");
        std::fs::write(
            launch_agents_dir.join("eu.lukehawkins.turbofig.plist"),
            "placeholder",
        )
        .expect("write plist");
        let launchctl = FakeLaunchctl::new();

        let plist_path =
            run_autostart_off(&launch_agents_dir, &launchctl, "501").expect("autostart off");

        assert!(!plist_path.exists());
        assert_eq!(
            *launchctl.calls.borrow(),
            vec!["bootout gui/501/eu.lukehawkins.turbofig".to_owned()]
        );

        std::fs::remove_dir_all(&launch_agents_dir).ok();
    }

    #[test]
    fn run_autostart_off_is_a_no_op_when_no_plist_exists() {
        let launch_agents_dir = unique_temp_dir("agents-autostart-off-missing");
        let launchctl = FakeLaunchctl::new();

        let outcome = run_autostart_off(&launch_agents_dir, &launchctl, "501");
        assert!(outcome.is_ok());
    }

    #[test]
    fn autostart_plist_exists_reflects_the_plist_file() {
        let launch_agents_dir = unique_temp_dir("agents-plist-exists");
        assert!(!autostart_plist_exists(&launch_agents_dir));

        std::fs::create_dir_all(&launch_agents_dir).expect("mkdir agents");
        std::fs::write(
            launch_agents_dir.join("eu.lukehawkins.turbofig.plist"),
            "placeholder",
        )
        .expect("write plist");
        assert!(autostart_plist_exists(&launch_agents_dir));

        std::fs::remove_dir_all(&launch_agents_dir).ok();
    }

    #[test]
    fn run_uninstall_without_purge_keeps_home_and_removes_the_plist() {
        let home = unique_temp_dir("home-uninstall");
        let launch_agents_dir = unique_temp_dir("agents-uninstall");
        std::fs::create_dir_all(&home).expect("mkdir home");
        std::fs::create_dir_all(&launch_agents_dir).expect("mkdir agents");
        std::fs::write(home.join("token"), "tok").expect("write token");
        std::fs::write(
            launch_agents_dir.join("eu.lukehawkins.turbofig.plist"),
            "placeholder",
        )
        .expect("write plist");
        let launchctl = FakeLaunchctl::new();

        let outcome =
            run_uninstall(&home, &launch_agents_dir, &launchctl, "501", false).expect("uninstall");

        assert!(!outcome.purged);
        assert!(home.exists(), "home must be kept without --purge");
        assert!(!launch_agents_dir
            .join("eu.lukehawkins.turbofig.plist")
            .exists());

        std::fs::remove_dir_all(&home).ok();
        std::fs::remove_dir_all(&launch_agents_dir).ok();
    }

    #[test]
    fn run_uninstall_with_purge_removes_home() {
        let home = unique_temp_dir("home-purge");
        let launch_agents_dir = unique_temp_dir("agents-purge");
        std::fs::create_dir_all(&home).expect("mkdir home");
        std::fs::create_dir_all(&launch_agents_dir).expect("mkdir agents");
        let launchctl = FakeLaunchctl::new();

        let outcome =
            run_uninstall(&home, &launch_agents_dir, &launchctl, "501", true).expect("uninstall");

        assert!(outcome.purged);
        assert!(!home.exists());

        std::fs::remove_dir_all(&launch_agents_dir).ok();
    }

    #[test]
    fn run_uninstall_with_purge_removes_only_known_entries_and_keeps_an_unrelated_file() {
        let home = unique_temp_dir("home-purge-mixed");
        let launch_agents_dir = unique_temp_dir("agents-purge-mixed");
        std::fs::create_dir_all(&home).expect("mkdir home");
        std::fs::create_dir_all(&launch_agents_dir).expect("mkdir agents");
        std::fs::write(home.join("token"), "deadbeef").expect("write token");
        std::fs::write(home.join("daemon.log"), "log").expect("write log");
        std::fs::create_dir_all(home.join("figma-plugin/dist")).expect("mkdir figma-plugin");
        std::fs::create_dir_all(home.join("inbox")).expect("mkdir inbox");
        std::fs::create_dir_all(home.join("outbox")).expect("mkdir outbox");
        std::fs::write(home.join("not-turbofigs.txt"), "keep me").expect("write unrelated file");
        let launchctl = FakeLaunchctl::new();

        let outcome =
            run_uninstall(&home, &launch_agents_dir, &launchctl, "501", true).expect("uninstall");

        assert!(outcome.purged);
        assert!(
            home.exists(),
            "home must survive purge when it still holds an unrelated file"
        );
        assert!(!home.join("token").exists());
        assert!(!home.join("daemon.log").exists());
        assert!(!home.join("figma-plugin").exists());
        assert!(!home.join("inbox").exists());
        assert!(!home.join("outbox").exists());
        assert!(
            home.join("not-turbofigs.txt").exists(),
            "an unrelated file in home must survive --purge"
        );

        std::fs::remove_dir_all(&home).ok();
        std::fs::remove_dir_all(&launch_agents_dir).ok();
    }

    #[test]
    fn run_uninstall_is_a_no_op_when_nothing_exists_yet() {
        let home = unique_temp_dir("home-missing");
        let launch_agents_dir = unique_temp_dir("agents-missing");
        let launchctl = FakeLaunchctl::new();

        let outcome =
            run_uninstall(&home, &launch_agents_dir, &launchctl, "501", true).expect("uninstall");
        assert!(outcome.purged);
    }

    #[test]
    fn uninstall_kept_home_message_names_the_directory() {
        let msg = uninstall_kept_home_message(Path::new("/Users/dev/.turbofig"));
        assert!(msg.contains("/Users/dev/.turbofig"));
        assert!(msg.contains("--purge"));
    }

    #[test]
    fn format_health_reports_version_uptime_and_no_files() {
        let body = json!({"version": "0.3.0", "uptimeSeconds": 42, "connectedFiles": []});
        let text = format_health(&body);
        assert!(text.contains("version 0.3.0"));
        assert!(text.contains("up 42s"));
        assert!(text.contains("No Figma file connected"));
    }

    #[test]
    fn format_health_lists_connected_files_with_a_version_warning() {
        let body = json!({
            "version": "0.3.0",
            "uptimeSeconds": 10,
            "connectedFiles": [
                {"fileKey": "fk1", "name": "Design", "pluginVersion": "0.2.0", "warning": "reopen the turbofig plugin in Figma"}
            ]
        });
        let text = format_health(&body);
        assert!(text.contains("Design"));
        assert!(text.contains("fk1"));
        assert!(text.contains("plugin 0.2.0"));
        assert!(text.contains("reopen the turbofig plugin in Figma"));
    }

    #[test]
    fn status_unreachable_message_names_the_port_and_suggests_start() {
        let msg = status_unreachable_message(18846);
        assert!(msg.contains("18846"));
        assert!(msg.contains("turbofig start"));
    }

    #[test]
    fn already_running_message_names_the_version_and_port() {
        let msg = already_running_message("1.2.3", 18846);
        assert!(msg.contains("already running"));
        assert!(msg.contains("1.2.3"));
        assert!(msg.contains("18846"));
    }

    #[test]
    fn started_message_names_the_version_and_both_ports() {
        let msg = started_message("1.2.3", 18846, 18847);
        assert!(msg.contains("1.2.3"));
        assert!(msg.contains("18846"));
        assert!(msg.contains("18847"));
    }

    #[test]
    fn stop_autostart_restart_hint_names_the_off_command() {
        assert!(stop_autostart_restart_hint().contains("turbofig autostart off"));
    }
}
