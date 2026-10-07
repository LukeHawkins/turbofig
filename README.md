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

- **Fast, start to finish.** It runs on your Mac and stays connected to
  Figma, so every request goes straight to your file: no server to start,
  no cloud round trip. One request can make many changes at once.
- **Far more token-efficient, so far cheaper to run.** Your AI gets a
  short list of 4 actions instead of a long manual, and screenshots stay
  small. The same Figma work uses much less of your AI plan, and long
  sessions last much longer before they need compacting.
- **Free to use.** No paid Figma seat, no monthly quota.

<sub>See [how turbofig compares](#how-turbofig-compares) and [how we measured](bench/results/session-cost-20261007-144824.md).</sub>

## Why turbofig

- **Easy.** A small native app, with no Node. One Homebrew command
  installs it, and `brew upgrade turbofig` updates it. Import the plugin into Figma once, paste the copy-prompt
  into your agent, and you are done. No Figma token, no sign-in, no
  shared billing, so a whole team or class can run it side by side.
- **Connects by itself, stays connected.** Open the plugin and it finds
  turbofig. No restarting plugins or gateways to get a connection. If the
  link drops, it reconnects on its own.
- **Screenshots stay small.** Screenshots are saved as files and shrunk
  to at most 1200 px, so they do not fill up your AI's memory. Full size
  is there when your AI asks for it.
- **Multi-file.** Several files and several agent sessions can run at
  once, handy when you juggle clients.
- **No approval clicks, if your setup asks for them.** Some setups ask you
  to approve every tool call, or block adding MCP servers. turbofig talks
  to your AI through plain files, so it avoids both and a long job can
  run without you. See [docs/reliability.md](docs/reliability.md).

## Why I made it

I build in Figma with AI agents every day, and two things kept getting in
the way: speed and cost. The bridges I tried were slow to respond, and
they spent a big part of my AI plan just loading their tools before any
work began, so long sessions filled up and needed compacting. In my setup
they also often needed restarts before they would connect.

turbofig is the bridge I wanted. It is always on and answers straight
away, so big jobs move fast. It gives the AI only what it needs, so the
same work costs far less. Since switching, I no longer have to compact
long design sessions in Claude Code, and I give it big jobs without
worrying about the cost.

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

**Prefer MCP (how apps like Claude Code plug in extra tools)?** Claude
Code: `claude mcp add turbofig -- turbofig mcp`. See
[docs/mcp-clients.md](docs/mcp-clients.md) for this and other MCP clients.

## Works with

- **Agents:** Cursor, Copilot CLI, and any other agent that can read and
  write local files use the file bridge: click **Copy agent prompt**, no
  MCP setup needed. An MCP client that starts its own local process, such
  as Claude Code, can instead run `turbofig mcp`.
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

turbofig can run any script a Figma plugin could run, so variables,
components, component sets and styles all work. Scriptable too: any process that
can write a job file to the bridge folder can drive turbofig, not only a
chat agent.

## How turbofig compares

| | turbofig | figma-console-mcp | Figma for Agents (Figma's MCP server) |
|---|---|---|---|
| Tool list size, as a share of AI memory (if loaded up front) | **0.3%** (about 680 tokens) | 18% (about 36,600 tokens) | Not published |
| Ready when your agent starts | **Under 0.1 s** (always running) | About 2 s (about 30 s on a first run) | Hosted, not measured |
| Runs any Figma plugin script (advanced work) | **Yes** | Yes | Partly: no images or custom fonts, 20 KB output per call |
| Cost | **Free** | Free | Editing needs a paid Full seat; write tools free in beta, later usage-based |
| Usage limits | **None** | None stated | Read tools: 20 calls a month (Starter) up to 600 a day (Enterprise) |
| Setup | Homebrew + one plugin import | Node.js (or Cloud Mode), a Figma access token, the Desktop Bridge plugin | Sign in with Figma |
| Several files at once | Yes | Yes | Not stated |
| Comments | No | Yes | Not stated |
| Works without Figma Desktop open | No | Partly | Yes |

<sub>AI memory: share of a 200K-token context window. Tokens are approximate,
counted with OpenAI's tokenizer because Claude's is not public; exact byte
sizes are 2.8 KB against 163 KB. Speed and cost per task will be added
after the full benchmark. Methods and raw data:
[tool tokens](bench/results/tool-tokens-20261007-221725.md),
[startup and size](bench/results/session-cost-20261007-144824.md).
Competitor facts and sources, dated: [docs/comparison.md](docs/comparison.md).</sub>

**Why tool count matters.** Many AI agents read the description of every
tool they have before doing any work, in every session, and you pay for
that reading. Loaded up front, 121 tools fill almost a fifth of a
200K-token memory. Claude Code avoids that by switching to looking tools
up on demand once they pass 10% of its memory
([docs](https://code.claude.com/docs/en/agent-sdk/tool-search)), which
adds a search step each time it needs a tool. turbofig needs neither: its
4 tools load up front at 0.3%, and one script does the work.

**When to use something else.** If you need comments, use
figma-console-mcp. If you must work without Figma Desktop open, use Figma
for Agents.

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
