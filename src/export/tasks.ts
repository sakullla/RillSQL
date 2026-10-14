import { reactive } from "vue";
import { gridExport, gridExportCancel, type ExportResult } from "../grid/api";
import type { Filter, SortSpec } from "../grid/types";
import { renderCsv, type CsvCell, type CsvOptions } from "./csv";
import type { ExportPlan } from "./plan";
import type { WriteControl, WriteResult } from "./write";

export interface ExportTask {
  id: string;
  fileName: string;
  path: string;
  status: "running" | "done" | "failed" | "cancelled" | "blocked";
  doneRows: number;
  totalRows: number;
  partial: boolean;
  message: string;
  residualPath: string | null;
}

export const exportTasks = reactive<ExportTask[]>([]);

let taskSeq = 1;

export function resetExportTasks(): void {
  exportTasks.splice(0, exportTasks.length);
}

export function clearFinishedTasks(): void {
  for (let index = exportTasks.length - 1; index >= 0; index -= 1) {
    if (exportTasks[index]!.status !== "running") exportTasks.splice(index, 1);
  }
}

function applyWriteResult(task: ExportTask, result: WriteResult): void {
  task.status = result.status;
  task.doneRows = result.writtenRows;
  task.totalRows = Math.max(task.totalRows, result.writtenRows);
  task.message = result.message;
  task.residualPath = result.residualPath;
  if (result.path) {
    task.path = result.path;
    task.fileName = result.path.split(/[/\\]/).pop() || task.fileName;
  }
}

function blockedExport(message: string): WriteResult {
  return { status: "blocked", success: false, writtenRows: 0, path: null, residualPath: null, message, readCalled: false };
}

function downloadCsv(fileName: string, body: string): void {
  const blob = new Blob([body], { type: "text/csv;charset=utf-8" });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = fileName;
  document.body.append(link);
  link.click();
  link.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 30_000);
}

function exportLoadedInBrowser(options: {
  plan: ExportPlan;
  acknowledged: boolean;
  path: string;
  columns: readonly string[];
  rows: readonly Record<string, CsvCell>[];
  csv: CsvOptions;
  readRows?: () => readonly Record<string, CsvCell>[];
}): WriteResult {
  if (!options.plan.allowed) return blockedExport(options.plan.rejectReason ?? "不能导出");
  if (options.plan.reexecutes && !options.acknowledged) return blockedExport("执行前需说明会重新执行这条 SELECT");
  let readCalled = false;
  let rows = options.rows;
  if (options.plan.serverRead) {
    if (!options.readRows) return blockedExport("没有可重新读取的数据源");
    readCalled = true;
    rows = options.readRows();
  }
  const fileName = options.path.split(/[/\\]/).pop() || options.plan.fileName;
  downloadCsv(fileName, renderCsv(options.columns, rows, options.csv));
  let message = `已导出 ${rows.length} 行，开始下载 ${fileName}。`;
  if (options.plan.partial) message = `已导出 ${rows.length} 行，开始下载 ${fileName}。这只是已读取的行，不是完整结果。`;
  if (options.plan.reexecutes) message = `${options.plan.notice}${message}`;
  return { status: "done", success: true, writtenRows: rows.length, path: options.path, residualPath: null, message, readCalled };
}

async function startExportOnNode(
  options: {
    plan: ExportPlan;
    acknowledged: boolean;
    path: string;
    columns: readonly string[];
    rows: readonly Record<string, CsvCell>[];
    csv: CsvOptions;
    control?: WriteControl;
    readRows?: () => readonly Record<string, CsvCell>[];
  },
  task: ExportTask,
): Promise<WriteResult> {
  const { startExport } = await import(/* @vite-ignore */ "./write");
  return startExport({
    ...options,
    onProgress: (progress) => {
      task.status = progress.status === "blocked" ? task.status : progress.status;
      task.doneRows = progress.writtenRows;
      task.totalRows = progress.totalRows;
    },
  });
}

export async function runExportTask(options: {
  plan: ExportPlan;
  acknowledged: boolean;
  path: string;
  columns: readonly string[];
  rows: readonly Record<string, CsvCell>[];
  csv: CsvOptions;
  control?: WriteControl;
  readRows?: () => readonly Record<string, CsvCell>[];
}): Promise<{ task: ExportTask; result: WriteResult }> {
  const fileName = options.path.split(/[/\\]/).pop() || options.plan.fileName;
  const task: ExportTask = {
    id: `export-${taskSeq}`,
    fileName,
    path: options.path,
    status: "running",
    doneRows: 0,
    totalRows: options.rows.length,
    partial: options.plan.partial,
    message: "正在导出",
    residualPath: null,
  };
  taskSeq += 1;
  exportTasks.unshift(task);
  const result = import.meta.env.VITEST ? await startExportOnNode(options, task) : exportLoadedInBrowser(options);
  applyWriteResult(task, result);
  return { task, result };
}

export interface RemoteExportOptions {
  plan: ExportPlan;
  acknowledged: boolean;
  path: string;
  csv: CsvOptions;
  mode: "table-loaded" | "table-full" | "query";
  connectionId: string;
  database: string;
  schema: string;
  table: string;
  engine: string;
  filters: readonly Filter[];
  sort: SortSpec | null;
  limit: number;
  truncated: boolean;
  statement: string;
}

function asStatus(status: string): ExportTask["status"] {
  if (status === "done" || status === "failed" || status === "cancelled" || status === "blocked" || status === "running") return status;
  return "failed";
}

function applyExportResult(task: ExportTask, result: ExportResult): void {
  task.status = asStatus(result.status);
  task.doneRows = result.writtenRows;
  task.totalRows = Math.max(task.totalRows, result.writtenRows);
  task.partial = result.partial;
  task.message = result.message;
  task.residualPath = result.residualPath;
  if (result.path) {
    task.path = result.path;
    task.fileName = result.path.split(/[/\\]/).pop() || task.fileName;
  }
  if (result.success && result.status !== "done") {
    task.status = "failed";
    task.message = result.message || "导出没有完成，不能记为成功";
  }
}

export function runRemoteExport(options: RemoteExportOptions): { task: ExportTask; done: Promise<ExportTask> } {
  const fileName = options.path.split(/[/\\]/).pop() || options.plan.fileName;
  const task: ExportTask = {
    id: `export-${taskSeq}`,
    fileName,
    path: options.path,
    status: "running",
    doneRows: 0,
    totalRows: options.mode === "table-loaded" ? options.limit : 0,
    partial: options.plan.partial,
    message: "正在从已打开的连接导出",
    residualPath: null,
  };
  taskSeq += 1;
  exportTasks.unshift(task);
  const done = gridExport({
    id: task.id,
    mode: options.mode,
    connectionId: options.connectionId,
    database: options.database,
    schema: options.schema,
    table: options.table,
    engine: options.engine,
    filters: options.filters,
    sort: options.sort,
    limit: options.limit,
    truncated: options.truncated,
    statement: options.statement,
    acknowledged: options.acknowledged,
    path: options.path,
    delimiter: options.csv.delimiter,
    nullToken: options.csv.nullToken,
    bom: options.csv.bom,
    header: options.csv.header,
  })
    .then((result) => {
      applyExportResult(task, result);
      return task;
    })
    .catch((error: unknown) => {
      task.status = "failed";
      task.message = error instanceof Error ? error.message : String(error);
      return task;
    });
  return { task, done };
}

export function cancelRemoteExport(id: string): Promise<void> {
  return gridExportCancel(id);
}

/** 已读取的行不再查询数据库。停在 0 行的旧任务改成未完成，标题不再一直显示「导出 0/N」。 */
export function failIdleRemoteExports(message: string): void {
  for (const task of exportTasks) {
    if (task.status !== "running" || task.doneRows !== 0 || task.totalRows <= 0) continue;
    if (!task.message.startsWith("正在从已打开的连接导出")) continue;
    task.status = "failed";
    task.message = message;
  }
}
