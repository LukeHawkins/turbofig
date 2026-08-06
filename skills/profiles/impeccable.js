/**
 * Turbofig `impeccable` taste profile.
 *
 * Anti-slop rules adapted from pbakaus/impeccable (Apache-2.0).
 * https://github.com/pbakaus/impeccable
 *
 * This file is a plain script, not an ES module. The daemon prepends it to
 * user eval code so that `taste` is in scope. Do not add export, import, or
 * require statements.
 */

const taste = {
  id: "impeccable",

  label: "Impeccable",

  /**
   * Spacing scale in px (ascending). Use these values for padding, gap, and
   * margin. Do not invent values outside this scale.
   */
  spacing: [4, 8, 12, 16, 24, 32, 48, 64, 96],

  /**
   * Type system. baseSize is the body copy size in px. ratio is the Major
   * Third (1.25) modular scale factor. scale lists the computed sizes from
   * smallest to largest. families are the preferred font family names in
   * priority order.
   */
  type: {
    baseSize: 16,
    ratio: 1.25,
    scale: [12, 14, 16, 20, 25, 31, 39, 49, 61],
    families: ["Inter", "Helvetica Neue", "Arial"],
  },

  /**
   * Grid system. columns is the number of layout columns. gutter is the
   * space between columns in px. margin is the outer edge margin in px.
   */
  grid: {
    columns: 12,
    gutter: 16,
    margin: 32,
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
   * Visual hierarchy rules. Apply them in order when building layouts.
   */
  hierarchy: [
    "Set one element as the clear focal point per screen.",
    "Use size before weight to create emphasis.",
    "Limit heading levels to three on any single screen.",
    "Align text to one axis per section.",
    "Group related elements closer than unrelated elements.",
  ],

  /**
   * Restraint rules. Each rule names one thing to remove or avoid.
   */
  restraint: [
    "Remove every decoration that carries no information.",
    "Use at most two typefaces per design.",
    "Limit the palette to one brand color plus neutrals per view.",
    "Do not add a border if whitespace already separates the element.",
    "Prefer one strong visual idea per screen over three weak ones.",
    "Remove drop shadows from flat-color elements.",
  ],

  /**
   * Blocklist of anti-slop rule ids. The design worker rejects patterns
   * identified by these ids. The first seven are the core floor and must
   * always be present.
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
    "icon-label-mismatch",
    "font-size-below-12",
    "all-caps-body-copy",
    "oversized-hero-text",
    "misaligned-grid",
  ],
};
