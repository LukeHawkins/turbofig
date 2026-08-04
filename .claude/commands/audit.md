Audit a phase, feature, or area for completeness and drift. Target: $ARGUMENTS

`$ARGUMENTS` names what to audit (e.g. "Phase 4", "daemon transport", "the routing registry"). Omitted: audit the most-recently-touched area.

Goal: a dense, honest status of what was claimed done vs. what is actually true in the code. This is a **READ-ONLY** audit. Report; do not fix.

## Steps

1. **Identify the surface** being audited and the files that make it up.
2. **Enumerate what should exist:** from PLAN.md checkboxes, DECISIONS.md, and CLAUDE.md.
3. **Enumerate what does exist:** read the actual implementation files.
4. **Produce a parity table:** one row per claimed item, columns: `Claimed (PLAN/docs)`, `Actual (code)`, `Status` (done / partial / missing / drifted).
5. **Check the gates:** are the PLAN.md checkboxes honest? Any marked `[x]` that have deltas get called out explicitly.
6. **State the test status:** does `cargo test` pass? Does `bun test` pass? Note any skipped or failing tests.

## Output shape

A single message, markdown tables, no fixes applied:

### What's there
List of the relevant files with a one-line purpose each.

### Parity table
Every claimed item, three columns, status marker. Reference exact `file:line` where useful.

### Gate state
PLAN.md checkbox honesty for the audited area. Discrepancies are called out.

### Recommended next action
One paragraph: what to do to close the remaining deltas before moving on.

## Important
- READ-ONLY. Do not change anything.
- Keep it dense: one line per row, no repetition.
- If something is legitimately deferred, mark it "deferred", not "missing".
- Check both Rust and TS: `cargo build` + `cargo test` + `cargo clippy` + `cargo fmt --check` + `bun run typecheck` + `bun test` + `bun run lint`.
