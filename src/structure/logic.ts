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

export function columnImpact(draft: ColumnDraft, baseline: StructureColumn | undefined): string {
  if (!baseline) return draft.dropped ? "" : "新增字段";
  if (draft.dropped) return `删除字段 ${baseline.name}，该列数据会丢掉`;
  const parts: string[] = [];
  if (draft.name.trim() !== baseline.name) parts.push(`重命名 ${baseline.name} → ${draft.name.trim()}`);
  if (draft.typeName.trim() !== baseline.typeName) parts.push(`类型 ${baseline.typeName} → ${draft.typeName.trim()}`);
  if (draft.nullable !== baseline.nullable) parts.push(draft.nullable ? "去掉 NOT NULL" : "设为 NOT NULL");
  if (draft.defaultSql.trim() !== (baseline.defaultSql ?? "").trim()) parts.push("默认值改变");
  return parts.join("；");
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
      return "成功";
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

export function foreignKeyLabel(value: boolean | null): string {
  if (value === true) return "ON";
  if (value === false) return "OFF";
  return "未知";
}
