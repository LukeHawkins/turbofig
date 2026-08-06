/**
 * Turbofig `editorial` taste profile.
 *
 * Art-directed style: asymmetric layout, expressive type, dense information
 * hierarchy. Use this profile for editorial pages, feature stories, and
 * content-heavy layouts where visual rhythm matters.
 *
 * This file is a plain script, not an ES module. The daemon prepends it to
 * user eval code so that `taste` is in scope. Do not add export, import, or
 * require statements.
 */

const taste = {
  id: "editorial",

  label: "Editorial",

  /**
   * Colour palette. Six hex tokens: one expressive editorial brand hue (deep
   * crimson/burgundy, classic print), one warm terracotta accent, two warm
   * paper surfaces, and two warm text values. text is not pure black (#000000)
   * per the pure-black blocklist entry.
   */
  palette: {
    brand: "#8B2330",
    accent: "#C4622D",
    surface: "#FAF8F3",
    surfaceAlt: "#F0EBE3",
    text: "#1A1410",
    textMuted: "#6B5A4E",
  },

  /**
   * Spacing scale in px (ascending). Denser than impeccable to support
   * high-information layouts. Use these values for padding, gap, and margin.
   */
  spacing: [2, 4, 8, 12, 16, 20, 28, 40, 56, 80],

  /**
   * Type system. A larger ratio (1.414, the augmented fourth) gives more
   * expressive size contrast between levels. The scale is derived from the
   * ratio at the top end and adjusted for legibility at small sizes.
   * displayFamilies uses expressive Figma-available serif display faces.
   * textFamilies uses a readable grotesque for body copy.
   */
  type: {
    baseSize: 16,
    ratio: 1.414,
    scale: [11, 14, 16, 22, 32, 45, 64, 90],
    displayFamilies: ["Playfair Display", "DM Serif Display", "Georgia"],
    textFamilies: ["Libre Franklin", "IBM Plex Sans", "Arial"],
  },

  /**
   * Grid system. More columns (16) give fine-grained control for asymmetric
   * layouts. A wider margin creates a strong outer frame.
   */
  grid: {
    columns: 16,
    gutter: 12,
    margin: 48,
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
   * Visual hierarchy rules. Apply them in order when building editorial
   * layouts.
   */
  hierarchy: [
    "Place the dominant headline at no less than triple the body size.",
    "Use one expressive display typeface and one neutral body typeface.",
    "Pull one quote or stat out of the flow at full-column width.",
    "Break the grid at most once per page to create tension.",
    "Lead with the image or the headline, never both at equal weight.",
    "Use column width as a hierarchy signal: wider means more important.",
  ],

  /**
   * Restraint rules. Each rule names one thing to remove or avoid.
   */
  restraint: [
    "Use at most two typeface families: one display, one text.",
    "Allow asymmetry only when it serves a clear reading path.",
    "Do not use more than four type sizes on a single spread.",
    "Reserve the accent color for one focal element per page.",
    "Remove any image treatment that the image does not need.",
    "Keep captions in one consistent size and weight throughout.",
    "Do not stack more than three text elements without a visual break.",
  ],

  /**
   * Blocklist of anti-slop rule ids. The design worker rejects patterns
   * identified by these ids. The first seven are the core floor and must
   * always be present. The remaining ids are editorial-specific.
   */
  blocklist: [
    "centered-everything",
    "generic-gradient",
    "emoji-bullets",
    "dead-whitespace",
    "gray-1px-borders-everywhere",
    "pure-black",
    "three-card-row",
    "stock-photo-hero",
    "all-caps-body-copy",
    "misaligned-grid",
    "uniform-column-widths",
    "decorative-divider-overuse",
    "oversized-hero-text",
    "rainbow-palette",
  ],
};
