import { i18n, tr } from "./index";

/**
 * 后端错误码 → 字典文案桥接（T6-A2/R6）。
 *
 * 后端约定（src-tauri/src/connections/error_codes.rs）：
 * - 结构化错误（TestReport/StageView）带 `code: "conn.auth_failed"` 形式的稳定码；
 * - 纯字符串错误用 `[code] 原文` 前缀携带码（coded_string）。
 *
 * 呈现规则：先查码对应的 errors 域 key，命中则按当前语言输出；无码或未知码回退原文。
 */

export interface CodedError {
  code?: string | null;
  message?: string | null;
  hint?: string | null;
}

const CODED_PREFIX = /^\[([a-z0-9]+(?:\.[a-z0-9_]+)+)\] /;

/** 剥离 `[code] ` 前缀：返回 {code, text}；无前缀时 code 为 null、text 原样。 */
export function parseCodedText(text: string): { code: string | null; text: string } {
  const match = CODED_PREFIX.exec(text);
  if (!match) return { code: null, text };
  return { code: match[1]!, text: text.slice(match[0].length) };
}

function hasMessageKey(code: string): boolean {
  // te（translation exists）探测，不经过 missing 守卫（测试环境对 t() 缺 key 抛错）。
  return i18n.global.te(`errors.${code}.message`);
}

/** 按码取主文案；未知/缺码返回 null（调用方回退原文）。 */
export function codeMessage(code: string | null | undefined): string | null {
  if (!code || !CODED_PREFIX.test(`[${code}] `)) return null;
  if (!hasMessageKey(code)) return null;
  return tr(`errors.${code}.message` as Parameters<typeof tr>[0]);
}

/** 按码取提示文案（分阶段报告的 hint 位）；无或为空返回 null。 */
export function codeHint(code: string | null | undefined): string | null {
  if (!code || !hasMessageKey(code)) return null;
  const hint = tr(`errors.${code}.hint` as Parameters<typeof tr>[0]);
  return hint ? hint : null;
}

/**
 * 统一呈现入口：把后端错误（字符串 / Error / 带 code 的对象）转成当前语言的文案。
 * - 对象带 code → 字典主文案；
 * - 字符串带 `[code] ` 前缀 → 剥前缀后按码取字典，取不到回退剥前缀的原文；
 * - 其余回退原文（或 Error.message）。
 */
export function backendErrorText(error: unknown, fallback = ""): string {
  if (typeof error === "string") {
    const parsed = parseCodedText(error);
    return codeMessage(parsed.code) ?? parsed.text ?? fallback;
  }
  if (error instanceof Error) {
    const parsed = parseCodedText(error.message);
    return codeMessage(parsed.code) ?? parsed.text ?? fallback;
  }
  if (error && typeof error === "object") {
    const coded = error as CodedError;
    const fromCode = codeMessage(coded.code);
    if (fromCode) return fromCode;
    const raw = coded.message ?? "";
    if (raw) {
      const parsed = parseCodedText(raw);
      return codeMessage(parsed.code) ?? parsed.text ?? fallback;
    }
  }
  return fallback;
}
