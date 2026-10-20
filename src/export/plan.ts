import { selectExportGate } from "../grid/sql";

export type ExportOrigin = "table" | "query";
export type ExportScope = "loaded" | "full";

export interface ExportPlanInput {
  origin: ExportOrigin;
  scope: ExportScope;
  statement?: string;
  truncated: boolean;
  fileName: string;
  /** 查询全量导出按这个引擎切语句。缺省仍按 PostgreSQL。 */
  engine?: string;
}

export interface ExportPlan {
  allowed: boolean;
  partial: boolean;
  complete: boolean;
  fileName: string;
  notice: string;
  rejectReason: string | null;
  reexecutes: boolean;
  serverRead: boolean;
  label: string;
}

export function partialFileName(fileName: string): string {
  if (fileName.includes("（部分）")) return fileName;
  const match = /\.(csv|json|sql|md)$/i.exec(fileName);
  if (match) return fileName.replace(new RegExp(`\\.${match[1]}$`, "i"), `（部分）.${match[1]!.toLowerCase()}`);
  return `${fileName}（部分）`;
}

export function planExport(input: ExportPlanInput): ExportPlan {
  const partial = input.scope === "loaded" && input.truncated;
  const fileName = partial ? partialFileName(input.fileName) : input.fileName;
  if (input.scope === "full" && input.origin === "query") {
    const gate = selectExportGate(input.statement ?? "", input.engine);
    if (!gate.ok) {
      return {
        allowed: false,
        partial: false,
        complete: false,
        fileName,
        notice: gate.reason ?? "只有 SELECT 可以重新执行并全量导出",
        rejectReason: gate.reason,
        reexecutes: false,
        serverRead: false,
        label: "不能全量导出",
      };
    }
    return {
      allowed: true,
      partial: false,
      complete: true,
      fileName,
      notice: "会重新执行这条 SELECT，一边读取一边写入文件。",
      rejectReason: null,
      reexecutes: true,
      serverRead: true,
      label: "全量结果（重新执行 SELECT）",
    };
  }
  if (partial) {
    return {
      allowed: true,
      partial: true,
      complete: false,
      fileName,
      notice: "不是完整结果：只包含已经读取的行。",
      rejectReason: null,
      reexecutes: false,
      serverRead: false,
      label: "部分结果",
    };
  }
  if (input.scope === "full") {
    return {
      allowed: true,
      partial: false,
      complete: true,
      fileName,
      notice: "按当前筛选条件在数据库里读取全部匹配的行，不会先把它们填进表格。",
      rejectReason: null,
      reexecutes: false,
      serverRead: true,
      label: "按当前条件的全部匹配行",
    };
  }
  return {
    allowed: true,
    partial: false,
    complete: true,
    fileName,
    notice: "当前读取的行已经覆盖这次结果。",
    rejectReason: null,
    reexecutes: false,
    serverRead: false,
    label: "已读取的全部行",
  };
}
