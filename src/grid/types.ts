export type Engine = "postgres" | "mysql" | "mariadb" | "sqlite";
export type TableKind = "table" | "view" | "query";
export type Cell = string | null;
export type FilterOp = "eq" | "ne" | "contains" | "starts" | "gt" | "lt" | "between" | "in" | "null" | "notnull" | "empty";

export interface Column {
  name: string;
  typeName: string;
  nullable: boolean;
  primary: boolean;
  unique: boolean;
  auto: boolean;
}

export interface Locator {
  columns: string[];
  values: Cell[];
}

export interface Row {
  id: string;
  cells: Record<string, Cell>;
  locator: Locator | null;
  located: boolean;
  /** 这一行的格子已经是完整原文，不再把协议形状当成未展开的预览。 */
  resolved?: boolean;
}

export interface TableSchema {
  engine: Engine;
  schema: string;
  name: string;
  kind: TableKind;
  columns: Column[];
  key: string[];
}

export interface TableStore {
  schema: TableSchema;
  rows: Row[];
  readError: string | null;
}

export interface Filter {
  column: string;
  op: FilterOp;
  value?: string;
  /** between 的上界，value 是下界。 */
  second?: string;
  /** in 的精确匹配列表。 */
  values?: string[];
}

export interface SortSpec {
  column: string;
  direction: "asc" | "desc";
}

export interface PageRequest {
  filters: Filter[];
  sort: SortSpec | null;
  offset: number;
  limit: number;
}

export interface PageOk {
  ok: true;
  rows: Row[];
  matched: number;
  truncated: boolean;
  sql: string;
}

export interface PageErr {
  ok: false;
  error: string;
  sql: string;
  /** 失败不得显示成无匹配。 */
  emptyMatch: false;
}

export type PageOutcome = PageOk | PageErr;

export type Change =
  | { kind: "update"; id: string; rowId: string; sets: Record<string, Cell> }
  | { kind: "insert"; id: string; values: Record<string, Cell | undefined> }
  | { kind: "delete"; id: string; rowId: string };

export interface CommitStep {
  id: string;
  sql: string;
  label: string;
}

export interface CommitReport {
  applied: CommitStep[];
  failed: (CommitStep & { message: string }) | null;
  pendingIds: string[];
  rolledBack: false;
  rule: string;
}

export const COMMIT_RULE = "按顺序逐条提交，遇到错误即停止；已成功的语句不撤销。";

export interface FilterOpMeta {
  id: FilterOp;
  label: string;
  needsValue: boolean;
}

export const FILTER_OPS: readonly FilterOpMeta[] = [
  { id: "eq", label: "=", needsValue: true },
  { id: "ne", label: "≠", needsValue: true },
  { id: "contains", label: "包含", needsValue: true },
  { id: "starts", label: "开头是", needsValue: true },
  { id: "gt", label: ">", needsValue: true },
  { id: "lt", label: "<", needsValue: true },
  { id: "between", label: "介于", needsValue: true },
  { id: "in", label: "属于(in)", needsValue: true },
  { id: "null", label: "为 NULL", needsValue: false },
  { id: "notnull", label: "不为 NULL", needsValue: false },
  { id: "empty", label: "为空字符串", needsValue: false },
];

export function opNeedsValue(op: FilterOp): boolean {
  return FILTER_OPS.find((item) => item.id === op)?.needsValue ?? true;
}

export function opLabel(op: FilterOp): string {
  return FILTER_OPS.find((item) => item.id === op)?.label ?? op;
}
