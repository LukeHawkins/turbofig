/**
 * Turbofig `minimal` taste profile.
 *
 * Restrained style: generous whitespace, few type sizes, single-column bias.
 * Use this profile for focused reading experiences, documentation, and
 * interfaces where clarity and calm are the primary design goals.
 *
 * This file is a plain script, not an ES module. The daemon prepends it to
 * user eval code so that `taste` is in scope. Do not add export, import, or
 * require statements.
 */

const taste = {
  id: "minimal",

  label: "Minimal",

  /**
   * Colour palette. Six hex tokens: near-monochrome. One dark charcoal-slate
   * brand (no strong hue), one cool gray accent (barely visible), two near-
   * white surfaces, and two dark text values. text is not pure black (#000000)
   * per the pure-black blocklist entry.
   */
  palette: {
    brand: "#2D3748",
    accent: "#718096",
    surface: "#FAFAFA",
    surfaceAlt: "#EDF2F7",
    text: "#1A202C",
    textMuted: "#7A8B9A",
  },

  /**
   * Spacing scale in px (ascending). Generous steps enforce visible breathing
   * room. Fewer values reduce the chance of arbitrary choices.
   */
  spacing: [8, 16, 32, 64, 128],

  /**
   * Type system. A small ratio (1.2, the minor third) keeps size differences
   * subtle. The scale has fewer steps than other profiles.
   * displayFamilies and textFamilies both use restrained system sans-serif
   * faces to keep the visual tone neutral.
   */
  type: {
    baseSize: 16,
    ratio: 1.2,
    scale: [13, 16, 19, 23, 28],
    displayFamilies: ["Inter", "Helvetica Neue", "Arial"],
    textFamilies: ["Inter", "Helvetica Neue", "Arial"],
  },

  /**
   * Grid system. Four columns encourage single-column content with optional
   * two-column splits. A large margin keeps content narrow and readable.
   */
  grid: {
    columns: 4,
    gutter: 24,
    margin: 64,
  },

  /**
   * WCAG AA contrast minimums. bodyMin applies to text below 18pt. largeMin
   * applies to large text (18pt+) and UI components.
   */
  contrast: {
    bodyMin: 4.5,
    largeMin: 3,
  },

  /**
   * Visual hierarchy rules. Apply them in order when building minimal layouts.
   */
  hierarchy: [
    "Use size alone, not weight or color, to signal the primary heading.",
    "Allow only two heading levels on any single screen.",
    "Separate sections with whitespace, not dividers.",
    "Align all content to a single left axis.",
  ],

  /**
   * Restraint rules. Each rule names one thing to remove or avoid.
   */
  restraint: [
    "Use one typeface and vary only weight and size.",
    "Use at most one accent color and apply it to one element per view.",
    "Remove every background color that is not white or near-white.",
    "Remove every icon that the label already communicates.",
    "Do not add animation to static content.",
    "Prefer prose over bullet lists when items have logical order.",
    "Never use more than two weights of the same typeface in one block.",
    "Remove borders when spacing already separates elements.",
  ],

  /**
   * Blocklist of anti-slop rule ids. The design worker rejects patterns
   * identified by these ids. The first seven are the core floor and must
   * always be present. The remaining ids reinforce minimal restraint.
   */
  blocklist: [
    "centered-everything",
    "generic-gradient",
    "emoji-bullets",
    "dead-whitespace",
    "gray-1px-borders-everywhere",
    "pure-black",
    "three-card-row",
    "rainbow-palette",
    "decorative-divider-overuse",
    "oversized-hero-text",
    "misaligned-grid",
    "all-caps-body-copy",
    "font-size-below-12",
    "drop-shadow-stack",
    "icon-label-mismatch",
    "multi-column-body-text",
  ],
};
