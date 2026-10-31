import { invoke } from "@tauri-apps/api/core";
import { tr } from "../i18n";
import { valueFamily } from "../grid/typeset";
import type { Column } from "../grid/types";

/** 跨库迁移（R8）的一端：表定位与 grid 的 TableTarget 同源，engine 由目录树给出。 */
export interface MigrateTableRef {
  connectionId: string;
  engine: string;
  database: string;
  schema: string;
  table: string;
}

export interface MigrateColumnPair {
  source: string;
  target: string;
}

export interface MigrateOptions {
  /** 每批提交的行数；0 用默认页粒度。 */
  batchRows?: number;
}

export interface MigratePayload {
  id: string;
  source: MigrateTableRef;
  target: MigrateTableRef;
  mapping: MigrateColumnPair[];
  /** 允许显式 null：MigrateDialog 固定传 null，Rust 端以 Option 承接并落到默认选项。 */
  options?: MigrateOptions | null;
}

export interface MigrateResult {
  status: string;
  success: boolean;
  message: string;
  inserted: number;
  skipped: number;
  failed: number;
  /** 失败行的主键（无主键时用行号）样本。 */
  failedIds: string[];
  /** 首批失败的具体错误（标识 + 原因）。 */
  firstErrors: string[];
  /** 类型兼容与目标列缺口的警告；警告不阻断。 */
  warnings: string[];
}

/** migrate_progress 事件的载荷：rows 是已处理行数（插入+跳过+失败）。 */
export interface MigrateProgressEvent {
  id: string;
  rows: number;
  inserted: number;
  skipped: number;
  failed: number;
}

export function migrateData(request: MigratePayload): Promise<MigrateResult> {
  return invoke<MigrateResult>("migrate_data", { request });
}

export function migrateCancel(id: string): Promise<void> {
  return invoke<void>("migrate_cancel", { id });
}

/** 同名列自动映射（精确优先，忽略大小写次之），对不上的留空。 */
export function suggestMigrationMapping(sourceColumns: readonly Column[], targetColumns: readonly Column[]): Record<string, string> {
  const mapping: Record<string, string> = {};
  for (const column of sourceColumns) {
    const exact = targetColumns.find((item) => item.name === column.name);
    const loose = targetColumns.find((item) => item.name.toLowerCase() === column.name.toLowerCase());
    mapping[column.name] = exact?.name ?? loose?.name ?? "";
  }
  return mapping;
}

/** 源列 → 目标列的兼容提示：null 完全兼容；字符串为警告文案（尽力转换，不阻断）。与 Rust 端 pair_note 同一矩阵。 */
export function migrationHint(source: Column | undefined, target: Column | undefined): string | null {
  if (!source || !target) return null;
  const from = valueFamily(source.typeName);
  const to = valueFamily(target.typeName);
  const lengthNote = target.typeName.includes("(") ? tr("export.migrateNotes.lengthLimited") : null;
  if (from === "unknown" || to === "unknown") return tr("export.migrateNotes.unknownSide");
  if (from === "raw" || to === "raw") return tr("export.migrateNotes.rawSide");
  if (from === "geometry" || to === "geometry") return tr("export.migrateNotes.geometry");
  if (from === to) return lengthNote;
  if (to === "text") {
    if (from === "binary") return tr("export.migrateNotes.binaryAsHex");
    return lengthNote;
  }
  if (from === "text") {
    if (to === "bool") return tr("export.migrateNotes.boolShapes");
    if (to === "integer" || to === "decimal" || to === "float") return tr("export.migrateNotes.numberNeeded");
    if (to === "date" || to === "time" || to === "datetime") return tr("export.migrateNotes.datetimeNeeded");
    return tr("export.migrateNotes.hexNeeded");
  }
  if (from === "integer" && (to === "decimal" || to === "float")) return lengthNote;
  if (from === "integer" && to === "bool") return tr("export.migrateNotes.intToBool");
  if ((from === "decimal" || from === "float") && to === "integer") return tr("export.migrateNotes.decimalToInt");
  if ((from === "decimal" && to === "float") || (from === "float" && to === "decimal")) return tr("export.migrateNotes.precisionLoss");
  if ((from === "decimal" || from === "float") && to === "bool") return tr("export.migrateNotes.decimalToBool");
  if (from === "bool" && (to === "integer" || to === "decimal" || to === "float")) return tr("export.migrateNotes.boolToNumber");
  if (from === "date" && to === "datetime") return lengthNote;
  if (from === "datetime" && to === "date") return tr("export.migrateNotes.datetimeToDate");
  if ((from === "date" && to === "time") || (from === "time" && to === "date")) return tr("export.migrateNotes.dateVsTime");
  if ((from === "time" && to === "datetime") || (from === "datetime" && to === "time")) return tr("export.migrateNotes.timeVsDatetime");
  return tr("export.migrateNotes.noDirect");
}
