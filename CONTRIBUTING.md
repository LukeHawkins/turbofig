# Contributing to turbofig

Thanks for your interest in turbofig. This file covers setup, checks, and
the PR process. Read `ARCHITECTURE.md` for the module layout and data flow,
and `DECISIONS.md` for the reasoning behind the design choices.

## Prerequisites

- macOS. Turbofig installs and runs on macOS only.
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
cargo build --release
./target/release/turbofig
```

The daemon starts an HTTP MCP endpoint on port `18846`, a WebSocket server on
port `18847`, and a file-bridge at `~/.turbofig`. Override ports with
`TURBOFIG_MCP_PORT` and `TURBOFIG_WS_PORT`.

## Load the plugin in Figma

The daemon's WebSocket port requires a pairing token (`SECURITY.md`,
`DECISIONS.md` #39). **Start the daemon once before you build the plugin**,
so `~/.turbofig/token` exists: `plugin/build-ui.ts` reads it and injects the
real token into your local `dist/ui.html`. Skip this step and the build still
succeeds, but prints a warning and embeds a placeholder that cannot connect.

1. Start the daemon once: `cargo run` (leave it running, or stop it after
   `~/.turbofig/token` is created).
2. Build the plugin: `cd plugin && bun run build`.
3. Open Figma Desktop.
4. Go to **Plugins > Development > Import plugin from manifest**.
5. Select `plugin/manifest.json`.
6. Open a file and run the plugin. The panel shows the connection status and
   the file key.

After any change under `plugin/src/`, rebuild with `cd plugin && bun run
build` before you test in Figma.

## Versioning

The version lives in one place, the `[workspace.package]` version in the
root `Cargo.toml`. To bump it everywhere (daemon, both `package.json`
files, `Cargo.lock`, and the changelog), run `bun scripts/bump-version.ts
<x.y.z>`. Never hand-edit a version field.

## Run all tests and checks

These are the same commands CI runs:

```bash
# Rust
cargo build
cargo build --release
cargo test
cargo clippy -- -D warnings
cargo fmt --check
bash scripts/handshake.sh

# TypeScript
bun install
bun run typecheck
bunx @biomejs/biome check .
bun test
cd plugin && bun run build
cd ..
bun bench/harness.ts --dry-run --scenario webpage --baseline bench/baseline.json
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
      and the rebuilt `dist/` output is included.
- [ ] No `Co-Authored-By`, `Signed-off-by`, or AI attribution trailer in any
      commit message.
- [ ] `ARCHITECTURE.md` is updated in the same commit if you changed the
      module layout, ports, the tool surface, or the data flow.
