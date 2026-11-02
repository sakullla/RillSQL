import { reactive } from "vue";
import { tr } from "../i18n";
import { backendErrorText } from "../i18n/backend-error";
import { gridExport, gridExportCancel, type ExportResult } from "../grid/api";
import type { Filter, SortSpec } from "../grid/types";
import { renderCsv, type CsvCell, type CsvOptions } from "./csv";
import { databaseDump, databaseDumpCancel, type DumpPayload, type DumpProgressEvent, type DumpResult } from "./dump";
import { migrateCancel, migrateData, type MigratePayload, type MigrateProgressEvent, type MigrateResult } from "./migrate";
import { renderInsert, type InsertTarget } from "./insert";
import { renderJson } from "./json";
import { renderMarkdown } from "./markdown";
import type { ExportPlan } from "./plan";
import type { ExportFormat, WriteControl, WriteResult } from "./write";
import { isNativeExport, writeNativeCsv } from "./native";

export interface ExportTask {
  id: string;
  /** dump 表示全库导出任务；migrate 表示跨库迁移；缺省是表/查询导出。取消通道按它分流。 */
  kind?: "export" | "dump" | "migrate";
  fileName: string;
  path: string;
  status: "running" | "done" | "failed" | "cancelled" | "blocked";
  doneRows: number;
  totalRows: number;
  partial: boolean;
  message: string;
  residualPath: string | null;
  cancellable?: boolean;
  /** dump 正在处理的对象名（进度事件给出）。 */
  object?: string;
  /** migrate 的完成报告：失败行的主键/行号样本与首批错误。 */
  report?: { failedIds: string[]; firstErrors: string[] };
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

function downloadBytes(fileName: string, bytes: Uint8Array, type: string): void {
  const body = new Uint8Array(bytes);
  const blob = new Blob([body.buffer as ArrayBuffer], { type });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = fileName;
  document.body.append(link);
  link.click();
  link.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 30_000);
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

export function renderExportText(options: {
  format?: ExportFormat;
  columns: readonly string[];
  rows: readonly Record<string, CsvCell>[];
  csv: CsvOptions;
  insert?: InsertTarget;
}): string {
  if (options.format === "xlsx") throw new Error(tr("export.tasks.notText"));
  if (options.format === "json") return renderJson(options.columns, options.rows);
  if (options.format === "markdown") return renderMarkdown(options.columns, options.rows, { nullToken: options.csv.nullToken });
  if (options.format === "insert") {
    if (!options.insert?.name) throw new Error(tr("export.tasks.noInsertTarget"));
    return renderInsert(options.insert, options.columns, options.rows);
  }
  return renderCsv(options.columns, options.rows, options.csv);
}

async function exportLoadedInBrowser(options: {
  plan: ExportPlan;
  acknowledged: boolean;
  path: string;
  columns: readonly string[];
  rows: readonly Record<string, CsvCell>[];
  csv: CsvOptions;
  format?: ExportFormat;
  insert?: InsertTarget;
  readRows?: () => readonly Record<string, CsvCell>[];
}): Promise<WriteResult> {
  if (!options.plan.allowed) return blockedExport(options.plan.rejectReason ?? tr("export.tasks.blockedExport"));
  if (options.plan.reexecutes && !options.acknowledged) return blockedExport(tr("export.tasks.needAck"));
  if ((options.format ?? "csv") === "insert" && !options.insert?.name) return blockedExport(tr("export.tasks.noInsertTarget"));
  let readCalled = false;
  let rows = options.rows;
  if (options.plan.serverRead) {
    if (!options.readRows) return blockedExport(tr("export.tasks.noSource"));
    readCalled = true;
    rows = options.readRows();
  }
  const fileName = options.path.split(/[/\\]/).pop() || options.plan.fileName;
  if ((options.format ?? "csv") === "xlsx") {
    const { renderXlsx } = await import("./xlsx");
    downloadBytes(fileName, renderXlsx(options.columns, rows), "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet");
  }
  else downloadCsv(fileName, renderExportText({ ...options, rows }));
  let message = tr("export.tasks.downloaded", { n: rows.length, name: fileName });
  if (options.plan.partial) message = tr("export.tasks.downloadedPartial", { n: rows.length, name: fileName });
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
    format?: ExportFormat;
    insert?: InsertTarget;
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
  format?: ExportFormat;
  insert?: InsertTarget;
  control?: WriteControl;
  readRows?: () => readonly Record<string, CsvCell>[];
}): Promise<{ task: ExportTask; result: WriteResult }> {
  const fileName = options.path.split(/[/\\]/).pop() || options.plan.fileName;
  const task = reactive<ExportTask>({
    id: `export-${taskSeq}`,
    fileName,
    path: options.path,
    status: "running",
    doneRows: 0,
    totalRows: options.rows.length,
    partial: options.plan.partial,
    message: tr("export.tasks.exporting"),
    cancellable: false,
    residualPath: null,
  });
  taskSeq += 1;
  exportTasks.unshift(task);
  let result: WriteResult;
  try {
    if (!options.plan.allowed) result = blockedExport(options.plan.rejectReason ?? tr("export.tasks.blockedExport"));
    else if (isNativeExport() && (options.format ?? "csv") !== "xlsx") {
      if (options.plan.serverRead) result = blockedExport(tr("export.tasks.needConnectionFull"));
      else {
        await writeNativeCsv(options.path, renderExportText(options));
        result = {
          status: "done", success: true, writtenRows: options.rows.length, path: options.path,
          residualPath: null, readCalled: false,
          message: `${tr("export.tasks.exportedTo", { n: options.rows.length, path: options.path })}${options.plan.partial ? tr("export.tasks.partialOnly") : ""}`,
        };
      }
    } else result = import.meta.env.VITEST ? await startExportOnNode(options, task) : await exportLoadedInBrowser(options);
  } catch (error) {
    result = { status: "failed", success: false, writtenRows: 0, path: null, residualPath: null, readCalled: false,
      message: tr("export.tasks.exportFailed", { error: error instanceof Error ? error.message : String(error) }) };
  }
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
  /** 整表关键字：table 模式全量导出与网格同一口径。 */
  search?: string | null;
  limit: number;
  truncated: boolean;
  statement: string;
  format?: ExportFormat;
  insert?: InsertTarget;
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
    task.message = result.message || tr("export.tasks.notFinished");
  }
}

export function runRemoteExport(options: RemoteExportOptions): { task: ExportTask; done: Promise<ExportTask> } {
  const fileName = options.path.split(/[/\\]/).pop() || options.plan.fileName;
  const task = reactive<ExportTask>({
    id: `export-${taskSeq}`,
    fileName,
    path: options.path,
    status: "running",
    doneRows: 0,
    totalRows: options.mode === "table-loaded" ? options.limit : 0,
    partial: options.plan.partial,
    message: tr("export.tasks.remoteExporting"),
    residualPath: null,
  });
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
    search: options.search ?? null,
    limit: options.limit,
    truncated: options.truncated,
    statement: options.statement,
    acknowledged: options.acknowledged,
    path: options.path,
    delimiter: options.csv.delimiter,
    nullToken: options.csv.nullToken,
    bom: options.csv.bom,
    header: options.csv.header,
    format: options.format ?? "csv",
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

// ---------------------------------------------------------------- 全库导出（R7）任务

function asDumpStatus(status: string): ExportTask["status"] {
  if (status === "done" || status === "failed" || status === "cancelled" || status === "blocked" || status === "running") return status;
  return "failed";
}

function applyDumpResult(task: ExportTask, result: DumpResult): void {
  task.status = asDumpStatus(result.status);
  task.doneRows = result.rows;
  task.message = result.message;
  task.residualPath = result.residualPath;
  if (result.path) {
    task.path = result.path;
    task.fileName = result.path.split(/[/\\]/).pop() || task.fileName;
  }
  if (result.success && result.status !== "done") {
    task.status = "failed";
    task.message = result.message || tr("export.tasks.notFinished");
  }
}

/** dump_progress 事件落到任务行：结构段报对象名，数据段报该表已写入的行数。 */
export function applyDumpProgress(payload: DumpProgressEvent): void {
  const task = exportTasks.find((item) => item.id === payload.id);
  if (!task || task.status !== "running") return;
  task.object = payload.object;
  if (payload.phase === "data") {
    task.doneRows = payload.rows;
    task.message = tr("export.tasks.dataProgress", { object: payload.object, rows: payload.rows });
  } else {
    task.message = tr("export.tasks.structureProgress", { object: payload.object });
  }
}

let dumpListener: Promise<void> | null = null;

/** 进度事件只在桌面端订阅；浏览器与测试环境由调用方直接喂 applyDumpProgress。 */
function ensureDumpProgressListener(): Promise<void> {
  if (!isNativeExport()) return Promise.resolve();
  if (!dumpListener) {
    dumpListener = import("@tauri-apps/api/event")
      .then(({ listen }) => listen<DumpProgressEvent>("dump_progress", (event) => applyDumpProgress(event.payload)))
      .then((unlisten) => {
        window.addEventListener("beforeunload", () => unlisten(), { once: true });
      })
      .catch(() => undefined);
  }
  return dumpListener;
}

export function runDatabaseDump(options: DumpPayload): { task: ExportTask; done: Promise<ExportTask> } {
  const fileName = options.path.split(/[/\\]/).pop() || "database.sql";
  const task = reactive<ExportTask>({
    id: options.id,
    kind: "dump",
    fileName,
    path: options.path,
    status: "running",
    doneRows: 0,
    totalRows: 0,
    partial: false,
    message: tr("export.tasks.dumpExporting"),
    residualPath: null,
  });
  exportTasks.unshift(task);
  const done = (async () => {
    await ensureDumpProgressListener();
    try {
      const result = await databaseDump(options);
      applyDumpResult(task, result);
    } catch (error: unknown) {
      task.status = "failed";
      // T6-A2/R6：dump/migrate 的后端错误带 [code] 前缀，先按码取字典，未知码回退原文。
      task.message = backendErrorText(error, error instanceof Error ? error.message : String(error));
    }
    return task;
  })();
  return { task, done };
}

// ---------------------------------------------------------------- 跨库迁移（R8）任务

function asMigrateStatus(status: string): ExportTask["status"] {
  if (status === "done" || status === "failed" || status === "cancelled" || status === "blocked" || status === "running") return status;
  return "failed";
}

function applyMigrateResult(task: ExportTask, result: MigrateResult): void {
  task.status = asMigrateStatus(result.status);
  task.doneRows = result.inserted + result.skipped + result.failed;
  task.message = result.message;
  task.report = { failedIds: result.failedIds, firstErrors: result.firstErrors };
  if (result.success && result.status !== "done") {
    task.status = "failed";
    task.message = result.message || tr("export.tasks.migrateNotDone");
  }
}

/** migrate_progress 事件落到任务行：迁移没有总行数，展示已处理的插入/跳过/失败。 */
export function applyMigrateProgress(payload: MigrateProgressEvent): void {
  const task = exportTasks.find((item) => item.id === payload.id);
  if (!task || task.status !== "running") return;
  task.doneRows = payload.rows;
  task.message = tr("export.migrate.migratingLine", { rows: payload.rows, inserted: payload.inserted, skipped: payload.skipped, failed: payload.failed });
}

let migrateListener: Promise<void> | null = null;

/** 迁移进度事件只在桌面端订阅；浏览器与测试环境由调用方直接喂 applyMigrateProgress。 */
function ensureMigrateProgressListener(): Promise<void> {
  if (!isNativeExport()) return Promise.resolve();
  if (!migrateListener) {
    migrateListener = import("@tauri-apps/api/event")
      .then(({ listen }) => listen<MigrateProgressEvent>("migrate_progress", (event) => applyMigrateProgress(event.payload)))
      .then((unlisten) => {
        window.addEventListener("beforeunload", () => unlisten(), { once: true });
      })
      .catch(() => undefined);
  }
  return migrateListener;
}

export function runMigrateTask(options: MigratePayload): { task: ExportTask; done: Promise<ExportTask> } {
  const name = options.source.table === options.target.table
    ? options.source.table
    : `${options.source.table} → ${options.target.table}`;
  const task = reactive<ExportTask>({
    id: options.id,
    kind: "migrate",
    fileName: tr("export.migrate.migrationLabel", { name }),
    path: `${options.source.connectionId}/${options.source.table} → ${options.target.connectionId}/${options.target.table}`,
    status: "running",
    doneRows: 0,
    totalRows: 0,
    partial: false,
    message: tr("export.migrate.preparing"),
    residualPath: null,
  });
  exportTasks.unshift(task);
  const done = (async () => {
    await ensureMigrateProgressListener();
    try {
      const result = await migrateData(options);
      applyMigrateResult(task, result);
    } catch (error: unknown) {
      task.status = "failed";
      // T6-A2/R6：dump/migrate 的后端错误带 [code] 前缀，先按码取字典，未知码回退原文。
      task.message = backendErrorText(error, error instanceof Error ? error.message : String(error));
    }
    return task;
  })();
  return { task, done };
}

/** 取消按任务类型分流：表/查询导出走 grid_export_cancel，全库导出走 database_dump_cancel，迁移走 migrate_cancel。 */
export function cancelTask(task: ExportTask): Promise<void> {
  if (task.kind === "dump") return databaseDumpCancel(task.id);
  if (task.kind === "migrate") return migrateCancel(task.id);
  return gridExportCancel(task.id);
}

/** 后台任务状态的直白文字：颜色不作唯一信息来源，状态词始终在。 */
export function taskStatusLabel(status: ExportTask["status"]): string {
  if (status === "running") return tr("export.tasks.statusRunning");
  if (status === "done") return tr("export.tasks.statusDone");
  if (status === "cancelled") return tr("export.tasks.statusCancelled");
  if (status === "blocked") return tr("export.tasks.statusBlocked");
  return tr("export.tasks.statusFailed");
}

export function taskStatusText(task: ExportTask): string {
  if (task.status === "running") {
    if (task.kind === "dump" || task.kind === "migrate") return task.message;
    return task.totalRows > 0 ? tr("export.tasks.exportProgressRows", { done: task.doneRows, total: task.totalRows }) : tr("export.tasks.exportProgressDone", { done: task.doneRows });
  }
  if (task.status === "done") {
    if (task.kind === "dump" || task.kind === "migrate") return task.message;
    return task.partial ? tr("export.tasks.doneRowsPartial", { n: task.totalRows }) : tr("export.tasks.doneRows", { n: task.totalRows });
  }
  if (task.status === "cancelled") {
    if (task.kind === "migrate") return task.message;
    return tr("export.tasks.cancelledDeleted");
  }
  if (task.status === "blocked") return tr("export.tasks.blockedStart", { message: task.message });
  if (task.kind === "migrate") return tr("export.tasks.migrateFailedShort", { message: task.message || tr("export.tasks.unknownReason") });
  return tr("export.tasks.exportFailedShort", { message: task.message || tr("export.tasks.unknownReason") });
}

/** 已读取的行不再查询数据库。停在 0 行的旧任务改成未完成，标题不再一直显示「导出 0/N」。 */
export function failIdleRemoteExports(message: string): void {
  for (const task of exportTasks) {
    if (task.status !== "running" || task.doneRows !== 0 || task.totalRows <= 0) continue;
    if (!task.message.startsWith(tr("export.tasks.remoteExporting")) && !task.message.startsWith(tr("export.tasks.dumpExporting"))) continue;
    task.status = "failed";
    task.message = message;
  }
}
