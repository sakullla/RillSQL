import { describe, expect, it } from "vitest";
import { readdirSync, readFileSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { setLocale, tr } from "./index";
import { zhCN } from "./zh-CN";

/**
 * 字典完整性守卫（T6-A2/P1）：
 * 1) 扫描 src 下全部 .vue/.ts（排除字典自身与测试）里 t("…") / tr("…") 字面量调用点，
 *    逐一断言 key 存在于 zh-CN 字典，能同时拦截同类 typo（引用不存在或挪错段的 key）。
 * 2) 渲染守卫：抽查曾因 vue-i18n 保留字符（竖线与花括号）出过编译问题的消息，断言完整输出。
 */

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");

interface DirEntry {
  name: string;
  isDirectory: () => boolean;
}

function walk(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir, { withFileTypes: true }) as unknown as DirEntry[]) {
    const p = `${dir}/${entry.name}`;
    if (entry.isDirectory()) {
      if (entry.name === "i18n") continue;
      walk(p, out);
    } else if (/\.(vue|ts)$/.test(entry.name) && !/\.test\.ts$/.test(entry.name)) {
      out.push(p);
    }
  }
  return out;
}

function flatten(obj: Record<string, unknown>, prefix = "", keys = new Set<string>()): Set<string> {
  for (const [k, v] of Object.entries(obj)) {
    const path = prefix ? `${prefix}.${k}` : k;
    if (typeof v === "string") keys.add(path);
    else flatten(v as Record<string, unknown>, path, keys);
  }
  return keys;
}

describe("i18n dictionary completeness guard", () => {
  it("every t()/tr() literal call site resolves to an existing zh-CN key", () => {
    const known = flatten(zhCN as unknown as Record<string, unknown>);
    const files = walk(resolve(root, "src"));
    expect(files.length).toBeGreaterThan(80);
    const missing: string[] = [];
    // 匹配 t("a.b") / t('a.b') / tr("a.b") / tr('a.b')；只收字面量且含点号的 key，
    // 变量 key 与字符串拼接由运行时 missing 守卫（测试环境抛错）覆盖。
    const call = /\btr?\(\s*(["'])((?:[^"'\\]|\\.)*?\.(?:[^"'\\]|\\.)+?)\1/g;
    for (const file of files) {
      const source = readFileSync(file, "utf8");
      for (const match of source.matchAll(call)) {
        const key = match[2];
        if (!known.has(key)) missing.push(`${file.replace(root + "/", "").split("\\").join("/")}: ${key}`);
      }
    }
    expect(missing, `missing keys:\n  ${missing.join("\n  ")}`).toEqual([]);
  });

  it("renders markdown description with a literal pipe completely in both locales", () => {
    expect(tr("export.dialog.markdownDesc")).toContain("|");
    setLocale("en");
    try {
      expect(tr("export.dialog.markdownDesc")).toContain("Pipe-separated");
    } finally {
      setLocale("zh-CN");
    }
  });

  it("renders mongo JSON examples with literal braces completely", () => {
    expect(tr("mongo.docs.required")).toContain('{"status": "active"}');
    expect(tr("mongo.tab.filterPlaceholder")).toContain('{"status": "active"}');
  });
});
