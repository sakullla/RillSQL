import { invoke } from "@tauri-apps/api/core";
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
  const lengthNote = target.typeName.includes("(") ? "目标列限长，超长行会计入失败而不会被截断。" : null;
  if (from === "unknown" || to === "unknown") return "有一侧类型未知，写入前先确认。";
  if (from === "raw" || to === "raw") return "有一侧类型不能判断写回方式，写入前先确认。";
  if (from === "geometry" || to === "geometry") return "几何类型没有跨引擎的通用转换，这些行通常会计入失败。";
  if (from === to) return lengthNote;
  if (to === "text") {
    if (from === "binary") return "二进制将按十六进制文本写入。";
    return lengthNote;
  }
  if (from === "text") {
    if (to === "bool") return "只有 t/f/true/false/0/1/yes/no 形状的值能转成布尔。";
    if (to === "integer" || to === "decimal" || to === "float") return "文本值必须是数字才会被目标接受。";
    if (to === "date" || to === "time" || to === "datetime") return "文本值必须符合目标引擎的日期时间格式。";
    return "文本值必须是偶数长度的十六进制才能写成二进制。";
  }
  if (from === "integer" && (to === "decimal" || to === "float")) return lengthNote;
  if (from === "integer" && to === "bool") return "只有 0/1 能转成布尔，其余值会计入失败。";
  if ((from === "decimal" || from === "float") && to === "integer") return "小数进整数列会被拒绝或截断，不会静默截断。";
  if ((from === "decimal" && to === "float") || (from === "float" && to === "decimal")) return "定点与浮点互换有精度损失。";
  if ((from === "decimal" || from === "float") && to === "bool") return "小数转布尔不可靠，这些行通常会计入失败。";
  if (from === "bool" && (to === "integer" || to === "decimal" || to === "float")) return "布尔写数值列在部分引擎（如 PostgreSQL）会被拒绝。";
  if (from === "date" && to === "datetime") return lengthNote;
  if (from === "datetime" && to === "date") return "日期时间进日期列会丢失时间部分。";
  if ((from === "date" && to === "time") || (from === "time" && to === "date")) return "日期与时刻语义不同，值可能被拒绝。";
  if ((from === "time" && to === "datetime") || (from === "datetime" && to === "time")) return "时刻与日期时间语义不同，值可能被拒绝。";
  return "这两个类型族没有直接转换，这些行通常会计入失败。";
}
