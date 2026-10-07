<div align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/wordmark-dark.png">
    <source media="(prefers-color-scheme: light)" srcset="docs/wordmark-light.png">
    <img alt="turbofig wordmark" src="docs/wordmark-light.png" height="80">
  </picture>

  <p><strong>Let AI drive Figma. Fast, light on tokens, hands-off.</strong><br>
  <sub>Bridge any AI to Figma, on macOS.</sub></p>

  [![CI](https://github.com/LukeHawkins/turbofig/actions/workflows/ci.yml/badge.svg)](https://github.com/LukeHawkins/turbofig/actions/workflows/ci.yml)
  [![Release](https://img.shields.io/github/v/release/LukeHawkins/turbofig)](https://github.com/LukeHawkins/turbofig/releases)
  [![License: MIT](https://img.shields.io/github/license/LukeHawkins/turbofig)](LICENSE)
  [![Platform: macOS](https://img.shields.io/badge/platform-macOS-lightgrey)](#)
</div>

<!-- DEMO VIDEO: replace with a GitHub user-attachments video URL. Drag the .mp4 into the README in GitHub's web editor to get one. -->

<p align="center">
  <img src="docs/menu-bar.png" alt="The turbofig menu-bar menu" width="185" valign="top">
  &nbsp;&nbsp;
  <img src="docs/about-window.png" alt="The turbofig About window" width="442" valign="top">
</p>

---

turbofig is a small menu-bar app that lets your AI agent work in the
Figma file you have open. Install it once, open the file, and give your
agent a brief. It builds, edits, and checks real Figma work while you walk
away.

## Why turbofig

> From the creator: since switching to turbofig, I no longer have to
> compact long design sessions in Claude Code, and big jobs finish much
> faster.

- **Fast.** turbofig runs all the time in the background, so your agent
  never waits for a server to start. One call can carry out many Figma
  operations at once.
- **Easy.** Install with Homebrew, run `turbofig`, import the plugin into
  Figma once, and paste the copy-prompt into your agent. No Figma account
  token or sign-in.
- **Token-light.** turbofig gives your agent only 4 tools, so the tool
  schema it loads every session is small. See [How turbofig
  compares](#how-turbofig-compares).
- **Keeps screenshots out of your agent's context.** A screenshot is saved
  to a file by default, so your agent gets a path, not image data, and it
  is shrunk so its longest edge is at most 1200 px unless the agent asks
  for full resolution. A screenshot never floods the conversation or wastes
  tokens.
- **Built to walk away from.** Give your agent a big task and leave it
  running: it keeps working on its own, so you do not watch or approve
  each step. See [docs/reliability.md](docs/reliability.md). It needs no
  MCP server, so it also works where adding one is restricted.
- **Multi-file.** Run the plugin in several Figma files, and run several
  agent sessions at once, one per file.

**Works with:**

- **Platform:** macOS (Apple Silicon and Intel). Windows and Linux are not
  supported yet.
- **Agents:** Cursor, Copilot CLI, and any other agent that can read and
  write local files use the file bridge: click **Copy agent prompt**, no
  MCP setup needed. An MCP client that starts its own local process, such
  as Claude Code, can instead run `turbofig mcp`. Any MCP client with
  streamable HTTP works too, as an advanced fallback.
- Chat apps that run in a browser cannot reach your Mac, so they cannot use
  turbofig.

## Get started

1. **Install.** New to the terminal? Open the Terminal app, paste each line
   below one at a time, and press Return. Need Homebrew first? Get it from
   [brew.sh](https://brew.sh).

   ```bash
   brew install LukeHawkins/tap/turbofig
   turbofig
   ```

   This installs and opens `turbofig.app` and starts it in the background.
   A tf icon appears in your menu bar. Click it and choose **About
   turbofig…** to get started.

2. **Add the plugin to Figma (once).** In the About window, click **Show
   plugin folder**. In Figma Desktop, choose **Plugins > Development >
   Import plugin from manifest…**, then drag `manifest.json` from the
   Finder window onto the dialog. This menu item exists only in Figma
   Desktop, not the web app. Then open a file and run turbofig from the
   Plugins menu once, and leave the panel open.

3. **Connect your agent.** Click **Copy agent prompt** in the About window
   or the plugin panel, and paste it into Claude Code, Cursor, Copilot, or
   any other agent with local file access. This works with no MCP setup
   and no permission dialog. See [docs/agents.md](docs/agents.md) for an
   agent-neutral version of the same prompt.

**Using Claude Code's own MCP support instead?**

```bash
claude mcp add turbofig -- turbofig mcp
```

See [docs/mcp-clients.md](docs/mcp-clients.md) for this and other MCP
clients.

## What you can ask it

- Build a pricing page frame from my existing components.
- Tidy the auto layout spacing and padding across a whole page.
- Rename layers across a page to match a naming rule.
- Create a set of button components with size and state variants.
- Update a text style or a color variable everywhere it is used.
- Make a dark-mode variant of this frame.
- Lay out a slide deck from a text outline.
- Screenshot a frame so the agent can check its own work before you look.

Any instruction a Figma Plugin API script can carry out works, because
`turbofig_execute` runs that script directly, including work on variables,
components, component sets, and styles.

## How turbofig compares

| | turbofig | Figma's MCP server | figma-console-mcp |
|---|---|---|---|
| Edits your canvas | Yes | Yes (remote server) | Yes (NPX/Local and Cloud modes) |
| What you need | Figma Desktop + plugin running. No token, no sign-in, no Node | OAuth sign-in. Desktop server also needs Figma Desktop + a paid seat | Node.js (NPX/Local) or none (Cloud); Figma Desktop + Desktop Bridge; a Figma personal access token |
| Tools your agent loads | 4 | Not listed | 96-121, depending on mode |
| Works without adding an MCP server | Yes, with the file bridge | No | No |
| Screenshots kept small by default | Yes: saved to a file, downscaled to 1200 px | See their docs | See their docs |

Fewer tools matters because many agents load every tool's name,
description, and settings into context at the start of a session, before
any work starts: Anthropic measured about 72K tokens to load 50+ MCP tools
up front ([Anthropic, "Advanced tool
use"](https://www.anthropic.com/engineering/advanced-tool-use)). Some
newer clients defer tools behind a search step instead, which adds a
lookup on every use. Either way, more tools means more for the agent to
choose between. turbofig gives the agent 4 tools and lets one script do
the work.

**When to use something else.** If you need comments, use
figma-console-mcp's REST tools. If you need to work without Figma Desktop
open, use Figma's remote MCP server. Full comparison, with sources:
[docs/comparison.md](docs/comparison.md).

## For developers

- [docs/how-it-works.md](docs/how-it-works.md): architecture overview.
- [docs/tools.md](docs/tools.md): the 4 tools, with `turbofig_execute`
  examples.
- [docs/mcp-clients.md](docs/mcp-clients.md): Claude Code and other MCP
  client setup, including the HTTP fallback.
- [docs/commands.md](docs/commands.md): the full CLI command table.
- [docs/configuration.md](docs/configuration.md): environment variables
  and ports.
- [docs/menu-bar.md](docs/menu-bar.md): the menu bar and its windows.
- [docs/reliability.md](docs/reliability.md): restart and retry behaviour.
- [docs/troubleshooting.md](docs/troubleshooting.md): common problems and
  fixes.
- [docs/comparison.md](docs/comparison.md): the full comparison, with
  sources.
- [ARCHITECTURE.md](ARCHITECTURE.md) and [DECISIONS.md](DECISIONS.md): the module layout, data flow, and the design reasoning.
- [skills/file-bridge.md](skills/file-bridge.md): the file-bridge protocol.

## Security

A script run through `turbofig_execute` runs inside Figma's own plugin
sandbox: it can change the open file, but it cannot read files on your
Mac. The plugin is allowed network access, because Figma rejects a rule
that allows only one port. Release binaries are not notarized; Homebrew
checks a SHA256 checksum before installing. See
[SECURITY.md](SECURITY.md) for the full threat model and how to report a
vulnerability.

Run `turbofig uninstall` before `brew uninstall turbofig`. Homebrew alone
leaves `turbofig.app` and the login item in place.

## Roadmap

- A screencast of a brief becoming a full Figma page.
- Wider platform support: Linux and Windows builds, `cargo install`, and a
  `curl | sh` installer.
- A community-safe command vocabulary alongside the eval-first tool.
- An optional job audit log.
- Notarized binaries, for a company that needs them.
- Narrow the plugin's network access to the daemon's own local port.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup, checks, and the PR
process, and [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) for the project's
code of conduct. Maintained by one person; issues and PRs are reviewed
best-effort.

## License

MIT. See [LICENSE](LICENSE).

turbofig is an independent project. It is not affiliated with or endorsed
by Figma, Inc.
