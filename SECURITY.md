# Security policy

## Reporting a vulnerability

Report a vulnerability through [GitHub private vulnerability
reporting](https://github.com/LukeHawkins/turbofig/security/advisories/new)
on this repository. This keeps the report private until a fix ships.

If you cannot use GitHub's reporting tool, email hi@lukehawkins.eu instead.

Do not open a public issue for a security problem.

## Threat model

Turbofig runs one local daemon that bridges an AI client to a Figma plugin.
Read this before you report a finding, so you know what is a design choice
and what is a bug.

- **`turbofig_execute` runs arbitrary Figma Plugin API JS by design.** This
  is the product. Any client that can call this tool has full script access
  to the open Figma file. Turbofig does not sandbox or restrict the JS it
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
- **The WebSocket upgrade also requires a pairing token.** Origin checking
  alone cannot tell the real Figma plugin UI apart from a sandboxed
  `<iframe>` on a malicious web page: both report Origin `null`. See
  "Pairing token" below.
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

## Pairing token

The WebSocket port (18847) requires a `token` query parameter on the upgrade
request, matching `~/.turbofig/token`. This closes a gap Origin checking
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

## Data handling

- **Turbofig makes no outbound network calls of its own.** The daemon only
  serves `127.0.0.1`; it never calls out to any remote service.
- **Design data moves only between the plugin, the daemon on `127.0.0.1`,
  and the local agent (AI client).** Nothing in that path leaves the
  machine.
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
  Actions, not by hand.** The `release.yml` workflow runs only on a tagged
  commit and publishes the artifacts and their SHA256 checksums.

## `GET /health`

`/health` (same HTTP port as `/mcp`, 18846) always answers, with no token
needed, but its payload depends on one. Without a valid pairing token it
returns only `version` and `uptimeSeconds`. With a valid token it also
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
