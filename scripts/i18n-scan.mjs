// 全仓界面文案字典化扫描（R6/T6）：
// 遍历 src/**/*.vue 与文案 TS 模块，剥离注释（TS 行/块注释、Vue HTML 注释、CSS 注释）后，
// 检出任何剩余的裸中文字符。排除：测试文件、纯数据/夹具文件、i18n 字典本身、env.d.ts。
import fs from "node:fs";
import path from "node:path";
import url from "node:url";

const ROOT = process.cwd();
const SRC = path.join(ROOT, "src");
const HAN = /[\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff]/;

const EXCLUDED_FILES = new Set(["fixtures.ts", "fixture.ts", "env.d.ts"]);

function isExcluded(file) {
  const base = path.basename(file);
  if (base.endsWith(".test.ts")) return true;
  if (EXCLUDED_FILES.has(base)) return true;
  const rel = path.relative(SRC, file).split(path.sep).join("/");
  if (rel.startsWith("i18n/")) return true;
  return false;
}

// 剥离注释：跟踪字符串/模板字面量状态，避免把字符串里的 // 当注释、把注释状态误伤字符串。
function stripTsComments(source) {
  let out = "";
  let i = 0;
  const n = source.length;
  let prev = "";
  while (i < n) {
    const c = source[i];
    const next = source[i + 1];
    // 字符串与模板原样保留（模板内的中文由「字符串中含中文」检出）
    if (c === '"' || c === "'" || c === "`") {
      const quote = c;
      out += c;
      i += 1;
      while (i < n) {
        out += source[i];
        if (source[i] === "\\") { out += source[i + 1] ?? ""; i += 2; continue; }
        if (source[i] === quote || source[i] === "\n") { i += 1; break; }
        i += 1;
      }
      prev = quote;
      continue;
    }
    if (c === "/" && next === "/") {
      while (i < n && source[i] !== "\n") i += 1;
      continue;
    }
    if (c === "/" && next === "*") {
      i += 2;
      while (i < n && !(source[i] === "*" && source[i + 1] === "/")) i += 1;
      i += 2;
      out += " ";
      continue;
    }
    out += c;
    prev = c;
    i += 1;
  }
  return out;
}

function stripVueComments(source) {
  let s = source.replace(/<!--[\s\S]*?-->/g, (m) => m.replace(/[^\n]/g, " "));
  // <style> 块内的 CSS 注释
  s = s.replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, " "));
  // <script> 段内注释按 TS 处理；整段统一再过一遍 TS 剥离是安全的（模板里没有 TS 注释语法）
  return s;
}

function walk(dir, out = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, entry.name);
    if (entry.isDirectory()) walk(p, out);
    else if (/\.(vue|ts)$/.test(entry.name)) out.push(p);
  }
  return out;
}

export function scanChineseUiText() {
  const violations = [];
  for (const file of walk(SRC)) {
    if (isExcluded(file)) continue;
    const rel = path.relative(ROOT, file).split(path.sep).join("/");
    const raw = fs.readFileSync(file, "utf8");
    const stripped = file.endsWith(".vue") ? stripVueComments(raw) : raw;
    const cleaned = stripTsComments(stripped);
    cleaned.split(/\r?\n/).forEach((line, index) => {
      const m = HAN.exec(line);
      if (m) violations.push(`${rel}:${index + 1}: ${line.trim().slice(0, 120)}`);
    });
  }
  return violations;
}

if (import.meta.url === url.pathToFileURL(process.argv[1]).href) {
  const violations = scanChineseUiText();
  if (violations.length) {
    console.error(`[i18n-scan] ${violations.length} 处模板/渲染层裸中文残留：`);
    for (const line of violations) console.error("  " + line);
    process.exit(1);
  }
  console.log("[i18n-scan] 界面文案字典化扫描通过：无裸中文残留");
}
