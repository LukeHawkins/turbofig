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
- **By design, any local process running as the same user can call the
  daemon.** This matches the trust model of other local MCP servers. The
  HTTP MCP port still has no token, so a local privilege boundary (a
  different user account, or a sandboxed process) is the only thing that
  stops another local process from driving it.
- **The file-bridge directories are mode `0700`.** `~/.turbofig`, its
  `inbox/`, and its `outbox/` are created and corrected to `0700` on every
  daemon start, because job and result files can carry arbitrary eval code
  and its output.

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
  is never logged, never returned in `turbofig_status`, and never appears in
  any error message.
- **Comparison:** constant-time, so a wrong guess cannot be distinguished by
  timing from a near-miss of the same length.
- **To rotate it:** stop the daemon, delete `~/.turbofig/token`, restart the
  daemon (it generates a fresh one), then reload the Figma plugin so it picks
  up the new token (rerun `turbofig setup` once that command exists, or
  rebuild and reimport the dev plugin manually).

Report a finding that breaks one of these guarantees (for example, a port
that becomes reachable from the network, an `Origin` check that can be
bypassed, or a file-bridge directory with loose permissions). A report that
only restates "the execute tool runs arbitrary code" is not a vulnerability;
that is the documented design.
