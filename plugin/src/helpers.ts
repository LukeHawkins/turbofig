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
  /**
   * Controls how the node resizes when its text content changes.
   * Default is "WIDTH_AND_HEIGHT" (the node grows on both axes).
   * Pass "HEIGHT" together with `width` to wrap text at a fixed width.
   */
  autoResize?: "WIDTH_AND_HEIGHT" | "HEIGHT" | "NONE" | "TRUNCATE";
  /**
   * When set, the node is resized to this pixel width before text is applied.
   * `autoResize` defaults to "HEIGHT" so the node wraps within the fixed width.
   */
  width?: number;
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

/**
 * Options for the slide factory.
 * All fields are the same as FrameOpts. Defaults are: width=1920, height=1080, name="Slide".
 */
export interface SlideOpts extends FrameOpts {}

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
 * Returns the top-left position of a slide at `index` in a grid layout.
 * Row = Math.floor(index / cols), column = index % cols.
 * x = col * (width + gap), y = row * (height + gap).
 * Throws a RangeError when cols is less than 1 or index is negative.
 */
export function slidePosition(
  index: number,
  opts: { cols: number; width: number; height: number; gap: number },
): { x: number; y: number } {
  if (opts.cols < 1) throw new RangeError("slidePosition: cols must be at least 1");
  if (index < 0) throw new RangeError("slidePosition: index must be non-negative");
  const row = Math.floor(index / opts.cols);
  const col = index % opts.cols;
  return {
    x: col * (opts.width + opts.gap),
    y: row * (opts.height + opts.gap),
  };
}

/**
 * Splits an array into consecutive sub-arrays of at most `size` elements.
 * Default size is 75, the midpoint of the 50-100 node batching rule.
 * Throws a RangeError when size is less than 1.
 * Returns an empty array for an empty input.
 */
export function chunk<T>(items: T[], size = 75): T[][] {
  if (size < 1) throw new RangeError("chunk: size must be at least 1");
  if (items.length === 0) return [];
  const result: T[][] = [];
  for (let i = 0; i < items.length; i += size) {
    result.push(items.slice(i, i + size));
  }
  return result;
}

/**
 * Maps an auto-layout direction and the presence of explicit dimensions to
 * per-axis sizing modes. Use the result to set primaryAxisSizingMode and
 * counterAxisSizingMode on a FrameNode.
 *
 * Returns null for direction "NONE". The frame has no auto-layout, so sizing
 * modes are invalid. The caller must not apply them to the node.
 *
 * Axis mapping:
 *   VERTICAL: primary axis is height, counter axis is width.
 *   HORIZONTAL: primary axis is width, counter axis is height.
 */
export function axisSizing(
  direction: "NONE" | "HORIZONTAL" | "VERTICAL",
  hasWidth: boolean,
  hasHeight: boolean,
): { primary: "FIXED" | "AUTO"; counter: "FIXED" | "AUTO" } | null {
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
  return null;
}

// --- Factory ---

/** Creates and returns the tf namespace bound to a live PluginAPI instance. */
export function createTf(figma: PluginAPI) {
  // Assign to a variable so that slide and deck can call tf.frame and tf.slide.
  const tf = {
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
      node.fontSize = size;
      // SolidPaint[] is assignable to ReadonlyArray<Paint>, the non-mixed branch.
      node.fills = [solidPaint(color)];
      if (opts.width !== undefined) {
        // Fixed-width wrapping: resize to the given width, then wrap within it.
        // Set textAutoResize before characters so wrapping applies immediately.
        node.resize(opts.width, node.height);
        node.textAutoResize = opts.autoResize ?? "HEIGHT";
      } else {
        node.textAutoResize = opts.autoResize ?? "WIDTH_AND_HEIGHT";
      }
      node.characters = opts.text;
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
      // Sizing modes are only valid on auto-layout frames. axisSizing returns null for "NONE".
      if (direction !== "NONE") {
        const sizing = axisSizing(direction, hasWidth, hasHeight);
        if (sizing !== null) {
          node.primaryAxisSizingMode = sizing.primary;
          node.counterAxisSizingMode = sizing.counter;
        }
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

    /** Creates and configures a RectangleNode with optional fill and corner radius. Transparent by default. */
    rect(opts: RectOpts): RectangleNode {
      const node = figma.createRectangle();
      node.resize(opts.width, opts.height);
      // Always set fills explicitly. An empty array makes the rect transparent.
      // SolidPaint[] is assignable to ReadonlyArray<Paint>, the non-mixed branch.
      node.fills = opts.fill !== undefined ? [solidPaint(opts.fill)] : [];
      if (opts.corner !== undefined) {
        node.cornerRadius = opts.corner;
      }
      return node;
    },

    /**
     * Removes all children from a node.
     * Use with findOrCreate to make a re-run idempotent: find or create the section,
     * clear it, then rebuild its children from scratch.
     */
    clear(node: BaseNode & ChildrenMixin): void {
      for (const c of [...node.children]) {
        c.remove();
      }
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

    /**
     * Sets figma.skipInvisibleInstanceChildren to `on` (default true).
     * When true, Figma skips hidden instance children during traversal,
     * which speeds up findAll calls on large documents.
     * No-op when the property is absent from this PluginAPI instance.
     */
    skipInvisible(on = true): void {
      // Guard: the property may not exist in all API versions or test mocks.
      if ("skipInvisibleInstanceChildren" in figma) {
        (figma as unknown as Record<string, unknown>).skipInvisibleInstanceChildren = on;
      }
    },

    /**
     * Wraps node.findAllWithCriteria(criteria).
     * Use this for fast, native type-based node queries instead of a predicate scan.
     * Example: tf.findAll(page, { types: ["TEXT"] }) returns TextNode[].
     * The node must be on the current page or a page already loaded with
     * figma.loadPageAsync(). An unloaded page throws under dynamic-page.
     */
    findAll<T extends NodeType[]>(
      node: BaseNode & ChildrenMixin,
      criteria: FindAllCriteria<T>,
    ): { type: T[number] }[] {
      return node.findAllWithCriteria(criteria);
    },

    /**
     * Splits an array into consecutive sub-arrays of at most `size` elements.
     * Default size is 75, the midpoint of the 50-100 node batching rule.
     * Throws a RangeError when size is less than 1.
     */
    chunk,

    /**
     * Returns the top-left grid position of a slide at `index`.
     * Use this to compute positions without creating nodes.
     * Throws a RangeError when cols is less than 1 or index is negative.
     */
    slidePosition,

    /**
     * Creates a slide frame. Default size is 1920x1080 with name "Slide".
     * Accepts all FrameOpts fields. Provide width/height to override the defaults.
     */
    slide(opts: SlideOpts = {}): FrameNode {
      // Default to a plain frame so slide children keep free x/y positions.
      // Auto-layout would override child positions. The caller can still set direction.
      return tf.frame({ name: "Slide", width: 1920, height: 1080, direction: "NONE", ...opts });
    },

    /**
     * Creates `count` slide frames, positions each in a grid, and appends each to `parent`.
     * Default grid is one column (cols=1) with an 80px gap between slides.
     * Calls `build(slide, index)` for each slide when provided. Returns the slides in order.
     * The parent must not have auto-layout. Auto-layout overrides the grid x/y positions.
     */
    async deck(opts: {
      parent: BaseNode & ChildrenMixin;
      count: number;
      cols?: number;
      gap?: number;
      build?: (slide: FrameNode, index: number) => void | Promise<void>;
    }): Promise<FrameNode[]> {
      // Guard: an auto-layout parent silently overrides the grid positions.
      if ("layoutMode" in opts.parent && (opts.parent as FrameNode).layoutMode !== "NONE") {
        throw new Error("deck: parent must not use auto-layout (set layoutMode to NONE)");
      }
      const cols = opts.cols ?? 1;
      const gap = opts.gap ?? 80;
      const slideWidth = 1920;
      const slideHeight = 1080;
      const slides: FrameNode[] = [];
      for (let i = 0; i < opts.count; i++) {
        const pos = slidePosition(i, { cols, width: slideWidth, height: slideHeight, gap });
        const s = tf.slide({ width: slideWidth, height: slideHeight });
        s.x = pos.x;
        s.y = pos.y;
        opts.parent.appendChild(s);
        if (opts.build) {
          await opts.build(s, i);
        }
        slides.push(s);
      }
      return slides;
    },

    /** Returns a new instance of the given component. */
    instance(component: ComponentNode): InstanceNode {
      return component.createInstance();
    },

    /** Imports a component by its key and returns a new instance. */
    async instanceByKey(key: string): Promise<InstanceNode> {
      const c = await figma.importComponentByKeyAsync(key);
      return c.createInstance();
    },

    /** Returns the Variable with the given id, or null when not found. */
    async getVariable(id: string): Promise<Variable | null> {
      return figma.variables.getVariableByIdAsync(id);
    },

    /** Sets a variable's value for the given mode id. */
    setVariableValue(variable: Variable, modeId: string, value: VariableValue): void {
      variable.setValueForMode(modeId, value);
    },

    /**
     * Reads a variable's value for the given mode id.
     * Returns undefined when the mode does not exist on the variable.
     */
    readVariableValue(variable: Variable, modeId: string): VariableValue | undefined {
      return variable.valuesByMode[modeId];
    },

    /**
     * Exports a node as an image. Defaults to PNG at 1x scale.
     * Pass a custom ExportSettings object to control format, constraints, and other options.
     */
    async export(node: SceneNode & ExportMixin, settings?: ExportSettings): Promise<Uint8Array> {
      return node.exportAsync(
        settings ?? { format: "PNG", constraint: { type: "SCALE", value: 1 } },
      );
    },
  };
  return tf;
}
