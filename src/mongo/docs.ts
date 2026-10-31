import type { MongoDoc } from "./api";
import { tr } from "../i18n";

/** 文档网格默认页大小（与表网格一致）。 */
export const DOC_PAGE_SIZE = 20;

/**
 * 前置 JSON 校验：拦截明显非法的输入，避免无谓的 IPC 往返。
 * 服务端（bson 反序列化）仍是权威校验，语法错误的定位提示以服务端消息为准。
 */
export function quickValidateJson(text: string): string | null {
  const trimmed = text.trim();
  if (!trimmed) return tr("mongo.docs.required");
  try {
    const parsed: unknown = JSON.parse(trimmed);
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
      return tr("mongo.docs.mustBeObject");
    }
    return null;
  } catch (error) {
    const message = error instanceof Error ? error.message : "";
    return message ? tr("mongo.docs.syntaxError", { message }) : tr("mongo.docs.syntaxErrorPlain");
  }
}

/** 文档网格列：_id 恒在首位，其余按当前页文档的顶层字段顺序并集（后到的文档补列）。 */
export function columnsOf(documents: readonly MongoDoc[]): string[] {
  const columns: string[] = [];
  const seen = new Set<string>();
  for (const doc of documents) {
    let parsed: Record<string, unknown>;
    try {
      parsed = JSON.parse(doc.json) as Record<string, unknown>;
    } catch {
      continue;
    }
    for (const key of Object.keys(parsed)) {
      if (!seen.has(key)) {
        seen.add(key);
        columns.push(key);
      }
    }
  }
  return ["_id", ...columns.filter((column) => column !== "_id")];
}

/** 单元格文本：顶层字段值的紧凑 JSON（对象/数组保持 JSON 形态，null 显示 null）。 */
export function cellText(json: string, column: string): string {
  let parsed: Record<string, unknown>;
  try {
    parsed = JSON.parse(json) as Record<string, unknown>;
  } catch {
    return "";
  }
  if (!(column in parsed)) return "";
  const value = parsed[column];
  if (value === null) return "null";
  if (typeof value === "string") return value;
  return JSON.stringify(value);
}

/** 整档 pretty JSON（查看/编辑默认文本）。 */
export function prettyJson(json: string): string {
  try {
    return JSON.stringify(JSON.parse(json), null, 2);
  } catch {
    return json;
  }
}

/** 计数文案：估算值或精确计数共用一套呈现。 */
export function describeCount(count: number | null): string {
  if (typeof count !== "number" || !Number.isFinite(count) || count < 0) return "—";
  return count >= 1000 ? `${(count / 1000).toFixed(count >= 10000 ? 0 : 1)}k` : String(count);
}

/** 翻页状态：skip 步进，最少停在 0。 */
export function nextSkip(current: number, pageSize: number, direction: 1 | -1): number {
  return Math.max(0, current + direction * pageSize);
}

export function canPrev(skip: number): boolean {
  return skip > 0;
}

/** 过滤输入的行内提示：空=未启用；非空给出当前生效语义。 */
export function filterApplied(filterText: string): boolean {
  return filterText.trim().length > 0;
}
