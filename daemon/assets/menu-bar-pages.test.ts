// A real DOM test for the 2 menu-bar webview pages (`about/about.html`,
// `settings/settings.html`), run with `happy-dom`. Both pages are now pure
// markup: no `<script>` anywhere, no inline `on*` handler, and every action
// is a plain `<a href="turbofig-action://...">` that Rust's navigation
// handler intercepts (see `about_window.rs`/`settings_window.rs`). This
// test exercises the static HTML only, with the template placeholders
// (`__VERSION__` etc.) substituted the same way Rust does before
// `with_html`, since there is no script left to load or run.
import { describe, expect, it } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { Window } from "happy-dom";

const ABOUT_HTML_TEMPLATE = readFileSync(join(import.meta.dir, "about/about.html"), "utf-8");
const SETTINGS_HTML_TEMPLATE = readFileSync(
  join(import.meta.dir, "settings/settings.html"),
  "utf-8",
);

/** The action names the plugin's own copy-prompt button uses, read straight
 * from its template, so this test fails if that markup ever changes without
 * the About page's "Copy agent prompt" control being updated to match. */
const PLUGIN_TEMPLATE = readFileSync(
  join(import.meta.dir, "..", "..", "plugin", "src", "ui", "template.html"),
  "utf-8",
);

const ALLOWED_ABOUT_ACTIONS = ["copy-path", "show-plugin-folder", "copy-agent-prompt"];
const ALLOWED_SETTINGS_ACTIONS = [
  "start-at-login-on",
  "start-at-login-off",
  "mcp-http-on",
  "mcp-http-off",
  "copy-path",
  "show-plugin-folder",
];
const WEBSITE_URL = "https://lukehawkins.eu";
const GITHUB_URL = "https://github.com/LukeHawkins/turbofig";

/** Loads `html` into a fresh headless window: no `window.ipc` stub needed
 * any more, since there is no script to call it. */
function loadPage(html: string) {
  const window = new Window({
    settings: {
      enableJavaScriptEvaluation: true,
      suppressInsecureJavaScriptEnvironmentWarning: true,
    },
  });
  window.document.write(html);
  return { window, document: window.document };
}

/** Every `turbofig-action://<name>` link on the page, as just the `<name>`
 * part. */
function actionLinkNames(document: ReturnType<typeof loadPage>["document"]) {
  return Array.from(document.querySelectorAll("a[href^='turbofig-action://']")).map((el) =>
    (el as unknown as HTMLAnchorElement).getAttribute("href")!.replace("turbofig-action://", ""),
  );
}

/** Every external (`http`/`https`) link on the page. */
function externalHrefs(document: ReturnType<typeof loadPage>["document"]) {
  return Array.from(document.querySelectorAll("a[href^='http']")).map((el) =>
    (el as unknown as HTMLAnchorElement).getAttribute("href"),
  );
}

describe("about.html", () => {
  const rendered = ABOUT_HTML_TEMPLATE.replace("__VERSION__", "1.2.3");
  const { document } = loadPage(rendered);

  it("has no script tag and no inline on* handler", () => {
    expect(rendered.toLowerCase()).not.toContain("<script");
    expect(rendered).not.toMatch(/\son[a-z]+\s*=/i);
  });

  it("uses only allow-listed turbofig-action:// links", () => {
    const names = actionLinkNames(document);
    expect(names.length).toBeGreaterThan(0);
    for (const name of names) {
      expect(ALLOWED_ABOUT_ACTIONS).toContain(name);
    }
  });

  it("has exactly the 2 allowed external links: website and GitHub", () => {
    const hrefs = externalHrefs(document).sort();
    expect(hrefs).toEqual([GITHUB_URL, WEBSITE_URL].sort());
  });

  it("renders the version into the footer", () => {
    expect(document.getElementById("version-text")?.textContent).toBe("v1.2.3");
  });

  it("step 3 points at the plugin's button only: no copy control in the app, and the button is a non-clickable visual", () => {
    expect(document.querySelector("a[href='turbofig-action://copy-agent-prompt']")).toBeNull();

    const visual = document.querySelector(".inline-btn") as unknown as HTMLElement;
    expect(visual.getAttribute("class")).toContain("icon-btn");
    expect(visual.getAttribute("class")).toContain("brand-btn");
    expect(visual.getAttribute("aria-label")).toBeTruthy();
    expect(visual.tagName).not.toBe("A");

    // The plugin's own button carries the same 2 classes.
    expect(PLUGIN_TEMPLATE).toContain('class="icon-btn brand-btn"');
  });
});

describe("settings.html", () => {
  const rendered = SETTINGS_HTML_TEMPLATE.replace("__VERSION__", "1.2.3")
    .replace("__START_AT_LOGIN_STATE__", "On")
    .replace("__START_AT_LOGIN_TOGGLE_ACTION__", "start-at-login-off")
    .replace("__START_AT_LOGIN_CHECKED__", "true")
    .replace("__MCP_HTTP_STATE__", "On")
    .replace("__MCP_HTTP_TOGGLE_ACTION__", "mcp-http-off")
    .replace("__MCP_HTTP_CHECKED__", "true");
  const { document } = loadPage(rendered);

  it("has no script tag and no inline on* handler", () => {
    expect(rendered.toLowerCase()).not.toContain("<script");
    expect(rendered).not.toMatch(/\son[a-z]+\s*=/i);
  });

  it("uses only allow-listed turbofig-action:// links", () => {
    const names = actionLinkNames(document);
    expect(names.length).toBeGreaterThan(0);
    for (const name of names) {
      expect(ALLOWED_SETTINGS_ACTIONS).toContain(name);
    }
  });

  it("has exactly the 2 allowed external links: website and GitHub", () => {
    expect(externalHrefs(document).sort()).toEqual([GITHUB_URL, WEBSITE_URL].sort());
  });

  it("renders the version and the current Start at Login state", () => {
    expect(document.getElementById("version-text")?.textContent).toBe("v1.2.3");
    expect(document.body.textContent).toContain("Start at login");
    expect(document.querySelector("a[href='turbofig-action://start-at-login-off']")).not.toBeNull();
  });

  it("renders the MCP over HTTP row with its toggle link", () => {
    expect(document.body.textContent).toContain("MCP over HTTP");
    const sw = document.querySelector("a[href='turbofig-action://mcp-http-off']") as unknown as HTMLElement;
    expect(sw).not.toBeNull();
    expect(sw.getAttribute("role")).toBe("switch");
    expect(sw.getAttribute("aria-checked")).toBe("true");
    expect(sw.getAttribute("aria-label")).toBe("MCP over HTTP");
    expect(sw.getAttribute("data-state")).toBe("On");
  });
});

describe("both pages", () => {
  it("every template placeholder was substituted, none left in the DOM test fixtures", () => {
    const aboutRendered = ABOUT_HTML_TEMPLATE.replace("__VERSION__", "1.2.3");
    expect(aboutRendered).not.toContain("__VERSION__");

    const settingsRendered = SETTINGS_HTML_TEMPLATE.replace("__VERSION__", "1.2.3")
      .replace("__START_AT_LOGIN_STATE__", "On")
      .replace("__START_AT_LOGIN_TOGGLE_ACTION__", "start-at-login-off")
    .replace("__START_AT_LOGIN_CHECKED__", "true")
      .replace("__MCP_HTTP_STATE__", "On")
      .replace("__MCP_HTTP_TOGGLE_ACTION__", "mcp-http-off")
    .replace("__MCP_HTTP_CHECKED__", "true");
    expect(settingsRendered).not.toContain("__MCP_HTTP");
    expect(settingsRendered).not.toContain("__START_AT_LOGIN");
    expect(settingsRendered).not.toContain("__VERSION__");
  });
});
