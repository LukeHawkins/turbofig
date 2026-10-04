## What changed

<!-- One coherent change. Describe it in a sentence or two. -->

## Why

<!-- The problem this solves, or the requirement it meets. -->

## How tested

<!-- Which commands you ran, and what they showed. -->

## Checklist

- [ ] One coherent change in this PR, with unrelated fixes or formatting
      split out.
- [ ] Tests ship in the same commit as the behaviour they cover.
- [ ] `cargo test`, `cargo clippy -- -D warnings`, and `cargo fmt --check`
      pass (Rust changes).
- [ ] `bun run typecheck`, `bunx @biomejs/biome check .`, and `bun test`
      pass (TypeScript changes).
- [ ] `cd plugin && bun run build` ran and the rebuilt `dist/` output is
      included (plugin changes).
- [ ] No `Co-Authored-By`, `Signed-off-by`, or AI attribution trailer in any
      commit message.
- [ ] `ARCHITECTURE.md` is updated if this PR changes the module layout,
      ports, the tool surface, or the data flow.
