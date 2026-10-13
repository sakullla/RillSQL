import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const css = readFileSync(resolve(dirname(fileURLToPath(import.meta.url)), "tokens.css"), "utf8");

function block(selector: string): string {
  const start = css.indexOf(selector);
  expect(start).toBeGreaterThanOrEqual(0);
  return css.slice(start, css.indexOf("}", start));
}

describe("prototype color tokens", () => {
  it("matches the light palette", () => {
    const light = block(":root[data-theme=\"light\"]");
    expect(light).toContain("--bg: #f3f5f4");
    expect(light).toContain("--chrome: #eef1f0");
    expect(light).toContain("--panel: #ffffff");
    expect(light).toContain("--text: #1b2623");
    expect(light).toContain("--text-2: #52615c");
    expect(light).toContain("--text-3: #7f8c87");
    expect(light).toContain("--accent: #0f7f68");
    expect(light).toContain("--ok: #157a4b");
    expect(light).toContain("--warn: #8f5507");
    expect(light).toContain("--err: #bb3529");
    expect(light).toContain("--info: #2563a8");
    expect(light).toContain("--env-dev: #0f7f68");
    expect(light).toContain("--env-test: #2563a8");
    expect(light).toContain("--env-prod: #c4321f");
    expect(light).toContain("--env-local: #7354e0");
  });

  it("matches the dark palette", () => {
    const dark = block(":root[data-theme=\"dark\"]");
    expect(dark).toContain("--bg: #0e1312");
    expect(dark).toContain("--chrome: #111716");
    expect(dark).toContain("--panel: #151b1a");
    expect(dark).toContain("--text: #dfe7e4");
    expect(dark).toContain("--text-2: #a3b1ac");
    expect(dark).toContain("--text-3: #73827d");
    expect(dark).toContain("--accent: #4fc7a2");
    expect(dark).toContain("--ok: #5fce90");
    expect(dark).toContain("--warn: #e8b660");
    expect(dark).toContain("--err: #f2897d");
    expect(dark).toContain("--info: #82b4ec");
    expect(dark).toContain("--env-dev: #4fc7a2");
    expect(dark).toContain("--env-test: #82b4ec");
    expect(dark).toContain("--env-prod: #f2897d");
    expect(dark).toContain("--env-local: #b3a1ff");
  });
});
