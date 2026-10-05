//! The `turbofig` CLI: argument parsing (`clap`) and the pure/testable
//! halves of `setup`, `uninstall`, and `status`. `main.rs` wires these to
//! the real filesystem, `launchctl`, and HTTP client; tests use the seams
//! here (`Launchctl`, explicit `home`/`launch_agents_dir`/`uid` arguments)
//! instead.

use crate::launchd::{
    carry_over_turbofig_env, domain_target, plist_contents, plist_file_name, service_target,
    stable_binary_path, Launchctl,
};
use crate::plugin_files::write_plugin_files;
use crate::token::ensure_token;
use clap::{Parser, Subcommand};
use std::io;
use std::path::{Path, PathBuf};

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
    /// Install the pairing token, the Figma plugin files, and the launchd
    /// service, then load it so the daemon starts now and on every login.
    Setup,
    /// Unload the launchd service and remove its plist.
    Uninstall {
        /// Also delete the turbofig entries in the home directory (token,
        /// plugin files, bridge inbox/outbox, log), then the directory
        /// itself if left empty. Without this flag, the home directory is
        /// kept.
        #[arg(long)]
        purge: bool,
    },
    /// Query the running daemon's `/health` endpoint.
    Status,
}

/// Result of a successful `run_setup` call.
#[derive(Debug, PartialEq, Eq)]
pub struct SetupOutcome {
    pub manifest_path: PathBuf,
    pub plist_path: PathBuf,
    /// Names of the `TURBOFIG_*` environment variables that were set at
    /// `setup` time and carried into the plist's `EnvironmentVariables`, in
    /// the order written. Empty when none were set.
    pub carried_over_env: Vec<String>,
}

/// Runs the full, idempotent setup sequence: ensure the pairing token,
/// write the embedded plugin out to `<home>/figma-plugin/`, write the
/// launchd plist, then `bootout` (ignoring "not loaded") and `bootstrap` it.
///
/// Every step is idempotent: `ensure_token` never overwrites an existing
/// token, `write_plugin_files` always overwrites with the same embedded
/// content plus the current token, the plist is byte-identical across runs
/// given the same binary path and home, and a `bootout` then `bootstrap`
/// pair gives the same end state whether or not the service was already
/// loaded. Running `turbofig setup` twice in a row is always safe.
pub fn run_setup(
    home: &Path,
    launch_agents_dir: &Path,
    launchctl: &dyn Launchctl,
    uid: &str,
    current_exe: &Path,
) -> io::Result<SetupOutcome> {
    let token = ensure_token(home)?;
    let manifest_path = write_plugin_files(home, &token)?;

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
    launchctl.bootstrap(&domain_target(uid), &plist_path)?;

    Ok(SetupOutcome {
        manifest_path,
        plist_path,
        carried_over_env: extra_env.into_iter().map(|(key, _)| key).collect(),
    })
}

/// Message printed after `setup` when 1 or more `TURBOFIG_*` variables were
/// carried from the `setup` process's own environment into the plist. Empty
/// when `carried_over_env` is empty.
pub fn carried_over_env_message(carried_over_env: &[String]) -> String {
    if carried_over_env.is_empty() {
        return String::new();
    }
    format!(
        "turbofig: carried {} into the launchd service.\n",
        carried_over_env.join(", ")
    )
}

/// The exact 3 numbered steps plus the 1 optional MCP line `setup` prints.
pub fn setup_steps_text(manifest_path: &Path) -> String {
    format!(
        "1. In Figma Desktop: Plugins > Development > Import plugin from manifest, then pick {}.\n\
2. Run the turbofig plugin in a file.\n\
3. Click the copy-prompt button in the plugin and paste it into your AI agent.\n\
\n\
Optional, for MCP clients: claude mcp add --transport http turbofig http://127.0.0.1:18846/mcp\n",
        manifest_path.display()
    )
}

/// Result of a successful `run_uninstall` call.
#[derive(Debug, PartialEq, Eq)]
pub struct UninstallOutcome {
    pub home: PathBuf,
    pub purged: bool,
}

/// Unloads the launchd service and removes its plist. With `purge`, also
/// deletes the known turbofig entries inside `home` (see `purge_home`). A
/// missing plist or a missing `home` is not an error: uninstall is
/// idempotent too.
pub fn run_uninstall(
    home: &Path,
    launch_agents_dir: &Path,
    launchctl: &dyn Launchctl,
    uid: &str,
    purge: bool,
) -> io::Result<UninstallOutcome> {
    launchctl.bootout(&service_target(uid));

    let plist_path = launch_agents_dir.join(plist_file_name());
    ignore_not_found(std::fs::remove_file(&plist_path))?;

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
        "turbofig: could not reach the daemon on port {mcp_port}.\nRun `turbofig setup` to install and start it."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launchd::Launchctl;
    use serde_json::json;
    use std::cell::RefCell;
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// `run_setup` reads the real process environment for `TURBOFIG_*`
    /// variables. Every test that touches one of those variables (directly,
    /// or indirectly by calling `run_setup`) must hold this lock first, so
    /// two such tests never race on shared global state.
    static ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct FakeLaunchctl {
        calls: RefCell<Vec<String>>,
    }

    impl FakeLaunchctl {
        fn new() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
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
    fn run_setup_is_idempotent_across_two_runs() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = unique_temp_dir("home");
        let launch_agents_dir = unique_temp_dir("agents");
        let launchctl = FakeLaunchctl::new();
        let fake_exe = std::env::current_exe().expect("current_exe");

        let first = run_setup(&home, &launch_agents_dir, &launchctl, "501", &fake_exe)
            .expect("first setup");
        let second = run_setup(&home, &launch_agents_dir, &launchctl, "501", &fake_exe)
            .expect("second setup");

        assert_eq!(first, second);
        assert!(home.join("token").exists());
        assert!(home.join("figma-plugin/manifest.json").exists());
        assert!(launch_agents_dir
            .join("eu.lukehawkins.turbofig.plist")
            .exists());
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
    fn run_setup_carries_a_turbofig_env_var_into_the_plist() {
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

        let outcome = run_setup(&home, &launch_agents_dir, &launchctl, "501", &fake_exe);

        unsafe {
            std::env::remove_var("TURBOFIG_MCP_PORT");
            std::env::remove_var("TURBOFIG_LAUNCH_AGENTS_DIR");
        }

        let outcome = outcome.expect("setup");
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
    fn carried_over_env_message_is_empty_with_nothing_carried() {
        assert_eq!(carried_over_env_message(&[]), "");
    }

    #[test]
    fn carried_over_env_message_names_the_carried_variables() {
        let msg = carried_over_env_message(&["TURBOFIG_MCP_PORT".to_owned()]);
        assert!(msg.contains("TURBOFIG_MCP_PORT"));
    }

    #[test]
    fn setup_steps_text_has_exactly_three_numbered_steps_and_one_optional_line() {
        let text = setup_steps_text(Path::new("/tmp/home/figma-plugin/manifest.json"));
        assert!(text.contains("1. In Figma Desktop"));
        assert!(text.contains("2. Run the turbofig plugin"));
        assert!(text.contains("3. Click the copy-prompt button"));
        assert!(text.contains("Optional, for MCP clients:"));
        assert!(text.contains("/tmp/home/figma-plugin/manifest.json"));
        assert_eq!(text.matches("claude mcp add").count(), 1);
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
    fn status_unreachable_message_names_the_port_and_suggests_setup() {
        let msg = status_unreachable_message(18846);
        assert!(msg.contains("18846"));
        assert!(msg.contains("turbofig setup"));
    }
}
