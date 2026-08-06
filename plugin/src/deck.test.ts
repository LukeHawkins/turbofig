/**
 * Integration test: build a multi-slide deck using only tf helpers.
 * Proves that deck, slide, text, and append compose into a complete deck.
 * No live Figma API is required.
 */

import { describe, expect, test } from "bun:test";
import { createTf } from "./helpers";

// --- Minimal in-memory mock PluginAPI ---

/** Creates a fake FrameNode with mutable props and an in-memory children list. */
function makeFakeFrame(): FrameNode {
  const node: Record<string, unknown> = {
    name: "",
    x: 0,
    y: 0,
    width: 100,
    height: 100,
    layoutMode: "NONE",
    itemSpacing: 0,
    paddingTop: 0,
    paddingRight: 0,
    paddingBottom: 0,
    paddingLeft: 0,
    fills: [],
    primaryAxisSizingMode: "AUTO",
    counterAxisSizingMode: "AUTO",
    primaryAxisAlignItems: "MIN",
    counterAxisAlignItems: "MIN",
    children: [],
    resize(w: number, h: number) {
      node.width = w;
      node.height = h;
    },
    appendChild(child: unknown) {
      (node.children as unknown[]).push(child);
      (child as Record<string, unknown>).parent = node;
    },
  };
  return node as unknown as FrameNode;
}

/** Creates a fake TextNode with mutable props. Text nodes have no children. */
function makeFakeText(): TextNode {
  const node: Record<string, unknown> = {
    name: "",
    characters: "",
    fontName: { family: "Inter", style: "Regular" },
    fontSize: 16,
    fills: [],
    textAutoResize: "WIDTH_AND_HEIGHT",
    width: 100,
    height: 100,
    parent: null,
    resize(w: number, h: number) {
      node.width = w;
      node.height = h;
    },
  };
  return node as unknown as TextNode;
}

/** Builds a mock PluginAPI that tracks font loads and creates fake nodes. */
function buildMockFigma(): { mockFigma: PluginAPI; loadedFonts: FontName[] } {
  const loadedFonts: FontName[] = [];
  const mockFigma = {
    createFrame: makeFakeFrame,
    createText: makeFakeText,
    async loadFontAsync(font: FontName): Promise<void> {
      loadedFonts.push(font);
    },
  } as unknown as PluginAPI;
  return { mockFigma, loadedFonts };
}

/** Creates a fake page/root node that accepts children. */
function makeFakePage(): BaseNode & ChildrenMixin {
  const page: Record<string, unknown> = {
    children: [],
    appendChild(child: unknown) {
      (page.children as unknown[]).push(child);
      (child as Record<string, unknown>).parent = page;
    },
  };
  return page as unknown as BaseNode & ChildrenMixin;
}

// --- Tests ---

describe("deck integration", () => {
  test("builds a 5-slide deck using only tf helpers", async () => {
    const { mockFigma, loadedFonts } = buildMockFigma();
    const tf = createTf(mockFigma);
    const page = makeFakePage();

    // Preload both fonts once before building the deck.
    await tf.loadFonts([
      { family: "Inter", style: "Bold" },
      { family: "Inter", style: "Regular" },
    ]);

    // The preload must have loaded exactly 2 font variants.
    expect(loadedFonts).toHaveLength(2);

    await tf.deck({
      parent: page,
      count: 5,
      cols: 1,
      gap: 80,
      build: async (slide, i) => {
        // Create a title and a body text node for each slide.
        const title = await tf.text({
          text: `Title ${i}`,
          family: "Inter",
          style: "Bold",
          size: 48,
        });
        const body = await tf.text({
          text: `Body ${i}`,
          family: "Inter",
          style: "Regular",
          size: 24,
        });
        // Append both children to the slide.
        tf.append(slide, title, body);
      },
    });

    // Cast children to a typed array for assertions.
    const slides = (page as unknown as Record<string, unknown>).children as Record<
      string,
      unknown
    >[];

    // Exactly 5 slides must appear on the page.
    expect(slides).toHaveLength(5);

    // Each slide is 1920x1080 and positioned in a single column with an 80px gap.
    const expectedY = [0, 1160, 2320, 3480, 4640];
    for (let i = 0; i < 5; i++) {
      const slide = slides[i];
      expect(slide.width).toBe(1920);
      expect(slide.height).toBe(1080);
      expect(slide.name).toBe("Slide");
      expect(slide.x).toBe(0);
      expect(slide.y).toBe(expectedY[i]);

      // Each slide must contain exactly 2 text children.
      const slideChildren = slide.children as Record<string, unknown>[];
      expect(slideChildren).toHaveLength(2);

      const titleNode = slideChildren[0];
      const bodyNode = slideChildren[1];
      expect(titleNode.characters).toBe(`Title ${i}`);
      expect(bodyNode.characters).toBe(`Body ${i}`);
    }

    // The loaded font set must contain only Inter Bold and Inter Regular.
    // tf.text calls loadFontAsync per node, so there will be more entries than 2,
    // but the unique family+style pairs must not exceed the two preloaded variants.
    const uniqueFontKeys = new Set(loadedFonts.map((f) => `${f.family}\x00${f.style}`));
    expect(uniqueFontKeys.size).toBe(2);
    expect(uniqueFontKeys.has("Inter\x00Bold")).toBe(true);
    expect(uniqueFontKeys.has("Inter\x00Regular")).toBe(true);
  });

  test("arranges slides in a grid when cols > 1", async () => {
    const { mockFigma } = buildMockFigma();
    const tf = createTf(mockFigma);
    const page = makeFakePage();

    // Build a 4-slide deck in a 2-column grid with a 40px gap.
    await tf.deck({ parent: page, count: 4, cols: 2, gap: 40 });

    const slides = (page as unknown as Record<string, unknown>).children as Record<
      string,
      unknown
    >[];
    expect(slides).toHaveLength(4);

    // Slide positions from slidePosition with cols=2, gap=40, 1920x1080 slides.
    // slide 0: col=0, row=0 -> x=0, y=0
    // slide 1: col=1, row=0 -> x=1*(1920+40)=1960, y=0
    // slide 2: col=0, row=1 -> x=0, y=1*(1080+40)=1120
    // slide 3: col=1, row=1 -> x=1960, y=1120
    expect({ x: slides[0].x, y: slides[0].y }).toEqual({ x: 0, y: 0 });
    expect({ x: slides[1].x, y: slides[1].y }).toEqual({ x: 1960, y: 0 });
    expect({ x: slides[2].x, y: slides[2].y }).toEqual({ x: 0, y: 1120 });
    expect({ x: slides[3].x, y: slides[3].y }).toEqual({ x: 1960, y: 1120 });
  });
});
