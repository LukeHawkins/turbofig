Turn a design brief into a full Figma page, autonomously and cheaply.

**Inputs ($ARGUMENTS):** A brief describing the design. Optionally include: a moodboard image path, brand notes (font families, palette hex values, tone keywords), or reference URLs. Pass everything as a single argument block.

This command orchestrates. The main session plans, checkpoints, and verifies. Every step that touches Figma or reads an image runs in a subagent. The main context never receives a screenshot or a full node tree.

---

## Steps

### 1. Intake

Parse `$ARGUMENTS` and extract:

- **Purpose.** What the design is for and who will use it.
- **Tone.** Formal, playful, minimal, bold, etc.
- **Key sections.** Named page sections the design must contain (for example: Nav, Hero, Features, Pricing, Footer).
- **Palette.** Hex values if provided; otherwise derive a 4-colour palette (brand, accent, surface, text) from the tone.
- **Type.** Font family and weight choices if provided; otherwise default to Inter with Bold headings and Regular body.
- **Reference material.** Moodboard image paths and reference URLs. Delegate reading of any moodboard image to a subagent that returns only a compact colour and mood summary. Do not load images into the main context.

If the brief is too vague to infer key sections, derive a sensible default structure (for example: Nav, Hero, Features, Footer) and proceed.

Generate a stable **job id**: a timestamp with a 4-character random suffix, for example `20260805-a3f7`.

### 2. Plan-first

Produce a structured plan before writing a single node. The plan is the source of truth for the entire run.

For each section, record:

- `name`: a stable PascalCase identifier, for example `"HeroSection"`. This is the Figma node name and the idempotency key.
- `purpose`: one sentence describing what the section does.
- `layout`: direction (`VERTICAL` or `HORIZONTAL`), gap, padding, and fixed width if known.
- `content`: bullet list of child elements (text nodes, rects, cards, etc.) with copy and sizes.
- `palette_tokens`: the 2-4 hex values this section uses from the palette.
- `type_tokens`: font family/weight/size for each text role in this section.
- `depends_on`: names of sections that must exist before this one (for example a sticky nav may depend on nothing; a pricing card may depend on a shared token frame). Leave empty if none.

Persist two files:

1. `~/.turbofig/design/<job-id>/plan.json` — the full plan array.
2. `~/.turbofig/design/<job-id>/status.json` — one entry per section: `{ "name": "HeroSection", "state": "pending" }`.

These two files are the resume checkpoint. A re-run with the same job id loads them and skips completed work.

### 3. Parallel builder subagents

Group sections that have no mutual dependency. Dispatch each independent group as parallel Sonnet subagents in one batch. Do not run them sequentially unless a section's `depends_on` list names a section not yet built.

Give each builder subagent ONLY:

- Its section spec (name, purpose, layout, content, palette tokens, type tokens).
- A pointer to `helpers/tf-api.md`. Do not paste the file; pass the path.
- The job id and the file-bridge protocol: write the job to `~/.turbofig/inbox/<id>.json`, read the result from `~/.turbofig/outbox/<id>.json`.
- The idempotency rule: wrap the root frame in `tf.findOrCreate(figma.currentPage, "<SectionName>", factory)` so a re-run never duplicates the section.

Each builder must:

1. Write ONE batched `execute` job to the file-bridge that builds the whole section. Batch all node operations into one `code` string. Prefer fewer jobs over many small ones.
2. Preload all fonts once with `await tf.loadFonts([...])` at the top of the eval.
3. Call `tf.commit("<SectionName>")` at the end of the eval.
4. Read the outbox result once. Parse it for the root node id.
5. Return a SHORT text status only: section name, root node id, and done or a blocker. Return NO images and NO node trees.

Example builder return format:

```
Section: HeroSection
Node id: 1:23
Status: done
```

After each builder returns, update `status.json`: set the section `state` to `"built"` and record the `nodeId`. This checkpoint update happens in the main session.

### 4. Automated visual QA (firewalled)

Run QA for each section after it is built, and once for the whole page after all sections are built.

Dispatch a QA subagent for each target. Give each QA subagent ONLY:

- The section name and node id (or the page id for the full-page check).
- The job id and the file-bridge protocol.
- The QA rubric (copy it verbatim into the subagent prompt):

  > Score the screenshot against these criteria:
  > 1. Visual hierarchy: headings are clearly larger than body text; the eye has a clear entry point.
  > 2. Spacing rhythm: gaps and padding follow a consistent scale (for example 8px increments). No collapsed or exploded gaps.
  > 3. Contrast: body text and interactive labels meet WCAG AA (4.5:1 for normal text, 3:1 for large text).
  > 4. Alignment and grid: elements align on a shared axis. No stray offsets.
  > 5. Restraint: no clutter, no element overlap, no text hidden behind other elements, no dead whitespace larger than the design intent.

Each QA subagent must:

1. Request a `screenshot` via the file-bridge: `{ "op": "screenshot", "nodeId": "<id>", "scale": 2, "return": "file" }`.
2. Read the PNG from the path in the outbox result.
3. Score it against all five criteria.
4. Return a SHORT TEXT verdict only:
   - `pass` if all criteria are met.
   - A numbered defect list with a specific fix for each failing criterion, for example: `1. Body text (16px Inter Regular, #888888 on #FFFFFF) fails WCAG AA. Fix: change fill to #767676 or darker.`
5. Return NO images. The PNG stays in the subagent context and is discarded when the subagent ends.

The main session reads only the text verdict. Update `status.json`: set `state` to `"passed"` or `"qa_failed"` and record the defect list.

### 5. Refine

For each section with `state: "qa_failed"`:

1. Dispatch a fix-builder subagent. Give it the section spec, the defect list, the node id, and the file-bridge protocol.
2. The fix-builder writes ONE batched `execute` job that applies all fixes. It must use `tf.findOrCreate` so it modifies the existing section in place and does not create a duplicate.
3. After the fix-builder returns, dispatch a new QA subagent for that section using the same rubric.
4. If the new verdict is `pass`, update `status.json` to `"passed"`.
5. Cap the refine loop at **2 passes per section**. If a section still has defects after 2 passes, mark it `"needs_review"` in `status.json` and continue. Do not loop forever.

Run fix-builders for independent sections in parallel where possible.

### 6. Resume behaviour

At the start of every run, check for an existing `~/.turbofig/design/<job-id>/status.json`.

- If it exists, load it and skip every section with `state: "passed"`.
- Build and QA only sections with `state: "pending"`, `"qa_failed"`, or `"needs_review"`.
- Because builders use `tf.findOrCreate`, re-running a builder for a section that already exists modifies it in place. It does not create a duplicate.
- The plan.json stays unchanged across a resume. It is the stable spec.

To resume, pass the original job id as part of `$ARGUMENTS`, for example: `resume job-id=20260805-a3f7 brief=...`.

### 7. Finish

Report a short summary to the user:

- Job id and checkpoint location.
- Sections built, with each section's root node id and QA result.
- Any sections marked `needs_review` with the outstanding defect list.
- The Figma page node id.

Do not dump node trees, images, or `status.json` contents into the reply. One line per section is enough.

---

## Token and firewall rules

Apply these rules throughout the run. They are not optional.

- **The main session never reads a screenshot.** Only QA subagents read images. Images live and die in the subagent context.
- **Minimum context per subagent.** Pass a section spec and a file path. Do not paste whole files. Do not pass the whole plan to a builder that only builds one section.
- **One batched execute per section.** One job file, one result file. Do not split a section build across multiple small execute calls.
- **Shape the result.** Ask for ids, not full node trees. A builder returns a node id. A QA subagent returns a text verdict. Neither returns raw Figma JSON.
- **Persist progress to disk.** Every state change (built, passed, qa_failed, needs_review) writes to `status.json` before the next action starts. A crash at any point loses at most one section's work.
- **Parallel by default.** Dispatch all independent sections in one batch. Go sequential only when a section's `depends_on` list names a section not yet marked `"built"`.
