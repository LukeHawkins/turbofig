<div align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/wordmark-dark.png">
    <source media="(prefers-color-scheme: light)" srcset="docs/wordmark-light.png">
    <img alt="turbofig: free Figma bridge for Claude Code, Cursor, Codex and Copilot, no MCP server needed" src="docs/wordmark-light.png" height="80">
  </picture>

  <p><strong>Let AI drive Figma. Instant, lightweight, free.</strong><br>
  <sub>No paid Figma seat. No quota. No Figma token. Free and open source, for macOS.</sub></p>

  [![CI](https://github.com/LukeHawkins/turbofig/actions/workflows/ci.yml/badge.svg)](https://github.com/LukeHawkins/turbofig/actions/workflows/ci.yml)
  [![Release](https://img.shields.io/github/v/release/LukeHawkins/turbofig)](https://github.com/LukeHawkins/turbofig/releases)
  [![License: MIT](https://img.shields.io/github/license/LukeHawkins/turbofig)](LICENSE)
  [![Platform: macOS](https://img.shields.io/badge/platform-macOS-lightgrey)](#)
</div>

<!-- DEMO VIDEO: replace with a GitHub user-attachments video URL. Drag the .mp4 into the README in GitHub's web editor to get one. Optional caption below it, only if true: <sub>Real speed, not sped up.</sub> -->

---

turbofig is a free, open-source **Figma bridge for AI agents** on macOS.
It talks to your agent through plain files, so it needs no MCP server (an
MCP mode is there if you prefer). Ask Claude Code, Cursor, Codex or
Copilot for real Figma work in plain English, and watch it build in the
file you have open.

- **Fast, start to finish.** Ready the moment your AI starts and already
  connected, so your first request lands in your file in seconds. Nothing
  to start, nothing to reconnect, no approval stops in the middle of a
  job. In a side-by-side design session, it finished in half the
  time.
- **Far lighter on tokens.** The same design session used 2.8× fewer
  tokens. Each screenshot costs about 3× fewer, and your AI learns 4
  actions instead of 121. Less of your AI plan goes on overhead, so long
  sessions go further.
- **Free to use.** No paid Figma seat, no monthly quota.

<sub>See [how turbofig compares](#how-turbofig-compares) and [the benchmark](#benchmark).</sub>

## Why turbofig

- **Easy.** A small native app, with no Node. One Homebrew command
  installs it, and `brew upgrade turbofig` updates it. Import the plugin
  into Figma once, paste the copy-prompt into your agent, and you are done. No Figma token, no sign-in, no
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
away, so big jobs move fast. It gives the AI only what it needs, so less
of every session goes on overhead. Since switching, I no longer have to
compact long design sessions in Claude Code, and I give it big jobs without
worrying about the cost.

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

## Get started

Takes a few minutes, most of it waiting for Homebrew.

You need an AI agent that can work with files on your Mac, such as Claude
Code, Cursor or Copilot CLI.

1. **Install.** New to the terminal? No coding needed: open the Terminal
   app, paste these 2 lines, and press Return after each. Need Homebrew
   first? Get it from [brew.sh](https://brew.sh).

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
   any other agent with local file access. This needs no MCP setup. See
   [docs/agents.md](docs/agents.md) for an agent-neutral version of the
   same prompt. Chat apps that run in a browser cannot reach your Mac, so
   they cannot use turbofig.

**Prefer MCP (how apps like Claude Code plug in extra tools)?** Claude
Code: `claude mcp add turbofig -- turbofig mcp`. See
[docs/mcp-clients.md](docs/mcp-clients.md) for this and other MCP clients.

## How turbofig compares

| | turbofig | figma-console-mcp (NPX) | figma-console-mcp (Cloud) | Figma for Agents |
|---|---|---|---|---|
| Tokens for one design session: a 4-step landing page with screenshots | ✓ **134K** | ✗ 375K (2.8× more) | Not tested | Not tested |
| Time for that session | ✓ **2.9 min** | ✗ 5.9 min, including approval waits | Not tested | Not tested |
| Tokens for one screenshot of the same frame | ✓ **1,225** | ✗ 3,264 (2.7× more) | Not tested | Not tested |
| Tokens for 4 small jobs, in apps that load every tool (est.) | ✓ **~57K** | ✗ ~405K (7× more) | Not tested | Not tested |
| Approval prompts in a 4-job test (tester's usual settings) | ✓ **0** | ✗ 6 | Not tested | Not tested |
| AI memory the tools take | ✓ **0.3%** | ✗ 18% | ✗ 14.5% est. (96 tools) | ✗ 10.3% (41 tools) |
| Ready when your AI starts | ✓ **Under 0.1 s** | ✗ About 2 s (30 s first run) | Hosted, no local start | Hosted, no local start |
| Cost and limits | ✓ **Free, no limits** | ✓ Free | ✓ Free | ✗ Paid Full seat to edit, read quotas, usage-based pricing coming |
| Works where MCP servers are blocked, with no MCP approval prompts | ✓ **Yes**, talks through plain files (an MCP server is there too, if you prefer) | ✗ No, it is an MCP server | ✗ No, it is an MCP server | ✗ No, it is an MCP server |
| Needs a Figma access token | ✓ **No** | ✗ Yes, you create one | ✗ Yes, you create one | Sign in with Figma |
| Extra setup | ✓ **None after one plugin import** | ✗ Node.js, MCP config, Desktop Bridge plugin | ✗ Pair the Desktop Bridge plugin | Connect your AI client |
| Runs any Figma plugin script | ✓ **Yes** | ✓ Yes | ✓ Yes | ~ Partly: no images or custom fonts |

**Bottom line:** if you work in Figma Desktop, turbofig is the only option
here that needs no MCP server, no token and no extra setup. It gives your
AI full scripting power with the lightest load, for free.

<sub>Measured: one run each, same prompts and model; time spent recovering from tool failures is left out on both sides; the cost gap is smaller than the token gap because both re-read a similar fixed context on every call; see [Benchmark](#benchmark). est. = the measured work plus each tool list re-read on each of 10 AI calls. Repeated tool lists are cached, so the cost gap is smaller than the token gap, and Claude Code [looks tools up on demand](https://code.claude.com/docs/en/agent-sdk/tool-search) once they pass 10% of its memory. AI memory = share of a 200K-token context, counted with an OpenAI tokenizer; Figma for Agents measured through Copilot CLI. Methods and dated sources: [docs/comparison.md](docs/comparison.md).</sub>

## Benchmark

The same jobs, prompts and model (Claude Sonnet 5), run once with each
tool. Every turbofig result was read back from Figma and matched the spec.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/benchmark-overview-dark.svg">
  <img src="docs/benchmark-overview-light.svg" width="860" alt="Measured side by side. Same prompts, same model (Claude Sonnet 5), one run each, lower is better. Tokens for one design session, a 4-step landing page with a screenshot after each step: turbofig 134,000, figma-console-mcp 375,000 (2.8 times more). Billed cost for that session, in input-token units: turbofig 228,000 total (89,400 for the work, 138,600 for the fixed context), figma-console-mcp 350,000 total, 35 percent more (163,600 for the work, 186,200 for the fixed context). Time for that session: turbofig 2.9 minutes, figma-console-mcp 5.9 minutes including approval waits. Tokens for one screenshot of the same frame at default settings: turbofig 1,225 at 1200 pixels, figma-console-mcp 3,264 at 2000 pixels. Session figures leave out time spent recovering from tool failures, on both sides.">
</picture>

- **Design session.** Build a landing page in 4 steps and check a
  screenshot after each one. turbofig used 2.8× fewer tokens (134K
  against 375K) and about a third less in billed cost, and finished in
  2.9 minutes against 5.9. Time spent recovering from tool failures is
  left out on both sides.
- **Why.** Each figma-console-mcp screenshot cost about 3× more and stayed
  in the conversation, so every later call re-read it. It also needed
  more steps: 24 AI calls against 17.
- **Small jobs without screenshots.** Both cost about the same: the same
  AI writes the same kind of script. There, turbofig's edge is no
  approval stops, a start in under 0.1 s, and connecting on the first
  try.

One run each, so treat the results as a guide. Evidence:
[design session](bench/results/side-by-side-20261008-095818/interactive/session-test.md), [recount](bench/results/side-by-side-20261008-095818/interactive/recount.md), [screenshot test](bench/results/side-by-side-20261008-095818/interactive/screenshot-test-2.md),
[results and method](bench/results/side-by-side-20261008-095818/interactive/results.md), [spec check](bench/results/side-by-side-20261008-095818/interactive/verification.json),
[figma-console-mcp log](bench/results/side-by-side-20261008-095818/interactive/runs/console.jsonl). The turbofig logs stay
private because they contain unrelated personal context.

## FAQ

### How do I connect Claude Code to Figma?

Install turbofig, import the plugin into Figma Desktop once, then paste
the copy-prompt into Claude Code. It drives Figma through plain files, with
no MCP setup. Prefer MCP? Run `claude mcp add turbofig -- turbofig mcp`.
See [Get started](#get-started).

### Is there a free alternative to Figma's own MCP server?

Yes. Figma's MCP server needs a paid Full seat to edit, and View or Collab
seats get 20 read calls a month, on every plan. turbofig needs no paid
seat, has no quota and needs no MCP server. In MCP mode, its tool list is
about 30× smaller. See
[docs/comparison.md](docs/comparison.md).

### How is turbofig different from figma-console-mcp?

turbofig gives your AI 4 tools instead of 121, needs no Node.js and no
Figma access token, and is always running. In a side-by-side design
session it used 2.8× fewer tokens and finished in half the time. See
[Benchmark](#benchmark).

### Does it work with Cursor, Codex, Copilot or other agents?

Yes. Any agent that can read and write files on your Mac can use the file
bridge, and any MCP client can use `turbofig mcp`. See
[docs/mcp-clients.md](docs/mcp-clients.md) and [docs/agents.md](docs/agents.md).

### Can it edit designs, or only read them?

Both. turbofig runs any script a Figma plugin could run: frames, text,
components, variants, variables and styles.

### Does it work in the Figma web app, or on Windows?

No. turbofig needs Figma Desktop on macOS.

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
