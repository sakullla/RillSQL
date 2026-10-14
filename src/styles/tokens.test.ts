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
    for (const name of ["--bg", "--chrome", "--panel", "--text", "--text-2", "--accent", "--accent-soft", "--on-accent", "--ok", "--warn", "--err", "--info", "--ring"]) {
      expect(tokenValue(light, name), name).not.toBe(tokenValue(dark, name));
    }
  });

  it("declares light as the default color scheme", () => {
    expect(block(":root[data-theme=\"light\"]")).toContain("color-scheme: light");
    expect(block(":root[data-theme=\"dark\"]")).toContain("color-scheme: dark");
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

  it("defaults to a roomier density and widens further in comfortable mode", () => {
    const root = block(":root {");
    const defaultRow = Number.parseInt(tokenValue(root, "--row-h"), 10);
    expect(defaultRow).toBeGreaterThan(28);
    const comfortable = block(":root[data-density=\"comfortable\"]");
    const comfortableRow = Number.parseInt(tokenValue(comfortable, "--row-h"), 10);
    expect(comfortableRow).toBeGreaterThan(defaultRow);
    expect(Number.parseInt(tokenValue(comfortable, "--tree-h"), 10)).toBeGreaterThan(
      Number.parseInt(tokenValue(root, "--tree-h"), 10),
    );
  });

  it("keeps legacy token names resolvable for transitional module css", () => {
    const light = block(":root[data-theme=\"light\"]");
    for (const name of ["--bg", "--text", "--accent", "--line", "--panel", "--hover", "--sel", "--sel-strong", "--shadow"]) {
      expect(tokenValue(light, name), name).toBeTruthy();
    }
  });
});
