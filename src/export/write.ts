import { mkdirSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import { renderCsv, type CsvCell, type CsvOptions } from "./csv";
import { insertStatements, type InsertTarget } from "./insert";
import { jsonLines } from "./json";
import { markdownHeader, markdownLines } from "./markdown";
import type { ExportPlan } from "./plan";

export type ExportFormat = "csv" | "json" | "insert" | "markdown";

export interface WriteControl {
  failAfterRows?: number;
  failMessage?: string;
  cancelAfterRows?: number;
}

export interface WriteProgress {
  status: "running" | "done" | "failed" | "cancelled" | "blocked";
  writtenRows: number;
  totalRows: number;
}

export interface WriteResult {
  status: "done" | "failed" | "cancelled" | "blocked";
  success: boolean;
  writtenRows: number;
  path: string | null;
  residualPath: string | null;
  message: string;
  readCalled: boolean;
}

function blocked(message: string): WriteResult {
  return { status: "blocked", success: false, writtenRows: 0, path: null, residualPath: null, message, readCalled: false };
}

export function writeCsvFile(options: {
  path: string;
  columns: readonly string[];
  rows: readonly Record<string, CsvCell>[];
  csv: CsvOptions;
  control?: WriteControl;
  onProgress?: (progress: WriteProgress) => void;
}): WriteResult {
  const total = options.rows.length;
  const partialPath = `${options.path}.partial`;
  const progress = (status: WriteProgress["status"], writtenRows: number) => {
    options.onProgress?.({ status, writtenRows, totalRows: total });
  };
  progress("running", 0);
  try {
    mkdirSync(dirname(options.path), { recursive: false });
  } catch (error) {
    const code = (error as NodeJS.ErrnoException).code;
    if (code !== "EEXIST") {
      return {
        status: "failed",
        success: false,
        writtenRows: 0,
        path: null,
        residualPath: null,
        message: `路径不可写：${(error as Error).message}。没有生成完整结果。`,
        readCalled: false,
      };
    }
  }
  const prefix = options.csv.bom ? "\uFEFF" : "";
  const lines: string[] = [];
  if (options.csv.header) lines.push(renderCsv(options.columns, [], { ...options.csv, bom: false }));
  try {
    writeFileSync(partialPath, prefix + lines.join("\r\n"), "utf8");
  } catch (error) {
    return {
      status: "failed",
      success: false,
      writtenRows: 0,
      path: null,
      residualPath: null,
      message: `路径不可写：${(error as Error).message}。没有生成完整结果。`,
      readCalled: false,
    };
  }
  const failAfter = options.control?.failAfterRows;
  const cancelAfter = options.control?.cancelAfterRows;
  for (let index = 0; index < options.rows.length; index += 1) {
    if (cancelAfter !== undefined && index === cancelAfter) {
      rmSync(partialPath, { force: true });
      progress("cancelled", index);
      return {
        status: "cancelled",
        success: false,
        writtenRows: index,
        path: null,
        residualPath: null,
        message: `已取消导出：写入 ${index} 行后停止，未完成的文件已删除。`,
        readCalled: false,
      };
    }
    if (failAfter !== undefined && index === failAfter) {
      progress("failed", index);
      return {
        status: "failed",
        success: false,
        writtenRows: index,
        path: null,
        residualPath: partialPath,
        message: `${options.control?.failMessage ?? "写入失败"}。已写入 ${index} 行，不完整的文件保留为 ${partialPath}，不会冒充完整结果。`,
        readCalled: false,
      };
    }
    lines.push(renderCsv(options.columns, [options.rows[index]!], { ...options.csv, bom: false, header: false }));
    writeFileSync(partialPath, prefix + lines.join("\r\n"), "utf8");
    progress("running", index + 1);
  }
  if (cancelAfter !== undefined && cancelAfter === options.rows.length) {
    rmSync(partialPath, { force: true });
    progress("cancelled", options.rows.length);
    return {
      status: "cancelled",
      success: false,
      writtenRows: options.rows.length,
      path: null,
      residualPath: null,
      message: `已取消导出：写入 ${options.rows.length} 行后停止，未完成的文件已删除。`,
      readCalled: false,
    };
  }
  try {
    rmSync(options.path, { force: true });
    renameSync(partialPath, options.path);
  } catch (error) {
    return {
      status: "failed",
      success: false,
      writtenRows: options.rows.length,
      path: null,
      residualPath: partialPath,
      message: `${(error as Error).message}。已写入 ${options.rows.length} 行，不完整的文件保留为 ${partialPath}，不会冒充完整结果。`,
      readCalled: false,
    };
  }
  progress("done", options.rows.length);
  return {
    status: "done",
    success: true,
    writtenRows: options.rows.length,
    path: options.path,
    residualPath: null,
    message: `已导出 ${options.rows.length} 行。`,
    readCalled: false,
  };
}

function refusePath(error: unknown): WriteResult {
  return {
    status: "failed",
    success: false,
    writtenRows: 0,
    path: null,
    residualPath: null,
    message: `路径不可写：${(error as Error).message}。没有生成完整结果。`,
    readCalled: false,
  };
}

/** 已读取的 JSON 或 INSERT。先写入临时文件，失败或取消都不替换目标文件。 */
export function writeRenderedFile(options: {
  path: string;
  lines: readonly string[];
  prefix: string;
  suffix: string;
  separator: string;
  empty: string;
  control?: WriteControl;
  onProgress?: (progress: WriteProgress) => void;
}): WriteResult {
  const total = options.lines.length;
  const partialPath = `${options.path}.partial`;
  const progress = (status: WriteProgress["status"], writtenRows: number) => {
    options.onProgress?.({ status, writtenRows, totalRows: total });
  };
  progress("running", 0);
  try {
    mkdirSync(dirname(options.path), { recursive: false });
  } catch (error) {
    const code = (error as NodeJS.ErrnoException).code;
    if (code !== "EEXIST") return refusePath(error);
  }
  const failAfter = options.control?.failAfterRows;
  const cancelAfter = options.control?.cancelAfterRows;
  const written: string[] = [];
  const publish = (body: string) => writeFileSync(partialPath, body, "utf8");
  try {
    publish("");
  } catch (error) {
    return refusePath(error);
  }
  for (let index = 0; index < options.lines.length; index += 1) {
    if (cancelAfter !== undefined && index === cancelAfter) {
      rmSync(partialPath, { force: true });
      progress("cancelled", index);
      return {
        status: "cancelled",
        success: false,
        writtenRows: index,
        path: null,
        residualPath: null,
        message: `已取消导出：写入 ${index} 行后停止，未完成的文件已删除。`,
        readCalled: false,
      };
    }
    if (failAfter !== undefined && index === failAfter) {
      progress("failed", index);
      return {
        status: "failed",
        success: false,
        writtenRows: index,
        path: null,
        residualPath: partialPath,
        message: `${options.control?.failMessage ?? "写入失败"}。已写入 ${index} 行，不完整的文件保留为 ${partialPath}，不会冒充完整结果。`,
        readCalled: false,
      };
    }
    written.push(options.lines[index]!);
    publish(`${options.prefix}${written.join(options.separator)}`);
    progress("running", index + 1);
  }
  if (cancelAfter !== undefined && cancelAfter === options.lines.length) {
    rmSync(partialPath, { force: true });
    progress("cancelled", options.lines.length);
    return {
      status: "cancelled",
      success: false,
      writtenRows: options.lines.length,
      path: null,
      residualPath: null,
      message: `已取消导出：写入 ${options.lines.length} 行后停止，未完成的文件已删除。`,
      readCalled: false,
    };
  }
  try {
    publish(written.length ? `${options.prefix}${written.join(options.separator)}${options.suffix}` : options.empty);
    rmSync(options.path, { force: true });
    renameSync(partialPath, options.path);
  } catch (error) {
    return {
      status: "failed",
      success: false,
      writtenRows: written.length,
      path: null,
      residualPath: partialPath,
      message: `${(error as Error).message}。已写入 ${written.length} 行，不完整的文件保留为 ${partialPath}，不会冒充完整结果。`,
      readCalled: false,
    };
  }
  progress("done", written.length);
  return {
    status: "done",
    success: true,
    writtenRows: written.length,
    path: options.path,
    residualPath: null,
    message: `已导出 ${written.length} 行。`,
    readCalled: false,
  };
}

export function startExport(options: {
  plan: ExportPlan;
  acknowledged: boolean;
  path: string;
  columns: readonly string[];
  rows: readonly Record<string, CsvCell>[];
  csv: CsvOptions;
  format?: ExportFormat;
  insert?: InsertTarget;
  control?: WriteControl;
  readRows?: () => readonly Record<string, CsvCell>[];
  onProgress?: (progress: WriteProgress) => void;
}): WriteResult {
  if (!options.plan.allowed) return blocked(options.plan.rejectReason ?? "不能导出");
  if (options.plan.reexecutes && !options.acknowledged) return blocked("执行前需说明会重新执行这条 SELECT");
  const format = options.format ?? "csv";
  if (format === "insert" && !options.insert?.name) return blocked("还不能确定 INSERT 要写入的表");
  let readCalled = false;
  let rows = options.rows;
  if (options.plan.serverRead) {
    if (!options.readRows) return blocked("没有可重新读取的数据源");
    readCalled = true;
    rows = options.readRows();
  }
  const result = format === "csv"
    ? writeCsvFile({
        path: options.path,
        columns: options.columns,
        rows,
        csv: options.csv,
        control: options.control,
        onProgress: options.onProgress,
      })
    : format === "markdown"
      ? writeRenderedFile({
          path: options.path,
          lines: markdownLines(options.columns, rows, { nullToken: options.csv.nullToken }),
          prefix: `${markdownHeader(options.columns).join("\r\n")}\r\n`,
          suffix: "",
          separator: "\r\n",
          empty: markdownHeader(options.columns).join("\r\n"),
          control: options.control,
          onProgress: options.onProgress,
        })
      : writeRenderedFile({
          path: options.path,
          lines: format === "json" ? jsonLines(options.columns, rows) : insertStatements(options.insert!, options.columns, rows),
          prefix: format === "json" ? "[" : "",
          suffix: format === "json" ? "]" : "",
          separator: format === "json" ? "," : "\r\n",
          empty: format === "json" ? "[]" : "",
          control: options.control,
          onProgress: options.onProgress,
        });
  result.readCalled = readCalled;
  if (result.success && options.plan.partial) {
    result.message = `已导出 ${result.writtenRows} 行到 ${result.path}（仅已读取的行，不是完整结果）`;
  }
  if (options.plan.reexecutes && result.success) result.message = `${options.plan.notice}${result.message}`;
  return result;
}
