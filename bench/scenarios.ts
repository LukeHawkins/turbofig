/**
 * Benchmark scenario definitions.
 *
 * Each scenario is a named list of file-bridge jobs. The jobs represent
 * realistic design work driven through the file-bridge without requiring
 * a live Figma plugin. The code strings use tf.* helpers from the
 * injected craft library but do not need to execute for benchmarking.
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
const page = tf.page({ name: "Marketing Page" });
const root = tf.frame({
  name: "Page",
  w: 1440,
  h: 5200,
  fill: "#FFFFFF",
  autoLayout: "V",
  gap: 0,
  parent: page,
});
tf.setVar("color/brand", "#0066FF");
tf.setVar("color/surface", "#F4F6FB");
tf.setVar("spacing/section", 80);
    `.trim(),
  },
  {
    op: "execute",
    code: `
const nav = tf.frame({
  name: "Nav",
  w: 1440,
  h: 72,
  fill: "#FFFFFF",
  autoLayout: "H",
  align: "center",
  gap: 0,
  paddingH: 64,
  parent: root,
});
const logo = tf.text({ content: "TurboFig", fontSize: 18, fontWeight: 700, fill: "#111111", parent: nav });
const spacer = tf.spacer({ parent: nav });
const navLinks = ["Features", "Pricing", "Docs", "Blog"].map((label) =>
  tf.text({ content: label, fontSize: 14, fill: "#444444", parent: nav }),
);
const cta = tf.button({ label: "Get started", fill: "#0066FF", textFill: "#FFFFFF", r: 6, parent: nav });
    `.trim(),
  },
  {
    op: "execute",
    code: `
const hero = tf.frame({
  name: "Hero",
  w: 1440,
  h: 680,
  fill: "#F4F6FB",
  autoLayout: "V",
  align: "center",
  gap: 24,
  paddingV: 120,
  parent: root,
});
tf.text({
  content: "The always-on Figma design agent",
  fontSize: 56,
  fontWeight: 800,
  fill: "#111111",
  align: "center",
  parent: hero,
});
tf.text({
  content: "Blazing fast, token-light, never re-pair a plugin.\nBuild real Figma work from a brief in seconds.",
  fontSize: 20,
  fill: "#555555",
  align: "center",
  parent: hero,
});
tf.deck([
  tf.button({ label: "Start free", fill: "#0066FF", textFill: "#FFFFFF", r: 8, h: 48, w: 160 }),
  tf.button({ label: "View docs", fill: "transparent", stroke: "#CCCCCC", textFill: "#111111", r: 8, h: 48, w: 160 }),
], { direction: "H", gap: 16, parent: hero });
    `.trim(),
  },
  {
    op: "execute",
    code: `
const grid = tf.frame({
  name: "Features",
  w: 1440,
  h: 560,
  fill: "#FFFFFF",
  autoLayout: "V",
  align: "center",
  gap: 48,
  paddingV: 80,
  paddingH: 120,
  parent: root,
});
tf.text({ content: "Everything you need", fontSize: 36, fontWeight: 700, fill: "#111111", align: "center", parent: grid });
const cards = tf.frame({ name: "Cards", autoLayout: "H", gap: 32, parent: grid });
const features = [
  { title: "One daemon, N files", body: "One always-on process manages every open Figma file over WebSocket." },
  { title: "Token-light output", body: "Shaped returns, depth limits, and milestone-only screenshots cut 90% of tokens." },
  { title: "Eval-first power", body: "Full Figma Plugin API available in every execute call. No canned operations." },
];
features.forEach(({ title, body }) => {
  const card = tf.frame({ name: title, w: 360, autoLayout: "V", gap: 12, fill: "#F9FAFB", r: 12, padding: 32, parent: cards });
  tf.text({ content: title, fontSize: 18, fontWeight: 600, fill: "#111111", parent: card });
  tf.text({ content: body, fontSize: 14, fill: "#666666", lineHeight: 1.6, parent: card });
});
    `.trim(),
  },
  {
    op: "execute",
    code: `
const footer = tf.frame({
  name: "Footer",
  w: 1440,
  h: 120,
  fill: "#111111",
  autoLayout: "H",
  align: "center",
  paddingH: 64,
  parent: root,
});
tf.text({ content: "© 2026 TurboFig", fontSize: 13, fill: "#AAAAAA", parent: footer });
const fSpacer = tf.spacer({ parent: footer });
["Privacy", "Terms", "Status"].forEach((label) => {
  tf.text({ content: label, fontSize: 13, fill: "#AAAAAA", parent: footer });
});
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
const page = tf.page({ name: "Deck" });
const deck = tf.frame({ name: "Deck", w: 13824, h: 7776, autoLayout: "H", gap: 48, fill: "#FFFFFF", parent: page });
tf.setVar("color/accent", "#0066FF");
tf.setVar("color/bg", "#0A0A0A");

function slide(n, bg) {
  return tf.frame({ name: "Slide " + n, w: 1920, h: 1080, fill: bg || "#0A0A0A", r: 0, parent: deck });
}

// Slide 1: title
const s1 = slide(1);
tf.text({ content: "TurboFig", fontSize: 96, fontWeight: 800, fill: "#FFFFFF", align: "center", parent: s1 });
tf.text({ content: "The always-on Figma design agent", fontSize: 32, fill: "#AAAAAA", align: "center", parent: s1 });

// Slide 2: agenda
const s2 = slide(2, "#0F0F1A");
tf.text({ content: "Agenda", fontSize: 48, fontWeight: 700, fill: "#FFFFFF", parent: s2 });
["Problem", "Architecture", "Token discipline", "Demo", "Roadmap", "Q&A"].forEach((item, i) => {
  tf.text({ content: (i + 1) + ". " + item, fontSize: 24, fill: "#CCCCCC", parent: s2 });
});

// Slides 3-7: content
const topics = ["The Problem", "Our Architecture", "Token Discipline", "Live Demo", "Key Metrics"];
topics.forEach((title, i) => {
  const s = slide(i + 3);
  tf.text({ content: title, fontSize: 56, fontWeight: 700, fill: "#FFFFFF", parent: s });
  tf.text({ content: "Detail for " + title.toLowerCase() + " goes here.", fontSize: 24, fill: "#888888", parent: s });
  const bar = tf.frame({ name: "Accent bar", w: 80, h: 6, fill: "#0066FF", r: 3, parent: s });
});
    `.trim(),
  },
  {
    op: "execute",
    code: `
// Slides 8-14: deep-dive content slides.
function slide(n, bg) {
  return tf.frame({ name: "Slide " + n, w: 1920, h: 1080, fill: bg || "#0A0A0A", r: 0, parent: deck });
}

const deepDive = [
  ["Daemon Internals", "Three tokio::spawn tasks share one Arc<AppState>: MCP HTTP, WebSocket, file-bridge."],
  ["Plugin Architecture", "Thin TS plugin. UI iframe owns WebSocket + reconnect. Main thread runs Figma API."],
  ["Routing Registry", "conn_id-keyed registry holds N plugins. resolve_route picks the right file per call."],
  ["File-Bridge Protocol", "Inbox JSON → daemon → outbox JSON. Locked-down clients need no curl or MCP."],
  ["Request Timeout", "Per-request timeout stops a silent plugin from hanging a call indefinitely."],
  ["Shaped Returns", "ids-first, opt-in fields, depth limit. Never dump a full node tree by default."],
  ["Screenshot Policy", "Downscaled + file-mode by default. Inline high-res only on explicit request."],
];
deepDive.forEach(([title, body], i) => {
  const s = slide(i + 8);
  tf.text({ content: title, fontSize: 48, fontWeight: 700, fill: "#FFFFFF", parent: s });
  tf.text({ content: body, fontSize: 22, fill: "#BBBBBB", lineHeight: 1.7, maxW: 1400, parent: s });
});
    `.trim(),
  },
  {
    op: "execute",
    code: `
// Slides 15-20: case studies, roadmap, summary, Q&A.
function slide(n, bg) {
  return tf.frame({ name: "Slide " + n, w: 1920, h: 1080, fill: bg || "#0A0A0A", r: 0, parent: deck });
}

// Slide 15: case study A
const s15 = slide(15, "#0F1820");
tf.text({ content: "Case Study: Marketing Page", fontSize: 48, fontWeight: 700, fill: "#FFFFFF", parent: s15 });
tf.text({ content: "Five execute calls. Nav, hero, feature grid, footer.\n~4 200 tokens in, ~380 tokens out. Wall time: 3.1 s.", fontSize: 22, fill: "#AAAAAA", lineHeight: 1.6, parent: s15 });

// Slide 16: case study B
const s16 = slide(16, "#0F1820");
tf.text({ content: "Case Study: 20-Slide Deck", fontSize: 48, fontWeight: 700, fill: "#FFFFFF", parent: s16 });
tf.text({ content: "Three batched execute calls. 20 slides, varied layouts.\n~6 800 tokens in, ~520 tokens out. Wall time: 4.7 s.", fontSize: 22, fill: "#AAAAAA", lineHeight: 1.6, parent: s16 });

// Slide 17: roadmap
const s17 = slide(17);
tf.text({ content: "Roadmap", fontSize: 56, fontWeight: 700, fill: "#FFFFFF", parent: s17 });
const phases = ["Phase 5: Image-first shapes", "Phase 6: Eval helpers", "Phase 7: Design skill", "Phase 8: Brand packs", "Phase 10: TLS + distribution"];
phases.forEach((p, i) => {
  tf.text({ content: "Q" + (i + 1) + " — " + p, fontSize: 22, fill: "#CCCCCC", parent: s17 });
});

// Slide 18: key numbers
const s18 = slide(18, "#001133");
tf.text({ content: "Key Numbers", fontSize: 56, fontWeight: 700, fill: "#FFFFFF", parent: s18 });
const stats = tf.frame({ name: "Stats", autoLayout: "H", gap: 64, parent: s18 });
[["90%", "token reduction vs console MCP"], ["<5 s", "wall time per scenario"], ["4", "tools in the public surface"], ["∞", "open files in parallel"]].forEach(([num, label]) => {
  const cell = tf.frame({ name: num, autoLayout: "V", gap: 8, align: "center", parent: stats });
  tf.text({ content: num, fontSize: 64, fontWeight: 800, fill: "#0066FF", parent: cell });
  tf.text({ content: label, fontSize: 16, fill: "#AAAAAA", align: "center", parent: cell });
});

// Slide 19: summary
const s19 = slide(19);
tf.text({ content: "Summary", fontSize: 56, fontWeight: 700, fill: "#FFFFFF", parent: s19 });
["One daemon process, always on.", "Token-light by design, not by accident.", "Eval-first: full API, no canned operations.", "File-bridge unlocks locked-down environments."].forEach((line) => {
  tf.text({ content: "✓  " + line, fontSize: 24, fill: "#DDDDDD", parent: s19 });
});

// Slide 20: thank you
const s20 = slide(20, "#0A0A0A");
tf.text({ content: "Thank you", fontSize: 96, fontWeight: 800, fill: "#FFFFFF", align: "center", parent: s20 });
tf.text({ content: "Questions?", fontSize: 32, fill: "#666666", align: "center", parent: s20 });
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
