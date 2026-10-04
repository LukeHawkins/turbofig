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
- **By design, any local process running as the same user can call the
  daemon.** This matches the trust model of other local MCP servers. There
  is no HTTP token, so a local privilege boundary (a different user account,
  or a sandboxed process) is the only thing that stops another local
  process from driving the daemon.
- **The file-bridge directories are mode `0700`.** `~/.turbofig`, its
  `inbox/`, and its `outbox/` are created and corrected to `0700` on every
  daemon start, because job and result files can carry arbitrary eval code
  and its output.

Report a finding that breaks one of these guarantees (for example, a port
that becomes reachable from the network, an `Origin` check that can be
bypassed, or a file-bridge directory with loose permissions). A report that
only restates "the execute tool runs arbitrary code" is not a vulnerability;
that is the documented design.
