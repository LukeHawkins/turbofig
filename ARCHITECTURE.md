# Architecture

## Transport chain

```
Claude / AI client
    |
    |  one of four inbound transports to the daemon:
    |   1. HTTP POST /mcp   (port 18846, MCP streamable-http, SSE response)
    |   2. stdio MCP        (`turbofig mcp`, a proxy forwarding onto POST /job)
    |   3. file-bridge      (~/.turbofig/inbox -> outbox; write and read files)
    |   4. curl on 18846    (the same HTTP endpoint, as a fallback)
    |
Rust daemon  (daemon/)
    |
    | WebSocket  (port 18847)
    |
Figma plugin UI thread  (plugin/src/ui.html)   holds the socket
    |
    | postMessage / onmessage
    |
Figma plugin main thread  (plugin/src/code.ts)   runs the Figma API
    |
    | Figma Plugin API
    v
Figma document
```

All four inbound transports converge on one shared `AppState`, so a call from
any of them routes to the plugin the same way. The file-bridge exists for
locked-down clients that cannot use curl or a native MCP server. The stdio
proxy (`turbofig mcp`) exists for a native MCP client (such as Claude Code's
`claude mcp add`) that speaks stdio, not HTTP: it forwards every tool call
onto the daemon's `POST /job`, starting the daemon first if it is not already
reachable (see "Daemon lifecycle" below). See `DECISIONS.md` item 15 and
`skills/file-bridge.md`.

## Daemon

The daemon is a single Rust process. It runs three servers as three `tokio::spawn` tasks that share one `Arc<AppState>`:

- **MCP HTTP server** (port 18846): speaks the MCP streamable-http protocol via `rmcp`. Uses `legacy_session_mode` so that clients on the 2025-03-26 spec can supply an `mcp-session-id` header. Each MCP session is stateful. Each tool reads the `mcp-session-id` from the HTTP request parts and maps the session to a plugin connection by `fileKey` (Phase 4). `allowed_hosts` is pinned explicitly to `localhost`, `127.0.0.1`, `::1` rather than left to `rmcp`'s default, so a future crate upgrade cannot silently widen the daemon's DNS-rebinding guard.
- **WebSocket server** (port 18847): requires a `token` query parameter on the upgrade, matching `~/.turbofig/token` (constant-time compare, checked after the Origin check, 401 on mismatch or absence). This closes a gap Origin checking alone leaves: a sandboxed `<iframe>` on a malicious web page reports Origin `null`, the same value the real Figma plugin UI reports. See `DECISIONS.md` #39 and `SECURITY.md`. Holds the persistent connection from the Figma plugin. The plugin sends `FILE_INFO` (`fileKey` + name) on connect, and again whenever it re-announces (e.g. a detected file rename). The daemon routes a tool call to the plugin by sending a request over this socket and awaiting a `RESULT`. A per-request timeout (`TURBOFIG_REQUEST_TIMEOUT_MS`, default 30000, clamped to 600000) stops a silent plugin from hanging a call; `EXECUTE`, `GET_SELECTION`, and `SCREENSHOT` all carry that timeout to the plugin as `timeoutMs`, and the daemon itself waits `timeoutMs + 1000ms` for `EXECUTE`, so the plugin's own "I gave up" reply usually beats the daemon's bare timeout (see `DECISIONS.md`). The daemon holds one connection per open file in a `conn_id`-keyed registry. Two live connections may hold the same `fileKey` at once (a reconnect, or a second window on the same file): neither evicts the other, so neither one's in-flight jobs are ever cancelled by the other connecting, and a later FILE_INFO from either connection can never knock the other out of the registry. Routing (`connections_named`/`resolve_route`) dedupes a shared `fileKey` down to the newest (highest conn_id) live connection, so a caller never sees Ambiguous for a single logical file; if that newest connection closes, routing falls back to an older one still open. A frame from a conn_id no longer in the registry is ignored outright. A `RESULT` only resolves the pending request if it arrives on the same connection the request was sent to, so one connection can never forge another's reply. Message and frame size are capped at 32 MiB; a keepalive ping goes out every 15s and a connection with no pong in 45s is dropped. On socket close the daemon drops only that connection and fails only that connection's in-flight requests at once, so other files keep working.
- **File-bridge** (default `~/.turbofig`): watches `inbox/` and writes `outbox/`. A client writes a job file and reads the result file, so no curl and no MCP connection are needed. This is the primary transport for locked-down Claude Enterprise accounts. It parses each job into the same typed parameter structs the MCP tools use (one `#[serde(tag = "op")]` enum, so a bad field fails to parse the same way for both transports): a file that reads and parses as JSON but fails the Job schema (a bad field, an unknown op) is rejected at once with a clear error, never held for the parse-grace window that exists only for a half-written file. A valid job is claimed (removes the inbox file), gets any stale same-id outbox result deleted, and runs in its own task so one slow job never blocks the loop. Job ids are a documented client contract: unique per job, never reused while the first job with that id may still be in flight (`skills/file-bridge.md`, `helpers/tf-api.md`, the plugin's connect prompt). A duplicate id that arrives while its twin is still running is left untouched in the inbox (not claimed, not answered) until the first finishes, so neither job's `.tmp` file or result is ever touched by the other. The ops are `status`, `execute`, `get_selection`, and `screenshot`. Screenshot file-mode writes the PNG into `outbox/<requestId>-<nanos>.png` (`AppState.screenshot_dir`) and returns its path; a subagent reads it. The outbox is a drop box, not storage: a backstop sweep deletes results and PNGs older than 24h. An inbox entry that never reads or parses (a bad write, an unreadable file, a non-UTF-8 name) gets one error result after a short grace window and is then left alone, instead of being retried and re-logged forever.

Each capability has one shared `run_*` routine, all funnelled through one send/await/timeout/cancel helper (`plugin_call::call_plugin`). Both the MCP tool and the file-bridge op call the same routine: `run_status`, `run_execute`, `run_get_selection`, and `run_screenshot`.

The daemon is always-on. A launchd service starts it at login and `KeepAlive` restarts it on crash. It is decoupled from any client session, so a client disconnect or session end never stops it.

### Module layout (`daemon/src/`)

| Module | Holds |
|---|---|
| `lib.rs` | Crate docs, `mod` declarations, re-exports only |
| `main.rs` | The `turbofig` binary: dispatches `Cli::command` to `cmd_run` (bare), `run_daemon` (`serve`), `cmd_start`, `cmd_stop`, `cmd_status`, `cmd_autostart`, `cmd_uninstall`, `cmd_mcp`; the supervised-restart loop |
| `config.rs` | Env-driven settings: ports, request timeout, bridge dir |
| `state.rs` | `AppState`, the connection registry, session pairing, the pending-request map, `begin_job`/`JobGuard` (whole-call job counting), `mark_plugin_seen` |
| `routing.rs` | `RouteError`, `resolve_route` |
| `plugin_call.rs` | The one register/send/await/timeout/cancel path all four ops share, plus the pending-cleanup drop guard |
| `ops/` | `status.rs`, `execute.rs`, `selection.rs`, `screenshot.rs` (the four `run_*` routines), `budget.rs` (context-firewall size warnings) |
| `image.rs` | A cheap PNG header probe (`probe_dims`) kept separate from the expensive decode/resize/encode path (`resize_png`), so a screenshot that needs no resize never pays for either |
| `mcp.rs` | MCP tool parameter structs, `TurbofigHandler`, `HELP_TEXT`, `build_router`, the Origin-rejection middleware |
| `ws.rs` | `handle_socket`, the WS Origin check, keepalive ping/pong, `serve_ws`; calls `AppState::mark_plugin_seen` on a successful token auth |
| `bridge/mod.rs`, `bridge/job.rs` | The inbox scan loop; `job.rs`'s typed `Job` enum reuses the MCP param structs |
| `embedded.rs` | `embedded_plugin()`, wrapping the `build.rs`-generated `include_str!`s |
| `plugin_files.rs` | `write_plugin_files`, `plugin_files_outdated`, `mark_plugin_seen` (the `<home>/plugin-seen` marker) |
| `token.rs` | `ensure_token`, `random_token_hex`, `constant_time_eq` |
| `spawn.rs` | `spawn_detached_daemon` (setsid, own session, log redirected to `<home>/daemon.log`), `fetch_health`/`wait_for_health`/`wait_for_unreachable`: shared by `turbofig` (bare), `turbofig start`, and `turbofig mcp` |
| `proxy.rs` | `turbofig mcp`: a stdio MCP server forwarding every tool call onto `POST /job`. It answers `initialize` and `tools/list` at once; the health check, the detached daemon start and the version-handoff restart (compares its own build version to the daemon's `/health` version) run in a background task that only tool calls wait on. Each proxy sends its own `X-Turbofig-Session` id, so `fileKey` pairing works as on an HTTP MCP session |
| `control.rs` | The authenticated local `POST /control` path (`stop`/`restart`) used to drain and restart the daemon; backs `turbofig stop` and the version handoff |
| `first_run.rs` | `first_run_text`, `status_text` (the two texts `turbofig`, the bare command, prints), the `Clipboard`/`AppOpener` seams (`RealClipboard`/`FakeClipboard`, `RealAppOpener`/`FakeOpener`) |
| `cli.rs` | The `clap` `Cli`/`Command` types, `run_autostart_on`/`run_autostart_off`, `run_uninstall`, `format_health`, and the other pure/testable halves of the CLI (`main.rs` wires these to the real filesystem, `launchctl`, and HTTP client) |
| `launchd.rs` | `stable_binary_path`, `plist_contents`, the `Launchctl` trait and its real/fake implementations |
| `supervisor.rs` | `installed_target`, `upgrade_detected`, `should_log_binary_gone`, `wait_for_drain`: the supervised-restart decision logic, seamed off the real clock and path resolver |
| `app_bundle.rs` | macOS-only (`cfg(target_os = "macos")`): `install_app_bundle` assembles `Turbofig.app` (`Info.plist`, a byte copy of the running binary, the embedded icon), ad-hoc signed best-effort; `app_bundle_outdated`, `running_inside_app_bundle`, `remove_turbofig_app_bundle`; the `CodeSigner` seam (`RealCodeSigner`/`NoopCodeSigner`). The bundle is assembled on the user's own Mac, so it carries no Gatekeeper quarantine flag |
| `agent_prompt.rs` | Not macOS-only: the agent-connect prompt's fill logic (`fill_agent_prompt`), shared byte-for-byte with the plugin's own copy via `prompts/agent-prompt.txt` (`include_str!` here, inlined by `plugin/build-ui.ts` there). A golden test on each side checks the same inputs give identical text |
| `menu_bar/` | macOS-only: the menu-bar app (`mod.rs`'s `run_menu_bar_app`, and `about_window.rs`'s `create_about_window`, the only things in the crate that build a real tray icon, window, webview, or event loop); `state.rs` (`MenuState`, the pure `/health`-to-menu translation); `icon.rs` (PNG decode for the 2 tray-icon states); `lock.rs` (the single-instance `flock` guard); `quit.rs` (the stop-then-confirm sequence, seamed off a real HTTP stopper via `DaemonStopper`); `about_state.rs` (the About window's IPC command parsing, `/health`-to-chip mapping, and first-use rule); `about_window.rs` (the `tao`/`wry` glue); `second_instance.rs` (the `<home>/app.sock` signal: a second launch asks the first to open the window, `turbofig uninstall` asks it to quit); `self_update.rs` (the relaunch-once-per-daemon-version decision, and its `<home>/app-relaunched-for` state file). See "Menu-bar app" below |

## Plugin

The plugin has two threads, as required by the Figma plugin model:

- **Main thread** (`plugin/src/code.ts`): runs in the Figma sandbox. Has access to the Figma Plugin API. Receives messages from the UI thread and executes Figma API calls.
- **UI thread** (`plugin/src/ui/` → built to `dist/ui.html`): runs in a sandboxed iframe. Holds the WebSocket connection to the daemon. Relays messages between the daemon and the main thread via `parent.postMessage` / `figma.ui.onmessage`. The panel has three screens toggled by `.screen`/`.active` class swap, each with its own `figma.ui.resize` call via a `RESIZE` message: **main** (300×150, default, wordmark + status + file row + footer nav), **advanced** (300×240, activity log + port field), **about** (300×375, large wordmark + tagline + description + repo/author links). The header wordmark uses a pure-CSS motion ghost effect (`text-shadow` with `color-mix(in srgb, var(--text) N%, transparent)`) so it is theme-aware without JS: light and dark mode both work via the injected `--figma-color-*` tokens.

The plugin dispatches on a `{type}` field in each message:

| Type | Direction | Description |
|---|---|---|
| `READY` | UI to main thread | Sent once, on load; the main thread replies with `FILE_INFO` and `PORT` (Phase 12) |
| `FILE_INFO` | plugin to daemon | Sent on connect, and again on a detected file rename: fileKey and root name (Phase 2) |
| `STATUS` | daemon to plugin | Liveness ping carrying a `requestId`; bypasses the job queue (Phase 2) |
| `RESULT` | plugin to daemon | Reply carrying the matching `requestId` (Phase 2) |
| `EXECUTE` | daemon to plugin | Run arbitrary Figma Plugin API JS; queued (Phase 3). Carries `timeoutMs`: the plugin must stop waiting and reply `ok:false` at that point; the daemon itself waits `timeoutMs + 1000ms` |
| `GET_SELECTION` | daemon to plugin | Return compact selection info; queued (Phase 3). Carries `timeoutMs`, raced the same way as `EXECUTE` |
| `SCREENSHOT` | daemon to plugin | Export PNG; queued (Phase 3). Carries `timeoutMs`, raced the same way as `EXECUTE` |

This dispatch table is hybrid-ready. A community-safe command vocabulary is additive: add new types without reworking the existing structure.

`EXECUTE`, `GET_SELECTION` and `SCREENSHOT` run one at a time, FIFO, through a single queue in the main thread (`createDispatcher` in `code.ts`), so two overlapping jobs can never interleave and create duplicate nodes. `STATUS`, `SET_PORT`, `RESIZE` and `READY` bypass the queue and run immediately. Every queued reply is capped at 16 MiB (`capResultMessage`); an oversized reply becomes an `ok:false` error naming the size instead of reaching the WebSocket.

Each queued job carries a deadline stamped when the daemon's message is received (enqueue time), not when the job reaches the front of the queue: `timeoutMs` (defaulting to 30000 when absent) is added to the receive time once, up front. If that deadline has already passed by the time a job is dequeued, it never runs at all; the plugin replies `ok:false` with "expired in the queue; the job did not run", making a retry obviously safe. Otherwise the job races against whatever time remains until the deadline, for all three types, not only `EXECUTE`: a hung `exportAsync` or `getNodeByIdAsync` can no longer block the queue forever just because `GET_SELECTION`/`SCREENSHOT` previously had no timeout race. A timeout that fires while the job is actually running states that it may still be running and a retry is not idempotent; a synchronous infinite loop in the user's code still cannot be interrupted this way, since JavaScript is single-threaded. `timeoutMs` is clamped to `2^31 - 1` ms both in the daemon's own config and again in the plugin, since a larger value overflows a JS `setTimeout` and fires almost immediately instead of waiting.

The queue itself is one chained promise (`queueTail`); each enqueue appends a `.then(run).then(post).catch(swallow)` link, so a `job()` or `post()` throw in one link can never leave `queueTail` permanently rejected and silently drop every job queued after it.

`EXECUTE` runs the JS as an async function built with the Function constructor (validated in Figma's sandbox, see `DECISIONS.md` #17). Two things are injected into the eval scope before user code runs: the sync-to-async deprecation preamble (runs first), and the `tf` craft namespace (`createTf(figma)`, passed as a second parameter beside `figma`). So generated code calls `figma.*` and `tf.*` directly. Eval errors return a clean message (with a line/column relative to the user's own code, adjusted for the preamble) and never crash the plugin.

## Routing registry (Phase 4)

The daemon keeps a connection registry keyed by two dimensions:

- `mcp-session-id`: assigned at the MCP `initialize` call; identifies a Claude session.
- `fileKey`: sent by the plugin on connect; identifies an open Figma file.

Each MCP session is paired to a `fileKey` by an explicit pick or the sole connected plugin. `resolve_route` picks the target connection by explicit `fileKey`, then the session pairing, then the sole named connection. It returns a clear error for no plugin, ambiguous target, or a not-connected file. A session pairing is pruned after 24h of inactivity and capped at 1000 entries, so a long-running daemon's session map cannot grow forever. A socket close cancels only that connection's in-flight requests. The registry handles N sessions and N files concurrently, fully isolated.

Two live connections may hold the same `fileKey` at once: `set_connection_info` no longer evicts the older one when a new connection claims an already-registered `fileKey`. Eviction closed neither socket (`ws.rs` kept its `tx`), so a later FILE_INFO from the evicted connection could remove the live one and lose the route, and eviction cancelled in-flight jobs that were still running in Figma. Instead, every live connection stays registered, even sharing a `fileKey`; `connections_named` (the routing-facing view) dedupes a shared key down to the newest (highest conn_id) entry, so a caller still never sees Ambiguous for one logical file, and the same dedupe applies to the status/Ambiguous file lists. If the newest connection closes, routing naturally falls back to an older one still open, since it is still in the raw registry `remove_connection` never touched. A frame tagged with a conn_id no longer in the registry is ignored outright (see `DECISIONS.md` #35).

## Helper layer

The `tf` namespace is a compact JS craft library injected into every eval. Source lives in `plugin/src/helpers.ts` (pure logic unit-tested; figma glue smoke-tested), exposed via `createTf(figma)` and passed into the eval as `tf`. The compact API reference is `helpers/tf-api.md` (this is what the model reads to learn the helpers cheaply). The library is complete as of Phase 6. Categories and members:

- Layout primitives: `frame` (per-axis sizing, transparent by default), `rect`, `append`, `clear`, `findOrCreate` (idempotent-by-name).
- Text and fonts: `text` (font-load, optional `width` for wrapping), `loadFonts`, `color`, `solid`.
- Decks and slides: `deck`, `slide`, `slidePosition`, `chunk`.
- Component instances: `instance`, `instanceByKey`.
- Variables: `getVariable`, `setVariableValue`, `readVariableValue`.
- Export: `export`.
- Utilities: `skipInvisible`, `findAll`, `commit`.

All functions use the async Figma API surface required by `documentAccess: dynamic-page`.

Idempotency pattern for re-runnable sections: `findOrCreate(parent, name, factory)` then `clear(node)` then rebuild. `findOrCreate` protects only the named node, so `clear` before rebuilding prevents duplicated children on a resume.

## Design orchestration

`.claude/commands/design.md` is the `/design` command: a brief becomes a full page via plan-first spec (persisted to `~/.turbofig/design/<job-id>/plan.json` + `status.json` as the checkpoint) -> parallel firewalled builder subagents (each one batched `tf.*` call, unique per-request bridge id) -> an assembly step that stacks sections in order (parallel builds otherwise overlap at 0,0) -> a QA critic subagent that reads the screenshot and returns text only -> a capped refine loop -> resume from the last completed section. Images live and die in subagents; the orchestrator never holds a screenshot.

## Skill layer

`skills/` (Phase 7+): the design-worker recipes, led by `design.md` (the `/design` command).

## Ports

Both ports are product contracts, not dev-server conventions. See `DECISIONS.md` item 3.

| Port | Protocol | Purpose |
|---|---|---|
| 18846 | HTTP (SSE) | MCP endpoint for Claude / AI clients |
| 18847 | WebSocket | Daemon-to-plugin persistent connection |

Both are overridable via `TURBOFIG_MCP_PORT` and `TURBOFIG_WS_PORT` environment variables.

## Embedded plugin and the pairing token

The daemon binary embeds the built Figma plugin (`plugin/manifest.json`,
`plugin/dist/code.js`, `plugin/dist/ui.html`) at compile time via
`daemon/build.rs` and `daemon/src/embedded.rs`'s `embedded_plugin()`, so a
Homebrew-installed daemon (no repo checkout) can still write the plugin out
to disk. `daemon/src/plugin_files.rs`'s `write_plugin_files(home, token)`
writes `manifest.json` to `<home>/figma-plugin/`, and `code.js`/`ui.html`
to `<home>/figma-plugin/dist/`, matching the `dist/code.js`/`dist/ui.html`
paths the manifest itself names: Figma's "Import plugin from manifest"
resolves `main`/`ui` relative to the manifest's own directory, so writing
those two files at the root (an earlier layout) made every import fail.
`write_plugin_files` replaces the `__TURBOFIG_PAIRING_TOKEN__` placeholder
in `ui.html` with the real pairing token, plus a version+token-hash marker
(`plugin_files_outdated` reads it to detect a stale copy), and removes any
stale root-level `code.js`/`ui.html` left by an older daemon version on
refresh. `plugin/dist` is a Bun build, not a Cargo artifact, so
`build.rs` degrades to a stub (`embedded_plugin()` returns `None`) when it is
missing, keeping a Rust-only `cargo build` and the CI Rust job working. The
daemon's own startup (`run_daemon` in `main.rs`), not a separate install
command, calls `write_plugin_files`: see "Daemon lifecycle" below; see also
`DECISIONS.md` #38 and the "Install model" entry that supersedes it.

`daemon/src/token.rs` owns the pairing token itself: see "WebSocket server"
above and `DECISIONS.md` #39 for the full design.

## Daemon lifecycle

`turbofig` never needs a separate install step: the daemon starts on demand
and writes its own files on every start.

- **On-demand detached start.** `spawn::spawn_detached_daemon` runs
  `<turbofig binary> serve` in its own session (`setsid`), with `<home>` as its
  working directory, stdin from `/dev/null`, and stdout/stderr appended to
  `<home>/daemon.log`. A background thread reaps the child, so a stopped
  daemon never stays a zombie. `daemon.log` over 5 MiB is rotated to
  `daemon.log.1` (one rotated file is kept) by whichever process starts the
  next daemon: `spawn_detached_daemon`'s caller for an ad-hoc start, or
  `run_daemon` itself (`spawn::rotate_daemon_log_and_reopen_std_streams`) for
  a launchd-managed one, since nothing else ever respawns that long-running
  process to trigger the first path. The launchd case also reopens stdout
  and stderr (`dup2`) onto the fresh file: the plist's `StandardOutPath`
  keeps them pointed at the pre-rotation inode otherwise. Both `turbofig`
  (bare, no subcommand) and `turbofig mcp` call this the same way: check
  `/health` first, start detached only if nothing answers, then poll
  `/health` until it does (`spawn::wait_for_health`) or give up with a clear
  error naming the log path. `turbofig start` is the same flow exposed as an
  explicit, idempotent subcommand.
- **Token and plugin files on every start.** `run_daemon` always calls
  `ensure_token` (never overwrites an existing token), then always checks
  `plugin_files_outdated` and calls `write_plugin_files` when it reports
  true: on a fresh install (no `<home>/figma-plugin/` yet) this creates it;
  on a later start with a newer embedded plugin or a rotated token, it
  refreshes it. Either case logs exactly one line ("wrote" or "refreshed").
  There is no "only if the directory already exists" gate: a user who has
  never opened Figma still gets a ready-to-import `manifest.json` the first
  time the daemon starts.
- **The `plugin-seen` marker.** The daemon writes `<home>/plugin-seen` (mode
  0600, holding a timestamp) the first time a plugin WebSocket connection
  presents a valid pairing token (`AppState::mark_plugin_seen`, called from
  `ws_handler` in `ws.rs`); a no-op on every later valid connection. This is
  how the bare `turbofig` command tells a genuine first run (no plugin has
  ever connected) from a later one (see "CLI" below).
- **Version handoff.** Once per proxy start, in the background task,
  `turbofig mcp` compares its own build version to the running daemon's
  `/health` version. An older daemon is told to drain and restart via the
  authenticated `POST /control` path (`control.rs`), so a `brew upgrade`
  reaches a long-running daemon without the user restarting it by hand. An
  older proxy never restarts a newer daemon, so sessions started before the
  upgrade cannot downgrade it. See `DECISIONS.md` and the
  "Supervised restart" section below for the launchd side of the same idea.
- **Launchd autostart is optional.** `turbofig autostart on`/`off` write or
  remove the `eu.lukehawkins.turbofig.plist`, so the daemon also starts at
  login and survives a crash via `KeepAlive`. Nothing above depends on
  autostart being on: the on-demand detached start is what makes the daemon
  always reachable even when it is off.

## CLI (`daemon/src/cli.rs`, `daemon/src/first_run.rs`, `daemon/src/main.rs`)

The `turbofig` binary is a `clap` (derive) CLI. `--version` and `--help`
come from `clap`.

| Command | Does |
|---|---|
| *(none)* | `cmd_run`: starts the daemon detached if not already running. On macOS, then installs/refreshes `Turbofig.app`, opens it, and prints a 2-line pointer at the tray icon (`try_app_first_run`); on any other OS, or if either step fails, prints the first-run walkthrough or a short status instead; see below |
| `serve` | Runs the daemon in the foreground (`run_daemon`): binds both ports, ensures the token, writes/refreshes the plugin files, serves until a subsystem dies |
| `start` | Starts the daemon detached if not already running, waits for `/health`, prints the version and both ports. Idempotent |
| `stop` | Stops the running daemon via authenticated `POST /control`, waits for it to go away. A no-op (not an error) if nothing was running. If `/health` answers but the token file is missing or no longer matches, `/control` cannot authenticate, so `stop` exits 1 and tells the user how to end the process by hand |
| `status` | A thin client for `GET /health`, printed as a short report |
| `autostart on [--headless]` / `autostart off` | `on` (default) installs the app LaunchAgent (`eu.lukehawkins.turbofig.app`, `cli::run_autostart_on_app`): the bundle's own executable, `RunAtLoad` true, `KeepAlive` false. `--headless` installs the daemon-only LaunchAgent instead (`eu.lukehawkins.turbofig`, unchanged from before). Either `on` bootouts and removes the other plist first: the 2 are never active together. `off` bootouts and removes whichever is present (in principle both) |
| `uninstall [--purge]` | Quits a running menu-bar app first (`menu_bar::signal_quit_running_app`, over `<home>/app.sock`), then stops autostart (both plists) and removes the app bundle; `--purge` also deletes the known home-directory entries |
| `mcp` | Runs the stdio MCP proxy (`proxy.rs`), starting the daemon via `spawn` if unreachable |
| `app install` | macOS-only, hidden. Assembles (or refreshes) `Turbofig.app` and prints its path (`app_bundle.rs`) |
| `app run` | macOS-only, hidden, dev-only. Starts the menu-bar app (`menu_bar::run_menu_bar_app`) with no bundle in place; a debug build refuses unless `TURBOFIG_DEV_REAL_DESKTOP=1`, since it shows real UI |

- **The bare command's macOS app-first-run path** (`try_app_first_run`,
  `first_run::app_first_run_outcome`): installs/refreshes `Turbofig.app`
  (`app_bundle::install_app_bundle`, same debug guard as `app install`),
  opens it (`AppOpener::open_url` on the bundle path), and on success prints
  exactly:
  ```
  Turbofig is now in your menu bar (look for the tf icon).
  Click it and choose About Turbofig… to get started. No icon? Run: turbofig status
  ```
  A failure at either step prints a 1-line reason first, then falls through
  to the ordinary text walkthrough below unchanged.

- **The bare command's first-run walkthrough.** When `<home>/plugin-seen`
  does not exist yet (see "Daemon lifecycle" above), `cmd_run` best-effort
  copies the manifest path to the clipboard and best-effort opens Figma
  Desktop (`open -a Figma`), both behind a seam (`first_run::Clipboard`,
  `first_run::AppOpener`) so a test never shells out to the real `pbcopy` or
  `open`, then prints (`first_run::first_run_text`):

  ```
  turbofig <version> is running (MCP 127.0.0.1:<mcp port>, plugin 127.0.0.1:<ws port>).

  1. Add the Figma plugin (once). Figma is opening now.
     Plugins > Development > Import plugin from manifest...
     Press Cmd+Shift+G, paste the path (it is on your clipboard), then press Return:
     <manifest path>

  2. Connect your agent (once):
     Claude Code:   claude mcp add turbofig -- turbofig mcp
     Other MCP clients, add this server:
       {"command": "<stable binary path>", "args": ["mcp"]}

  3. MCP blocked on your machine? Run the plugin in Figma and click "Copy prompt".
  ```

  If `open -a Figma` failed, "Figma is opening now." becomes "Open Figma
  Desktop." instead; nothing else in the text changes, including the
  clipboard line, since the manifest path is always printed too and can
  always be pasted by hand. `<stable binary path>` is the Homebrew
  `<prefix>/bin/turbofig` symlink when running from a Cellar, otherwise the
  running binary's own path (`launchd::stable_binary_path`; see below).
- **The bare command's later-run status.** When `<home>/plugin-seen`
  already exists, `cmd_run` prints a 3-line status instead
  (`first_run::status_text`), reading connected files from `/health`:

  ```
  turbofig <version> is running (MCP 127.0.0.1:<mcp port>, plugin 127.0.0.1:<ws port>).
  Connected files: <names, or "none, open the turbofig plugin in Figma">
  Plugin manifest: <manifest path>
  ```
- **`turbofig autostart on`** is idempotent: running it twice gives the same
  end state. It (a) writes
  `~/Library/LaunchAgents/eu.lukehawkins.turbofig.plist`
  (`daemon/src/launchd.rs`'s `plist_contents`): `ProgramArguments` is the
  *stable* binary path (see below) plus `serve`, `RunAtLoad` true and
  `KeepAlive` set to `{SuccessfulExit: false}`, not plain `true`: a clean
  exit (`turbofig stop`, which always exits 0) leaves the daemon stopped
  until the next login, while a crash (any non-zero exit) still gets
  restarted at once. `StandardOutPath`/`StandardErrorPath` set to
  `<home>/daemon.log`, and `EnvironmentVariables` holding
  `TURBOFIG_SUPERVISED=1` plus every other `TURBOFIG_*` variable set in
  `autostart on`'s own environment at the time it ran
  (`launchd::carry_over_turbofig_env`, excluding `TURBOFIG_SUPERVISED` itself and the
  seam-only `TURBOFIG_LAUNCH_AGENTS_DIR`): a `TURBOFIG_BRIDGE_DIR` or
  `TURBOFIG_*_PORT` override given to `autostart on` would otherwise never
  reach the launchd-started daemon, which ran with none of them and pointed
  at the default `~/.turbofig` instead; (b) runs `launchctl bootout
  gui/<uid>/eu.lukehawkins.turbofig` (ignoring a "not loaded" failure) then
  retries `launchctl bootstrap gui/<uid> <plist>` up to 5 times with a
  backoff (`cli::bootstrap_with_retry`), since `bootstrap` right after
  `bootout` often fails on macOS while the old service instance is still
  shutting down; (c) prints a warning if the pinned binary is not inside a
  Homebrew Cellar. It writes no token and no plugin files: the daemon's own
  startup owns those, whether launchd, `turbofig start`, or `turbofig serve`
  started it (see "Daemon lifecycle" above). The real LaunchAgents directory
  is read from `TURBOFIG_LAUNCH_AGENTS_DIR` when set, so a test never
  touches `~/Library/LaunchAgents`; `launchctl` itself sits behind the
  `launchd::Launchctl` trait, faked in tests.
- **Stable binary path rule** (`launchd::stable_binary_path`): if the
  canonicalized `current_exe` path contains `/Cellar/turbofig/`, the
  plist (and the first-run text's connect-prompt command) points at
  `<prefix>/bin/turbofig` (everything before `/Cellar`), the stable symlink
  Homebrew repoints on every `brew upgrade`. Any other path (a source
  checkout, a non-Cellar symlink target) is used as-is. This is also the
  path the supervised-restart loop below polls.
- **`turbofig uninstall [--purge]`**: `bootout`s the service and removes
  the plist. Without `--purge`, `<home>` (the token, the plugin files, the
  bridge inbox/outbox) is left in place and the command says so. With
  `--purge`, `cli::purge_home` deletes only the known entries turbofig
  itself writes (`token`, `figma-plugin/`, `inbox/`, `outbox/`,
  `daemon.log`, `daemon.log.1`, `plugin-seen`), then removes `<home>` itself only if that leaves it
  empty: `<home>` is `TURBOFIG_BRIDGE_DIR`-controlled and can be a shared
  folder or `$HOME`, so `--purge` must never `remove_dir_all` the whole
  thing. A missing plist or a missing `<home>` is not an error: uninstall
  is idempotent.
- **`turbofig status`**: a thin CLI client for `GET /health` (below) on
  `127.0.0.1:<TURBOFIG_MCP_PORT>`, printed as a short, readable report,
  using a `reqwest` client built with `.no_proxy()` so a corporate Mac's
  `HTTP_PROXY`/`HTTPS_PROXY` can never intercept this always-local
  request. If the daemon is unreachable, it says so and suggests
  `turbofig start`.

## Menu-bar app (`daemon/src/menu_bar/`)

Steps 2 (tray icon, menu, status polling, Quit), 3 (the About window), and
4a (app lifecycle: first-run opens the app, Start at Login means the app,
self-update, `uninstall` quits the app) of the macOS app bundle, all here
(step 1: `app_bundle.rs`, above; step 4b: docs, still to come). Reached
either by opening
`Turbofig.app` (`main.rs`'s `cmd_run_or_app_mode` dispatches into it when
`running_inside_app_bundle()` is true) or the hidden dev command `turbofig
app run`. macOS-only, same target-gating as its 3 extra dependencies,
`tray-icon` (with its `muda` menus), `tao` (the main-thread event loop), and
`wry` (the About window's webview), all
`[target.'cfg(target_os = "macos")'.dependencies]` so Linux CI never
resolves any of them.

- **Single instance.** Before doing anything else, `run_menu_bar_app` takes
  an exclusive, non-blocking `flock` on `<home>/app.lock` (`lock::try_acquire`).
  A second launch while one is already running gets `None` back and exits 0
  at once: no second tray icon ever appears. The lock lives on the open file
  description, so it releases automatically on process exit; nothing ever
  deletes the lock file itself.
- **Startup.** Ensures the daemon is running the same way the bare command
  does (`spawn::fetch_health`, `spawn_detached_daemon`, `wait_for_health`),
  then builds the tray icon and the menu once, starts the background health
  poller, and runs the tao event loop forever on the main thread (required
  on macOS). Every exit path goes through `std::process::exit`.
- **Status polling.** A background `std::thread` with its own small
  single-thread `tokio` runtime (not the outer one: the main thread is
  committed to tao's blocking event loop) polls `GET /health` with the
  pairing token every 2s (`spawn::fetch_health_with_token`, re-reading
  `<home>/token` each time in case the daemon restarted with a new one), and
  sends the parsed body (or `None`, unreachable) into the event loop as a
  `UserEvent::Health` via `EventLoopProxy`.
- **`state::MenuState`** is the pure translation from a `/health` body (or
  `None`) into everything the menu shows: the disabled header
  (`Turbofig <version>`), the disabled status line, which of the 2 tray-icon
  states to show, and the filled agent-connect prompt
  (`agent_prompt::fill_agent_prompt`, see above). Status line and icon:

  | `/health` | Status line | Icon |
  |---|---|---|
  | unreachable | "Bridge not running" | dimmed |
  | reachable, 0 files | "Waiting for the Figma plugin" | dimmed |
  | reachable, 1 file | "Connected: `<name>`" | normal |
  | reachable, >1 files | "Connected: `<n>` files" | normal |

- **`icon::icon_for_state`** decodes 1 of 2 checked-in 44x44 PNGs
  (`daemon/assets/tray-icon/`, see its README) to RGBA and builds a
  `tray_icon::Icon`, loaded as an AppKit template image (`with_icon_templated`/
  `set_icon_templated`) so macOS tints it for light and dark mode.
- **Menu**, in order: a disabled header, a disabled status line, a
  separator, "Copy Agent Prompt", "Copy Plugin Manifest Path", "Open Figma",
  "About Turbofig…" (opens, or focuses, the About window below), a
  separator, "Start at Login" (a `CheckMenuItem`; shown checked when
  `cli::app_autostart_plist_exists` is true at build time, toggled through
  `set_start_at_login`, below), "Open Log" (`open -a Console
  <home>/daemon.log`, through the `first_run::AppOpener` seam's
  `open_app_with_path`), a separator, "Quit Turbofig". Clicks are read each
  event-loop tick from `muda::MenuEvent::receiver()` and dispatched by
  comparing `event.id` against each item's own id.
- **`set_start_at_login(enabled, home)`** is shared by the tray's "Start at
  Login" checkbox and the About window's footer checkbox (its 2 IPC
  commands, `start_at_login_on`/`start_at_login_off`): `enabled` calls
  `cli::run_autostart_on_app` (installing the bundle first if its
  executable is somehow missing); disabled calls `cli::run_autostart_off`
  (removing whichever of the app/headless plists are present). A failure
  reverts the tray checkbox to its pre-click state; the About window's
  checkbox is not reverted (its initial state is re-read from the plist
  the next time the window opens).
- **Clipboard and opener.** "Copy Agent Prompt" and "Copy Plugin Manifest
  Path" go through `first_run::Clipboard`; "Open Figma" and "Open Log" go
  through `first_run::AppOpener` (`open_figma`/`open_app_with_path`). Both
  seams carry the same debug-build guard as the bare command: a debug build
  only touches the real clipboard/opener with `TURBOFIG_DEV_REAL_DESKTOP=1`,
  otherwise a `Null`/fake stands in, so no test run or local `cargo run` can
  ever touch the real desktop.
- **`quit::quit_sequence`** is pure control flow over a `DaemonStopper` seam
  (`stop`, `wait_unreachable`): "Quit Turbofig" issues `POST /control` with
  the pairing token, waits up to 10s for `/health` to go unreachable, then
  exits regardless (a user clicking Quit wants the app gone now). The real
  `DaemonStopper` owns its own small `tokio` runtime for the same main-thread
  reason as the health poller.
- **`turbofig app run`** is a hidden dev command that starts app mode with
  no bundle in place, for manual testing. A debug build refuses it unless
  `TURBOFIG_DEV_REAL_DESKTOP=1` is set, since it shows a real tray icon and
  menu; a release build (what `Turbofig.app` itself launches) always runs it.
- **Self-update (`self_update.rs`).** Every health poll also compares the
  daemon's reported `version` against this app binary's own
  `CARGO_PKG_VERSION` (semver). A `brew upgrade` refreshes `Turbofig.app`
  from the newly installed daemon's next start (`app_bundle::app_bundle_outdated`),
  but the already-running app process is still the old binary until it
  relaunches itself: once the daemon is strictly newer,
  `maybe_relaunch_for_upgrade` records the daemon version in
  `<home>/app-relaunched-for` (so the same version never triggers a second
  relaunch: `self_update::should_relaunch_for_upgrade`), drops `app.lock`
  (so the new instance's own `lock::try_acquire` can succeed), reopens the
  bundle as a new instance (`AppOpener::open_new_instance`, `open -n`), and
  exits. The app never relaunches when it is the same version or newer than
  the daemon: the version handoff (`proxy.rs`) already covers the daemon
  side of an upgrade.
- **`turbofig uninstall` quits a running app first.** `main.rs`'s
  `cmd_uninstall` calls `menu_bar::signal_quit_running_app`, which sends
  `second_instance::SignalMessage::Quit` over `<home>/app.sock`; a listening
  app's `UserEvent::QuitRequested` runs the same `perform_quit` as its own
  "Quit Turbofig" menu item and the About window's `quit` IPC command, so
  all 3 quit identically. Best-effort: no app running at all (the ordinary
  case for a headless install) is not an error.

### About window (step 3: `about_window.rs`, `about_state.rs`, `second_instance.rs`)

A real `tao` window (420x520, not resizable, titled "Turbofig") hosting 1
`wry` webview over exactly 1 embedded page
(`daemon/assets/about/about.html`, `include_str!`'d, never a URL; see
`SECURITY.md`'s "Menu-bar app: the About window's webview"). Opens
automatically on first use (`about_state::should_auto_open_about_window`:
`<home>/plugin-seen` does not exist yet), from "About Turbofig…", or from a
second instance's signal; already open, any of those 3 just calls
`AboutWindowHandle::focus` instead of building a second window.

- **The page** reuses the plugin panel's CSS tokens and ghost-wordmark trick
  (`plugin/src/ui/template.html`'s `--figma-color-*`-named custom
  properties and the `.wordmark` text-shadow), with its own light/dark
  values (no Figma host to inject them here) switched by
  `prefers-color-scheme`. Header: wordmark, version, the tagline "Bridge any
  AI to Figma". Two live status chips. Two tabs: "How to use" (default: add
  the plugin, run it in a file, ask the agent, with a checkmark on step 2
  once a file connects) and "Claude Code / MCP" (the `claude mcp add`
  command and the other-clients JSON, both with Copy; the same 2 strings
  `first_run::first_run_text` already shows in the bare command's
  first-run walkthrough, kept in sync by hand today, not a shared
  constant). Footer: a "Start at login" checkbox (wired to
  `set_start_at_login` via its own 2 IPC commands, below; its initial
  checked state is pushed in `create_about_window`'s own
  `window.turbofigSetStatic` call, from `cli::app_autostart_plist_exists`),
  "Docs" (the GitHub repo), "Quit Turbofig".
- **IPC**: the page only ever calls `window.ipc.postMessage("<command>")`
  with 1 of 9 fixed strings (the original 7, plus `start_at_login_on`/
  `start_at_login_off`); `about_state::parse_ipc_command` parses them into
  an `IpcCommand`, rejecting anything else. `about_window::handle_ipc_message`
  dispatches each to the same `Clipboard`/`AppOpener` seams, the same
  `perform_quit` the tray menu's "Quit Turbofig" and the second-instance
  `Quit` signal use, and the same `set_start_at_login` the tray's checkbox
  uses (`copy_mcp_command`/`copy_mcp_json` build their own text directly,
  from the stable binary path, rather than reading anything back from the
  DOM).
- **Navigation** is blocked everywhere except the initial load; see
  `about_window::navigation_is_allowed` and `SECURITY.md`.
- **Live status**: the same background health poller that updates the tray
  (`UserEvent::Health`) also calls `AboutWindowHandle::push_status`, which
  runs `window.turbofigSetStatus(...)` (`WebView::evaluate_script`) with the
  2 chip strings (`about_state::chips_from_connected_files`, the same
  bridge-reachable/connected-file-names shape `MenuState` is built from) and
  whether the step-2 checkmark should show.
- **Second instance** (`second_instance.rs`): a Unix socket at
  `<home>/app.sock`, mode `0600`, carrying 1 of exactly 2 fixed
  `SignalMessage`s. A second launch, once `lock::try_acquire` finds
  `<home>/app.lock` already held, connects and sends `OpenAbout`, then
  exits 0; the first instance's listener thread (bound before the tray is
  built) forwards that as `UserEvent::OpenAboutWindow` into the event loop,
  which opens or focuses the window exactly like "About Turbofig…" does.
  `turbofig uninstall` sends `Quit` the same way (`menu_bar::signal_quit_running_app`),
  forwarded as `UserEvent::QuitRequested` into `perform_quit`.

## `/health`, `/job`, and `/mcp` auth

`POST /job` and `POST /mcp` now require `Authorization: Bearer <pairing
token>`, enforced by `mcp.rs`'s `require_bearer_token` middleware, the same
check `/control` already used. A missing or wrong token gives 401 before
the request runs a job or reaches a tool. This closes the gap that another
local macOS account, also able to reach `127.0.0.1`, could otherwise drive
the Figma plugin through the HTTP port. The file-bridge is unaffected: it
stays tokenless, protected instead by its `~/.turbofig` directory mode
(0700).

`GET /health` (same HTTP port as `/mcp`, same Origin and Host checks, see
`mcp.rs`'s `reject_browser_origin` and the new `reject_bad_host`) never
fails with 401; instead its payload now depends on the same bearer token
(`health_handler`). Without a valid token it returns only `version`
(`CARGO_PKG_VERSION`) and `uptimeSeconds`: enough to confirm the daemon is
alive, without naming the open file to an unauthenticated local caller.
With a valid token it also returns `connectedFiles` (one entry per
connected file: `fileKey`, `name`, `pluginVersion`, and, if the plugin's
reported version differs from the daemon's, a `warning` of `"reopen the
turbofig plugin in Figma"`), `pid` (`std::process::id()`), and `supervised`
(`supervisor::is_supervised()`), which `proxy::restart_for_upgrade` reads to
decide whether to wait for launchd's own relaunch instead of racing it with
its own spawn. Never includes the pairing token itself: `connectedFiles`
reuses `AppState::named_connections_json`, the same JSON `turbofig_status`
returns, so the two surfaces can never drift. `turbofig_status` gained the
same `pluginVersion`/`warning` fields on each entry in its `plugins` list.

## Supervised restart (`TURBOFIG_SUPERVISED=1`)

The plist `turbofig autostart on` writes sets `TURBOFIG_SUPERVISED=1`. When
set, `main.rs` spawns a loop (`run_supervisor_loop`) that every 30s resolves
the stable binary path (the same rule `autostart on` used to build the plist) and
compares its canonical target to the one captured at startup
(`supervisor::upgrade_detected`, a pure path comparison). `supervisor::
installed_target` returns `Option<PathBuf>`, `None` when `canonicalize`
fails (mid-upgrade, or after `brew uninstall` removed the binary
entirely); `upgrade_detected` treats `None` as "skip this check", never as
a difference from the baseline, so neither case causes a spurious restart
or a restart-loop-forever. After 3 consecutive unresolved checks
(`supervisor::should_log_binary_gone`), the daemon logs a line saying the
binary is gone. A real path difference means `brew upgrade` repointed the
Cellar symlink. On detection the daemon: calls
`AppState::set_draining(true)`, so `resolve_route` refuses every new job
with `RouteError::Draining` across all three transports (MCP, WS-routed
ops, file-bridge; they all call `resolve_route`); waits up to 60s
(`supervisor::wait_for_drain`, polling `AppState::jobs_in_flight`) for
in-flight jobs to finish; then waits a further 250ms grace
(`SUPERVISOR_EXIT_GRACE`) so a just-finished response can flush; then exits
`supervisor::SUPERVISED_RESTART_EXIT_CODE` (75), a non-zero code, so
launchd's `KeepAlive: {SuccessfulExit: false}` (above) restarts it with the
new binary rather than leaving it stopped the way a clean `turbofig stop`
(always exit 0) does. `/control`'s own `restart` action exits the same code
under supervision, for the same reason (`control.rs`'s `exit_code_for`).
The clock and the
path resolver are pure/seamed (`supervisor.rs`) so the decision logic is
unit tested without a real 30s wait or a real Homebrew upgrade.

`AppState::jobs_in_flight` counts whole tool calls, not plugin-reply waits:
`AppState::begin_job` returns an RAII `JobGuard` that increments a shared
counter on creation and decrements it on drop. Both the 4 MCP tool
handlers (`mcp.rs`) and the bridge's per-job spawned task (`bridge/mod.rs`)
hold one guard from entry to their final response or result write, so the
drain wait above covers the whole call (routing, the plugin round trip,
and any work after the reply: a screenshot resize, a file write, building
the response), not just the earlier narrower window of "waiting on the
`pending` map".

## Message contract: plugin version reporting

`FILE_INFO` (plugin to daemon) now carries `pluginVersion` alongside
`fileKey` and `name` (`plugin/src/protocol.ts`'s `FileInfoMessage`,
built by `buildFileInfo`; `plugin/build-code.ts` injects
`__PLUGIN_VERSION__` from `plugin/package.json` the same way
`build-ui.ts` already does for the UI bundle). The daemon stores it on
`PluginConn` (`state.rs`) and surfaces it, plus the reopen-the-plugin
warning when it differs from the daemon's own version, in both
`turbofig_status` and `/health` (see above).

`WELCOME` (daemon to plugin) carries `version`, `mcpPort`, and
`bridgeHome`: the file-bridge home directory (`config::bridge_dir_display`),
shown with the real `$HOME` prefix replaced by `~` for the default install.
The plugin panel's copy-prompt button (`formatConnectPrompt`) uses it to
name the real inbox/outbox paths even under a custom
`TURBOFIG_BRIDGE_DIR`, falling back to `~/.turbofig` before the first
`WELCOME` arrives or against an older daemon that never sent it.

## Environment variables

| Variable | Default | Purpose |
|---|---|---|
| `TURBOFIG_MCP_PORT` | 18846 | HTTP MCP port |
| `TURBOFIG_WS_PORT` | 18847 | Plugin WebSocket port |
| `TURBOFIG_REQUEST_TIMEOUT_MS` | 30000 | Wait for a plugin reply before returning a timeout result. Clamped to 600000 (10 min); a larger value is logged and clamped, since it would otherwise overflow a JS `setTimeout` on the plugin side |
| `TURBOFIG_BRIDGE_DIR` | `~/.turbofig` | File-bridge inbox and outbox root |
| `TURBOFIG_SUPERVISED` | unset | Set by the launchd plist `turbofig autostart on` writes; enables the supervised-restart loop above |
| `TURBOFIG_LAUNCH_AGENTS_DIR` | `~/Library/LaunchAgents` | Overrides where `autostart`/`uninstall` read and write the plist; a test seam, not meant for normal use |
