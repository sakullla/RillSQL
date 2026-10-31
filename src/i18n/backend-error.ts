import { currentLocale, i18n, tr } from "./index";

/**
 * 后端错误码 → 字典文案桥接（T6-A2/R6，T6-A3 修订）。
 *
 * 后端约定（src-tauri/src/connections/error_codes.rs）：
 * - 结构化错误（TestReport/StageView）带 `code: "conn.auth_failed"` 形式的稳定码；
 * - 纯字符串错误用 `[code] 原文` 前缀携带码，带插值参数时为 `[code key="value"] 原文`。
 *
 * 呈现规则（T6-A3）：仅非 zh-CN locale 走字典——zh-CN 下后端原文就是面向用户的中文
 * （含驱动细节、分引擎措辞与动态参数），直接返回原文，避免被字典摘要覆盖丢细节；
 * 其他 locale 按码查 errors 域（en 专用字典），未知码或无码回退原文。
 */

export interface CodedError {
  code?: string | null;
  message?: string | null;
  hint?: string | null;
}

// 参数段引号感知（T6-A4）：值允许含空格；双引号与反斜杠按后端约定转义为 \" 与 \\。
const CODED_PREFIX = /^\[([a-z0-9]+(?:\.[a-z0-9_]+)+)((?:\s+[a-z_][a-z0-9_]*="(?:[^"\\]|\\.)*")*)\] /;
const PARAM_PAIR = /[a-z_][a-z0-9_]*="((?:[^"\\]|\\.)*)"/g;

/** 反转义参数值：\" → "、\\ → \；未约定的 \x 序列宽容地还原为 x。 */
function unescapeParamValue(value: string): string {
  return value.replace(/\\(.)/g, "$1");
}

/** 剥离 `[code key="value"] ` 前缀：返回 {code, params, text}；无前缀时 code 为 null、text 原样。 */
export function parseCodedText(text: string): { code: string | null; params: Record<string, string>; text: string } {
  const match = CODED_PREFIX.exec(text);
  if (!match) return { code: null, params: {}, text };
  const params: Record<string, string> = {};
  // 逐对 matchAll 解析（不按空白 split）：值中的空格属于引号边界内，不能当分隔符。
  for (const pair of match[2]!.matchAll(PARAM_PAIR)) {
    const equals = pair[0].indexOf("=");
    params[pair[0].slice(0, equals)] = unescapeParamValue(pair[1]!);
  }
  return { code: match[1]!, params, text: text.slice(match[0].length) };
}

/** 从任意错误形态（字符串 / Error / 带 code 的对象）提取稳定码；无码返回 null。 */
export function errorCodeOf(error: unknown): string | null {
  if (typeof error === "string") return parseCodedText(error).code;
  if (error instanceof Error) return parseCodedText(error.message).code;
  if (error && typeof error === "object") {
    const coded = error as CodedError;
    if (typeof coded.code === "string" && coded.code) return coded.code;
    if (typeof coded.message === "string") return parseCodedText(coded.message).code;
  }
  return null;
}

/** 非 zh-CN locale 才启用字典映射；zh-CN 原文直出（返回 null 让调用方回退原文）。 */
function bridgesToDictionary(): boolean {
  return currentLocale() !== "zh-CN";
}

function hasMessageKey(code: string): boolean {
  // te（translation exists）探测，不经过 missing 守卫（测试环境对 t() 缺 key 抛错）。
  return i18n.global.te(`errors.${code}.message`);
}

/** 按码取主文案（可带插值参数）；zh-CN、未知/缺码返回 null（调用方回退原文）。 */
export function codeMessage(code: string | null | undefined, params: Record<string, string> = {}): string | null {
  if (!bridgesToDictionary()) return null;
  if (!code || !CODED_PREFIX.test(`[${code}] `)) return null;
  if (!hasMessageKey(code)) return null;
  return tr(`errors.${code}.message` as Parameters<typeof tr>[0], params);
}

/** 按码取提示文案（分阶段报告的 hint 位）；zh-CN、无码或为空返回 null。 */
export function codeHint(code: string | null | undefined): string | null {
  if (!bridgesToDictionary()) return null;
  if (!code || !hasMessageKey(code)) return null;
  const hint = tr(`errors.${code}.hint` as Parameters<typeof tr>[0]);
  return hint ? hint : null;
}

/**
 * 统一呈现入口：把后端错误（字符串 / Error / 带 code 的对象）转成当前语言的文案。
 * - zh-CN：始终原文（对象带码也回退 message 原文），保留动态细节；
 * - 其他 locale：对象带 code 或字符串带 `[code] ` 前缀 → 字典主文案（含参数插值），取不到回退剥前缀原文。
 */
export function backendErrorText(error: unknown, fallback = ""): string {
  if (typeof error === "string") {
    const parsed = parseCodedText(error);
    return codeMessage(parsed.code, parsed.params) ?? parsed.text ?? fallback;
  }
  if (error instanceof Error) {
    const parsed = parseCodedText(error.message);
    return codeMessage(parsed.code, parsed.params) ?? parsed.text ?? fallback;
  }
  if (error && typeof error === "object") {
    const coded = error as CodedError;
    const raw = coded.message ?? "";
    if (raw) {
      const parsed = parseCodedText(raw);
      const fromCode = coded.code ? codeMessage(coded.code) : null;
      if (fromCode) return fromCode;
      const fromText = codeMessage(parsed.code, parsed.params);
      if (fromText) return fromText;
      return parsed.text || fallback;
    }
  }
  return fallback;
}
