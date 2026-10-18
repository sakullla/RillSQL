import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const workbench = readFileSync(resolve(here, "workbench.css"), "utf8");
const tokens = readFileSync(resolve(here, "tokens.css"), "utf8");

function tokenValue(name: string): string {
  const match = tokens.match(new RegExp(`${name}\\s*:\\s*([^;]+);`));
  expect(match, `missing ${name}`).toBeTruthy();
  return match![1].trim();
}

describe("shell geometry", () => {
  it("pins the S7.2 part sizes", () => {
    expect(tokenValue("--titlebar-h")).toBe("26px");
    expect(tokenValue("--toolbar-h")).toBe("36px");
    expect(tokenValue("--tab-h")).toBe("30px");
    expect(tokenValue("--target-h")).toBe("32px");
    expect(tokenValue("--result-head-h")).toBe("30px");
    expect(tokenValue("--hint-h")).toBe("24px");
    expect(tokenValue("--status-h")).toBe("24px");
    expect(tokenValue("--rail-w")).toBe("44px");
  });

  it("routes the rail through nav tokens instead of raw hex", () => {
    expect(workbench).toMatch(/\.app-root \.rail-btn\.active::before \{[^}]*background: var\(--nav-active\)/);
    expect(workbench).not.toMatch(/#61d3bc/);
  });

  it("uses the tab height token on the tab strip", () => {
    expect(workbench).toMatch(/\.app-root \.tabbar \{[^}]*height: var\(--tab-h\)/);
  });

  it("consumes the three workbench bar heights", () => {
    const query = readFileSync(resolve(here, "../query/QueryTab.vue"), "utf8");
    expect(query).toMatch(/\.query-pane \.q-toolbar \{[^}]*height: var\(--target-h\)/);
    expect(query).toMatch(/height: var\(--result-head-h\)/);
    expect(query).toMatch(/height: var\(--hint-h\)/);
    expect(query).toMatch(/flex-wrap: nowrap/);
  });

  it("chrome strips do not pair a fixed token height with a physical border", () => {
    const titleBar = readFileSync(resolve(here, "../shell/TitleBar.vue"), "utf8");
    const shell = readFileSync(resolve(here, "shell.css"), "utf8");
    // 固定高度 + 实体边框在 border-box 下会吃掉内容 1px；分隔线必须用 inset 阴影
    expect(titleBar).not.toMatch(/height:\s*var\(--toolbar-h\)[^}]*border-(top|bottom):\s*1px/);
    expect(titleBar).toMatch(/box-shadow:\s*inset 0 1px 0 var\(--line\)/);
    expect(shell).not.toMatch(/\.tabbar\s*\{[^}]*border-(bottom|top):\s*1px/);
    expect(shell).toMatch(/\.tabbar\s*\{[^}]*box-shadow:\s*inset 0 -1px 0 var\(--line\)/);
  });
});
