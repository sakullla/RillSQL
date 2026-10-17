import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const css = readFileSync(resolve(dirname(fileURLToPath(import.meta.url)), "base.css"), "utf8");

function block(selector: string): string {
  const needle = selector.endsWith("{") ? selector : `${selector} {`;
  const start = css.indexOf(needle);
  expect(start, `selector ${selector} not found`).toBeGreaterThanOrEqual(0);
  return css.slice(start, css.indexOf("}", start));
}

describe("desktop control base styles", () => {
  it("gives buttons a pressed state, the 4px radius and the compact height", () => {
    const btn = block(".btn {");
    expect(btn).toContain("height: var(--control-h)");
    expect(btn).toContain("border-radius: var(--r)");
    expect(css).toContain(".btn:active:not(:disabled)");
    expect(css).toContain(".btn.primary:active:not(:disabled)");
    expect(css).toContain("box-shadow: inset 0 1px 2px var(--press)");
    expect(css).toContain("box-shadow: inset 0 1px 2px var(--press-accent)");
    expect(css).toContain("box-shadow: inset 0 1px 2px var(--press-danger)");
    expect(css).toContain(".btn:disabled {");
  });

  it("sizes icon buttons from the control scale", () => {
    const icon = block(".icon-btn {");
    expect(icon).toContain("width: var(--control-h)");
    expect(icon).toContain("height: var(--control-h)");
    expect(css).toContain(".icon-btn.sm { width: var(--control-h-sm); height: var(--control-h-sm); }");
    expect(css).toContain(".icon-btn:active:not(:disabled)");
  });

  it("keeps focus rings at 2px and routes overlays through the z tokens", () => {
    expect(css).toContain("box-shadow: 0 0 0 2px var(--ring)");
    expect(block(".menu {")).toContain("z-index: var(--z-menu)");
    expect(block(".tooltip {")).toContain("z-index: var(--z-float)");
    expect(block(".skip {")).toContain("z-index: var(--z-toast)");
  });

  it("uses square tags, compact menu rows, small page titles and a 12px body", () => {
    expect(block(".badge {")).toContain("border-radius: var(--r-sm)");
    expect(block(".menu-item {")).toContain("height: 24px");
    expect(block(".page-head h1 {")).toContain("font-size: var(--fs-xxl)");
    expect(block(".empty {")).toContain("padding: var(--sp-6)");
    // base.css 里 `body {` 出现两次（html,body 重置 + 字体规则），block() 无法区分，改为全文断言同一条声明
    expect(css).toContain("font: var(--fs-sm)/1 var(--font);");
  });

  it("keeps menus on the panel token with the single menu-level shadow", () => {
    expect(block(".menu {")).toContain("box-shadow: var(--shadow)");
    expect(block(".menu {")).toContain("border-radius: var(--r-md)");
  });
});
