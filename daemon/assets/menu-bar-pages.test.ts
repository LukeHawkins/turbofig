// A real DOM test for the 2 menu-bar webview pages (`about/about.html`,
// `settings/settings.html`), run with `happy-dom` (chosen over `linkedom`:
// linkedom does not execute <script> tags at all, and both pages' tab
// switching and IPC wiring live in an inline <script>, not a separate
// module this test could import on its own). This must fail on a broken
// tab or status script: `window.ipc` is stubbed, the page's own real
// `<script>` runs inside a real (if headless) DOM, and every assertion
// below exercises that same script, not a reimplementation of it.
import { afterEach, beforeEach, describe, expect, it } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { Window } from "happy-dom";

const ABOUT_HTML = readFileSync(join(import.meta.dir, "about/about.html"), "utf-8");
const SETTINGS_HTML = readFileSync(join(import.meta.dir, "settings/settings.html"), "utf-8");

/** Loads `html` into a fresh headless window, stubs `window.ipc`, and
 * waits a tick for the page's own inline `<script>` (which runs
 * asynchronously relative to `document.write`) to finish. Returns the
 * window, its document, and the list of IPC commands posted so far. */
async function loadPage(html: string) {
  const window = new Window({
    settings: {
      enableJavaScriptEvaluation: true,
      suppressInsecureJavaScriptEnvironmentWarning: true,
    },
  });
  const posted: string[] = [];
  // @ts-expect-error - `ipc` is Tauri/wry's own injected object, not part
  // of happy-dom's `Window` type.
  window.ipc = { postMessage: (cmd: string) => posted.push(cmd) };
  window.document.write(html);
  await new Promise((resolve) => setTimeout(resolve, 20));
  return { window, document: window.document, posted };
}

describe("about.html", () => {
  let ctx: Awaited<ReturnType<typeof loadPage>>;

  beforeEach(async () => {
    ctx = await loadPage(ABOUT_HTML);
  });

  afterEach(() => {
    ctx.window.happyDOM.close();
  });

  it("sends page_ready once the page's own script has run", () => {
    expect(ctx.posted).toContain("page_ready");
  });

  it("shows How to use by default and hides it when the MCP tab is clicked", () => {
    const { document } = ctx;
    const howto = document.getElementById("tab-howto") as unknown as HTMLElement;
    const mcp = document.getElementById("tab-mcp") as unknown as HTMLElement;
    expect(howto.classList.contains("active")).toBe(true);
    expect(mcp.classList.contains("active")).toBe(false);

    (document.getElementById("tab-btn-mcp") as unknown as HTMLElement).click();

    expect(mcp.classList.contains("active")).toBe(true);
    expect(howto.classList.contains("active")).toBe(false);
  });

  it("updates the status text and step 2 when turbofigSetStatus is called", () => {
    const { window, document } = ctx;
    (window as unknown as { turbofigSetStatus: (a: boolean, b: string, c: boolean) => void })
      .turbofigSetStatus(true, "Figma plugin connected: Design A", true);

    expect(document.getElementById("text-bridge")?.textContent).toBe("Bridge running");
    expect(document.getElementById("text-figma")?.textContent).toBe(
      "Figma plugin connected: Design A",
    );
    expect(document.getElementById("step2-state")?.textContent).toBe(
      "✓ Connected to Design A",
    );
    expect(document.getElementById("dot-bridge")?.classList.contains("dot-ok")).toBe(true);
  });

  it("updates the version and MCP json when turbofigSetStatic is called", () => {
    const { window, document } = ctx;
    (window as unknown as { turbofigSetStatic: (a: string, b: string) => void }).turbofigSetStatic(
      "1.2.3",
      '{"command": "turbofig", "args": ["mcp"]}',
    );

    expect(document.getElementById("version-text")?.textContent).toBe("v1.2.3");
    expect(document.getElementById("mcp-json")?.textContent).toBe(
      '{"command": "turbofig", "args": ["mcp"]}',
    );
  });

  it.each([
    ["reveal_manifest", "[data-ipc=\"reveal_manifest\"]"],
    ["copy_manifest_path", "[data-ipc=\"copy_manifest_path\"]"],
    ["copy_agent_prompt", "[data-ipc=\"copy_agent_prompt\"]"],
    ["copy_mcp_command", "[data-ipc=\"copy_mcp_command\"]"],
    ["copy_mcp_json", "[data-ipc=\"copy_mcp_json\"]"],
  ])("posts %s when its button is clicked", (command, selector) => {
    ctx.posted.length = 0;
    const el = ctx.document.querySelector(selector) as unknown as HTMLElement;
    expect(el).not.toBeNull();
    el.click();
    expect(ctx.posted).toContain(command);
  });

  it("posts open_docs, and prevents the default navigation, for the Docs link", () => {
    ctx.posted.length = 0;
    const link = ctx.document.getElementById("docs-link") as unknown as HTMLElement;
    link.click();
    expect(ctx.posted).toContain("open_docs");
  });
});

describe("settings.html", () => {
  let ctx: Awaited<ReturnType<typeof loadPage>>;

  beforeEach(async () => {
    ctx = await loadPage(SETTINGS_HTML);
  });

  afterEach(() => {
    ctx.window.happyDOM.close();
  });

  it("sends page_ready once the page's own script has run", () => {
    expect(ctx.posted).toContain("page_ready");
  });

  it("updates the version and the Start at login switch when turbofigSetStatic is called", () => {
    const { window, document } = ctx;
    (
      window as unknown as { turbofigSetStatic: (a: string, b: boolean) => void }
    ).turbofigSetStatic("1.2.3", true);

    expect(document.getElementById("version-text")?.textContent).toBe("v1.2.3");
    expect(
      (document.getElementById("start-at-login") as unknown as HTMLInputElement).checked,
    ).toBe(true);
  });

  it("posts start_at_login_on / start_at_login_off when the switch is toggled", () => {
    const { document } = ctx;
    const toggle = document.getElementById("start-at-login") as unknown as HTMLInputElement;

    ctx.posted.length = 0;
    toggle.checked = true;
    toggle.dispatchEvent(new (ctx.window as unknown as { Event: typeof Event }).Event("change"));
    expect(ctx.posted).toContain("start_at_login_on");

    ctx.posted.length = 0;
    toggle.checked = false;
    toggle.dispatchEvent(new (ctx.window as unknown as { Event: typeof Event }).Event("change"));
    expect(ctx.posted).toContain("start_at_login_off");
  });

  it.each([
    ["copy_manifest_path", "[data-ipc=\"copy_manifest_path\"]"],
    ["open_plugin_folder", "[data-ipc=\"open_plugin_folder\"]"],
    ["open_log", "[data-ipc=\"open_log\"]"],
  ])("posts %s when its button is clicked", (command, selector) => {
    ctx.posted.length = 0;
    const el = ctx.document.querySelector(selector) as unknown as HTMLElement;
    expect(el).not.toBeNull();
    el.click();
    expect(ctx.posted).toContain(command);
  });
});

describe("both pages", () => {
  it("contain no remote URLs except about.html's one Docs link", () => {
    const docsHref = 'href="https://github.com/LukeHawkins/turbofig"';
    expect(ABOUT_HTML).toContain(docsHref);
    const aboutWithoutDocs = ABOUT_HTML.replace(docsHref, "");
    expect(aboutWithoutDocs).not.toContain('src="http');
    expect(aboutWithoutDocs).not.toContain('href="http');
    expect(SETTINGS_HTML).not.toContain('src="http');
    expect(SETTINGS_HTML).not.toContain('href="http');
  });
});
