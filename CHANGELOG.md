# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Releases are listed below, newest first. Unreleased changes go under [Unreleased].

## [Unreleased]

## [0.2.0] - 2026-11-26

### Added

- The plugin tells you when a newer turbofig exists: a dismissible banner and an
  Update page with the `brew upgrade turbofig` step. It checks GitHub once a
  day, and you can switch it off in Advanced under "Update check".
- Screenshots default to 800 px on the longest edge. Set `maxDim` or `fullRes`
  for more. `turbofig js --shot=N` sets the size.
- `turbofig js`: runs JS or a job JSON from stdin through the file bridge and
  prints the result. `--shot` builds and screenshots in one call. WELCOME
  carries `binPath`, so the connect prompt names the daemon binary.
- `turbofig js --guide`: prints the full agent guide for the installed version
  (version, binary, bridge folder, connected files with keys, run line,
  `tf.*` helpers, every error code) and exits 0, also with 0 files. It reads
  no stdin and is covered by the same allow rule as every `js` call. The file
  list comes from a new `files` job op that reads the daemon's registry, so
  the guide never waits on a plugin. `<bridge>/guide.md` holds the same guide
  for agents with file tools only (mode 0600, written on every daemon start
  next to `tf-api.md`).
- One shared "Connection" setting: File bridge (default) or MCP. Change it in
  the Settings window (a segmented control) or in the plugin (gear). Both stay
  in sync: the daemon saves it as the marker file `<home>/mcp-mode` (mode
  0600; present means MCP) and sends `CONNECTION_MODE` to every plugin. WELCOME
  carries `connectionMode`. The plugin sends `SET_CONNECTION_MODE`. In File
  bridge mode `/mcp` returns 404 with `{"ok":false,"code":"mcp_http_off",...}`.
  `/job`, `/health`, `/control` and `turbofig mcp` (stdio) work in both modes.
  Upgrade note: if you added the HTTP transport by hand, switch Connection to
  MCP once. A leftover `mcp-http-off` file is deleted at startup.
- Challenge handshake on the plugin WebSocket: the plugin connects with
  `?nonce=`, the daemon sends `CHALLENGE`, the plugin answers `AUTH`. The
  token never goes on the wire. `GET /health` signs a client nonce
  (`X-Turbofig-Nonce`, `X-Turbofig-Proof`), and the CLI, the stdio proxy and
  the menu bar verify the proof before they send the token.
- Connection caps (HTTP 64, WS 32), idle timers, and at most 8 pending
  handshakes (the oldest is closed when all 8 are taken).
- File bridge error codes `invalid_job` and `io_error`. The daemon writes
  `tf-api.md` to the bridge dir on start.
- Modules `daemon_auth`, `daemon_client`, `listener` and `secure_fs`.

### Changed

- The plugin and menu-bar connect prompt ("Copy connect prompt") is a tiny
  pointer to `turbofig js --guide` (about 46 tokens, was about 435). It says to
  use turbofig for all Figma work and never other Figma tools, so an agent does
  not pick Figma's own connector ("claude.ai Figma"); docs/troubleshooting.md
  explains how to switch that connector off in Claude Code. The MCP prompt is
  shorter (about 216 tokens, was 306).
- Upgrades: `brew upgrade turbofig` converges with no user action except
  reopening the Figma plugin when it asks. Every daemon (not only a
  launchd-supervised one) watches the stable Homebrew binary and, on a new
  build, drains and restarts in place (`exec`, same pid, bridge folder lock
  never dropped). A daemon that runs from the app bundle watches the Homebrew
  path the bundle records (`TurbofigSourcePath`). The app starts the daemon
  from the Homebrew path, hands off any older daemon, and relaunches when the
  build id in its bundle changes. The bare `turbofig` command waits for the app
  to quit, reopens it with `open -n`, and makes sure a daemon runs.
- The bridge folder lock is taken after both ports are bound. `daemon.lock`
  records `version` and `buildId`. `turbofig js` restarts an older daemon once:
  a 0.1.0 daemon (no lock file) or one whose lock info shows an older version.
  It never asks the same daemon build twice, and it does not wait out a
  restart that ends in an in-place `exec`. A draining daemon leaves new job
  files that need a plugin in the inbox instead of claiming and failing them;
  `files` is still answered, so `turbofig js --guide` does not wait.
- First run on the app path prints the Figma steps, the manifest path and an
  "AI agents" line, opens Figma and copies the path. If Figma Desktop is
  missing, it prints the download link. A later bare `turbofig` reopens the
  About window. The output leads with the file bridge and tells AI agents to
  add an MCP server only if the user asks.
- Plugin: the version banner says which side is older. A Reload button
  (disabled while jobs run) closes the plugin and shows "Press ⌥⌘P to
  reopen". A token reset shows "turbofig was reset. Close and reopen this
  plugin." Labels cover a stopped daemon ("turbofig is not running. Open the
  turbofig app, or run turbofig in Terminal."), an older daemon, and another
  app on the port.
  The plugin files marker includes a hash of the plugin bundle, so a changed
  plugin under the same version is rewritten on the next daemon start. The
  activity squares stay on for 3 s after the last job.
- MCP `turbofig_screenshot` with `return: inline` sends an image content block
  (plus a text block with the metadata) instead of base64 text, so Claude
  Desktop sees it.
- The README has a copy-paste prompt that lets an AI install turbofig
  correctly (file bridge, no MCP server), and explains why an agent with a
  shell, such as Claude Code, works best.
- `turbofig_status` omits zero, null and false fields. `turbofig_execute`
  with no result returns `{"ok":true}`. Returned nodes reduce to
  `{id,name,type}`. Float noise is rounded. Errors carry the source line and
  a hint. Unknown `tf` options throw. `tools/list` text is shorter (1066 to
  658 bytes).
- Screenshot files have shorter names. The plugin exports at the `maxDim` cap.
- The WebSocket plugin reconnect delay caps at 2 s.
- A foreign listener makes the CLI, the proxy and the menu bar exit non-zero.
  An older same-user daemon is still handed off.

### Fixed

- Recovery: a young daemon waits up to 3 s for the first plugin. A deleted
  token file is restored from memory. Stale inbox jobs at startup get
  `not_started` instead of running. Busy-port and second-user messages name
  the port and owner. `turbofig uninstall --purge` lists every file it
  creates and reports truthfully. Full-disk result failures are logged.
- Menu bar: Start at login never boots out the running app or daemon. The app
  restarts a dead daemon after 3 failed checks (not after Quit or `turbofig
  stop`: `turbofig stop` alone leaves `~/.turbofig/stopped-by-user`, which the
  daemon deletes when it starts and an app launch clears). A Start turbofig menu item and startup
  failure alerts exist. Settings reflects headless autostart too. The app
  relaunches onto a refreshed bundle only after the refresh finishes.
- A 0.1.0 plugin left open across an upgrade (it connects with `?token=`)
  gets a `WELCOME` and shows its "Reload the plugin" banner, instead of
  reconnecting in silence. A wrong token still gets 401.
- The legacy daemon hand-off treats a launchd-supervised daemon as supervised,
  so it no longer races launchd's own restart.

### Security

- The daemon refuses a symlinked or foreign-owned bridge dir, inbox, outbox,
  token or plugin dir, a token readable by others, and an unset `HOME`. Under
  launchd a refusal exits 0.
- The file bridge reads jobs with `O_NOFOLLOW|O_NONBLOCK`, rejects FIFOs and
  symlinks, caps jobs at 30 MiB and ids at 200 bytes, writes results and PNGs
  with mode 0600, and prunes only daemon-named files.
- The daemon trusts an app bundle only when the user or root owns it, never
  overwrites a foreign `turbofig.app`, and calls system binaries by absolute
  path. The dev `dist/ui.html` is mode 0600.
- `/control` checks the token before it reads the body. Client reads are
  capped. The daemon raises `RLIMIT_NOFILE` at start.
- The daemon accepts a Figma file key only as letters, digits, `_` and `-`,
  so a key is always safe in a shell command (`--file KEY`).
- The bridge folder itself must not be group or other writable, even with
  the sticky bit. Private files are written through a new temp file
  (`O_EXCL|O_NOFOLLOW`), never through a symlink.
- `SECURITY.md` lists the residual risks.

### Performance

- Screenshot on a 3840x2160 node: 222 to 91 ms (CatmullRom resize, fast PNG
  encode, `maxDim` sent to the plugin).
- A `turbofig_status` file entry: 261 to 117 bytes.
- The file bridge skill and agents guide shrank (6.2 to 2.6 KB and 4.5 to
  2.0 KB). `tf-api.md` shrank from 13.7 to 8.1 KB.

## [0.1.0] - 2026-10-07

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
- `POST /job` and `POST /mcp` require the pairing token as a bearer token
  in the `Authorization` header, like `POST /control`. Another macOS account on the same Mac can
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
- macOS menu-bar app: `turbofig.app` is assembled on the user's own Mac
  (ad-hoc signed, no quarantine flag, so no "unidentified developer"
  prompt), installed/refreshed and opened automatically by the bare
  `turbofig` command. A tray icon (template image, 2 states: a plugin
  connected, or waiting/unreachable) shows a menu: Copy Agent Prompt, Copy
  Plugin Manifest Path, Show Plugin in Finder (`open -R`, for a Figma file
  picker that cannot browse into the hidden `~/.turbofig/figma-plugin/`),
  Open Figma, About turbofig…, Start at Login, Open Log, Quit turbofig. A
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

- The legacy `?token=` WebSocket upgrade. A `?token=` upgrade now gets 401.
  Old 0.1.0 plugins must be reopened in Figma after you upgrade. The daemon
  rewrites the plugin files on start.

- The built-in taste-profile feature (`impeccable` / `editorial` /
  `minimal` profiles, `SET_PROFILE`, `tf.taste` injection,
  `TURBOFIG_PROFILES_DIR`, and the plugin profile selector), ahead of the
  public release. Taste is a per-user, per-project judgement call; it now
  belongs in each user's own repo, not in turbofig. See `DECISIONS.md` #29.

