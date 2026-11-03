import type { StatementDto } from "./api";
import type { RunSnapshot } from "./runtime";
import { inspectSql } from "./split";

/** 大纲里的一条语句：位置、行号、首行预览，以及最近一次执行的状态。 */
export interface OutlineStatement {
  /** 1 起始的语句序号。 */
  index: number;
  /** 语句在文档中的起止偏移（与 inspectSql 一致，不含前导注释）。 */
  start: number;
  end: number;
  /** 语句起始行（1 起始）。 */
  line: number;
  /** 语句全文，用于和执行结果按文本匹配。 */
  text: string;
  /** 首行压缩预览（不含换行，截断加省略号）。 */
  preview: string;
  /** 最近一次执行状态；未执行过为 none。 */
  status: "none" | "pending" | "running" | "ok" | "error" | "skipped" | "cancelled" | "unknown";
  /** 返回行数（无结果集为 0）。 */
  rows: number;
  /** 受影响行数；无结果集时为 null。 */
  affected: number | null;
}

const PREVIEW_LIMIT = 72;

/** 从草稿切出语句大纲：跳过空白与注释，首行预览压平空白并截断。 */
export function outlineStatements(sql: string, engine = "postgres"): OutlineStatement[] {
  const pieces = inspectSql(sql, engine).pieces;
  return pieces.map((piece, position) => {
    const firstBreak = piece.text.indexOf("\n");
    const head = (firstBreak < 0 ? piece.text : piece.text.slice(0, firstBreak)).replace(/\s+/g, " ").trim();
    const preview = head.length > PREVIEW_LIMIT ? `${head.slice(0, PREVIEW_LIMIT)}…` : head;
    return {
      index: position + 1,
      start: piece.start,
      end: piece.end,
      line: countLines(sql, piece.start),
      text: piece.text,
      preview,
      status: "none",
      rows: 0,
      affected: null,
    };
  });
}

/**
 * 把一次运行的逐句结果并到大纲上：按语句文本精确匹配，重复文本按出现顺序配对。
 * 执行的是选区或外部载入的脚本时，文本对不上就保持 none，不猜测。
 */
export function mergeRunStatus(list: OutlineStatement[], run: RunSnapshot | undefined): OutlineStatement[] {
  if (!run) return list;
  const byText = new Map<string, StatementDto[]>();
  for (const statement of run.statements) {
    const key = statement.sql.trim();
    const queue = byText.get(key);
    if (queue) queue.push(statement);
    else byText.set(key, [statement]);
  }
  return list.map((item) => {
    const queue = byText.get(item.text.trim());
    const statement = queue?.shift();
    if (!statement) return item;
    return {
      ...item,
      status: toStatus(statement.status),
      rows: statement.columns.length ? statement.rows.length : 0,
      affected: statement.columns.length ? null : statement.affected ?? null,
    };
  });
}

function toStatus(status: string): OutlineStatement["status"] {
  if (status === "pending" || status === "running" || status === "ok" || status === "error" || status === "skipped" || status === "cancelled" || status === "unknown") return status;
  return "unknown";
}

/** 光标/视口偏移落在大纲里的哪一条：含端点匹配，落在语句之间时取前一条。 */
export function statementIndexAt(list: readonly OutlineStatement[], offset: number): number {
  if (!list.length) return -1;
  for (let i = 0; i < list.length; i += 1) {
    const item = list[i]!;
    if (offset < item.start) return i === 0 ? 0 : i - 1;
    if (offset <= item.end) return i;
  }
  return list.length - 1;
}

function countLines(sql: string, offset: number): number {
  let lines = 1;
  const stop = Math.min(offset, sql.length);
  for (let i = 0; i < stop; i += 1) {
    if (sql[i] === "\n") lines += 1;
  }
  return lines;
}
