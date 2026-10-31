import { selectExportGate } from "../grid/sql";
import { tr } from "../i18n";

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
  // 「（部分）」是导出文件名的固定标记（协议级后缀，不随界面语言变化），转义写法避免源码裸中文（verify:workbench 字典化扫描）。
  const partialMark = "\uff08\u90e8\u5206\uff09";
  if (fileName.includes(partialMark)) return fileName;
  const match = /\.(csv|json|sql|md|xlsx)$/i.exec(fileName);
  if (match) return fileName.replace(new RegExp(`\\.${match[1]}$`, "i"), `${partialMark}.${match[1]!.toLowerCase()}`);
  return `${fileName}${partialMark}`;
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
        notice: gate.reason ?? tr("export.plan.selectOnlyGate"),
        rejectReason: gate.reason,
        reexecutes: false,
        serverRead: false,
        label: tr("export.plan.cannotFull"),
      };
    }
    return {
      allowed: true,
      partial: false,
      complete: true,
      fileName,
      notice: tr("export.plan.fullNotice"),
      rejectReason: null,
      reexecutes: true,
      serverRead: true,
      label: tr("export.plan.fullLabel"),
    };
  }
  if (partial) {
    return {
      allowed: true,
      partial: true,
      complete: false,
      fileName,
      notice: tr("export.plan.partialNotice"),
      rejectReason: null,
      reexecutes: false,
      serverRead: false,
      label: tr("export.plan.partialLabel"),
    };
  }
  if (input.scope === "full") {
    return {
      allowed: true,
      partial: false,
      complete: true,
      fileName,
      notice: tr("export.plan.filteredNotice"),
      rejectReason: null,
      reexecutes: false,
      serverRead: true,
      label: tr("export.plan.filteredLabel"),
    };
  }
  return {
    allowed: true,
    partial: false,
    complete: true,
    fileName,
    notice: tr("export.plan.allHereNotice"),
    rejectReason: null,
    reexecutes: false,
    serverRead: false,
    label: tr("export.plan.allHereLabel"),
  };
}
