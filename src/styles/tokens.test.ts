import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const css = readFileSync(resolve(dirname(fileURLToPath(import.meta.url)), "tokens.css"), "utf8");

function block(selector: string): string {
  const needle = selector.endsWith("{") ? selector : `${selector} {`;
  const start = css.indexOf(needle);
  expect(start).toBeGreaterThanOrEqual(0);
  return css.slice(start, css.indexOf("}", start));
}

function tokenValue(scope: string, name: string): string {
  const match = scope.match(new RegExp(`${name}\\s*:\\s*([^;]+);`));
  expect(match, `missing token ${name}`).toBeTruthy();
  return match![1].trim();
}

const THEME_TOKENS = [
  "--bg",
  "--chrome",
  "--panel",
  "--panel-2",
  "--sunken",
  "--hover",
  "--active",
  "--text",
  "--text-2",
  "--text-3",
  "--line",
  "--line-2",
  "--accent",
  "--accent-hover",
  "--accent-active",
  "--accent-soft",
  "--accent-text",
  "--on-accent",
  "--ok",
  "--ok-soft",
  "--warn",
  "--warn-soft",
  "--warn-line",
  "--err",
  "--err-soft",
  "--err-line",
  "--info",
  "--info-soft",
  "--sel",
  "--sel-strong",
  "--ring",
  "--disabled-fg",
  "--disabled-bg",
  "--shadow-sm",
  "--shadow",
  "--shadow-lg",
  "--press",
  "--press-accent",
  "--press-danger",
  "--tk-kw",
  "--tk-str",
  "--tk-num",
  "--tk-com",
  "--tk-fn",
  "--tk-id",
  "--env-dev",
  "--env-test",
  "--env-prod",
  "--env-local",
  "--env-none",
  "--eng-postgres",
  "--eng-mysql",
  "--eng-mariadb",
  "--eng-sqlite",
  "--nav-bg",
  "--nav-fg",
  "--nav-active",
  "--nav-hover",
  "--nav-active-bg",
  "--overlay",
];

describe("design tokens", () => {
  it("defines the full token set for both light and dark themes", () => {
    const light = block(":root[data-theme=\"light\"]");
    const dark = block(":root[data-theme=\"dark\"]");
    for (const name of THEME_TOKENS) {
      expect(tokenValue(light, name), `light ${name}`).toBeTruthy();
      expect(tokenValue(dark, name), `dark ${name}`).toBeTruthy();
    }
  });

  it("uses different values for key tokens across themes", () => {
    const light = block(":root[data-theme=\"light\"]");
    const dark = block(":root[data-theme=\"dark\"]");
    for (const name of ["--bg", "--chrome", "--panel", "--text", "--text-2", "--accent", "--accent-soft", "--on-accent", "--ok", "--warn", "--err", "--info", "--ring", "--nav-bg", "--nav-fg", "--nav-active", "--nav-hover", "--nav-active-bg", "--overlay"]) {
      expect(tokenValue(light, name), name).not.toBe(tokenValue(dark, name));
    }
  });

  it("keeps activity rail colors out of the theme-invariant root", () => {
    const root = block(":root {");
    expect(root).not.toContain("--nav-bg");
    expect(root).not.toContain("--nav-fg");
    expect(root).not.toContain("--nav-active");
    expect(root).not.toContain("--nav-hover");
    expect(root).not.toContain("--overlay");
    expect(tokenValue(root, "--fs-xxl")).toBe("15px");
  });

  it("declares light as the default color scheme", () => {
    expect(block(":root[data-theme=\"light\"]")).toContain("color-scheme: light");
    expect(block(":root[data-theme=\"dark\"]")).toContain("color-scheme: dark");
  });

  it("uses the desktop control shape scale: 4px controls, 6px panels, 3px tags", () => {
    const root = block(":root {");
    expect(tokenValue(root, "--r")).toBe("4px");
    expect(tokenValue(root, "--r-sm")).toBe("3px");
    expect(tokenValue(root, "--r-md")).toBe("6px");
    expect(tokenValue(root, "--r-lg")).toBe("6px");
    expect(tokenValue(root, "--r-xl")).toBe("6px");
  });

  it("defines the five-step type scale and the float layer scale", () => {
    const root = block(":root {");
    expect(tokenValue(root, "--fs-xs")).toBe("11px");
    expect(tokenValue(root, "--fs-sm")).toBe("12px");
    expect(tokenValue(root, "--fs-md")).toBe("13px");
    expect(tokenValue(root, "--fs-lg")).toBe("15px");
    expect(tokenValue(root, "--fs-xl")).toBe("18px");
    // 六档浮层层级必须严格递增，否则菜单/对话框/Toast 的叠放会互相压住
    const zLayers = ["--z-base", "--z-sticky", "--z-float", "--z-menu", "--z-dialog", "--z-toast"]
      .map((name) => Number.parseInt(tokenValue(root, name), 10));
    expect(zLayers).toEqual([0, 10, 100, 200, 300, 400]);
    expect([...zLayers].sort((a, b) => a - b)).toEqual(zLayers);
  });

  it("provides spacing, font-size, radius and layout scales", () => {
    const root = block(":root {");
    for (const name of ["--sp-1", "--sp-2", "--sp-3", "--sp-4", "--sp-5", "--sp-6", "--sp-7", "--sp-8"]) {
      expect(tokenValue(root, name), name).toMatch(/^\d+px$/);
    }
    for (const name of ["--fs-xs", "--fs-sm", "--fs-md", "--fs-lg", "--fs-xl", "--fs-xxl"]) {
      expect(tokenValue(root, name), name).toMatch(/^\d+(\.\d+)?px$/);
    }
    for (const name of ["--r-sm", "--r", "--r-md", "--r-lg", "--r-xl", "--r-full"]) {
      expect(tokenValue(root, name), name).toMatch(/^\d+px$/);
    }
    for (const name of ["--titlebar-h", "--status-h", "--rail-w", "--editor-font", "--row-h", "--tree-h", "--control-h", "--control-h-sm"]) {
      expect(tokenValue(root, name), name).toBeTruthy();
    }
  });

  it("provides motion duration tokens and a reduced-motion override", () => {
    const root = block(":root {");
    for (const name of ["--dur-fast", "--dur", "--dur-slow"]) {
      expect(tokenValue(root, name), name).toMatch(/^\d+ms$/);
    }
    expect(tokenValue(root, "--ease")).toBeTruthy();
    const reduced = block("@media (prefers-reduced-motion: reduce)");
    expect(reduced).toContain("--dur-fast: 0ms");
    expect(reduced).toContain("--dur: 0ms");
    expect(reduced).toContain("--dur-slow: 0ms");
  });

  it("defaults to compact density and widens in standard mode", () => {
    const root = block(":root {");
    expect(Number.parseInt(tokenValue(root, "--row-h"), 10)).toBe(26);
    expect(Number.parseInt(tokenValue(root, "--tree-h"), 10)).toBe(23);
    expect(Number.parseInt(tokenValue(root, "--control-h"), 10)).toBe(26);
    expect(Number.parseInt(tokenValue(root, "--control-h-sm"), 10)).toBe(24);
    const standard = block(":root[data-density=\"standard\"]");
    expect(Number.parseInt(tokenValue(standard, "--row-h"), 10)).toBe(32);
    expect(Number.parseInt(tokenValue(standard, "--tree-h"), 10)).toBe(30);
    expect(Number.parseInt(tokenValue(standard, "--control-h"), 10)).toBe(30);
    expect(Number.parseInt(tokenValue(standard, "--control-h-sm"), 10)).toBe(26);
  });

  it("keeps legacy token names resolvable for transitional module css", () => {
    const light = block(":root[data-theme=\"light\"]");
    for (const name of ["--bg", "--text", "--accent", "--line", "--panel", "--hover", "--sel", "--sel-strong", "--shadow"]) {
      expect(tokenValue(light, name), name).toBeTruthy();
    }
  });
});
