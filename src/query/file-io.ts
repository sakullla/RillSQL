import { chooseExportPath, isNativeExport, writeNativeFile } from "../export/native";
import type { OpenPlan } from "../history/plan";

/** 打开对话框只接受 SQL 与纯文本文件，accept 写法与导入对话框一致。 */
export const SQL_FILE_ACCEPT = ".sql,.txt,text/plain";

/** 编辑器一次载入的上限：约 2 MB，超大的脚本不适合整篇放进草稿。 */
export const MAX_SQL_FILE_BYTES = 2 * 1024 * 1024;

/** 保存 SQL 的默认文件名：query-日期.sql，与导出对话框的「fileStem-日期.扩展名」同一模式。 */
export function sqlSaveName(now: Date = new Date()): string {
  return `query-${now.toISOString().slice(0, 10)}.sql`;
}

/** 载入前的检查：超大文件直接说明原因，不进入编辑器。 */
export function sqlFileProblem(file: File): string | null {
  if (file.size <= MAX_SQL_FILE_BYTES) return null;
  return `文件有 ${(file.size / 1024 / 1024).toFixed(1)} MB，超过 2 MB 上限，没有载入。`;
}

export async function readSqlFile(file: File): Promise<string> {
  return file.text();
}

/** 从文件载入的草稿计划：只替换当前标签的文本，不携带执行目标，也不会执行。 */
export function fileOpenPlan(sql: string): OpenPlan {
  return { sql, connectionId: null, connectionName: "", database: null, engine: "", needsTarget: false, execute: false };
}

/** 浏览器兜底：与导出下载相同的方式，用临时链接把草稿存成文件。 */
function downloadSqlFile(fileName: string, contents: string): void {
  const blob = new Blob([contents], { type: "text/plain;charset=utf-8" });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = fileName;
  document.body.append(link);
  link.click();
  link.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 30_000);
}

export interface SqlFileOutcome {
  status: "saved" | "cancelled" | "failed";
  message: string;
}

/** 把草稿保存成 .sql 文件：原生先弹保存对话框再写文件，浏览器直接下载。 */
export async function saveSqlFile(options: { defaultPath: string; contents: string }): Promise<SqlFileOutcome> {
  let path: string | null;
  try {
    path = await chooseExportPath(options.defaultPath, "sql");
  } catch (error) {
    return { status: "failed", message: `无法打开保存对话框：${error instanceof Error ? error.message : String(error)}` };
  }
  if (!path) return { status: "cancelled", message: "" };
  const fileName = path.split(/[/\\]/).pop() || options.defaultPath;
  if (isNativeExport()) {
    try {
      await writeNativeFile(path, options.contents);
      return { status: "saved", message: `已保存到 ${path}。` };
    } catch (error) {
      return { status: "failed", message: error instanceof Error ? error.message : String(error) };
    }
  }
  downloadSqlFile(fileName, options.contents);
  return { status: "saved", message: `已开始下载 ${fileName}。` };
}
