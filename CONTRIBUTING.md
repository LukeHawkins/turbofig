# Contributing to turbofig

Thanks for your interest in turbofig. This file covers setup, checks, and
the PR process. Read `ARCHITECTURE.md` for the module layout and data flow,
and `DECISIONS.md` for the reasoning behind the design choices.

## Prerequisites

- macOS. turbofig installs and runs on macOS only.
- Figma Desktop (not the web app).
- Rust. The pinned toolchain is in `rust-toolchain.toml` (1.94.1, with
  `rustfmt` and `clippy`). `rustup` installs it automatically when you build.
- Bun `>=1.3.10`. Never use npm, pnpm, or yarn in this repo.

## Clone and build

```bash
git clone https://github.com/LukeHawkins/turbofig.git
cd turbofig
git config core.hooksPath .githooks
cargo build
bun install
```

The `git config core.hooksPath .githooks` step enables the commit-message
hook. Do this once per clone. See "Commit messages" below.

## Run the daemon

```bash
cargo run -- serve
```

Or, against a release build: `cargo build --release`, then `turbofig start`
to run it detached, or `./target/release/turbofig serve` to run it in the
foreground. The daemon starts an HTTP MCP endpoint on port `18846`, a
WebSocket server on port `18847`, and a file-bridge at `~/.turbofig`.
Override ports with `TURBOFIG_MCP_PORT` and `TURBOFIG_WS_PORT`.

## Load the plugin in Figma

The daemon's WebSocket port requires a pairing token (`SECURITY.md`,
`DECISIONS.md` #39). **Start the daemon once before you build the plugin**,
so `~/.turbofig/token` exists: `plugin/build-ui.ts` reads it and injects the
real token into your local `dist/ui.html`. Skip this step and the build still
succeeds, but prints a warning and embeds a placeholder that cannot connect.

1. Start the daemon once: `cargo run -- serve` (leave it running, or stop it
   after `~/.turbofig/token` is created).
2. Build the plugin: `cd plugin && bun run build`, or `bun run watch` to
   rebuild on every change under `src/`.
3. Open Figma Desktop.
4. Go to **Plugins > Development > Import plugin from manifest**.
5. Select `plugin/manifest.json`.
6. Open a file and run the plugin. The panel shows the connection status and
   the file key.

After any change under `plugin/src/`, rebuild with `cd plugin && bun run
build` before you test in Figma.

## Work on the menu-bar app

The app (`daemon/src/app_bundle.rs`, `daemon/src/menu_bar/`) is macOS-only
and assembles `turbofig.app` on your own Mac. A debug build (`cargo build`,
`cargo test`, `cargo run`) never touches either of your real Applications
folders (`/Applications` or `~/Applications`) or `~/Library/LaunchAgents`,
never runs `launchctl` or `lsregister`, and never opens a real tray icon or
window unless you explicitly opt in:

- `cargo build && ./target/debug/turbofig app run` is the manual-testing
  surface for the menu-bar app with no bundle installed. It refuses to run
  at all in a debug build unless `TURBOFIG_DEV_REAL_DESKTOP=1` is set,
  because **it shows real UI**: a real tray icon, a real menu, and (on
  first use, or from the menu) a real About window. Set the env var only
  when you actually want to see it:
  ```bash
  TURBOFIG_DEV_REAL_DESKTOP=1 ./target/debug/turbofig app run
  ```
  Without that env var, every seam behind it (the clipboard, opening Figma
  or a URL, `launchctl`) is faked; `cargo test` never needs it and never
  sets it.
- `./target/release/turbofig app install` assembles (or refreshes)
  `turbofig.app` into `TURBOFIG_APPLICATIONS_DIR` (defaults to
  `~/Applications`) and prints its path; this one is safe to run in a
  release build without the env var, since a release binary is what
  Homebrew actually installs.
- Edit `daemon/assets/about/about.html` for the About window's content; it
  is a single static page, `include_str!`'d at compile time, no build step.
  After any change, just `cargo build` again; there is no separate bundle
  step to rerun.

### Swap in the real brand icons

The brand sources live in `docs/brand/`: `app-icon-1024.png` (the app
icon) and `menubar-glyph.png` (the menu-bar glyph, black on transparent).
After you change either one, regenerate the embedded assets:

- **App icon** (`turbofig.app`'s `.icns`, shown in Finder and the Dock):
  `scripts/make-app-icon.sh docs/brand/app-icon-1024.png`. Needs a
  1024x1024 PNG; rebuilds `daemon/assets/app-icon/AppIcon.icns` with
  `sips`/`iconutil`, no third-party tool.
- **Tray (menu-bar) icon**, 2 states, normal and dimmed:
  `scripts/make-tray-icon.sh docs/brand/menubar-glyph.png`. Needs a 44x44
  (`@2x`) PNG, black on transparent (a template image: AppKit tints it, so
  only alpha matters). It rebuilds `daemon/assets/tray-icon/icon-tf-44.png`
  (the "connected" state) and makes `icon-tf-44-dimmed.png` (the "waiting"
  or unreachable state) at 35% alpha from the same source.

Commit the regenerated PNGs/`.icns` (and, for the app icon, the source
PNG) alongside the `docs/brand/` artwork; neither script touches anything
outside `daemon/assets/`.

## Versioning

The version lives in one place, the `[workspace.package]` version in the
root `Cargo.toml`. To bump it everywhere (daemon, both `package.json`
files, `Cargo.lock`, and the changelog), run `bun scripts/bump-version.ts
<x.y.z>`. Never hand-edit a version field.

## Run all tests and checks

These are close to the commands CI runs; see `.github/workflows/ci.yml` for
the exact steps:

```bash
# Rust
cargo build
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
bash scripts/handshake.sh

# TypeScript
bun install
bun run typecheck
bunx @biomejs/biome check .
bun test
cd plugin && bun run build
cd ..
bun bench/harness.ts --dry-run --scenario webpage-plain --baseline bench/baseline.json --max-ratio 1.2

# Rust again, to prove the daemon builds with the plugin just built embedded
cargo build
```

Run only the checks that cover the files you changed day to day; run the
full list before you open a PR.

## Commit messages

Look at `git log` for the house style: an imperative, descriptive subject
line, no `feat:`/`chore:`/`fix:` prefixes. One logical change per commit.

**Never add a `Co-Authored-By` or `Signed-off-by` trailer, or any AI
attribution line, to a commit message.** A tracked hook enforces this:

```bash
git config core.hooksPath .githooks
```

With the hook enabled, a commit containing either trailer is rejected at
commit time. If you did not run the config step above, enable it now.

## Good first contributions

- Add a new `tf.*` helper to `plugin/src/helpers.ts` and `helpers/tf-api.md`.
- Linux build support.
- Error codes in `turbofig_execute` results, instead of a plain message.
- More examples in `helpers/tf-api.md`.
- An optional job audit log for the file bridge.
- The real brand icons (see "Swap in the real brand icons" above).

The maintainer reviews PRs on a best-effort basis.

## Benchmarking

A reproducible benchmark harness lives in `bench/`, documented in
`bench/README.md`. It measures exact wire bytes and real Claude Code token
counts, never an estimate. Run it with:

```bash
bun bench/harness.ts --dry-run --scenario webpage-plain --baseline bench/baseline.json --max-ratio 1.2
```

## Pull request checklist

- [ ] One coherent change per PR. Separate unrelated fixes, features, and
      formatting churn.
- [ ] Tests ship in the same commit as the behaviour they cover. Untested
      behaviour is not done.
- [ ] `cargo test`, `cargo clippy -- -D warnings`, and `cargo fmt --check`
      pass for any Rust change.
- [ ] `bun run typecheck`, `bunx @biomejs/biome check .`, and `bun test`
      pass for any TypeScript change.
- [ ] If you touched `plugin/src/`, you ran `cd plugin && bun run build`
      to regenerate `dist/ui.html`. `dist/` is gitignored, so do not commit
      it; CI and the release build regenerate it themselves.
- [ ] No `Co-Authored-By`, `Signed-off-by`, or AI attribution trailer in any
      commit message.
- [ ] `ARCHITECTURE.md` is updated in the same commit if you changed the
      module layout, ports, the tool surface, or the data flow.
