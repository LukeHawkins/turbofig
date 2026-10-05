# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project has not reached a tagged release yet; all work so far is
recorded under Unreleased.

## [Unreleased] - 0.1.0

### Added

- `turbofig` CLI (`clap`): `turbofig` with no subcommand or `turbofig serve`
  runs the daemon in the foreground as before. `turbofig setup` installs the
  pairing token, writes the embedded Figma plugin to `<home>/figma-plugin/`,
  writes and loads a launchd service (`~/Library/LaunchAgents/eu.lukehawkins.turbofig.plist`,
  `RunAtLoad` and `KeepAlive`), then prints the 3 steps to import the plugin
  into Figma. `turbofig uninstall [--purge]` unloads the service and removes
  the plist, keeping `<home>` unless `--purge` is given. `turbofig status`
  queries the running daemon's `/health` endpoint and prints a readable
  report. `--version` and `--help` come from `clap`.
- `GET /health`: daemon version, uptime, and the connected files (name, key,
  plugin version), with the same Origin and `Host` checks as `/mcp`. Never
  includes the pairing token.
- Automatic updates after `brew upgrade`: on startup, a daemon with an
  existing `<home>/figma-plugin/` refreshes it if the on-disk copy is stale.
  Under launchd supervision (`TURBOFIG_SUPERVISED=1`, set by the plist
  `turbofig setup` writes), the daemon polls every 30s for a Homebrew
  upgrade (the stable binary path resolving to a new Cellar version), stops
  accepting new jobs, waits up to 60s for in-flight jobs to finish, then
  exits cleanly so launchd starts the new binary.
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

### Removed

- The built-in taste-profile feature (`impeccable` / `editorial` /
  `minimal` profiles, `SET_PROFILE`, `tf.taste` injection,
  `TURBOFIG_PROFILES_DIR`, and the plugin profile selector), ahead of the
  public release. Taste is a per-user, per-project judgement call; it now
  belongs in each user's own repo, not in turbofig. See `DECISIONS.md` #29.
