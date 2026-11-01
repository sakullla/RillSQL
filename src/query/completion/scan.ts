export interface CompletionStatement {
  start: number;
  end: number;
  /** 光标落在无法可靠切分的结构里（未闭合字符串/注释、dollar-quote、delimiter）。 */
  refusal: boolean;
}

/**
 * 只为补全定位当前语句：从文档头扫到光标，跳过字符串、注释与引号标识符；
 * 不构建语句列表、不判断可执行性，因此比 inspectSql 轻。
 */
export function locateForCompletion(text: string, pos: number): CompletionStatement {
  let start = 0;
  let i = 0;
  let refusal = false;
  const limit = Math.min(pos, text.length);
  while (i < limit) {
    const ch = text[i] ?? "";
    if (ch === "'" || ch === '"' || ch === "`") {
      const end = quotedEnd(text, i);
      if (end < 0) {
        refusal = true;
        i = limit;
        break;
      }
      i = end;
      continue;
    }
    if (ch === "-" && text[i + 1] === "-") {
      const end = text.indexOf("\n", i);
      i = end < 0 ? text.length : end;
      continue;
    }
    if (ch === "/" && text[i + 1] === "*") {
      const end = text.indexOf("*/", i + 2);
      if (end < 0) {
        refusal = true;
        i = limit;
        break;
      }
      i = end + 2;
      continue;
    }
    if (ch === "$") {
      const tag = dollarTag(text, i);
      if (tag) {
        const end = text.indexOf(tag, i + tag.length);
        if (end < 0) {
          refusal = true;
          i = limit;
          break;
        }
        i = end + tag.length;
        continue;
      }
    }
    if (ch === ";") start = i + 1;
    i += 1;
  }
  const semi = text.indexOf(";", pos);
  return { start, end: semi < 0 ? text.length : semi, refusal };
}

function quotedEnd(text: string, start: number): number {
  const quote = text[start] ?? "";
  let i = start + 1;
  while (i < text.length) {
    if (text[i] === quote && text[i + 1] === quote) {
      i += 2;
      continue;
    }
    if (text[i] === quote) return i + 1;
    i += 1;
  }
  return -1;
}

function dollarTag(text: string, start: number): string | null {
  const match = /^\$[A-Za-z_0-9]*\$/.exec(text.slice(start));
  return match ? match[0] : null;
}
