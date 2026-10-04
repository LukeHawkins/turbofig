/**
 * Benchmark scenario definitions.
 *
 * Each scenario is a named list of file-bridge jobs. A scenario carries two
 * labels:
 *   - "transport": the jobs use only the plain Figma Plugin API. Both
 *     turbofig and console-mcp can run them, so the comparison measures the
 *     transport, not a helper library.
 *   - "transport + helpers": the jobs call turbofig's `tf.*` craft library.
 *     Only turbofig targets can run these. They show the extra value the
 *     helper library adds on top of the raw transport.
 *
 * Every scenario names a `pageName`. The harness creates a fresh page with
 * that name before the first job and deletes it after the last job, so a
 * benchmark run never touches the user's existing content.
 */

import type { TargetId } from "./targets.js";

/** One file-bridge job. Matches the daemon's bridge-job shape. */
export interface BridgeJob {
  op: "execute" | "get_selection" | "screenshot" | "status";
  code?: string;
  fileKey?: string;
  /** get_selection: extra node property names. */
  fields?: string[];
  /** get_selection: child traversal depth (0-5). */
  depth?: number;
  /** screenshot: node to capture; omit to use the current selection. */
  nodeId?: string;
  /** screenshot: export scale factor. */
  scale?: number;
  /** screenshot: "file" (default) or "inline". */
  returnMode?: "file" | "inline";
  /** screenshot: longest-edge downscale cap. */
  maxDim?: number;
  /** screenshot: skip downscaling. */
  fullRes?: boolean;
}

/** A named collection of jobs that forms one benchmark scenario. */
export interface Scenario {
  name: string;
  label: "transport" | "transport + helpers";
  /** Targets able to run this scenario. */
  targets: TargetId[];
  /** Name given to the fresh page the harness creates for this scenario run. */
  pageName: string;
  jobs: BridgeJob[];
}

const ALL_TARGETS: TargetId[] = ["turbofig-mcp", "turbofig-bridge", "console-mcp"];
const TURBOFIG_ONLY: TargetId[] = ["turbofig-mcp", "turbofig-bridge"];

// ---------------------------------------------------------------------------
// Scenario: webpage (transport + helpers)
// Builds a marketing page with nav, hero, feature grid, and footer using
// turbofig's tf.* craft library. Five execute calls, one per section.
// ---------------------------------------------------------------------------

const webpageJobs: BridgeJob[] = [
  {
    op: "execute",
    code: `
// Set up the page frame.
const root = tf.frame({
  name: "Page",
  direction: "VERTICAL",
  width: 1440,
  height: 5200,
  fill: "#FFFFFF",
  gap: 0,
});
figma.currentPage.appendChild(root);
    `.trim(),
  },
  {
    op: "execute",
    code: `
// Build the navigation bar.
const root = await tf.findOrCreate(
  figma.currentPage,
  "Page",
  () => tf.frame({ name: "Page", direction: "VERTICAL", width: 1440, height: 5200 }),
);
const nav = tf.frame({
  name: "Nav",
  direction: "HORIZONTAL",
  width: 1440,
  height: 72,
  fill: "#FFFFFF",
  gap: 32,
  padding: { top: 0, right: 64, bottom: 0, left: 64 },
  counterAlign: "CENTER",
});
root.appendChild(nav);
const logo = await tf.text({ text: "TurboFig", size: 18, family: "Inter", style: "Bold", color: "#111111" });
nav.appendChild(logo);
for (const label of ["Features", "Pricing", "Docs", "Blog"]) {
  nav.appendChild(await tf.text({ text: label, size: 14, color: "#444444" }));
}
const ctaBox = tf.frame({
  name: "CTA",
  direction: "HORIZONTAL",
  fill: "#0066FF",
  padding: { top: 10, right: 20, bottom: 10, left: 20 },
});
tf.append(ctaBox, await tf.text({ text: "Get started", size: 14, color: "#FFFFFF" }));
nav.appendChild(ctaBox);
    `.trim(),
  },
  {
    op: "execute",
    code: `
// Build the hero section.
const root = await tf.findOrCreate(
  figma.currentPage,
  "Page",
  () => tf.frame({ name: "Page", direction: "VERTICAL", width: 1440, height: 5200 }),
);
const hero = tf.frame({
  name: "Hero",
  direction: "VERTICAL",
  width: 1440,
  height: 680,
  fill: "#F4F6FB",
  gap: 24,
  padding: { top: 120, right: 0, bottom: 120, left: 0 },
  primaryAlign: "CENTER",
  counterAlign: "CENTER",
});
root.appendChild(hero);
await tf.loadFonts([{ family: "Inter", style: "Bold" }, { family: "Inter", style: "Regular" }]);
hero.appendChild(await tf.text({ text: "The always-on Figma design agent", size: 56, family: "Inter", style: "Bold", color: "#111111" }));
hero.appendChild(await tf.text({
  text: "Blazing fast, token-light, never re-pair a plugin.\nBuild real Figma work from a brief in seconds.",
  size: 20,
  color: "#555555",
  width: 760,
}));
const ctaRow = tf.frame({ name: "CTA row", direction: "HORIZONTAL", gap: 16 });
const primaryBtn = tf.frame({ name: "Start free", direction: "HORIZONTAL", fill: "#0066FF", padding: { top: 14, right: 32, bottom: 14, left: 32 } });
tf.append(primaryBtn, await tf.text({ text: "Start free", size: 14, color: "#FFFFFF" }));
const secondaryBtn = tf.frame({ name: "View docs", direction: "HORIZONTAL", padding: { top: 14, right: 32, bottom: 14, left: 32 } });
tf.append(secondaryBtn, await tf.text({ text: "View docs", size: 14, color: "#111111" }));
tf.append(ctaRow, primaryBtn, secondaryBtn);
hero.appendChild(ctaRow);
    `.trim(),
  },
  {
    op: "execute",
    code: `
// Build the feature grid section.
const root = await tf.findOrCreate(
  figma.currentPage,
  "Page",
  () => tf.frame({ name: "Page", direction: "VERTICAL", width: 1440, height: 5200 }),
);
const grid = tf.frame({
  name: "Features",
  direction: "VERTICAL",
  width: 1440,
  height: 560,
  fill: "#FFFFFF",
  gap: 48,
  padding: { top: 80, right: 120, bottom: 80, left: 120 },
  counterAlign: "CENTER",
});
root.appendChild(grid);
grid.appendChild(await tf.text({ text: "Everything you need", size: 36, family: "Inter", style: "Bold", color: "#111111" }));
const cards = tf.frame({ name: "Cards", direction: "HORIZONTAL", gap: 32 });
grid.appendChild(cards);
const features = [
  ["One daemon, N files", "One always-on process manages every open Figma file over WebSocket."],
  ["Token-light output", "Shaped returns, depth limits, and milestone-only screenshots cut request size."],
  ["Eval-first power", "Full Figma Plugin API available in every execute call. No canned operations."],
];
for (const [title, body] of features) {
  const card = tf.frame({ name: title, direction: "VERTICAL", width: 360, gap: 12, fill: "#F9FAFB", padding: 32 });
  tf.append(
    card,
    await tf.text({ text: title, size: 18, family: "Inter", style: "Semi Bold", color: "#111111" }),
    await tf.text({ text: body, size: 14, color: "#666666", width: 296 }),
  );
  cards.appendChild(card);
}
    `.trim(),
  },
  {
    op: "execute",
    code: `
// Build the footer section.
const root = await tf.findOrCreate(
  figma.currentPage,
  "Page",
  () => tf.frame({ name: "Page", direction: "VERTICAL", width: 1440, height: 5200 }),
);
const footer = tf.frame({
  name: "Footer",
  direction: "HORIZONTAL",
  width: 1440,
  height: 120,
  fill: "#111111",
  padding: { top: 0, right: 64, bottom: 0, left: 64 },
  counterAlign: "CENTER",
});
root.appendChild(footer);
footer.appendChild(await tf.text({ text: "© 2026 TurboFig", size: 13, color: "#AAAAAA" }));
for (const label of ["Privacy", "Terms", "Status"]) {
  footer.appendChild(await tf.text({ text: label, size: 13, color: "#AAAAAA" }));
}
    `.trim(),
  },
];

// ---------------------------------------------------------------------------
// Scenario: webpage-plain (transport)
// Same visual result as webpage, built with only the plain Figma Plugin API,
// so console-mcp can run it too. No tf.* helper calls.
// ---------------------------------------------------------------------------

const webpagePlainJobs: BridgeJob[] = [
  {
    op: "execute",
    code: `
// Set up the page frame.
const root = figma.createFrame();
root.name = "Page";
root.layoutMode = "VERTICAL";
root.resize(1440, 5200);
root.fills = [{ type: "SOLID", color: { r: 1, g: 1, b: 1 } }];
root.itemSpacing = 0;
figma.currentPage.appendChild(root);
    `.trim(),
  },
  {
    op: "execute",
    code: `
// Build the navigation bar.
await figma.loadFontAsync({ family: "Inter", style: "Bold" });
await figma.loadFontAsync({ family: "Inter", style: "Regular" });
const root = figma.currentPage.findOne((n) => n.name === "Page" && n.type === "FRAME");
const nav = figma.createFrame();
nav.name = "Nav";
nav.layoutMode = "HORIZONTAL";
nav.resize(1440, 72);
nav.fills = [{ type: "SOLID", color: { r: 1, g: 1, b: 1 } }];
nav.itemSpacing = 32;
nav.paddingLeft = 64;
nav.paddingRight = 64;
nav.counterAxisAlignItems = "CENTER";
root.appendChild(nav);
const logo = figma.createText();
logo.fontName = { family: "Inter", style: "Bold" };
logo.fontSize = 18;
logo.characters = "TurboFig";
logo.fills = [{ type: "SOLID", color: { r: 0.067, g: 0.067, b: 0.067 } }];
nav.appendChild(logo);
for (const label of ["Features", "Pricing", "Docs", "Blog"]) {
  const t = figma.createText();
  t.fontName = { family: "Inter", style: "Regular" };
  t.fontSize = 14;
  t.characters = label;
  t.fills = [{ type: "SOLID", color: { r: 0.267, g: 0.267, b: 0.267 } }];
  nav.appendChild(t);
}
const ctaBox = figma.createFrame();
ctaBox.name = "CTA";
ctaBox.layoutMode = "HORIZONTAL";
ctaBox.fills = [{ type: "SOLID", color: { r: 0, g: 0.4, b: 1 } }];
ctaBox.paddingTop = 10;
ctaBox.paddingBottom = 10;
ctaBox.paddingLeft = 20;
ctaBox.paddingRight = 20;
const ctaText = figma.createText();
ctaText.fontName = { family: "Inter", style: "Regular" };
ctaText.fontSize = 14;
ctaText.characters = "Get started";
ctaText.fills = [{ type: "SOLID", color: { r: 1, g: 1, b: 1 } }];
ctaBox.appendChild(ctaText);
nav.appendChild(ctaBox);
    `.trim(),
  },
  {
    op: "execute",
    code: `
// Build the hero section.
await figma.loadFontAsync({ family: "Inter", style: "Bold" });
await figma.loadFontAsync({ family: "Inter", style: "Regular" });
const root = figma.currentPage.findOne((n) => n.name === "Page" && n.type === "FRAME");
const hero = figma.createFrame();
hero.name = "Hero";
hero.layoutMode = "VERTICAL";
hero.resize(1440, 680);
hero.fills = [{ type: "SOLID", color: { r: 0.957, g: 0.965, b: 0.984 } }];
hero.itemSpacing = 24;
hero.paddingTop = 120;
hero.paddingBottom = 120;
hero.primaryAxisAlignItems = "CENTER";
hero.counterAxisAlignItems = "CENTER";
root.appendChild(hero);
const h1 = figma.createText();
h1.fontName = { family: "Inter", style: "Bold" };
h1.fontSize = 56;
h1.characters = "The always-on Figma design agent";
h1.fills = [{ type: "SOLID", color: { r: 0.067, g: 0.067, b: 0.067 } }];
hero.appendChild(h1);
const sub = figma.createText();
sub.fontName = { family: "Inter", style: "Regular" };
sub.fontSize = 20;
sub.resize(760, sub.height);
sub.characters = "Blazing fast, token-light, never re-pair a plugin.\\nBuild real Figma work from a brief in seconds.";
sub.fills = [{ type: "SOLID", color: { r: 0.333, g: 0.333, b: 0.333 } }];
hero.appendChild(sub);
const ctaRow = figma.createFrame();
ctaRow.name = "CTA row";
ctaRow.layoutMode = "HORIZONTAL";
ctaRow.itemSpacing = 16;
const primaryBtn = figma.createFrame();
primaryBtn.name = "Start free";
primaryBtn.layoutMode = "HORIZONTAL";
primaryBtn.fills = [{ type: "SOLID", color: { r: 0, g: 0.4, b: 1 } }];
primaryBtn.paddingTop = 14;
primaryBtn.paddingBottom = 14;
primaryBtn.paddingLeft = 32;
primaryBtn.paddingRight = 32;
const primaryText = figma.createText();
primaryText.fontName = { family: "Inter", style: "Regular" };
primaryText.fontSize = 14;
primaryText.characters = "Start free";
primaryText.fills = [{ type: "SOLID", color: { r: 1, g: 1, b: 1 } }];
primaryBtn.appendChild(primaryText);
const secondaryBtn = figma.createFrame();
secondaryBtn.name = "View docs";
secondaryBtn.layoutMode = "HORIZONTAL";
secondaryBtn.paddingTop = 14;
secondaryBtn.paddingBottom = 14;
secondaryBtn.paddingLeft = 32;
secondaryBtn.paddingRight = 32;
const secondaryText = figma.createText();
secondaryText.fontName = { family: "Inter", style: "Regular" };
secondaryText.fontSize = 14;
secondaryText.characters = "View docs";
secondaryText.fills = [{ type: "SOLID", color: { r: 0.067, g: 0.067, b: 0.067 } }];
secondaryBtn.appendChild(secondaryText);
ctaRow.appendChild(primaryBtn);
ctaRow.appendChild(secondaryBtn);
hero.appendChild(ctaRow);
    `.trim(),
  },
  {
    op: "execute",
    code: `
// Build the feature grid section.
await figma.loadFontAsync({ family: "Inter", style: "Bold" });
await figma.loadFontAsync({ family: "Inter", style: "Regular" });
const root = figma.currentPage.findOne((n) => n.name === "Page" && n.type === "FRAME");
const grid = figma.createFrame();
grid.name = "Features";
grid.layoutMode = "VERTICAL";
grid.resize(1440, 560);
grid.fills = [{ type: "SOLID", color: { r: 1, g: 1, b: 1 } }];
grid.itemSpacing = 48;
grid.paddingTop = 80;
grid.paddingBottom = 80;
grid.paddingLeft = 120;
grid.paddingRight = 120;
grid.counterAxisAlignItems = "CENTER";
root.appendChild(grid);
const h2 = figma.createText();
h2.fontName = { family: "Inter", style: "Bold" };
h2.fontSize = 36;
h2.characters = "Everything you need";
h2.fills = [{ type: "SOLID", color: { r: 0.067, g: 0.067, b: 0.067 } }];
grid.appendChild(h2);
const cards = figma.createFrame();
cards.name = "Cards";
cards.layoutMode = "HORIZONTAL";
cards.itemSpacing = 32;
grid.appendChild(cards);
const features = [
  ["One daemon, N files", "One always-on process manages every open Figma file over WebSocket."],
  ["Token-light output", "Shaped returns, depth limits, and milestone-only screenshots cut request size."],
  ["Eval-first power", "Full Figma Plugin API available in every execute call. No canned operations."],
];
for (const [title, body] of features) {
  const card = figma.createFrame();
  card.name = title;
  card.layoutMode = "VERTICAL";
  card.resize(360, card.height);
  card.itemSpacing = 12;
  card.fills = [{ type: "SOLID", color: { r: 0.976, g: 0.980, b: 0.984 } }];
  card.paddingTop = 32;
  card.paddingBottom = 32;
  card.paddingLeft = 32;
  card.paddingRight = 32;
  const title_t = figma.createText();
  title_t.fontName = { family: "Inter", style: "Bold" };
  title_t.fontSize = 18;
  title_t.characters = title;
  title_t.fills = [{ type: "SOLID", color: { r: 0.067, g: 0.067, b: 0.067 } }];
  const body_t = figma.createText();
  body_t.fontName = { family: "Inter", style: "Regular" };
  body_t.fontSize = 14;
  body_t.resize(296, body_t.height);
  body_t.characters = body;
  body_t.fills = [{ type: "SOLID", color: { r: 0.4, g: 0.4, b: 0.4 } }];
  card.appendChild(title_t);
  card.appendChild(body_t);
  cards.appendChild(card);
}
    `.trim(),
  },
  {
    op: "execute",
    code: `
// Build the footer section.
await figma.loadFontAsync({ family: "Inter", style: "Regular" });
const root = figma.currentPage.findOne((n) => n.name === "Page" && n.type === "FRAME");
const footer = figma.createFrame();
footer.name = "Footer";
footer.layoutMode = "HORIZONTAL";
footer.resize(1440, 120);
footer.fills = [{ type: "SOLID", color: { r: 0.067, g: 0.067, b: 0.067 } }];
footer.paddingLeft = 64;
footer.paddingRight = 64;
footer.counterAxisAlignItems = "CENTER";
root.appendChild(footer);
const copy = figma.createText();
copy.fontName = { family: "Inter", style: "Regular" };
copy.fontSize = 13;
copy.characters = "© 2026 TurboFig";
copy.fills = [{ type: "SOLID", color: { r: 0.667, g: 0.667, b: 0.667 } }];
footer.appendChild(copy);
for (const label of ["Privacy", "Terms", "Status"]) {
  const t = figma.createText();
  t.fontName = { family: "Inter", style: "Regular" };
  t.fontSize = 13;
  t.characters = label;
  t.fills = [{ type: "SOLID", color: { r: 0.667, g: 0.667, b: 0.667 } }];
  footer.appendChild(t);
}
    `.trim(),
  },
];

// ---------------------------------------------------------------------------
// Scenario: deck20 (transport + helpers)
// Builds a 20-slide presentation in three batched execute calls, using tf.*.
// ---------------------------------------------------------------------------

const deck20Jobs: BridgeJob[] = [
  {
    op: "execute",
    code: `
// Slides 1-7: title, agenda, and first five content slides.
await tf.loadFonts([{ family: "Inter", style: "Regular" }, { family: "Inter", style: "Bold" }]);

const deckFrame = tf.frame({ name: "Deck", direction: "HORIZONTAL", gap: 80, fill: "#FFFFFF" });
figma.currentPage.appendChild(deckFrame);

// Slide 1: title slide.
const s1 = tf.slide({ fill: "#0A0A0A" });
deckFrame.appendChild(s1);
tf.append(s1,
  await tf.text({ text: "TurboFig", size: 96, family: "Inter", style: "Bold", color: "#FFFFFF" }),
  await tf.text({ text: "The always-on Figma design agent", size: 32, color: "#AAAAAA" }),
);

// Slide 2: agenda.
const s2 = tf.slide({ fill: "#0F0F1A" });
deckFrame.appendChild(s2);
s2.appendChild(await tf.text({ text: "Agenda", size: 48, family: "Inter", style: "Bold", color: "#FFFFFF" }));
for (const [i, item] of ["Problem", "Architecture", "Token discipline", "Demo", "Roadmap", "Q&A"].entries()) {
  s2.appendChild(await tf.text({ text: (i + 1) + ". " + item, size: 24, color: "#CCCCCC" }));
}

// Slides 3-7: content slides.
for (const [i, title] of ["The Problem", "Our Architecture", "Token Discipline", "Live Demo", "Key Metrics"].entries()) {
  const s = tf.slide({ fill: "#0A0A0A" });
  deckFrame.appendChild(s);
  tf.append(s,
    await tf.text({ text: title, size: 56, family: "Inter", style: "Bold", color: "#FFFFFF" }),
    await tf.text({ text: "Detail for " + title.toLowerCase() + " goes here.", size: 24, color: "#888888" }),
    tf.rect({ width: 80, height: 6, fill: "#0066FF", corner: 3 }),
  );
}
    `.trim(),
  },
  {
    op: "execute",
    code: `
// Slides 8-14: deep-dive content slides.
const deckFrame = await tf.findOrCreate(
  figma.currentPage,
  "Deck",
  () => tf.frame({ name: "Deck", direction: "HORIZONTAL", gap: 80 }),
);

const deepDive = [
  ["Daemon Internals", "Three tokio::spawn tasks share one Arc<AppState>: MCP HTTP, WebSocket, file-bridge."],
  ["Plugin Architecture", "Thin TS plugin. UI iframe owns WebSocket + reconnect. Main thread runs Figma API."],
  ["Routing Registry", "conn_id-keyed registry holds N plugins. resolve_route picks the right file per call."],
  ["File-Bridge Protocol", "Inbox JSON -> daemon -> outbox JSON. Locked-down clients need no curl or MCP."],
  ["Request Timeout", "Per-request timeout stops a silent plugin from hanging a call indefinitely."],
  ["Shaped Returns", "ids-first, opt-in fields, depth limit. Never dump a full node tree by default."],
  ["Screenshot Policy", "Downscaled + file-mode by default. Inline high-res only on explicit request."],
];
for (const [title, body] of deepDive) {
  const s = tf.slide({ fill: "#0A0A0A" });
  deckFrame.appendChild(s);
  tf.append(s,
    await tf.text({ text: title, size: 48, family: "Inter", style: "Bold", color: "#FFFFFF" }),
    await tf.text({ text: body, size: 22, color: "#BBBBBB", width: 1400 }),
  );
}
    `.trim(),
  },
  {
    op: "execute",
    code: `
// Slides 15-20: case studies, roadmap, summary, Q&A.
const deckFrame = await tf.findOrCreate(
  figma.currentPage,
  "Deck",
  () => tf.frame({ name: "Deck", direction: "HORIZONTAL", gap: 80 }),
);

// Slide 15: case study A.
const s15 = tf.slide({ fill: "#0F1820" });
deckFrame.appendChild(s15);
tf.append(s15,
  await tf.text({ text: "Case Study: Marketing Page", size: 48, family: "Inter", style: "Bold", color: "#FFFFFF" }),
  await tf.text({ text: "Five execute calls. Nav, hero, feature grid, footer.\nFull numbers: see the published bench report.", size: 22, color: "#AAAAAA", width: 1400 }),
);

// Slide 16: case study B.
const s16 = tf.slide({ fill: "#0F1820" });
deckFrame.appendChild(s16);
tf.append(s16,
  await tf.text({ text: "Case Study: 20-Slide Deck", size: 48, family: "Inter", style: "Bold", color: "#FFFFFF" }),
  await tf.text({ text: "Three batched execute calls. 20 slides, varied layouts.\nFull numbers: see the published bench report.", size: 22, color: "#AAAAAA", width: 1400 }),
);

// Slide 17: roadmap.
const s17 = tf.slide({ fill: "#0A0A0A" });
deckFrame.appendChild(s17);
s17.appendChild(await tf.text({ text: "Roadmap", size: 56, family: "Inter", style: "Bold", color: "#FFFFFF" }));
for (const [i, p] of ["Phase 5: Image-first shapes", "Phase 6: Eval helpers", "Phase 7: Design skill", "Phase 8: Brand packs", "Phase 10: TLS + distribution"].entries()) {
  s17.appendChild(await tf.text({ text: "Q" + (i + 1) + " — " + p, size: 22, color: "#CCCCCC" }));
}

// Slide 18: what the harness measures.
const s18 = tf.slide({ fill: "#001133" });
deckFrame.appendChild(s18);
s18.appendChild(await tf.text({ text: "How We Measure", size: 56, family: "Inter", style: "Bold", color: "#FFFFFF" }));
const stats = tf.frame({ name: "Stats", direction: "HORIZONTAL", gap: 64 });
s18.appendChild(stats);
for (const [num, label] of [["4", "tools in the public surface"], ["N", "open files in parallel"]]) {
  const cell = tf.frame({ name: num, direction: "VERTICAL", gap: 8, counterAlign: "CENTER" });
  tf.append(cell,
    await tf.text({ text: num, size: 64, family: "Inter", style: "Bold", color: "#0066FF" }),
    await tf.text({ text: label, size: 16, color: "#AAAAAA" }),
  );
  stats.appendChild(cell);
}

// Slide 19: summary.
const s19 = tf.slide({ fill: "#0A0A0A" });
deckFrame.appendChild(s19);
s19.appendChild(await tf.text({ text: "Summary", size: 56, family: "Inter", style: "Bold", color: "#FFFFFF" }));
for (const line of ["One daemon process, always on.", "Token-light by design, not by accident.", "Eval-first: full API, no canned operations.", "File-bridge unlocks locked-down environments."]) {
  s19.appendChild(await tf.text({ text: "✓  " + line, size: 24, color: "#DDDDDD" }));
}

// Slide 20: thank you.
const s20 = tf.slide({ fill: "#0A0A0A" });
deckFrame.appendChild(s20);
tf.append(s20,
  await tf.text({ text: "Thank you", size: 96, family: "Inter", style: "Bold", color: "#FFFFFF" }),
  await tf.text({ text: "Questions?", size: 32, color: "#666666" }),
);
    `.trim(),
  },
];

// ---------------------------------------------------------------------------
// Scenario: deck20-plain (transport)
// Builds the same 20-slide deck structure with only the plain Plugin API
// (no tf.slide/tf.text/tf.rect helpers), as four auto-layout frames per
// section, so console-mcp can run it too.
// ---------------------------------------------------------------------------

function plainSlide(fill: string): string {
  return `(() => {
  const s = figma.createFrame();
  s.layoutMode = "VERTICAL";
  s.resize(1920, 1080);
  s.fills = [{ type: "SOLID", color: ${fill} }];
  s.itemSpacing = 24;
  s.paddingTop = 96;
  s.paddingLeft = 96;
  s.paddingRight = 96;
  return s;
})()`;
}

const deck20PlainJobs: BridgeJob[] = [
  {
    op: "execute",
    code: `
// Slides 1-7: title, agenda, and first five content slides.
await figma.loadFontAsync({ family: "Inter", style: "Regular" });
await figma.loadFontAsync({ family: "Inter", style: "Bold" });

const deckFrame = figma.createFrame();
deckFrame.name = "Deck";
deckFrame.layoutMode = "HORIZONTAL";
deckFrame.itemSpacing = 80;
deckFrame.fills = [{ type: "SOLID", color: { r: 1, g: 1, b: 1 } }];
figma.currentPage.appendChild(deckFrame);

function text(s, str, size, style, r, g, b) {
  const t = figma.createText();
  t.fontName = { family: "Inter", style };
  t.fontSize = size;
  t.characters = str;
  t.fills = [{ type: "SOLID", color: { r, g, b } }];
  s.appendChild(t);
  return t;
}

// Slide 1: title slide.
const s1 = ${plainSlide("{ r: 0.04, g: 0.04, b: 0.04 }")};
deckFrame.appendChild(s1);
text(s1, "TurboFig", 96, "Bold", 1, 1, 1);
text(s1, "The always-on Figma design agent", 32, "Regular", 0.667, 0.667, 0.667);

// Slide 2: agenda.
const s2 = ${plainSlide("{ r: 0.059, g: 0.059, b: 0.102 }")};
deckFrame.appendChild(s2);
text(s2, "Agenda", 48, "Bold", 1, 1, 1);
for (const [i, item] of ["Problem", "Architecture", "Token discipline", "Demo", "Roadmap", "Q&A"].entries()) {
  text(s2, (i + 1) + ". " + item, 24, "Regular", 0.8, 0.8, 0.8);
}

// Slides 3-7: content slides.
for (const title of ["The Problem", "Our Architecture", "Token Discipline", "Live Demo", "Key Metrics"]) {
  const s = ${plainSlide("{ r: 0.04, g: 0.04, b: 0.04 }")};
  deckFrame.appendChild(s);
  text(s, title, 56, "Bold", 1, 1, 1);
  text(s, "Detail for " + title.toLowerCase() + " goes here.", 24, "Regular", 0.533, 0.533, 0.533);
}
    `.trim(),
  },
  {
    op: "execute",
    code: `
// Slides 8-14: deep-dive content slides.
const deckFrame = figma.currentPage.findOne((n) => n.name === "Deck" && n.type === "FRAME");

function text(s, str, size, style, r, g, b) {
  const t = figma.createText();
  t.fontName = { family: "Inter", style };
  t.fontSize = size;
  t.characters = str;
  t.fills = [{ type: "SOLID", color: { r, g, b } }];
  s.appendChild(t);
  return t;
}

const deepDive = [
  ["Daemon Internals", "Three tokio::spawn tasks share one Arc<AppState>: MCP HTTP, WebSocket, file-bridge."],
  ["Plugin Architecture", "Thin TS plugin. UI iframe owns WebSocket + reconnect. Main thread runs Figma API."],
  ["Routing Registry", "conn_id-keyed registry holds N plugins. resolve_route picks the right file per call."],
  ["File-Bridge Protocol", "Inbox JSON -> daemon -> outbox JSON. Locked-down clients need no curl or MCP."],
  ["Request Timeout", "Per-request timeout stops a silent plugin from hanging a call indefinitely."],
  ["Shaped Returns", "ids-first, opt-in fields, depth limit. Never dump a full node tree by default."],
  ["Screenshot Policy", "Downscaled + file-mode by default. Inline high-res only on explicit request."],
];
for (const [title, body] of deepDive) {
  const s = ${plainSlide("{ r: 0.04, g: 0.04, b: 0.04 }")};
  deckFrame.appendChild(s);
  const bodyText = text(s, body, 22, "Regular", 0.733, 0.733, 0.733);
  bodyText.resize(1400, bodyText.height);
  text(s, title, 48, "Bold", 1, 1, 1);
}
    `.trim(),
  },
  {
    op: "execute",
    code: `
// Slides 15-20: case studies, roadmap, summary, Q&A.
const deckFrame = figma.currentPage.findOne((n) => n.name === "Deck" && n.type === "FRAME");

function text(s, str, size, style, r, g, b) {
  const t = figma.createText();
  t.fontName = { family: "Inter", style };
  t.fontSize = size;
  t.characters = str;
  t.fills = [{ type: "SOLID", color: { r, g, b } }];
  s.appendChild(t);
  return t;
}

// Slide 15: case study A.
const s15 = ${plainSlide("{ r: 0.059, g: 0.094, b: 0.125 }")};
deckFrame.appendChild(s15);
text(s15, "Case Study: Marketing Page", 48, "Bold", 1, 1, 1);
text(s15, "Five execute calls. Nav, hero, feature grid, footer.\\nFull numbers: see the published bench report.", 22, "Regular", 0.667, 0.667, 0.667);

// Slide 16: case study B.
const s16 = ${plainSlide("{ r: 0.059, g: 0.094, b: 0.125 }")};
deckFrame.appendChild(s16);
text(s16, "Case Study: 20-Slide Deck", 48, "Bold", 1, 1, 1);
text(s16, "Three batched execute calls. 20 slides, varied layouts.\\nFull numbers: see the published bench report.", 22, "Regular", 0.667, 0.667, 0.667);

// Slide 17: roadmap.
const s17 = ${plainSlide("{ r: 0.04, g: 0.04, b: 0.04 }")};
deckFrame.appendChild(s17);
text(s17, "Roadmap", 56, "Bold", 1, 1, 1);
for (const [i, p] of ["Phase 5: Image-first shapes", "Phase 6: Eval helpers", "Phase 7: Design skill", "Phase 8: Brand packs", "Phase 10: TLS + distribution"].entries()) {
  text(s17, "Q" + (i + 1) + " — " + p, 22, "Regular", 0.8, 0.8, 0.8);
}

// Slide 18: what the harness measures.
const s18 = ${plainSlide("{ r: 0, g: 0.067, b: 0.2 }")};
deckFrame.appendChild(s18);
text(s18, "How We Measure", 56, "Bold", 1, 1, 1);
const stats = figma.createFrame();
stats.layoutMode = "HORIZONTAL";
stats.itemSpacing = 64;
s18.appendChild(stats);
for (const [num, label] of [["4", "tools in the public surface"], ["N", "open files in parallel"]]) {
  const cell = figma.createFrame();
  cell.layoutMode = "VERTICAL";
  cell.itemSpacing = 8;
  cell.counterAxisAlignItems = "CENTER";
  text(cell, num, 64, "Bold", 0, 0.4, 1);
  text(cell, label, 16, "Regular", 0.667, 0.667, 0.667);
  stats.appendChild(cell);
}

// Slide 19: summary.
const s19 = ${plainSlide("{ r: 0.04, g: 0.04, b: 0.04 }")};
deckFrame.appendChild(s19);
text(s19, "Summary", 56, "Bold", 1, 1, 1);
for (const line of ["One daemon process, always on.", "Token-light by design, not by accident.", "Eval-first: full API, no canned operations.", "File-bridge unlocks locked-down environments."]) {
  text(s19, "✓  " + line, 24, "Regular", 0.867, 0.867, 0.867);
}

// Slide 20: thank you.
const s20 = ${plainSlide("{ r: 0.04, g: 0.04, b: 0.04 }")};
deckFrame.appendChild(s20);
text(s20, "Thank you", 96, "Bold", 1, 1, 1);
text(s20, "Questions?", 32, "Regular", 0.4, 0.4, 0.4);
    `.trim(),
  },
];

// ---------------------------------------------------------------------------
// Scenario: read-selection (transport)
// Creates one rectangle, selects it, then issues a read of the selection.
// ---------------------------------------------------------------------------

const readSelectionJobs: BridgeJob[] = [
  {
    op: "execute",
    code: `
const rect = figma.createRectangle();
rect.name = "Bench Target";
rect.resize(200, 120);
rect.fills = [{ type: "SOLID", color: { r: 0.2, g: 0.5, b: 1 } }];
figma.currentPage.appendChild(rect);
figma.currentPage.selection = [rect];
    `.trim(),
  },
  {
    op: "get_selection",
    fields: ["fills"],
    depth: 0,
  },
];

// ---------------------------------------------------------------------------
// Scenario: read-screenshot (transport)
// Creates one rectangle, selects it, then screenshots it.
// ---------------------------------------------------------------------------

const readScreenshotJobs: BridgeJob[] = [
  {
    op: "execute",
    code: `
const rect = figma.createRectangle();
rect.name = "Bench Target";
rect.resize(200, 120);
rect.fills = [{ type: "SOLID", color: { r: 1, g: 0.4, b: 0.2 } }];
figma.currentPage.appendChild(rect);
figma.currentPage.selection = [rect];
    `.trim(),
  },
  {
    // console-mcp's figma_take_screenshot always fetches the rendered image
    // and returns it inline as base64 (see figma-console-mcp dist/local.js,
    // the "Return as MCP image content type" path). turbofig defaults to
    // file mode (a ~100-byte path), which is not the same shape of
    // response. returnMode: "inline" and fullRes: true make turbofig return
    // an undownscaled inline image too, so both targets are compared on the
    // same kind of payload. See bench/README.md "Known limits" for what
    // still cannot be made exactly equal (maxDim/scale are not the same
    // downscale knob; see Known limits).
    op: "screenshot",
    scale: 2,
    returnMode: "inline",
    fullRes: true,
  },
];

// ---------------------------------------------------------------------------
// Exports
// ---------------------------------------------------------------------------

export const webpage: Scenario = {
  name: "webpage",
  label: "transport + helpers",
  targets: TURBOFIG_ONLY,
  pageName: "bench-webpage",
  jobs: webpageJobs,
};

export const webpagePlain: Scenario = {
  name: "webpage-plain",
  label: "transport",
  targets: ALL_TARGETS,
  pageName: "bench-webpage-plain",
  jobs: webpagePlainJobs,
};

export const deck20: Scenario = {
  name: "deck20",
  label: "transport + helpers",
  targets: TURBOFIG_ONLY,
  pageName: "bench-deck20",
  jobs: deck20Jobs,
};

export const deck20Plain: Scenario = {
  name: "deck20-plain",
  label: "transport",
  targets: ALL_TARGETS,
  pageName: "bench-deck20-plain",
  jobs: deck20PlainJobs,
};

export const readSelection: Scenario = {
  name: "read-selection",
  label: "transport",
  targets: ALL_TARGETS,
  pageName: "bench-read-selection",
  jobs: readSelectionJobs,
};

export const readScreenshot: Scenario = {
  name: "read-screenshot",
  label: "transport",
  targets: ALL_TARGETS,
  pageName: "bench-read-screenshot",
  jobs: readScreenshotJobs,
};

export const scenarios: Record<string, Scenario> = {
  webpage,
  "webpage-plain": webpagePlain,
  deck20,
  "deck20-plain": deck20Plain,
  "read-selection": readSelection,
  "read-screenshot": readScreenshot,
};
