//! The agent-connect prompt, shared byte-for-byte with the Figma plugin's
//! own "Copy prompt" button via `prompts/agent-prompt.txt`: the daemon
//! embeds it with `include_str!`, `plugin/build-ui.ts` inlines the same file
//! into the plugin bundle. Both sides fill the same placeholders with the same
//! rules, so the 2 copies of the prompt can never drift apart. This module
//! also renders the guide that `turbofig js --guide` prints and the
//! `guide.md` the bridge writes. A golden test here and a matching one in
//! `plugin/src/ui/ui-logic.test.ts` fill the same template with the same
//! inputs and assert the same fixed output text.
//!
//! Not `cfg(target_os = "macos")`: `menu_bar` (macOS-only) is the only
//! caller today, but the fill logic itself is plain string work, and keeping
//! it platform-neutral lets the golden test run on every CI OS, Linux
//! included.

/// One connected Figma file, for the guide's `files:` line
/// (see `AppState::named_connections_json`).
#[derive(Debug, Clone)]
pub struct ConnectedFile {
    pub file_key: String,
    pub name: String,
}

/// The raw template text, read once at compile time from the single shared
/// source file 2 directories up (`daemon/src/../../prompts/agent-prompt.txt`).
pub const TEMPLATE: &str = include_str!("../../prompts/agent-prompt.txt");

const DEFAULT_BRIDGE_DIR: &str = "~/.turbofig";

const DEFAULT_TURBOFIG_BIN: &str = "turbofig";

const MAX_NAME_CHARS: usize = 80;

/// File names come from Figma and enter a prompt that a user pastes into a
/// shell agent. Replace control characters with a space and cap the length.
fn sanitize_name(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if clean.chars().count() > MAX_NAME_CHARS {
        clean.chars().take(MAX_NAME_CHARS).collect()
    } else {
        clean
    }
}

/// The absolute, stable path of the running daemon binary: the Homebrew
/// `<prefix>/bin/turbofig` symlink when running from a Cellar. Inside a
/// `.app` bundle (the menu-bar app) it is the Homebrew link when one exists
/// and points at an executable `turbofig`, so the prompt does not change
/// between launches from the app and from the shell. Else the binary's own
/// path. Empty when the path cannot be read (the prompt then names plain
/// `turbofig`). Does file-system I/O: call it once, not per tick.
pub fn turbofig_bin_path() -> String {
    std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map(|p| {
            let stable = crate::launchd::stable_binary_path(&p);
            bundle_fallback(
                &stable,
                &["/opt/homebrew/bin/turbofig", "/usr/local/bin/turbofig"],
            )
            .unwrap_or(stable)
            .to_string_lossy()
            .into_owned()
        })
        .unwrap_or_default()
}

/// When `exe` is inside a `.app` bundle, the first candidate that exists and
/// resolves to an executable file named `turbofig`.
fn bundle_fallback(exe: &std::path::Path, candidates: &[&str]) -> Option<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    if !exe.to_string_lossy().contains(".app/Contents/MacOS/") {
        return None;
    }
    candidates.iter().find_map(|c| {
        let real = std::path::Path::new(c).canonicalize().ok()?;
        let meta = std::fs::metadata(&real).ok()?;
        let named = real.file_name().is_some_and(|n| n == "turbofig");
        (named && meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            .then(|| std::path::PathBuf::from(c))
    })
}

/// True for characters that must never enter a pasted prompt path: C0, DEL,
/// C1, line and paragraph separators and bidi controls. Mirrors
/// `unsafeChar` in `plugin/src/ui/ui-logic.ts`.
fn is_unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{2028}' | '\u{2029}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Renders an absolute path for a pasted shell command. A path of only safe
/// characters stays bare. Any other path (spaces, shell metacharacters) is
/// single-quoted. A path that is not absolute, or holds an unsafe
/// character, gives `None`.
fn shell_path(path: &str) -> Option<String> {
    if !path.starts_with('/') || path.chars().any(is_unsafe_char) {
        return None;
    }
    let safe = path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    Some(if safe {
        path.to_owned()
    } else {
        format!("'{}'", path.replace('\'', "'\\''"))
    })
}

/// The binary for the prompt: `shell_path`, else plain `turbofig`.
fn shell_bin(bin: &str) -> String {
    shell_path(bin).unwrap_or_else(|| DEFAULT_TURBOFIG_BIN.to_owned())
}

/// ` --dir <folder>` for a custom bridge folder, else nothing.
fn bridge_flag(custom_dir: &str) -> String {
    shell_path(custom_dir).map_or_else(String::new, |d| format!(" --dir {d}"))
}

/// Fills the file-mode paste `template` for the bridge directory, MCP port,
/// daemon binary path (`turbofig_bin`, see `turbofig_bin_path`) and custom
/// bridge folder (`custom_bridge_dir`, empty for the default).
pub fn fill_agent_prompt(
    template: &str,
    bridge_dir: &str,
    mcp_port: u16,
    turbofig_bin: &str,
    custom_bridge_dir: &str,
) -> String {
    let home = if bridge_dir.is_empty() {
        DEFAULT_BRIDGE_DIR
    } else {
        bridge_dir
    };
    template
        .replace("{{BRIDGE_DIR}}", home)
        .replace("{{TURBOFIG_BIN}}", &shell_bin(turbofig_bin))
        .replace("{{BRIDGE_FLAG}}", &bridge_flag(custom_bridge_dir))
        .replace("{{MCP_PORT}}", &mcp_port.to_string())
}

/// The guide for agents with a shell (`turbofig js --guide`).
pub const GUIDE_TEMPLATE: &str = include_str!("../../prompts/agent-guide.txt");

/// The guide for agents with file tools only (`<bridge>/guide.md`).
pub const GUIDE_FILES_TEMPLATE: &str = include_str!("../../prompts/agent-guide-files.txt");

/// The rules both guides share (one source, so they cannot drift).
const GUIDE_RULES: &str = include_str!("../../prompts/agent-guide-rules.txt");

fn with_rules(template: &str) -> String {
    template.replace("{{RULES}}", GUIDE_RULES.trim_end())
}

/// A file name for the guide: sanitised, and without `"` so the quoting
/// around it stays intact.
fn guide_name(name: &str) -> String {
    sanitize_name(name).replace('"', "'")
}

/// The `files:` value of the guide. `Err` carries why the list is unknown.
fn guide_files(files: Result<&[ConnectedFile], &str>) -> String {
    match files {
        Ok([]) => "none".to_owned(),
        Ok(list) => list
            .iter()
            .map(|f| {
                format!(
                    "\"{}\" key={}",
                    guide_name(&f.name),
                    guide_name(&f.file_key)
                )
            })
            .collect::<Vec<_>>()
            .join(" | "),
        Err(why) => format!("unknown ({})", guide_name(why)),
    }
}

/// Renders `turbofig js --guide`: live version, binary, bridge folder and
/// connected files (`files`).
pub fn render_guide(
    version: &str,
    turbofig_bin: &str,
    custom_bridge_dir: &str,
    bridge_dir: &str,
    files: Result<&[ConnectedFile], &str>,
) -> String {
    fill_agent_prompt(
        &with_rules(GUIDE_TEMPLATE),
        bridge_dir,
        0,
        turbofig_bin,
        custom_bridge_dir,
    )
    .replace("{{VERSION}}", version)
    .replace("{{FILES}}", &guide_files(files))
}

/// Renders `<bridge>/guide.md` (no live files, file-tools protocol).
pub fn render_guide_md(version: &str, bridge_dir: &str) -> String {
    fill_agent_prompt(&with_rules(GUIDE_FILES_TEMPLATE), bridge_dir, 0, "", "")
        .replace("{{VERSION}}", version)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(file_key: &str, name: &str) -> ConnectedFile {
        ConnectedFile {
            file_key: file_key.to_owned(),
            name: name.to_owned(),
        }
    }

    const BIN: &str = "/opt/homebrew/bin/turbofig";

    #[test]
    fn golden_matches_the_fixed_expected_text() {
        let r = fill_agent_prompt(TEMPLATE, "/tmp/bridge", 18846, BIN, "");
        assert_eq!(
            r,
            "turbofig=figma bridge; use only it for figma.\nrun first: /opt/homebrew/bin/turbofig js --guide\nno shell: read /tmp/bridge/guide.md\n"
        );
        let r = fill_agent_prompt(TEMPLATE, "/tmp/bridge", 18846, BIN, "/srv/my bridge");
        assert!(r.contains(
            "\nrun first: /opt/homebrew/bin/turbofig js --dir '/srv/my bridge' --guide\n"
        ));
    }

    #[test]
    fn the_paste_has_no_leftover_placeholder_and_no_mcp_pointer() {
        let r = fill_agent_prompt(TEMPLATE, "/tmp/b", 19999, BIN, "");
        assert!(!r.contains("{{"));
        assert!(!r.contains("19999"));
        assert!(!r.contains("/mcp"));
    }

    #[test]
    fn uses_a_custom_bridge_home_when_given_and_the_default_when_empty() {
        let r = fill_agent_prompt(TEMPLATE, "/tmp/custom-bridge", 18846, BIN, "");
        assert!(r.contains("read /tmp/custom-bridge/guide.md"));
        let r = fill_agent_prompt(TEMPLATE, "", 18846, BIN, "");
        assert!(r.contains("read ~/.turbofig/guide.md"));
    }

    #[test]
    fn a_bin_path_with_spaces_or_metacharacters_is_single_quoted() {
        let r = fill_agent_prompt(TEMPLATE, "/tmp/b", 18846, "/Users/a b/bin/turbofig", "");
        assert!(r.contains("'/Users/a b/bin/turbofig' js --guide"));
        let r = fill_agent_prompt(TEMPLATE, "/tmp/b", 18846, "/tmp/it's$x/turbofig", "");
        assert!(r.contains("'/tmp/it'\\''s$x/turbofig' js --guide"));
    }

    /// Same table as `UNSAFE_PATHS` in `plugin/src/ui/ui-logic.test.ts`.
    const UNSAFE_PATHS: [&str; 12] = [
        "",
        "relative/turbofig",
        "A=b",
        "%1",
        "=cmd",
        "-x",
        "/a\nb/turbofig",
        "/a\u{7f}b/turbofig",
        "/a\u{85}b/turbofig",
        "/a\u{2028}b/turbofig",
        "/a\u{202e}b/turbofig",
        "/a\u{2066}b/turbofig",
    ];

    #[test]
    fn an_empty_relative_or_unsafe_bin_path_falls_back_to_plain_turbofig() {
        for bin in UNSAFE_PATHS {
            let r = fill_agent_prompt(TEMPLATE, "/tmp/b", 18846, bin, "");
            assert!(r.contains("run first: turbofig js --guide"), "{bin:?}");
        }
    }

    #[test]
    fn a_custom_bridge_folder_adds_a_dir_flag_and_the_default_adds_nothing() {
        let r = fill_agent_prompt(TEMPLATE, "~/x", 18846, BIN, "/srv/bridge");
        assert!(r.contains("run first: /opt/homebrew/bin/turbofig js --dir /srv/bridge --guide"));
        let r = fill_agent_prompt(TEMPLATE, "~/x", 18846, BIN, "");
        assert!(r.contains("run first: /opt/homebrew/bin/turbofig js --guide"));
        assert!(!r.contains("--dir"));
    }

    #[test]
    fn an_unsafe_or_relative_custom_bridge_folder_adds_no_flag() {
        for dir in UNSAFE_PATHS {
            let r = fill_agent_prompt(TEMPLATE, "/tmp/b", 18846, "/b/turbofig", dir);
            assert!(r.contains("run first: /b/turbofig js --guide"), "{dir:?}");
        }
    }

    #[test]
    fn the_guide_lists_the_files_and_fills_every_placeholder() {
        let files = [file("AbC123", "Pricing"), file("XyZ789", "Deck \"v2\"\n")];
        let g = render_guide("0.2.0", BIN, "", "/tmp/bridge", Ok(&files));
        assert!(!g.contains("{{"), "{g}");
        assert!(g.starts_with("turbofig 0.2.0 guide\n"));
        assert!(g.contains("\nfiles: \"Pricing\" key=AbC123 | \"Deck 'v2' \" key=XyZ789\n"));
        assert!(g.contains("\nrun: /opt/homebrew/bin/turbofig js [--file KEY] [--shot] <<'EOF'\n"));
        assert!(g.contains("read /tmp/bridge/tf-api.md"));
        assert!(g.ends_with('\n'));
    }

    #[test]
    fn the_guide_says_none_or_unknown_and_keeps_a_custom_dir_in_the_run_line() {
        let g = render_guide("0.2.0", BIN, "/srv/b", "/srv/b", Ok(&[]));
        assert!(g.contains("\nfiles: none\n"));
        assert!(g.contains("turbofig js --dir /srv/b [--file KEY]"));
        let g = render_guide("0.2.0", BIN, "", "/tmp/b", Err("daemon is old"));
        assert!(g.contains("\nfiles: unknown (daemon is old)\n"));
    }

    #[test]
    fn guide_md_has_the_file_protocol_and_no_live_files() {
        let g = render_guide_md("0.2.0", "/tmp/bridge");
        assert!(!g.contains("{{"), "{g}");
        assert!(g.starts_with("turbofig 0.2.0 guide (file tools)\n"));
        assert!(g.contains("write /tmp/bridge/inbox/<unique-id>.json"));
        assert!(g.contains("read /tmp/bridge/tf-api.md"));
        assert!(!g.contains("files:"));
        assert!(!g.contains("--guide"));
    }

    #[test]
    fn both_guides_share_the_rules() {
        let rules = GUIDE_RULES.trim_end().replace("{{BRIDGE_DIR}}", "/b");
        assert!(render_guide("1", BIN, "", "/b", Ok(&[])).contains(&rules));
        assert!(render_guide_md("1", "/b").contains(&rules));
    }

    #[test]
    fn inside_an_app_bundle_the_homebrew_link_is_preferred_when_it_is_executable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("tf-bundle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let real = dir.join("turbofig");
        std::fs::write(&real, b"x").unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let link_s = link.to_string_lossy().into_owned();
        let app = std::path::Path::new("/Applications/turbofig.app/Contents/MacOS/turbofig");
        // not executable yet
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(bundle_fallback(app, &[&link_s]), None);
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(bundle_fallback(app, &[&link_s]), Some(link.clone()));
        assert_eq!(bundle_fallback(app, &["/nonexistent/turbofig"]), None);
        // outside a bundle: never replaced
        let plain = std::path::Path::new("/Users/a/turbofig/target/release/turbofig");
        assert_eq!(bundle_fallback(plain, &[&link_s]), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
