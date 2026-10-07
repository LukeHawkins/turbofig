<div align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/wordmark-dark.png">
    <source media="(prefers-color-scheme: light)" srcset="docs/wordmark-light.png">
    <img alt="turbofig wordmark" src="docs/wordmark-light.png" height="80">
  </picture>

  <p><strong>Let AI drive Figma. Instant, lightweight, free.</strong><br>
  <sub>No paid Figma seat. No quota. No Figma token. Free and open source, for macOS.</sub></p>

  [![CI](https://github.com/LukeHawkins/turbofig/actions/workflows/ci.yml/badge.svg)](https://github.com/LukeHawkins/turbofig/actions/workflows/ci.yml)
  [![Release](https://img.shields.io/github/v/release/LukeHawkins/turbofig)](https://github.com/LukeHawkins/turbofig/releases)
  [![License: MIT](https://img.shields.io/github/license/LukeHawkins/turbofig)](LICENSE)
  [![Platform: macOS](https://img.shields.io/badge/platform-macOS-lightgrey)](#)
</div>

<!-- DEMO VIDEO: replace with a GitHub user-attachments video URL. Drag the .mp4 into the README in GitHub's web editor to get one. -->

---

Ask your AI for real Figma work in plain English, and watch it build in
the file you have open.

- **Starts instantly.** Ready in under a tenth of a second.
  figma-console-mcp takes about 2 seconds each session, and about 30 on a
  first run.
- **58× lighter.** Your AI reads 58 times less setup before it starts, so
  more of your AI plan goes into the actual design.
- **Free to use.** No paid Figma seat, no monthly quota.

<sub>[How we measured](bench/results/session-cost-20261007-144824.md)</sub>

## Why turbofig

> From the creator: since switching to turbofig, I no longer have to
> compact long design sessions in Claude Code, and big jobs finish much
> faster.

- **Fast.** turbofig is always running in the background, so there is no
  start-up wait, and one call can carry out many Figma operations at
  once.
- **No paid seat, no quota.** Figma's own official AI connector (an MCP
  server) needs a paid Full seat to edit files with agents, and caps read
  tools at 20 calls a month on the Starter plan, 200 a day on
  Professional. turbofig needs neither. See [How turbofig
  compares](#how-turbofig-compares). No shared login or billing either, so
  a whole team or class can run it side by side.
- **Easy.** One Homebrew command installs it, and `brew upgrade turbofig`
  updates it. Import the plugin into Figma once, paste the copy-prompt
  into your agent, and you are done. No Node, no Figma token, no sign-in.
- **Token-light.** Your agent loads 4 tools instead of 121, so long
  sessions fill up more slowly.
- **Connects by itself, stays connected.** Open the plugin and it finds
  turbofig. No restarting plugins or gateways to get a connection. If the
  link drops, it reconnects on its own.
- **Screenshots stay small.** A screenshot is saved to a file by default,
  so your agent gets a path, not image data, and it is shrunk so its
  longest edge is at most 1200 px unless the agent asks for full
  resolution.
- **Multi-file.** Several files and several agent sessions can run at
  once, handy when you juggle clients.
- **No approval clicks, if your setup asks for them.** Some setups ask you
  to approve every tool call, or block adding MCP servers. The file bridge
  avoids both, so a long job can run without you. See
  [docs/reliability.md](docs/reliability.md).

## Get started

Takes a few minutes, most of it waiting for Homebrew.

You need an AI agent that can work with files on your Mac, such as Claude
Code, Cursor or Copilot CLI.

1. **Install.** New to the terminal? No typing needed: open the Terminal
   app, paste each line below, and press Return. Need Homebrew first? Get
   it from [brew.sh](https://brew.sh).

   ```bash
   brew install LukeHawkins/tap/turbofig
   turbofig
   ```

   This installs and opens `turbofig.app` and starts it in the background.
   A tf icon appears in your menu bar. Click it and choose **About
   turbofig…** to get started.

   <p align="center">
     <img src="docs/menu-bar.png" alt="The turbofig menu-bar menu" width="185" valign="top">
     &nbsp;&nbsp;
     <img src="docs/about-window.png" alt="The turbofig About window" width="442" valign="top">
   </p>

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

**Prefer MCP?** Claude Code: `claude mcp add turbofig -- turbofig mcp`. See
[docs/mcp-clients.md](docs/mcp-clients.md) for this and other MCP clients.

## Works with

- **Agents:** Cursor, Copilot CLI, and any other agent that can read and
  write local files use the file bridge: click **Copy agent prompt**, no
  MCP setup needed. An MCP client that starts its own local process, such
  as Claude Code, can instead run `turbofig mcp`. Any MCP client with
  streamable HTTP works too, as an advanced fallback.
- Chat apps that run in a browser cannot reach your Mac, so they cannot use
  turbofig.

## What you can ask it

- Build a pricing page frame from my existing components.
- Resize one master frame into a full set of ad and social formats.
- Swap copy across dozens of frames and keep every text style intact.
- Rename or restyle hundreds of layers to match a naming rule.
- Create a set of button components with size and state variants.
- Update a text style or a color variable everywhere it is used.
- Make a dark-mode variant of this frame.
- Lay out a slide deck or a storyboard from a text outline.
- Generate a grid of frames from a CSV or JSON list.
- Match a coded component's spacing, colors and type in Figma so design
  and code agree.

Any instruction a Figma Plugin API script can carry out works, because
`turbofig_execute` runs that script directly, including work on variables,
components, component sets, and styles. Scriptable too: any process that
can write a job file to the bridge folder can drive turbofig, not only a
chat agent.

## How turbofig compares

| | turbofig | Figma's MCP server | figma-console-mcp |
|---|---|---|---|
| Needs a paid Figma seat to edit | No | Yes, a Full seat (Dev seats are read-only) | No |
| Usage limits | None | Read tools: 20 calls/month (Starter, View/Collab), 200/day (Professional, Organization), 600/day (Enterprise) | See their docs |
| What you need | Figma Desktop + plugin. No token, no sign-in, no Node | OAuth sign-in | Figma Desktop + Desktop Bridge, a Figma personal access token, Node.js (not needed in Cloud Mode) |
| Tool definitions your agent loads every session | **4 tools, 2.8 KB** | Not published | 121 tools, 163 KB (NPX mode; 96 tools in Cloud Mode) |
| Ready to use after the agent starts | **About 66 ms** (already running in the background) | Hosted remote server, not measured | About 1.9 s: npx starts a fresh process each session (29 s on a first run) |
| Several files at once | Yes | Not stated in their docs | Yes |
| Works without adding an MCP server | Yes, file bridge | No | No |
| Screenshots kept small by default | Yes, file + 1200 px | Not stated in their docs | Not stated in their docs |
| Comments, or work without Figma open | No | Without Figma open: yes (remote server) | Comments: yes (REST tools) |

turbofig has no quota and needs no paid seat. If you need comments,
figma-console-mcp's REST tools cover them. If you must work without Figma
Desktop open, Figma's remote server can. Full comparison, with sources:
[docs/comparison.md](docs/comparison.md).

Fewer tools matters because many agents load every tool's name,
description, and settings into context at the start of a session, before
any work starts: Anthropic measured about 72K tokens to load 50+ MCP tools
up front ([Anthropic, "Advanced tool
use"](https://www.anthropic.com/engineering/advanced-tool-use)). Some
newer clients defer tools behind a search step instead, which adds a
lookup on every use. Either way, more tools means more for the agent to
choose between. turbofig gives the agent 4 tools and lets one script do
the work.

Tool size and startup time are measured: see
[bench/results](bench/results/session-cost-20261007-144824.md) for the
method and raw data, and run `bench/mcp-stdio-probe.mjs` to check them
yourself. Sizes are exact bytes, not tokens: a token count needs a real
agent session, which the full benchmark will measure. Speed and token
cost per edit will be added after the full benchmark run.

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
