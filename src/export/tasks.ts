import { reactive } from "vue";
import type { CsvCell, CsvOptions } from "./csv";
import type { ExportPlan } from "./plan";
import { startExport, type WriteControl, type WriteResult } from "./write";

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

export function runExportTask(options: {
  plan: ExportPlan;
  acknowledged: boolean;
  path: string;
  columns: readonly string[];
  rows: readonly Record<string, CsvCell>[];
  csv: CsvOptions;
  control?: WriteControl;
  readRows?: () => readonly Record<string, CsvCell>[];
}): { task: ExportTask; result: WriteResult } {
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
  const result = startExport({
    ...options,
    onProgress: (progress) => {
      task.status = progress.status === "blocked" ? task.status : progress.status;
      task.doneRows = progress.writtenRows;
      task.totalRows = progress.totalRows;
    },
  });
  task.status = result.status;
  task.doneRows = result.writtenRows;
  task.message = result.message;
  task.residualPath = result.residualPath;
  if (result.path) task.path = result.path;
  return { task, result };
}
