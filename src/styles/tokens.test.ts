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

function hexValue(scope: string, name: string): string {
  const value = tokenValue(scope, name).toLowerCase();
  expect(value, `${name} must be a hex color, got ${value}`).toMatch(/^#[0-9a-f]{6}$/);
  return value;
}

function channel(byte: number): number {
  const s = byte / 255;
  return s <= 0.04045 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
}

function luminance(hex: string): number {
  const n = Number.parseInt(hex.slice(1), 16);
  return 0.2126 * channel((n >> 16) & 255) + 0.7152 * channel((n >> 8) & 255) + 0.0722 * channel(n & 255);
}

function contrast(fg: string, bg: string): number {
  const [hi, lo] = [luminance(fg), luminance(bg)].sort((a, b) => b - a);
  return (hi + 0.05) / (lo + 0.05);
}

const THEME_TOKENS = [
  "--surface-app",
  "--surface-chrome",
  "--surface-panel",
  "--surface-sunken",
  "--surface-overlay",
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
  "--stroke",
  "--stroke-strong",
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

const THEME_SCOPES = {
  light: () => block(":root[data-theme=\"light\"]"),
  dark: () => block(":root[data-theme=\"dark\"]"),
};

describe("design tokens", () => {
  it("defines the full token set for both light and dark themes", () => {
    for (const scope of [THEME_SCOPES.light(), THEME_SCOPES.dark()]) {
      for (const name of THEME_TOKENS) {
        expect(tokenValue(scope, name), name).toBeTruthy();
      }
    }
  });

  it("uses different values for key tokens across themes", () => {
    const light = THEME_SCOPES.light();
    const dark = THEME_SCOPES.dark();
    for (const name of [
      "--surface-app",
      "--surface-chrome",
      "--surface-panel",
      "--text",
      "--text-2",
      "--stroke",
      "--accent-hover",
      "--accent-active",
      "--accent-soft",
      "--accent-text",
      "--ok",
      "--warn",
      "--err",
      "--info",
      "--ring",
      "--nav-bg",
      "--nav-fg",
      "--nav-active",
      "--nav-hover",
      "--nav-active-bg",
      "--overlay",
    ]) {
      expect(tokenValue(light, name), name).not.toBe(tokenValue(dark, name));
    }
  });

  it("anchors the accent family on radix iris-9 in both themes", () => {
    for (const scope of [THEME_SCOPES.light(), THEME_SCOPES.dark()]) {
      expect(hexValue(scope, "--accent")).toBe("#5b5bd6");
    }
  });

  it("keeps the legacy indigo family fully removed", () => {
    expect(css.toLowerCase()).not.toContain("#4f46e5");
    expect(css.toLowerCase()).not.toContain("#8b7ff7");
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
    expect(THEME_SCOPES.light()).toContain("color-scheme: light");
    expect(THEME_SCOPES.dark()).toContain("color-scheme: dark");
  });

  it("raises the radius scale by one step: 8px controls, 12px panels, 6px tags", () => {
    const root = block(":root {");
    expect(tokenValue(root, "--r")).toBe("8px");
    expect(tokenValue(root, "--r-sm")).toBe("6px");
    expect(tokenValue(root, "--r-md")).toBe("10px");
    expect(tokenValue(root, "--r-lg")).toBe("12px");
    expect(tokenValue(root, "--r-xl")).toBe("14px");
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

  it("leads the font stacks with the bundled Inter Variable and JetBrains Mono faces", () => {
    const root = block(":root {");
    expect(tokenValue(root, "--font").split(",")[0].trim()).toBe('"Inter Variable"');
    expect(tokenValue(root, "--mono").split(",")[0].trim()).toBe('"JetBrains Mono"');
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

  it("defaults to compact density and widens in standard and relaxed modes", () => {
    const root = block(":root {");
    expect(Number.parseInt(tokenValue(root, "--row-h"), 10)).toBe(24);
    expect(Number.parseInt(tokenValue(root, "--tree-h"), 10)).toBe(22);
    expect(Number.parseInt(tokenValue(root, "--control-h"), 10)).toBe(28);
    expect(Number.parseInt(tokenValue(root, "--control-h-sm"), 10)).toBe(24);
    const standard = block(":root[data-density=\"standard\"]");
    expect(Number.parseInt(tokenValue(standard, "--row-h"), 10)).toBe(30);
    expect(Number.parseInt(tokenValue(standard, "--tree-h"), 10)).toBe(28);
    expect(Number.parseInt(tokenValue(standard, "--control-h"), 10)).toBe(34);
    expect(Number.parseInt(tokenValue(standard, "--control-h-sm"), 10)).toBe(30);
    const relaxed = block(":root[data-density=\"relaxed\"]");
    expect(Number.parseInt(tokenValue(relaxed, "--row-h"), 10)).toBe(34);
    expect(Number.parseInt(tokenValue(relaxed, "--tree-h"), 10)).toBe(32);
    expect(Number.parseInt(tokenValue(relaxed, "--control-h"), 10)).toBe(38);
    expect(Number.parseInt(tokenValue(relaxed, "--control-h-sm"), 10)).toBe(34);
  });

  it("keeps legacy token names resolvable for transitional module css", () => {
    const light = THEME_SCOPES.light();
    for (const name of ["--bg", "--text", "--accent", "--line", "--panel", "--hover", "--sel", "--sel-strong", "--shadow"]) {
      expect(tokenValue(light, name), name).toBeTruthy();
    }
  });
});

describe("token contrast (WCAG relative luminance)", () => {
  for (const [theme, scope] of [["light", THEME_SCOPES.light()], ["dark", THEME_SCOPES.dark()]] as const) {
    it(`keeps text tokens at >= 4.5:1 in the ${theme} theme`, () => {
      const panel = hexValue(scope, "--surface-panel");
      const textPairs: Array<[string, string, string]> = [
        ["text on panel", "--text", panel],
        ["text on app", "--text", hexValue(scope, "--surface-app")],
        ["text-2 on panel", "--text-2", panel],
        ["text-2 on chrome", "--text-2", hexValue(scope, "--surface-chrome")],
        ["on-accent on accent", "--on-accent", hexValue(scope, "--accent")],
        ["accent-text on panel", "--accent-text", panel],
        ["accent-text on accent-soft", "--accent-text", hexValue(scope, "--accent-soft")],
        ["ok on panel", "--ok", panel],
        ["warn on panel", "--warn", panel],
        ["err on panel", "--err", panel],
        ["info on panel", "--info", panel],
        ["tk-kw on editor bg", "--tk-kw", panel],
        ["tk-str on editor bg", "--tk-str", panel],
        ["tk-num on editor bg", "--tk-num", panel],
        ["tk-com on editor bg", "--tk-com", panel],
        ["tk-fn on editor bg", "--tk-fn", panel],
        ["tk-id on editor bg", "--tk-id", panel],
        ["env-dev on panel", "--env-dev", panel],
        ["env-test on panel", "--env-test", panel],
        ["env-prod on panel", "--env-prod", panel],
        ["env-local on panel", "--env-local", panel],
        ["env-none on panel", "--env-none", panel],
        ["eng-postgres on panel", "--eng-postgres", panel],
        ["eng-mysql on panel", "--eng-mysql", panel],
        ["eng-mariadb on panel", "--eng-mariadb", panel],
        ["eng-sqlite on panel", "--eng-sqlite", panel],
      ];
      for (const [label, fgName, bg] of textPairs) {
        const ratio = contrast(hexValue(scope, fgName), bg);
        expect(ratio, `${theme} ${label}`).toBeGreaterThanOrEqual(4.5);
      }
    });

    it(`keeps non-text tokens at >= 3:1 in the ${theme} theme`, () => {
      const nonTextPairs: Array<[string, string, string]> = [
        ["accent on app", "--accent", "--surface-app"],
        ["accent on chrome", "--accent", "--surface-chrome"],
        ["ok on ok-soft", "--ok", "--ok-soft"],
        ["warn on warn-soft", "--warn", "--warn-soft"],
        ["err on err-soft", "--err", "--err-soft"],
        ["info on info-soft", "--info", "--info-soft"],
        ["accent-text on nav-bg", "--nav-active", "--nav-bg"],
      ];
      for (const [label, fgName, bgName] of nonTextPairs) {
        const ratio = contrast(hexValue(scope, fgName), hexValue(scope, bgName));
        expect(ratio, `${theme} ${label}`).toBeGreaterThanOrEqual(3);
      }
    });
  }
});
