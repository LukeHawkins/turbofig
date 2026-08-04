Review and harden recently completed work, fixing what is safe to fix. Target: $ARGUMENTS

`$ARGUMENTS` names what to harden (e.g. "Phase 4", "daemon transport"). Omitted: the work just completed in this session.

This is the **active** counterpart to `/audit`. `/audit` reports and stops; `/harden` goes back over the work, finds problems and improvements, and **fixes the ones it is confident about**, committing each as it goes. Run at the end of a phase to clean up before moving on.

## Mindset

- **Obvious fix: just do it.** Do not ask permission to fix a clear bug, delete dead code, or tighten a type.
- **Unsure but there is a sensible default: do it, and note it** in the summary so the user can veto. Lean toward action.
- **Genuinely ambiguous, risky, or changes product behaviour/scope: ask.** Batch into one short set of questions at the end with a recommended option for each. Never block mid-pass waiting on an answer you can reasonably default.
- Stay within the audited surface. Do not rewrite unrelated areas. MVP mindset: clean, do not gold-plate.

## Steps

1. **Scope it.** Identify the files the target covers. Use `git log`/`git diff` for the phase's commits. Read the relevant code before judging it.
2. **Sweep for issues**, in roughly this order:
   - **Correctness:** bugs, wrong edge-case handling, unhandled errors, race conditions, off-by-ones, missing `await`s, leaks.
   - **Conventions and drift:** does it match CLAUDE.md? Fix violations.
   - **Dead weight and duplication:** unused code/exports/deps, copy-paste that should be shared.
   - **Simplification:** clearer control flow, removing premature abstraction, obvious perf wins. Only optimise where it is clearly worth it.
   - **Tests:** run `cargo test` + `bun test`. Close coverage gaps for the new code.
3. **Fix as you go.** Apply each safe fix:
   - One logical change per commit, imperative message, no Co-Authored-By, author Luke Hawkins <hi@lukehawkins.eu>.
   - `cargo build` + `cargo test` + `cargo clippy` must pass for Rust changes. `bun run typecheck` + `bun test` + `bun run lint` must pass for TS changes.
   - If the change touches architecture, ports, the tool surface, or data flow: update CLAUDE.md and ARCHITECTURE.md in the same commit.
4. **Final verification.** Rust: `cargo build` + `cargo test` + `cargo clippy` + `cargo fmt --check`. TS: `bun run typecheck` + `bun test` + `bun run lint`. Clean up any daemon or background processes you started.
5. **Report.** End with:
   - **Fixed:** bullet list of what you changed, with commit hashes.
   - **Judgement calls:** defaults you chose that the user might want to override.
   - **Needs a decision:** only genuinely ambiguous items, each with options and your recommended choice. Empty is the good outcome.

## Important
- This pass writes code and commits. It is NOT read-only. (For a read-only check, use `/audit`.)
- Never weaken or delete a test just to make the suite pass. Fix the cause.
- Do not skip the fix step and just produce a list. Fix what is safe, then list the rest.
- If `temp/` exists it is reference material. Read from it; never modify it.
