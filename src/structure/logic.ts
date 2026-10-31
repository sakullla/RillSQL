import { tr } from "../i18n";
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

export type ColumnMark = "" | "new" | "changed" | "dropped";

export function columnMark(draft: ColumnDraft, baseline: StructureColumn | undefined): ColumnMark {
  if (draft.dropped && draft.original) return "dropped";
  if (!baseline && !draft.dropped) return "new";
  if (columnChanged(draft, baseline)) return "changed";
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
    return draft.dropped ? "" : tr("structure.logic.addColumnUnnamed", { name: draft.name.trim() || tr("structure.logic.unnamed"), type: draft.typeName.trim() || tr("structure.logic.unfilled") });
  }
  if (draft.dropped) return tr("structure.logic.dropColumn", { name: baseline.name });
  const parts: string[] = [];
  if (draft.name.trim() !== baseline.name) parts.push(tr("structure.logic.renameColumn", { from: baseline.name, to: draft.name.trim() }));
  if (draft.typeName.trim() !== baseline.typeName)
    parts.push(tr("structure.logic.typeChange", { from: baseline.typeName, to: draft.typeName.trim() }));
  if (draft.nullable !== baseline.nullable) parts.push(draft.nullable ? tr("structure.logic.nullableOn") : tr("structure.logic.nullableOff"));
  if (draft.defaultSql.trim() !== (baseline.defaultSql ?? "").trim()) parts.push(tr("structure.logic.defaultChange"));
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
      return tr("structure.logic.done");
    case "failed":
      return tr("structure.logic.failed");
    case "skipped":
      return tr("structure.logic.skipped");
    case "unknown":
      return tr("structure.logic.unknown");
    case "rolled_back":
      return tr("structure.logic.rolledBack");
    default:
      return status;
  }
}

export function riskLevelLabel(level: string): string {
  if (level === "danger") return tr("structure.logic.danger");
  if (level === "warn") return tr("structure.logic.warn");
  return tr("structure.logic.info");
}

/** 事务语义一句大白话；与后端 rule_text 口径一致，不承诺这里没有的回滚能力。 */
export function transactionSummary(engine: string, rebuild: boolean): string {
  if (rebuild) {
    return tr("structure.logic.txRebuild");
  }
  if (engine === "postgres") {
    return tr("structure.logic.txPg");
  }
  if (engine === "mysql" || engine === "mariadb") {
    return tr("structure.logic.txMysql");
  }
  return tr("structure.logic.txPlain");
}

export function foreignKeyLabel(value: boolean | null): string {
  if (value === true) return "ON";
  if (value === false) return "OFF";
  return tr("structure.logic.unknownStatus");
}
