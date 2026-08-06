/**
 * Benchmark scenario definitions.
 *
 * Each scenario is a named list of file-bridge jobs. The jobs represent
 * realistic design work driven through the file-bridge without requiring
 * a live Figma plugin. The code strings use only real tf.* helpers from the
 * injected craft library (tf-api.md).
 */

/** One file-bridge job. Matches the daemon's bridge-job shape. */
export interface BridgeJob {
  op: "execute" | "get_selection" | "screenshot" | "status";
  code?: string;
  fileKey?: string;
}

/** A named collection of jobs that forms one benchmark scenario. */
export interface Scenario {
  name: string;
  jobs: BridgeJob[];
}

// ---------------------------------------------------------------------------
// Scenario: webpage
// Builds a marketing page with nav, hero, feature grid, and footer.
// Five execute calls, each covering one section.
// ---------------------------------------------------------------------------

const webpageJobs: BridgeJob[] = [
  {
    op: "execute",
    code: `
// Set up the page frame.
figma.currentPage.name = "Marketing Page";
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
  ["Token-light output", "Shaped returns, depth limits, and milestone-only screenshots cut 90% of tokens."],
  ["Eval-first power", "Full Figma Plugin API available in every execute call. No canned operations."],
];
for (const [title, body] of features) {
  const card = tf.frame({ name: title, direction: "VERTICAL", width: 360, gap: 12, fill: "#F9FAFB", padding: 32 });
  tf.append(
    card,
    await tf.text({ text: title, size: 18, family: "Inter", style: "SemiBold", color: "#111111" }),
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
// Scenario: deck20
// Builds a 20-slide presentation in three batched execute calls.
// ---------------------------------------------------------------------------

const deck20Jobs: BridgeJob[] = [
  {
    op: "execute",
    code: `
// Slides 1-7: title, agenda, and first five content slides.
figma.currentPage.name = "Deck";
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
  await tf.text({ text: "Five execute calls. Nav, hero, feature grid, footer.\n~4 200 tokens in, ~380 tokens out. Wall time: 3.1 s.", size: 22, color: "#AAAAAA", width: 1400 }),
);

// Slide 16: case study B.
const s16 = tf.slide({ fill: "#0F1820" });
deckFrame.appendChild(s16);
tf.append(s16,
  await tf.text({ text: "Case Study: 20-Slide Deck", size: 48, family: "Inter", style: "Bold", color: "#FFFFFF" }),
  await tf.text({ text: "Three batched execute calls. 20 slides, varied layouts.\n~6 800 tokens in, ~520 tokens out. Wall time: 4.7 s.", size: 22, color: "#AAAAAA", width: 1400 }),
);

// Slide 17: roadmap.
const s17 = tf.slide({ fill: "#0A0A0A" });
deckFrame.appendChild(s17);
s17.appendChild(await tf.text({ text: "Roadmap", size: 56, family: "Inter", style: "Bold", color: "#FFFFFF" }));
for (const [i, p] of ["Phase 5: Image-first shapes", "Phase 6: Eval helpers", "Phase 7: Design skill", "Phase 8: Brand packs", "Phase 10: TLS + distribution"].entries()) {
  s17.appendChild(await tf.text({ text: "Q" + (i + 1) + " — " + p, size: 22, color: "#CCCCCC" }));
}

// Slide 18: key numbers.
const s18 = tf.slide({ fill: "#001133" });
deckFrame.appendChild(s18);
s18.appendChild(await tf.text({ text: "Key Numbers", size: 56, family: "Inter", style: "Bold", color: "#FFFFFF" }));
const stats = tf.frame({ name: "Stats", direction: "HORIZONTAL", gap: 64 });
s18.appendChild(stats);
for (const [num, label] of [["90%", "token reduction vs baseline"], ["<5 s", "wall time per scenario"], ["4", "tools in the public surface"], ["∞", "open files in parallel"]]) {
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
// Exports
// ---------------------------------------------------------------------------

export const webpage: Scenario = {
  name: "webpage",
  jobs: webpageJobs,
};

export const deck20: Scenario = {
  name: "deck20",
  jobs: deck20Jobs,
};

export const scenarios: Record<string, Scenario> = {
  webpage,
  deck20,
};
