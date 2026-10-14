export type EngineName = "postgres" | "mysql" | "mariadb" | "sqlite";

export interface ColumnDraft {
  original: string | null;
  name: string;
  typeName: string;
  nullable: boolean;
  defaultSql: string;
  primary: boolean;
  auto: boolean;
  dropped: boolean;
}

export interface StructureColumn {
  name: string;
  typeName: string;
  nullable: boolean;
  defaultSql: string;
  primary: boolean;
  auto: boolean;
  generated: boolean;
}

export interface StructureIndex {
  name: string;
  unique: boolean;
  columns: { name: string; descending: boolean }[];
  protected: boolean;
  reason: string;
}

export interface StructureForeignKey {
  name: string;
  columns: string[];
  refSchema: string;
  refTable: string;
  refColumns: string[];
  onDelete: string;
  onUpdate: string;
}

export interface StructureIncoming {
  name: string;
  table: string;
  schema: string;
  columns: string[];
  refColumns: string[];
}

export interface StructureDescription {
  engine: EngineName;
  database: string;
  schema: string;
  name: string;
  kind: string;
  storage: string | null;
  sqliteVersion: string;
  foreignKeys: boolean | null;
  recordedForeignKeys: boolean | null;
  rowCount: number | null;
  columns: StructureColumn[];
  indexes: StructureIndex[];
  foreignKeyList: StructureForeignKey[];
  incoming: StructureIncoming[];
  displaySql: string;
}

export interface PlanStep {
  sql: string;
  label: string;
}

export interface PlanRisk {
  level: string;
  text: string;
}

export interface StructurePlan {
  blocked: string[];
  risks: PlanRisk[];
  steps: PlanStep[];
  rule: string;
  rebuild: boolean;
  target: string;
}

export interface ReceiptStep extends PlanStep {
  status: "ok" | "failed" | "skipped" | "unknown" | "rolled_back" | string;
  message: string;
}

export interface StructureReceipt {
  blocked: string[];
  rule: string;
  target: string;
  rebuild: boolean;
  steps: ReceiptStep[];
}

export type StructureAction =
  | { kind: "createTable"; name: string; columns: ColumnDraft[] }
  | { kind: "alterColumns"; table: string; columns: ColumnDraft[] }
  | { kind: "createIndex"; table: string; name: string; unique: boolean; columns: { name: string; descending: boolean }[] }
  | { kind: "dropIndex"; table: string; name: string }
  | { kind: "createForeignKey"; table: string; foreignKey: StructureForeignKey }
  | { kind: "dropForeignKey"; table: string; name: string }
  | { kind: "dropTable"; table: string }
  | { kind: "setForeignKeys"; enabled: boolean };
