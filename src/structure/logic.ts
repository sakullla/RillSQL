import type { ColumnDraft, StructureColumn, StructureIndex } from "./types";

export function draftsFromColumns(columns: readonly StructureColumn[]): ColumnDraft[] {
  return columns.map((column) => ({
    original: column.name,
    name: column.name,
    typeName: column.typeName,
    nullable: column.nullable,
    defaultSql: column.defaultSql,
    primary: column.primary,
    auto: column.auto,
    dropped: false,
  }));
}

export function blankColumn(): ColumnDraft {
  return {
    original: null,
    name: "",
    typeName: "TEXT",
    nullable: true,
    defaultSql: "",
    primary: false,
    auto: false,
    dropped: false,
  };
}

export function columnMark(draft: ColumnDraft, baseline: StructureColumn | undefined): "" | "新" | "改" | "删" {
  if (draft.dropped && draft.original) return "删";
  if (!baseline && !draft.dropped) return "新";
  if (columnChanged(draft, baseline)) return "改";
  return "";
}

export function columnChanged(draft: ColumnDraft, baseline: StructureColumn | undefined): boolean {
  if (!baseline) return !draft.dropped;
  if (draft.dropped) return true;
  return (
    draft.name.trim() !== baseline.name ||
    draft.typeName.trim() !== baseline.typeName ||
    draft.nullable !== baseline.nullable ||
    draft.defaultSql.trim() !== (baseline.defaultSql ?? "").trim() ||
    draft.primary !== baseline.primary ||
    draft.auto !== baseline.auto
  );
}

/** 每行「将要发生什么」的直白摘要；与后端 ddl 文案口径一致。 */
export function columnImpact(draft: ColumnDraft, baseline: StructureColumn | undefined): string {
  if (!baseline) {
    return draft.dropped ? "" : `新增字段 ${draft.name.trim() || "（还没有名字）"}，类型 ${draft.typeName.trim() || "（还没有填写）"}`;
  }
  if (draft.dropped) return `删除字段 ${baseline.name}：该列的数据会永久丢掉`;
  const parts: string[] = [];
  if (draft.name.trim() !== baseline.name) parts.push(`重命名 ${baseline.name} → ${draft.name.trim()}：仍在用旧名字的查询要跟着改`);
  if (draft.typeName.trim() !== baseline.typeName)
    parts.push(`类型由 ${baseline.typeName} 改为 ${draft.typeName.trim()}：无法转换的值会使执行失败，数据不会被自动截断或清空`);
  if (draft.nullable !== baseline.nullable) parts.push(draft.nullable ? "改为允许空" : "改为不允许空：现有的空值会使执行失败");
  if (draft.defaultSql.trim() !== (baseline.defaultSql ?? "").trim()) parts.push("默认值改变：只影响之后写入的行");
  return parts.join("；");
}

/** 行级提示的强调程度：删除与收窄类型/空值约束最值得警惕。 */
export function impactLevel(draft: ColumnDraft, baseline: StructureColumn | undefined): "danger" | "warn" | "info" {
  if (draft.dropped && baseline) return "danger";
  if (baseline && draft.typeName.trim() !== baseline.typeName) return "warn";
  if (baseline && !draft.nullable && baseline.nullable) return "warn";
  return "info";
}

/** 相对基线有改动的行数，用于「N 处更改未执行」的持续可见标识。 */
export function pendingCount(rows: readonly ColumnDraft[], baseline: readonly StructureColumn[]): number {
  return rows.filter((row) => columnMark(row, baseline.find((column) => column.name === row.original)) !== "").length;
}

export function canDropIndex(index: StructureIndex): boolean {
  return !index.protected;
}

export function fkActions(engine: string): string[] {
  if (engine === "mysql" || engine === "mariadb") return ["RESTRICT", "CASCADE", "SET NULL", "NO ACTION"];
  return ["RESTRICT", "CASCADE", "SET NULL", "SET DEFAULT", "NO ACTION"];
}

export function statusLabel(status: string): string {
  switch (status) {
    case "ok":
      return "已完成";
    case "failed":
      return "失败";
    case "skipped":
      return "未执行";
    case "unknown":
      return "结果未知";
    case "rolled_back":
      return "已回滚";
    default:
      return status;
  }
}

export function riskLevelLabel(level: string): string {
  if (level === "danger") return "危险";
  if (level === "warn") return "注意";
  return "说明";
}

/** 事务语义一句大白话；与后端 rule_text 口径一致，不承诺这里没有的回滚能力。 */
export function transactionSummary(engine: string, rebuild: boolean): string {
  if (rebuild) {
    return "这次修改会重建整张表：建新表、复制数据、替换旧表在同一个事务里完成，任一步失败都会整体回滚，不会留下半截改动。";
  }
  if (engine === "postgres") {
    return "PostgreSQL 可以把多次修改放进一个事务、失败时整体回滚；但这次逐条执行、失败即停，已经成功的语句不会撤销。";
  }
  if (engine === "mysql" || engine === "mariadb") {
    return "MySQL 与 MariaDB 的每条结构语句改完立即生效，不能整体回滚；逐条执行、失败即停，已经成功的语句不会撤销。";
  }
  return "这些语句逐条执行，失败即停：已经成功的不会撤销，后面的不再执行。";
}

export function foreignKeyLabel(value: boolean | null): string {
  if (value === true) return "ON";
  if (value === false) return "OFF";
  return "未知";
}
