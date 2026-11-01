# Security policy

## Reporting a vulnerability

Report a vulnerability through [GitHub private vulnerability
reporting](https://github.com/LukeHawkins/turbofig/security/advisories/new)
on this repository. This keeps the report private until a fix ships.

If you cannot use GitHub's reporting tool, email hi@lukehawkins.eu instead.

Do not open a public issue for a security problem.

## Threat model

turbofig runs one local daemon that bridges an AI client to a Figma plugin.
Read this before you report a finding, so you know what is a design choice
and what is a bug.

- **`turbofig_execute` runs arbitrary Figma Plugin API JS by design.** This
  is the product. Any client that can call this tool has full script access
  to the open Figma file. turbofig does not sandbox or restrict the JS it
  runs.
- **Both ports bind to `127.0.0.1` only.** The HTTP MCP endpoint and the
  WebSocket server never listen on a network-reachable interface.
- **The HTTP MCP port rejects any request carrying an `Origin` header with
  403.** A real MCP client (curl, a native MCP client, the file-bridge)
  never sends an `Origin` header; only a browser does. This stops a
  malicious web page from calling the daemon over `fetch()`. The `Host`
  header is also allow-listed to `localhost`, `127.0.0.1`, and `::1`, which
  stops a DNS-rebinding attack.
- **The WebSocket port accepts only a null or missing `Origin`.** The Figma
  plugin UI runs in a sandboxed iframe, which reports a null origin, so the
  real plugin still connects. Any other `Origin` is rejected at the upgrade.
- **The WebSocket upgrade also requires proof of the pairing token.** Origin
  checking alone cannot tell the real Figma plugin UI apart from a sandboxed
  `<iframe>` on a malicious web page: both report Origin `null`. The plugin
  connects with a nonce, so the token never goes on the wire. See "Pairing
  token" below.
- **Clients verify the daemon before they send the token.** `GET /health`
  signs a client nonce. The CLI, the stdio proxy (before every job) and the
  menu bar check the proof first. A foreign listener on the port makes them
  exit non-zero with a clear message, and they never send it the token.
- **The daemon limits connections.** It holds at most 64 HTTP and 32 WS
  connections. It closes an idle connection (30 s HTTP, 10 s WS before the
  upgrade). At most 8 WS handshakes may be pending, each for 2 s at most; a new one closes the oldest.
  `/control` checks the token before it reads the body.
- **The daemon refuses an unsafe bridge path at start.** A symlinked or
  foreign-owned bridge dir, `inbox/`, `outbox/`, `token` or `figma-plugin/`,
  a `token` with group or other bits, or an unset `HOME` stops the start.
  Under launchd the refusal exits 0, so launchd does not restart it. Every
  parent of the bridge dir must belong to the user or root, and must not be
  group or other writable unless it has the sticky bit. The bridge skips a
  job file that another user owns. It prunes the outbox only when it is a
  real folder, owned by the user, with the `.turbofig-outbox` marker.
- **The file-bridge rejects hostile inbox entries.** Job reads use
  `O_NOFOLLOW|O_NONBLOCK`. A FIFO or a symlink is rejected. Jobs are capped
  at 30 MiB and ids at 200 bytes. Results and PNGs are mode `0600`. The
  daemon creates `.tmp` files with `O_EXCL`, and the 24 h prune touches only
  daemon-named files.
- **The app bundle and launchd paths are checked.** The daemon trusts
  `turbofig.app` only when the user or root owns it, and never overwrites a
  foreign one. It calls system binaries by absolute path. The dev build
  writes `dist/ui.html` with mode `0600`, because it embeds the token.
- **`POST /job` and `POST /mcp` require the pairing token, like `POST
  /control`.** A caller must send `Authorization: Bearer <pairing token>`.
  This closes the gap that an earlier version of this file documented as
  open: another macOS account on the same Mac can reach `127.0.0.1`, but it
  cannot read a different user's `~/.turbofig/token`, so it can no longer
  drive the Figma plugin through the HTTP port. A missing or wrong token
  gets 401 before the request does anything else.
- **`GET /health` splits its payload by the same token.** Without a valid
  token it returns only `version` and `uptimeSeconds`: enough for a caller
  to confirm the daemon is alive. With a valid token it also returns the
  connected-files list and the daemon's `pid`. `/health` never fails with
  401; the reduced payload is the point, not an error.
- **Any local process running as the same user that already holds the
  token can still call the daemon.** This matches the trust model of other
  local MCP servers: the token stops a different account or a sandboxed
  process from reading it, not a process that already runs as the owner
  and can read `~/.turbofig/token` for itself.
- **The file-bridge directories are mode `0700`.** The default home
  `~/.turbofig` is created and corrected to `0700` on every daemon start,
  because job and result files can carry arbitrary eval code and its output.
  A custom `TURBOFIG_BRIDGE_DIR` folder keeps whatever mode it already has;
  only its `inbox/` and `outbox/` are created and corrected to `0700`.

## Residual risks

These risks remain. They are known and accepted.

- **A relay across ports is blocked.** Both daemon proofs name the port the
  daemon is bound to, and each client checks it against the port it connected
  to. A relay on the same port is not possible: the attacker cannot hold the
  port the daemon has bound.
- **Same-user processes are out of scope.** A process that runs as you can
  read `~/.turbofig/token` and call the daemon.
- **The legacy same-user path uses `lsof`.** A client restarts an older
  daemon that has no proof support after an `lsof` uid check. It sends that
  listener only the stop or restart control call, never a job.
- **A sub-millisecond race exists.** An attacker that takes the port after
  the proof check and before a job on a new connection can receive that job.
  This needs a same-user process or a port swap in that window.
- **Idle keep-alive slots can fill.** A local process can hold the 64 HTTP or
  32 WS slots until the idle timers close them, and so delay other clients.
- **`allowedDomains: ["*"]` stays.** Figma rejects a wildcard port, so
  allow-all is the only value that covers a configurable daemon port.
- **MCP curl users must run `turbofig status` first.** A raw `curl` with the
  bearer token does not verify the daemon. `turbofig status` does.
- **Release binaries are unsigned.** See "Data handling" below.
- **The release workflow has `contents: write`.** We accept this because
  only the owner can open same-repo pull requests.
- **Use a fine-grained token for the tap.** Make `HOMEBREW_TAP_TOKEN` a
  fine-grained PAT scoped to the tap repository only.

## Pairing token

The WebSocket port (18847) needs proof of the pairing token on the upgrade:
the plugin connects with `?nonce=`, the daemon sends `CHALLENGE` (an HMAC
proof), the plugin verifies it and answers `AUTH`. A legacy `?token=` upgrade
from an old plugin is still accepted. The token matches `~/.turbofig/token`.
This closes a gap Origin checking
alone leaves open: a sandboxed `<iframe>` on any web page reports Origin
`null`, the same value the real Figma plugin UI reports, so a malicious page
could otherwise open the socket and receive the AI's jobs.

- **Where it lives:** `~/.turbofig/token` (or `<TURBOFIG_BRIDGE_DIR>/token`
  when that env var is set). 32 random bytes, hex-encoded, mode `0600`.
  Created on first daemon start; never overwritten by a later start.
- **Who has it:** the daemon (reads it at startup) and the Figma plugin UI
  (built in with `write_plugin_files`, or injected locally by
  `plugin/build-ui.ts` for a manual dev install; see `CONTRIBUTING.md`). It
  is never logged, never returned in `turbofig_status` or `GET /health`, and
  never appears in any error message. `build.rs` also normalizes a local
  `dist/ui.html` back to the `__TURBOFIG_PAIRING_TOKEN__` placeholder before
  embedding it into the daemon binary, even if a contributor's own
  `~/.turbofig/token` was baked in by a local `bun run build`: otherwise that
  real token would ship inside the binary with no placeholder left for
  `write_plugin_files` to replace (`daemon/build.rs`'s `normalize_ui_html`,
  tested by `embedded::tests::embedded_ui_html_always_carries_the_placeholder_never_a_real_token`).
- **Comparison:** constant-time, so a wrong guess cannot be distinguished by
  timing from a near-miss of the same length.
- **To rotate it:** run `turbofig stop`, delete `~/.turbofig/token`, then run
  `turbofig start`. The daemon creates a new token and rewrites the plugin
  files at start. Then reopen the plugin in Figma. A clean `turbofig stop`
  leaves the daemon stopped even with autostart on (`turbofig autostart
  on`), so the `turbofig start` step is always required to finish the
  rotation.
- **Shared Mac:** each account has its own token. A second account's daemon
  that holds a port cannot read your token. Your clients verify the
  `/health` proof before they send the token, so they refuse that daemon and
  exit non-zero. The plugin never sends its token. Another account can still
  block your port. turbofig is designed for a single-user Mac.

## Data handling

- **turbofig makes no outbound network calls of its own.** The daemon only
  serves `127.0.0.1`; it never calls out to any remote service.
- **The plugin's manifest allows it to reach any domain
  (`networkAccess.allowedDomains: ["*"]`).** See "Residual risks". This is deliberate: Figma
  rejects a wildcard port, so allow-all is the only value that covers a
  user-configurable daemon port (see `DECISIONS.md` #23). The real limit is
  that JS run through `turbofig_execute` could use this to make a network
  request from inside Figma. turbofig itself still sends nothing off the
  machine; narrowing the allowed domain to the daemon's configured port is
  planned.
- **The file-bridge inbox and outbox hold job and result files, including
  eval code and its output.** The daemon prunes outbox entries (results and
  file-mode screenshot PNGs) older than 24 hours; a result nobody reads in
  that window is deleted, not kept.
- **`daemon.log` carries operational messages only:** startup, port binds,
  the bridge directory path, and errors from the MCP, WebSocket, and bridge
  tasks. It never logs job eval code or file content. Above 5 MiB it rotates
  to `daemon.log.1` at every daemon start, including a launchd relaunch
  under autostart, not only an ad-hoc detached start.
- **Release binaries are not code-signed or notarized today.** Each GitHub
  release ships through `cargo-dist` with a Homebrew installer; the
  generated formula pins a SHA256 checksum for each artifact, and
  `brew install` verifies that checksum before installing. See
  `RELEASING.md`'s "Unsigned binaries" section for the Gatekeeper caveat.
- **The Homebrew tap and the releases are built and published by GitHub
  Actions, not by hand.** The `release.yml` workflow's `plan` job also runs
  on every pull request, as a dry run with no publish step; only a tagged
  commit builds and publishes the real artifacts and their SHA256
  checksums.

## `GET /health`

`/health` (same HTTP port as `/mcp`, 18846) always answers, with no token
needed, but its payload depends on one. Without a valid pairing token it
returns only `version` and `uptimeSeconds`. A client nonce in
`X-Turbofig-Nonce` gets an `X-Turbofig-Proof` header back. With a valid token it also
returns the connected files' names, keys, plugin versions, and the daemon's
`pid`. It never includes the pairing token itself, and it is subject to the
same Origin and `Host` checks as every other route on this port (above), so
it is no more reachable from a browser or a DNS-rebinding attack than `/mcp`
is.

## `/job`, `/mcp`, and `/control`

`POST /job` (used by `turbofig mcp`'s stdio proxy), `POST /mcp` (used by a
native HTTP MCP client), and `POST /control` (used by `turbofig stop` and the
upgrade restart) bind to `127.0.0.1` only, like every other route, and carry
the same Origin and `Host` checks as `/health`. All three also require the
pairing token as a Bearer auth header: a request without the correct token
is rejected with 401 before it can run a job, call a tool, or stop or
restart the daemon.

Report a finding that breaks one of these guarantees (for example, a port
that becomes reachable from the network, an `Origin` check that can be
bypassed, or a file-bridge directory with loose permissions). A report that
only restates "the execute tool runs arbitrary code" is not a vulnerability;
that is the documented design.

## Menu-bar app: the About and Settings windows' webviews

The macOS menu-bar app (`daemon/src/menu_bar/`) opens 2 native windows, each
hosting its own `wry` webview: About (About turbofig…, or automatically on
first use) and Settings (Settings…). Both are deliberately narrow, and both
follow the same rules below unless noted otherwise.

- **Each loads exactly 1 embedded page, never a URL.** `about_window.rs`/
  `settings_window.rs` call `WebViewBuilder::with_html` on a string
  `include_str!`'d from `daemon/assets/about/about.html` /
  `daemon/assets/settings/settings.html` at compile time. No `with_url`, no
  network fetch, no remote resource of any kind: each page's own CSS and
  JS are inline. `about_state`'s `the_about_page_has_no_remote_urls_except_the_docs_link`
  test asserts no `src="http`/`href="http` appears anywhere in `about.html`
  except the one deliberate exception, the "Docs" link's own `href` to the
  GitHub repo; `settings_state`'s `the_settings_page_has_no_remote_urls`
  test asserts the same with no exception at all for `settings.html` (it
  has no "Docs" link).
- **A navigation handler blocks every navigation away from that page.**
  `about_window::navigation_is_allowed` allows only the initial `about:`
  load `with_html` itself performs; the "Docs" link's `onclick` intercepts
  the click first (prevents the default navigation and sends `open_docs`
  over IPC instead), and the navigation handler is a backstop that opens
  that one URL externally (through the `AppOpener` seam, i.e. `open <url>`,
  never inside the webview) and cancels the in-webview navigation either
  way. The Settings window's navigation handler is the same backstop with
  no exception at all (no link in that page ever needs one). Nothing else
  ever reaches an `Allow` decision in either window.
- **Each webview can only ever send 1 of 6 fixed IPC commands.** The
  About page's JS calls `window.ipc.postMessage("<command>")`;
  `about_state::parse_ipc_command` accepts exactly `copy_manifest_path`,
  `reveal_manifest`, `copy_agent_prompt`, `copy_mcp_command`,
  `copy_mcp_json`, `open_docs`, and `page_ready`, rejecting anything else
  (including a reasonable-looking payload like `{"op":"quit"}` or an
  unknown command) with no action taken. The Settings page sends 1 of its
  own 6: `start_at_login_on`, `start_at_login_off`, `copy_manifest_path`,
  `open_plugin_folder`, `open_log`, `page_ready`
  (`settings_state::parse_ipc_command`, the same reject-anything-else
  rule). Neither handler ever evaluates or interprets the raw message as
  code; each is a plain string compared against its own fixed literal set.
  `reveal_manifest`/`open_plugin_folder` both run `open -R <manifest
  path>` (reveal in Finder, a fixed path under `<home>/figma-plugin/`,
  never a path taken from the page), never an arbitrary path. Neither page
  can send `quit`: Quit turbofig lives only in the tray menu now.
- **Every command that touches the clipboard or opens something goes
  through the existing `Clipboard`/`AppOpener` seams**, the same ones the
  bare `turbofig` command and the tray menu use, including their debug-build
  guard (a debug build never touches the real clipboard, Figma, Finder, or
  the browser unless `TURBOFIG_DEV_REAL_DESKTOP=1` is set).
- **`start_at_login_on`/`start_at_login_off` (Settings window only now)
  only ever toggle the app LaunchAgent**
  (`cli::run_autostart_on_app`/`run_autostart_off`, see "The app
  LaunchAgent" below); the page cannot pass any other launchd target or
  plist content.
- **`page_ready` only ever triggers a re-push of status Rust already
  holds.** It carries no data and cannot be used to request anything new;
  see `ARCHITECTURE.md`'s "Live status, and the page-ready race" for why it
  exists.
- **Devtools are debug-build-only.** `WebViewBuilder::with_devtools(cfg!(debug_assertions))`
  on both windows: a release build (what `turbofig.app` itself launches)
  never ships the Web Inspector.
- **The second-instance signal (`<home>/app.sock`) is a local Unix socket,
  mode `0600`, carrying 1 of exactly 2 fixed literal messages**
  (`second_instance::SignalMessage`): `open_about` tells the first instance
  to open or focus the About window; `quit_app` (sent only by `turbofig
  uninstall`) tells it to quit, through the same `perform_quit` path as its
  own "Quit turbofig" menu item. Neither message can run a job, read a
  file, or carry arbitrary data, and (like every other `<home>` path) only
  the owning user's account can read or connect to the socket.

### The app LaunchAgent

`turbofig autostart on` (the default, no `--headless`) writes
`~/Library/LaunchAgents/eu.lukehawkins.turbofig.app.plist`
(`launchd::app_plist_contents`): `ProgramArguments` is exactly
`[<applications_dir>/turbofig.app/Contents/MacOS/turbofig]`, the bundle's
own executable, with no extra argument; `RunAtLoad` true; `KeepAlive` plain
`false` (a user who quits the app keeps it quit until the next login,
unlike the headless service's crash-only restart). It carries the same
`TURBOFIG_*` environment carry-over as the headless plist, but never
`TURBOFIG_SUPERVISED`: the app is not `serve`, so the daemon's
supervised-restart loop never applies to it. `autostart on` and `autostart
on --headless` are mutually exclusive: turning either one on bootouts and
removes the other's plist first, so the 2 are never active together, and a
service plist can never point at an arbitrary attacker-supplied binary:
the only 2 possible `ProgramArguments` values are the bundle's fixed path
and the daemon's own stable binary path (see `ARCHITECTURE.md`'s "Stable
binary path rule").
