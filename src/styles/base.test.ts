import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const dir = dirname(fileURLToPath(import.meta.url));
const css = readFileSync(resolve(dir, "base.css"), "utf8");
const workbench = readFileSync(resolve(dir, "workbench.css"), "utf8");
const shell = readFileSync(resolve(dir, "shell.css"), "utf8");

function rule(source: string, selector: string): string {
  // 以逗号收尾的选择器本身就是多选择器规则的开头，直接按原文定位
  const needle = selector.endsWith("{") || selector.endsWith(",") ? selector : `${selector} {`;
  const start = source.indexOf(needle);
  expect(start, `selector ${selector} not found`).toBeGreaterThanOrEqual(0);
  return source.slice(start, source.indexOf("}", start));
}

function block(selector: string): string {
  return rule(css, selector);
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
    // 禁用态的内容要断言，不能只断言选择器存在
    const disabled = block(".btn:disabled {");
    expect(disabled).toContain("box-shadow: none");
    expect(disabled).toContain("color: var(--disabled-fg)");
  });

  it("sizes icon buttons from the control scale", () => {
    const icon = block(".icon-btn {");
    expect(icon).toContain("width: var(--control-h)");
    expect(icon).toContain("height: var(--control-h)");
    expect(css).toContain(".icon-btn.sm { width: var(--control-h-sm); height: var(--control-h-sm); }");
    expect(css).toContain(".icon-btn:active:not(:disabled)");
  });

  it("keeps focus rings at 2px and routes overlays through the z tokens", () => {
    // 输入框焦点环要锁在焦点规则本身，避免别处出现同款字符串就假通过
    expect(block('input:not([type="checkbox"]):not([type="radio"]):focus,')).toContain("box-shadow: 0 0 0 2px var(--ring)");
    expect(block(".menu {")).toContain("z-index: var(--z-menu)");
    expect(block(".tooltip {")).toContain("z-index: var(--z-float)");
    expect(block(".skip {")).toContain("z-index: var(--z-toast)");
    // 焦点环继续用强调色，浅色和深色都能看见
    expect(css).toContain("outline: 2px solid var(--accent)");
  });

  it("uses square tags, compact menu rows, small page titles and a 12px body", () => {
    expect(block(".badge {")).toContain("border-radius: var(--r-sm)");
    expect(block(".menu-item {")).toContain("height: var(--control-h-sm)");
    expect(block(".page-head h1 {")).toContain("font-size: var(--fs-xxl)");
    // 页面标题的字重是 Task 3 的改动，必须钉住
    expect(block(".page-head h1 {")).toContain("font-weight: 600");
    expect(block(".empty {")).toContain("padding: var(--sp-6)");
    // base.css 里 `body {` 出现两次（html,body 重置 + 字体规则），block() 无法区分，改为全文断言同一条声明
    expect(css).toContain("font: var(--fs-sm)/1 var(--font);");
  });

  it("keeps menus on the panel token with the single menu-level shadow", () => {
    expect(block(".menu {")).toContain("box-shadow: var(--shadow)");
    expect(block(".menu {")).toContain("border-radius: var(--r-md)");
  });

  it("scrolls pages and the welcome surface vertically, and wide tables inside the grid", () => {
    const page = block(".page {");
    expect(page).toContain("overflow-x: hidden");
    expect(page).toContain("overflow-y: auto");
    expect(page).not.toMatch(/overflow:\s*auto/);
    const welcome = rule(workbench, ".welcome {");
    expect(welcome).toContain("overflow-x: hidden");
    expect(welcome).toContain("overflow-y: auto");
    expect(welcome).not.toMatch(/overflow:\s*auto/);
    expect(rule(workbench, ".app-root .grid-scroll {")).toContain("overflow: auto");
  });

  it("keeps welcome and page titles on the 15px token", () => {
    expect(block(".page-head h1 {")).toContain("font-size: var(--fs-xxl)");
    const welcomeTitle = rule(workbench, ".desk-heading h1 {");
    expect(welcomeTitle).toContain("font-size: var(--fs-xxl)");
    expect(welcomeTitle).not.toContain("clamp(");
    expect(rule(workbench, ".page .page-head h1 {")).not.toMatch(/font-size:\s*[\d.]+px/);
    expect(workbench).not.toContain("font-size: 26px");
    expect(workbench).not.toContain("clamp(24px, 2.8vw, 36px)");
  });

  it("uses theme tokens for rail hover and page masks", () => {
    const hover = rule(workbench, ".app-root .rail-btn:hover {");
    expect(hover).toContain("background: var(--nav-hover)");
    expect(hover).not.toMatch(/#[0-9a-fA-F]{3,8}/);
    expect(rule(workbench, ".app-root .rail-btn.active {")).toContain("background: var(--nav-active-bg)");
    const mark = rule(workbench, ".app-root .rail-btn.active::before {");
    expect(mark).toContain("background: var(--accent)");
    expect(mark).not.toContain("#61d3bc");
    expect(workbench).toContain("background: var(--overlay)");
    expect(workbench).not.toContain("#ffffff0d");
    expect(workbench).not.toContain("#ffffff14");
    expect(workbench).not.toContain("#091c2433");
    expect(block("dialog::backdrop")).toContain("background: var(--overlay)");
    expect(rule(shell, "#palette {")).toContain("background: var(--overlay)");
  });
});
