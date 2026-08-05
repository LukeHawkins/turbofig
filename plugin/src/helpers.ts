/**
 * Compact craft helper library for the Turbofig eval context.
 *
 * Pure functions are exported standalone for unit testing.
 * Figma-touching functions live inside createTf, bound to a live PluginAPI.
 * Do not import this file into code.ts directly. A later step injects it.
 */

/** Padding sides as explicit numbers. Missing sides default to 0. */
export type PaddingObject = {
  top?: number;
  right?: number;
  bottom?: number;
  left?: number;
};

/** Options for the text factory. */
export interface TextOpts {
  text: string;
  size?: number;
  family?: string;
  style?: string;
  color?: string;
}

/** Options for the frame factory. */
export interface FrameOpts {
  name?: string;
  direction?: "NONE" | "HORIZONTAL" | "VERTICAL";
  gap?: number;
  padding?: number | PaddingObject;
  fill?: string;
  width?: number;
  height?: number;
  primaryAlign?: "MIN" | "CENTER" | "MAX" | "SPACE_BETWEEN";
  counterAlign?: "MIN" | "CENTER" | "MAX";
}

/** Options for the rect factory. */
export interface RectOpts {
  width: number;
  height: number;
  fill?: string;
  corner?: number;
}

// --- Pure functions ---

/**
 * Converts a hex colour string to an RGB triple with components in the range 0..1.
 * Accepts #RRGGBB, RRGGBB, and short #RGB. Returns black on invalid input.
 */
export function hexToRgb(hex: string): { r: number; g: number; b: number } {
  // Strip a leading hash if present.
  const raw = hex.startsWith("#") ? hex.slice(1) : hex;
  // Expand short RGB to RRGGBB.
  const full =
    raw.length === 3
      ? raw
          .split("")
          .map((c) => c + c)
          .join("")
      : raw;
  if (!/^[0-9a-fA-F]{6}$/.test(full)) return { r: 0, g: 0, b: 0 };
  const n = parseInt(full, 16);
  return {
    r: ((n >> 16) & 0xff) / 255,
    g: ((n >> 8) & 0xff) / 255,
    b: (n & 0xff) / 255,
  };
}

/** Builds a SolidPaint from a hex colour string and an optional opacity value. */
export function solidPaint(hex: string, opacity = 1): SolidPaint {
  return { type: "SOLID", color: hexToRgb(hex), opacity };
}

/**
 * Normalises a padding value to a four-side object.
 * A single number applies to all four sides. Missing sides default to 0.
 */
export function normalizePadding(p: number | PaddingObject): {
  top: number;
  right: number;
  bottom: number;
  left: number;
} {
  if (typeof p === "number") {
    return { top: p, right: p, bottom: p, left: p };
  }
  return {
    top: p.top ?? 0,
    right: p.right ?? 0,
    bottom: p.bottom ?? 0,
    left: p.left ?? 0,
  };
}

/**
 * Returns a unique list of fonts, preserving the order of first occurrence.
 * Two fonts are equal when family and style both match.
 */
export function dedupeFonts(fonts: FontName[]): FontName[] {
  const seen = new Set<string>();
  return fonts.filter((f) => {
    const key = `${f.family}\x00${f.style}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

/**
 * Maps an auto-layout direction and the presence of explicit dimensions to
 * per-axis sizing modes. Use the result to set primaryAxisSizingMode and
 * counterAxisSizingMode on a FrameNode.
 *
 * For direction "NONE" the frame has no auto-layout, so sizing modes are
 * invalid. Both fields return "AUTO" as a safe sentinel; the caller must not
 * apply them to the node.
 *
 * Axis mapping:
 *   VERTICAL  — primary axis is height, counter axis is width.
 *   HORIZONTAL — primary axis is width, counter axis is height.
 */
export function axisSizing(
  direction: "NONE" | "HORIZONTAL" | "VERTICAL",
  hasWidth: boolean,
  hasHeight: boolean,
): { primary: "FIXED" | "AUTO"; counter: "FIXED" | "AUTO" } {
  if (direction === "VERTICAL") {
    return {
      primary: hasHeight ? "FIXED" : "AUTO",
      counter: hasWidth ? "FIXED" : "AUTO",
    };
  }
  if (direction === "HORIZONTAL") {
    return {
      primary: hasWidth ? "FIXED" : "AUTO",
      counter: hasHeight ? "FIXED" : "AUTO",
    };
  }
  // direction "NONE": auto-layout sizing modes do not apply.
  return { primary: "AUTO", counter: "AUTO" };
}

// --- Factory ---

/** Creates and returns the tf namespace bound to a live PluginAPI instance. */
export function createTf(figma: PluginAPI) {
  return {
    /** Parses a hex colour string to an RGB triple with 0..1 components. */
    color: hexToRgb,

    /** Builds a SolidPaint from a hex string and an optional opacity. */
    solid: solidPaint,

    /** Deduplicates fonts then loads them all in parallel. */
    async loadFonts(fonts: FontName[]): Promise<void> {
      await Promise.all(dedupeFonts(fonts).map((f) => figma.loadFontAsync(f)));
    },

    /** Creates and configures a TextNode. Loads the font before creating the node. */
    async text(opts: TextOpts): Promise<TextNode> {
      const size = opts.size ?? 16;
      const family = opts.family ?? "Inter";
      const style = opts.style ?? "Regular";
      const color = opts.color ?? "#000000";
      await figma.loadFontAsync({ family, style });
      const node = figma.createText();
      node.fontName = { family, style };
      node.characters = opts.text;
      node.fontSize = size;
      // SolidPaint[] is assignable to ReadonlyArray<Paint>, the non-mixed branch.
      node.fills = [solidPaint(color)];
      return node;
    },

    /** Creates and configures a FrameNode with auto-layout options. */
    frame(opts: FrameOpts): FrameNode {
      const node = figma.createFrame();
      const direction = opts.direction ?? "VERTICAL";
      node.layoutMode = direction;
      if (opts.gap !== undefined) {
        node.itemSpacing = opts.gap;
      }
      if (opts.padding !== undefined) {
        const pad = normalizePadding(opts.padding);
        node.paddingTop = pad.top;
        node.paddingRight = pad.right;
        node.paddingBottom = pad.bottom;
        node.paddingLeft = pad.left;
      }
      // Always set fills explicitly. An empty array makes the frame transparent.
      // SolidPaint[] is assignable to ReadonlyArray<Paint>, the non-mixed branch.
      node.fills = opts.fill !== undefined ? [solidPaint(opts.fill)] : [];
      const hasWidth = opts.width !== undefined;
      const hasHeight = opts.height !== undefined;
      // Resize whenever at least one dimension is given.
      if (hasWidth || hasHeight) {
        node.resize(opts.width ?? node.width, opts.height ?? node.height);
      }
      // Sizing modes are only valid on auto-layout frames.
      if (direction !== "NONE") {
        const sizing = axisSizing(direction, hasWidth, hasHeight);
        node.primaryAxisSizingMode = sizing.primary;
        node.counterAxisSizingMode = sizing.counter;
      }
      if (opts.primaryAlign !== undefined) {
        node.primaryAxisAlignItems = opts.primaryAlign;
      }
      if (opts.counterAlign !== undefined) {
        node.counterAxisAlignItems = opts.counterAlign;
      }
      if (opts.name !== undefined) {
        node.name = opts.name;
      }
      return node;
    },

    /** Creates and configures a RectangleNode with optional fill and corner radius. */
    rect(opts: RectOpts): RectangleNode {
      const node = figma.createRectangle();
      node.resize(opts.width, opts.height);
      if (opts.fill !== undefined) {
        // SolidPaint[] is assignable to ReadonlyArray<Paint>, the non-mixed branch.
        node.fills = [solidPaint(opts.fill)];
      }
      if (opts.corner !== undefined) {
        node.cornerRadius = opts.corner;
      }
      return node;
    },

    /** Appends each child to the parent node and returns the parent for chaining. */
    append<T extends BaseNode & ChildrenMixin>(parent: T, ...children: SceneNode[]): T {
      for (const child of children) {
        parent.appendChild(child);
      }
      return parent;
    },

    /**
     * Returns an existing direct child named `name`, or calls the factory to create one.
     * Sets the node name, appends it to the parent, and returns it.
     */
    async findOrCreate(
      parent: BaseNode & ChildrenMixin,
      name: string,
      factory: () => SceneNode | Promise<SceneNode>,
    ): Promise<SceneNode> {
      const existing = parent.children.find((c) => c.name === name);
      if (existing) return existing;
      const node = await factory();
      node.name = name;
      parent.appendChild(node);
      return node;
    },

    /**
     * Calls figma.commitUndo() when the method exists on this PluginAPI instance.
     * The label parameter is reserved for future use.
     */
    commit(label?: string): void {
      // label is reserved. No action until commitUndo accepts a label upstream.
      void label;
      // Guard: commitUndo may not exist in all plugin API versions.
      if (typeof figma.commitUndo === "function") {
        figma.commitUndo();
      }
    },
  };
}
