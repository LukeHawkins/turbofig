Run phase $ARGUMENTS from PLAN.md.

This command orchestrates. It delegates the actual implementation to subagents and keeps the main session as the coordinator that verifies and commits.

Instructions:

1. Read PLAN.md and find "Phase $ARGUMENTS".
2. If the phase is not found, tell the user and stop.
3. If all items in the phase are already checked `[x]`, tell the user the phase is complete and stop.
4. Work through the unchecked `- [ ]` items in dependency order. **Parallelise by default:** scan ahead and dispatch every run of mutually independent items (no shared files, no ordering dependency) as parallel subagents in ONE batch. Only fall back to one-at-a-time when an item depends on a previous item's output or touches the same files. Still commit each item separately once verified (see 5d-5e).
5. For each item:
   a. Announce which item you are starting.
   b. **Delegate implementation to a subagent.** Spawn a `sonnet` worker (use `haiku`/Explore for pure search, reserve `opus` only for the transport/session or routing design). Give it ONLY the context it needs: the item text, the exact files to touch, the relevant convention from CLAUDE.md, and the acceptance check. Do not paste whole files into the prompt; pass paths. **Require tests:** the worker MUST write tests that cover the behaviour it adds (Rust `#[test]`/`#[tokio::test]` co-located or in `tests/`; plugin logic via `bun test`). Code without tests is not done. Tell the worker to read before writing and to end with:
      ```
      CHANGED: <file paths>
      TESTS: <test names added + what they cover>
      VERIFICATION: <cargo build+test+clippy pass | bun typecheck+test pass | FAILED: reason>
      BLOCKERS: <none | description>
      ```
   c. **Verify in the main session.** Do not trust the worker's word alone. Run the real gate: for Rust items `cargo build` + `cargo test` + `cargo clippy -- -D warnings` + `cargo fmt --check`; for TS items `bun run typecheck` + `bun test`; always the `lint` script. Confirm the new tests exist, run, and actually exercise the item's behaviour (not empty stubs). Fix or re-dispatch until it passes. Never mark an item done on an unverified or untested change.
   d. Stage the relevant files, including PLAN.md with the checkbox flipped to `[x]`.
   e. Commit with a clear imperative message describing what was done. Do NOT add any Co-Authored-By lines or AI attribution.
   f. Announce the item is complete, show the commit hash.
6. If an item fails and cannot be fixed after a reasonable attempt (including one re-dispatch with sharper context), stop and report clearly. Do not skip items.
7. After completing all items, summarise what was done across the full phase.
8. **Harden the phase.** Run a review-and-fix pass over everything built in this phase by following `.claude/commands/harden.md` (scoped to this phase). You MAY run the review itself in a subagent that returns a findings list; apply safe fixes as separate commits, and bring back only genuinely ambiguous decisions with a recommended option for each. Skip this only if the phase made no code changes.

Important:
- One commit per checklist item. Each commit must leave the project in a working state (builds + tests green).
- **Tests are mandatory, not deferred.** Every item that adds behaviour ships with tests in the same commit. Never postpone tests to a later phase. A phase is not done if any new behaviour is untested.
- **Author + no attribution (enforced).** Confirm `git config user.email` is `hi@lukehawkins.eu` before the first commit. Every commit is authored by the git config only. Never add Co-Authored-By, Signed-off-by, or any AI/tool attribution to the message or trailer.
- NEVER add Co-Authored-By, Signed-off-by, or any AI attribution. All commits are authored by the git user config only (Luke Hawkins <hi@lukehawkins.eu>).
- Update PLAN.md from `- [ ]` to `- [x]` as part of each commit.
- Context discipline: the main session is the memory. Workers are disposable. Pass file paths, not file contents. Pass the one relevant convention, not the whole document.
- If a `temp/` directory exists it is reference material: read from it, never modify it.
- After completing a phase (or if stopped mid-phase), kill any daemon, dev server, or background process you started. Always clean up.
- If the work changed architecture, ports, the tool surface, files, prop chains, or data flow, update CLAUDE.md (and DESIGN/ARCHITECTURE where relevant) in the same commit.
