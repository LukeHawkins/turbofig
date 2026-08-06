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
- **Profile.** Call `turbofig_status` and read `plugin.profileId`. Record the profile id for the whole job. Then run one `turbofig_execute` eval to read the active profile's constants from `tf.taste`: `return { spacing: tf.taste.spacing, type: tf.taste.type, grid: tf.taste.grid, contrast: tf.taste.contrast, blocklist: tf.taste.blocklist };`. Persist the result to `~/.turbofig/design/<job-id>/profile.json`. If `profileId` is `none`, `tf.taste` is undefined; write `{ "profileId": "none" }` and skip all profile constraints. If the profile id is not a known value, the daemon uses `impeccable` as the fallback.

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

1. `~/.turbofig/design/<job-id>/plan.json`: the full plan array.
2. `~/.turbofig/design/<job-id>/status.json`: one entry per section:
   `{ "name": "HeroSection", "state": "pending", "refinePasses": 0 }`.

These two files are the resume checkpoint. A re-run with the same job id loads them and skips completed work.

### 3. Parallel builder subagents

Group sections that have no mutual dependency. Dispatch each independent group as parallel Sonnet subagents in one batch. Do not run them sequentially unless a section's `depends_on` list names a section not yet built.

Give each builder subagent ONLY:

- Its section spec (name, purpose, layout, content, palette tokens, type tokens).
- A pointer to `helpers/tf-api.md`. Do not paste the file; pass the path.
- The job id and the file-bridge protocol. Each builder must use its OWN unique bridge job id for the inbox/outbox filenames: `<design-job-id>-<SectionName>-<random4>` (for example `20260805-a3f7-HeroSection-b2c9`). Two builders sharing the same id would overwrite each other's inbox file.
- The idempotency rule: use `tf.findOrCreate` to get or create the root section frame, then call `tf.clear` on it to remove all existing children before rebuilding. `findOrCreate` alone protects only the section frame; `tf.clear` before rebuilding makes a re-run fully safe.
- A pointer to `~/.turbofig/design/<job-id>/profile.json`. Each builder must read this file and use its `spacing` scale, `type` scale, and `grid` values when placing and sizing nodes. When `profile.json` contains `{ "profileId": "none" }`, skip profile constraints.

  ```js
  const s = await tf.findOrCreate(figma.currentPage, 'HeroSection', factory);
  tf.clear(s);
  // build all children fresh below
  ```

Each builder must:

1. Write ONE batched `execute` job to the file-bridge that builds the whole section. Batch all node operations into one `code` string. Prefer fewer jobs over many small ones.
2. Preload all fonts once with `await tf.loadFonts([...])` at the top of the eval.
3. Use `tf.text` with a `width` property for any text node that must wrap.
4. Call `tf.commit("<SectionName>")` at the end of the eval.
5. Poll for the outbox file: wait approximately 0.2 s between attempts, retry up to 50 times (approximately 10 s total). A complex section can take several seconds to build. Do not read the outbox only once.
6. If the outbox result has `ok: false`, return a blocker containing the `error` string. Do NOT record a node id in status.json.
7. Return a SHORT text status only: section name, root node id, and done or a blocker. Return NO images and NO node trees.

Example builder return format:

```
Section: HeroSection
Node id: 1:23
Status: done
```

After each builder returns, update `status.json`: set the section `state` to `"built"` and record the `nodeId`. This checkpoint update happens in the main session.

### 4. Assembly

After all builders return their node ids, dispatch ONE batched execute (not one per section) that assembles the page.

This step is mandatory. Parallel-built sections otherwise land at the origin (0, 0) and overlap each other.

The orchestrator dispatches one `execute` job that:

1. Calls `tf.findOrCreate(figma.currentPage, '<job-id>-Root', factory)` to get or create a root container frame.
2. Sets the root frame to VERTICAL auto-layout at the full page width.
3. Appends each section frame into the root in the order defined in `plan.json`. Use the node ids recorded in `status.json` after step 3.

Poll for the outbox file using the same polling rule (0.2 s intervals, up to 50 tries). Record the root container node id in `status.json` under the key `"rootNodeId"`.

Whole-page QA in step 5 runs on this assembled root, not on the raw page.

### 5. Automated visual QA (firewalled)

Run QA for each section after it is built, and once for the whole page after all sections are assembled.

Dispatch a QA subagent for each target. Give each QA subagent ONLY:

- The section name and node id (or the root container node id for the full-page check).
- The job id and the file-bridge protocol.
- A pointer to `~/.turbofig/design/<job-id>/profile.json`.
- The QA rubric (copy it verbatim into the subagent prompt):

  > Score the design against these criteria. Read profile.json before scoring. Report each objective check as pass or fail with the measured value.
  >
  > **Objective checks (profile-driven):**
  > 1. Grid adherence: element x positions and widths align to the profile `grid.columns`, `grid.gutter`, and `grid.margin`. Measure at least five elements. Report as: `pass` or `fail: <element> at x=<n>px, expected <m>px`.
  > 2. Type-scale conformance: all text node sizes come from the profile `type.scale`. Report each text node using a size not in the scale as: `fail: <element> uses <n>px, not in scale`.
  > 3. Contrast/AA: text-on-background contrast meets `contrast.bodyMin` for body text and `contrast.largeMin` for large text. Report each failing pair as: `fail: <element> measured <ratio>:1, required <min>:1`.
  > 4. Spacing rhythm: all gaps and padding values come from the profile `spacing` scale. Report each value not in the scale.
  > 5. Blocklist: none of the profile `blocklist` anti-slop patterns appear. Report each pattern found.
  >
  > When profile.json contains `{ "profileId": "none" }`, skip checks 1 to 5. Apply checks 6 and 7 to all runs.
  >
  > **Qualitative checks:**
  > 6. Visual hierarchy: headings are clearly larger than body text. The eye has a clear entry point.
  > 7. Restraint: no clutter, no element overlap, no text hidden behind other elements, no dead whitespace larger than the design intent.

Each QA subagent must:

1. Request a `screenshot` via the file-bridge: `{ "op": "screenshot", "nodeId": "<id>", "scale": 2, "return": "file" }`.
2. Poll for the outbox file (0.2 s intervals, up to 50 tries). Read the PNG from the path in the outbox result.
3. Read profile.json. Score the design against all seven criteria.
4. Return a SHORT TEXT verdict only:
   - `pass` if all criteria are met.
   - List objective check results first (checks 1 to 5), then qualitative check results (checks 6 and 7). For each failure, give the measured value and a specific fix. For example: `3. Contrast/AA fail: body text #888888 on #FFFFFF measured 3.5:1, required 4.5:1. Fix: change fill to #767676 or darker.`
5. Return NO images. The PNG stays in the subagent context and is discarded when the subagent ends.

The main session reads only the text verdict. Update `status.json`: set `state` to `"passed"` or `"qa_failed"` and record the defect list.

### 6. Refine

For each section with `state: "qa_failed"`:

1. Read `refinePasses` from `status.json` for that section. Subtract from the 2-pass budget. If the remaining budget is zero, mark the section `"needs_review"` immediately and skip to the next section.
2. Dispatch a fix-builder subagent. Give it the section spec, the defect list, the node id, and the file-bridge protocol.
3. The fix-builder applies targeted edits from the saved defect list. It does NOT do a full rebuild. It must use `tf.findOrCreate` to locate the existing section without creating a duplicate. It does NOT call `tf.clear`.
4. The fix-builder must use its own unique bridge id (same format: `<design-job-id>-<SectionName>-<random4>`).
5. The fix-builder polls for the outbox file (0.2 s intervals, up to 50 tries).
6. If the outbox result has `ok: false`, return a blocker containing the `error` string. Do NOT increment `refinePasses`.
7. After the fix-builder returns successfully, increment `refinePasses` in `status.json` and write the file before dispatching QA.
8. Dispatch a new QA subagent for that section using the same rubric.
9. If the new verdict is `pass`, update `status.json` to `"passed"`.
10. Cap the refine loop at **2 passes per section** (using the `refinePasses` counter). If a section still has defects after 2 passes, mark it `"needs_review"` and continue. Do not loop forever.

Run fix-builders for independent sections in parallel where possible.

### 7. Resume behaviour

At the start of every run, check for an existing `~/.turbofig/design/<job-id>/status.json`.

If it exists, load it. Apply this logic per section:

- `state: "passed"`: skip. No action.
- `state: "pending"`: run the full builder (findOrCreate + clear + fresh rebuild), then QA.
- `state: "built"`: skip the builder. Run QA only.
- `state: "qa_failed"`: run the fix-builder (targeted edits from the saved defect list), then QA. Do NOT run a full rebuild.
- `state: "needs_review"`: do NOT auto-retry. A `needs_review` section already used its 2-pass budget. List it as outstanding in the final report. Only retry it if the user supplies an explicit override instruction in `$ARGUMENTS`; in that case, pass the override to the fix-builder as an additional directive.

Read `refinePasses` for each section that will enter the refine loop. Subtract from the 2-pass budget before dispatching any fix-builder. This prevents a crash mid-refine from granting extra passes.

The plan.json stays unchanged across a resume. It is the stable spec.

To resume, pass the original job id as part of `$ARGUMENTS`, for example: `resume job-id=20260805-a3f7 brief=...`.

### 8. Finish

Report a short summary to the user:

- Job id and checkpoint location.
- Sections built, with each section's root node id and QA result.
- Any sections marked `needs_review` with the outstanding defect list.
- The assembled root container node id and the Figma page node id.

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
