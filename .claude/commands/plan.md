Help plan (or refine) work in PLAN.md. Topic / scope: $ARGUMENTS

This is the "preplan heavily" step. Planning is cheaper than rework. Be thorough before any code is written.

Instructions:

1. **Understand the goal.** If `$ARGUMENTS` is vague, ask the user clarifying questions about scope, constraints, and what "done" looks like before drafting. Do not guess at product direction.

2. **Read the lay of the land:** CLAUDE.md, STACK.md, DECISIONS.md, and the current PLAN.md. This ensures the new plan fits what already exists and does not contradict prior decisions.

3. **Draft a phase** in the PLAN.md format:
   - A short phase heading (`## Phase N: <name>`) with a one-line goal.
   - A checklist of `- [ ]` items, each a single, independently-committable, verifiable unit of work, in dependency order.
   - Each item maps 1:1 to a commit when run via `/phase`.
   - Note the verification for non-obvious items.

4. **Surface decisions, do not bury them.** If the plan implies an architectural or product choice, call it out and record the chosen option (and the rejected alternatives) in `DECISIONS.md`. Add a per-phase model hint where it helps.

5. **Flag risks and unknowns:** anything that needs research, an external account, or a spike before it can be estimated.

6. **Show the draft to the user for sign-off** before writing it into PLAN.md. Once approved, write it in.

Important:
- Plan in `PLAN.md`; record the *why* in `DECISIONS.md`. Keep them in sync.
- No feature creep. If it is not needed for the goal, it does not go in the phase.
- Optimise for solo-dev-plus-AI velocity. MVP mindset: ship, validate, iterate.
- Do NOT start implementing. This command only plans.
- Commits: imperative message, no `feat:`/`chore:` prefixes, no Co-Authored-By, author is Luke Hawkins <hi@lukehawkins.eu>.
- Verify before commit: Rust (`cargo build` + `cargo test` + `cargo clippy` + `cargo fmt --check`) AND TS (`bun run typecheck` + `bun test` + `bun run lint`).
