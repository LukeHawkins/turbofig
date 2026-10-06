# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project has not reached a tagged release yet; all work so far is
recorded under Unreleased.

## [Unreleased] - 0.1.0

### Added

- `turbofig` CLI (`clap`): `turbofig` with no subcommand, on its first run,
  installs the pairing token, writes the embedded Figma plugin to
  `<home>/figma-plugin/`, starts the daemon in the background, and prints
  the 3 connect steps; a later run prints a 3-line status instead.
  `turbofig mcp` runs a stdio MCP server that forwards tool calls onto the
  daemon, starting it first if it is not reachable. `turbofig start` and
  `turbofig stop` start and stop the background daemon directly.
  `turbofig serve` runs the daemon in the foreground, for development or for
  the optional launchd autostart service. `turbofig autostart on|off` turns
  that launchd service on or off (writes/removes
  `~/Library/LaunchAgents/eu.lukehawkins.turbofig.plist`, `RunAtLoad` and
  `KeepAlive`). `turbofig uninstall [--purge]` turns autostart off and
  removes the plist, keeping `<home>` unless `--purge` is given. `turbofig
  status` queries the running daemon's `/health` endpoint and prints a
  readable report. `--version` and `--help` come from `clap`. There is no
  `turbofig setup` command.
- `GET /health`: daemon version and uptime always; with a valid pairing
  token, also the connected files (name, key, plugin version) and the
  daemon's `pid`. Same Origin and `Host` checks as `/mcp`. Never includes
  the pairing token itself.
- `POST /job` and `POST /mcp` require `Authorization: Bearer <pairing
  token>`, like `POST /control`. Another macOS account on the same Mac can
  no longer drive Figma through the HTTP port. The file-bridge stays
  tokenless, protected instead by its directory mode (`0700`).
- `POST /control` replies `202` at once and drains and exits in the
  background; a caller polls `/health` until it stops answering (up to 65s
  for `turbofig stop`, autostart on's pre-stop, and the version handoff)
  instead of holding the connection open for the whole drain.
- `turbofig stop`, with a missing or changed token file while a daemon still
  answers `/health`, now exits 1 and names the fallback
  (`pkill -f 'turbofig serve'`) instead of reporting the same success as
  "nothing is running".
- Autostart's `KeepAlive` is `{SuccessfulExit: false}`, not plain `true`: a
  clean `turbofig stop` (exit 0) leaves the daemon stopped until the next
  login; a crash still restarts it. The supervised upgrade restart and a
  supervised `/control restart` exit 75, so `KeepAlive` restarts those with
  the new binary.
- CLI calls to `/health` and `/control` (status, start, stop, the version
  handoff) time out after 2s per request.
- Automatic updates after `brew upgrade`: on startup, a daemon with an
  existing `<home>/figma-plugin/` refreshes it if the on-disk copy is stale.
  Under launchd supervision (`TURBOFIG_SUPERVISED=1`, set by the plist
  `turbofig autostart on` writes), the daemon polls every 30s for a Homebrew
  upgrade (the stable binary path resolving to a new Cellar version), stops
  accepting new jobs, waits up to 60s for in-flight jobs to finish, then
  exits cleanly so launchd starts the new binary. Without autostart, the
  next `turbofig mcp` start sees an older daemon, waits for its jobs to
  finish, then restarts it on the new version.
- The Figma plugin reports its own version in `FILE_INFO`; `turbofig_status`
  and `/health` flag a mismatch against the daemon's version with
  "reopen the turbofig plugin in Figma".
- `daemon/build.rs` normalizes the pairing-token slot in the embedded
  `dist/ui.html` back to its placeholder before embedding, even when a
  contributor's local build injected a real token from their own
  `~/.turbofig/token`, so the embedded daemon binary never carries a real
  token baked in.
- Rust daemon (`daemon/`) running three servers in one process on three
  `tokio::spawn` tasks: an HTTP MCP endpoint (`rmcp`, streamable-http,
  `legacy_session_mode`, stateful `mcp-session-id`), a WebSocket server for
  the Figma plugin (one connection per open file), and a file-bridge that
  watches `~/.turbofig/inbox` and writes `outbox`.
- Locked four-tool surface: `turbofig_execute`, `turbofig_get_selection`,
  `turbofig_screenshot`, `turbofig_status`. All capability flows through
  `execute`.
- Multi-file routing by `fileKey`: a `conn_id`-keyed connection registry
  holds several open files at once, and each call routes to the right file
  by explicit `fileKey`, by session-to-file pairing, or by the sole
  connected plugin.
- Figma plugin (`plugin/`): a thin UI iframe holding the WebSocket and an
  infinite-backoff reconnect, and a main thread dispatching Figma API calls.
  The panel shows connection status, the active file, and a copyable
  `fileKey`, matches the live Figma theme, and copies a ready-to-paste
  Claude Code connect prompt.
- Helper library (`helpers/`): a compact `tf.*` craft namespace injected
  into the eval context (auto-layout, decks, components, variables,
  performance rules).
- File-bridge transport: a locked-down client drives the daemon with file
  writes and reads only, no curl and no MCP client needed.
- Self-describing HTTP port: a plain GET on `/` or any non-`/mcp` path
  returns a help payload naming the tools, the file-bridge protocol, and the
  ports, so a client told only the port can bootstrap without the repo.
- Origin validation on both ports: the HTTP MCP port rejects any request
  carrying an `Origin` header with 403; the WebSocket port accepts only a
  null or missing `Origin`. The file-bridge directories are set to mode
  `0700` on every daemon start.
- Always-on install via launchd (`install/install-macos.sh`), with
  `KeepAlive` restart on crash.
- Design-worker skill (`skills/design.md`): brief to plan to parallel
  builder subagents to a QA critic subagent to refine, with checkpoint and
  resume for long jobs.
- Benchmark harness (`bench/`) measuring token and wall-time cost.
- macOS menu-bar app: `Turbofig.app` is assembled on the user's own Mac
  (ad-hoc signed, no quarantine flag, so no "unidentified developer"
  prompt), installed/refreshed and opened automatically by the bare
  `turbofig` command. A tray icon (template image, 2 states: a plugin
  connected, or waiting/unreachable) shows a menu: Copy Agent Prompt, Copy
  Plugin Manifest Path, Show Plugin in Finder (`open -R`, for a Figma file
  picker that cannot browse into the hidden `~/.turbofig/figma-plugin/`),
  Open Figma, About Turbofig…, Start at Login, Open Log, Quit Turbofig. A
  background poller refreshes the tray and the About window every 2s from
  `/health`.
- About window: a native window hosting 1 embedded webview page (no remote
  URLs, no navigation away from it), reusing the plugin panel's visual
  style and following light/dark mode. Shows live status chips, a "How to
  use" tab (import the plugin, including the new Show in Finder button;
  run it; copy the agent prompt) and a "Claude Code / MCP" tab. Opens
  automatically on first use, from the menu, or from a second launch
  signalling the first over a local Unix socket.
- Start at Login: `turbofig autostart on` now installs a LaunchAgent for
  the app itself by default (`--headless` keeps the daemon-only one); the
  tray menu and the About window both carry a checkbox for it.
- App self-update: after `brew upgrade` refreshes the bundle, the
  already-running app detects the daemon is now on a newer version and
  relaunches itself once, so an open app picks up the new bundle without
  the user having to quit and reopen it by hand.

### Removed

- The built-in taste-profile feature (`impeccable` / `editorial` /
  `minimal` profiles, `SET_PROFILE`, `tf.taste` injection,
  `TURBOFIG_PROFILES_DIR`, and the plugin profile selector), ahead of the
  public release. Taste is a per-user, per-project judgement call; it now
  belongs in each user's own repo, not in turbofig. See `DECISIONS.md` #29.
