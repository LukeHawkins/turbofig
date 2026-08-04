Review the current conversation and persist any important context to repo-local files before the context closes.

Instructions:

1. **Scan the conversation** for decisions, patterns, fixes, or architectural knowledge that a future session would need.

2. **Categorise** each item:
   - **Architectural or product decision** — append to `DECISIONS.md`
   - **Architecture or data-flow change** — update `ARCHITECTURE.md`
   - **Stack change** — update `STACK.md`
   - **Build rules, commit rules, or always-on conventions** — update `CLAUDE.md`
   - **Scope or plan change** — update `PLAN.md`

3. **Skip** anything already captured by the code diff itself or already documented.

4. **Write concisely.** Each entry is 2-5 sentences max. Include the what, why, and where so a future session does not need to re-derive it.

5. **Show the user** a short summary of what you saved and where, so they can sanity-check before closing.

Important:
- Save to **repo files only** (not `~/.claude/` device memory). The goal is full context portability across machines.
- Do NOT create new standalone files unless the information genuinely fits no existing doc. Prefer appending to existing sections.
- Do NOT pad entries — if nothing worth saving happened, say "Nothing to persist" and stop.
- This repo does not use a DEVOPS.md or DESIGN.md. Route infrastructure decisions to DECISIONS.md and visual/UI decisions to ARCHITECTURE.md.
