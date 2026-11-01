import type { Engine } from "../engines";
import type { SequenceInfo } from "../catalog/types";
import type { AdminSections, DatabaseAction, SequenceDraft } from "./types";

/** SQLite 允许的 journal_mode 取值；与 ddl 侧校验的表一致。 */
export const JOURNAL_MODES: readonly string[] = ["delete", "truncate", "persist", "memory", "wal", "off"];

/**
 * 每个引擎只有这些分节：SQLite 看 PRAGMA/附加库/维护，PostgreSQL 看扩展/序列，
 * MySQL 与 MariaDB 看事件/分区。其余引擎（SQL Server 等）没有数据库级工具。
 */
export function adminSections(engine: Engine): AdminSections {
  const sqlite = engine === "sqlite";
  const postgres = engine === "postgres";
  const mysql = engine === "mysql" || engine === "mariadb";
  return {
    pragma: sqlite,
    attached: sqlite,
    maintenance: sqlite,
    extensions: postgres,
    sequences: postgres,
    events: mysql,
    partitions: mysql,
  };
}

function numberOrNull(text: string): number | null {
  const trimmed = text.trim();
  if (trimmed === "") return null;
  const value = Number(trimmed);
  return Number.isFinite(value) ? Math.trunc(value) : null;
}

export function sequenceDraft(source: SequenceInfo): SequenceDraft {
  return {
    increment: String(source.increment),
    minValue: String(source.minValue),
    maxValue: String(source.maxValue),
    start: String(source.startValue),
    cycle: source.cycle,
  };
}

/**
 * 把序列草稿变成 ALTER SEQUENCE 动作：只有真的改过的参数进语句（其余传 null），
 * 一项都没改就返回 null —— 面板据此不打开预览，也不发送任何语句。
 * 返回值带上序列自己的 schema，因为一次读到的序列可能跨多个 schema。
 */
export function sequenceAction(
  source: SequenceInfo,
  draft: SequenceDraft,
): { action: DatabaseAction; schema: string } | null {
  const increment = numberOrNull(draft.increment);
  const minValue = numberOrNull(draft.minValue);
  const maxValue = numberOrNull(draft.maxValue);
  const start = numberOrNull(draft.start);
  const change = {
    increment: increment !== null && increment !== source.increment ? increment : null,
    minValue: minValue !== null && minValue !== source.minValue ? minValue : null,
    maxValue: maxValue !== null && maxValue !== source.maxValue ? maxValue : null,
    start: start !== null && start !== source.startValue ? start : null,
    cycle: draft.cycle === source.cycle ? null : draft.cycle,
  };
  const changed = Object.values(change).some((value) => value !== null);
  if (!changed) return null;
  return { action: { kind: "alterSequence", sequence: source.name, ...change }, schema: source.schema };
}
