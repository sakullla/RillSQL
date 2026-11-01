import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const SRC = join(process.cwd(), "src");

function vueFiles(dir: string): string[] {
  const found: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) found.push(...vueFiles(full));
    else if (entry.name.endsWith(".vue")) found.push(full);
  }
  return found;
}

describe("native select", () => {
  it("keeps every choice control on Listbox", () => {
    const offenders = vueFiles(SRC)
      .filter((file) => /<select\b/.test(readFileSync(file, "utf8")))
      .map((file) => file.slice(SRC.length).replaceAll("\\", "/"));
    expect(offenders).toEqual([]);
  });
});
