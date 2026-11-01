import type { Engine } from "../engines";

export const DEFAULT_OBJECT_PAGE = 100;

export type { Engine };
export type EnvTone = "dev" | "test" | "prod" | "local" | "none";
export type ConnStatus = "connected" | "disconnected" | "connecting" | "error" | "lost";
export type ObjectKind = "table" | "view" | "function" | "procedure" | "trigger" | "sequence" | "enum" | "domain";

export interface CatalogObject {
  name: string;
  kind: ObjectKind;
  estimatedRows: number | null;
  stale: boolean;
  signature?: string;
  relation?: string;
  /** PostgreSQL 物化视图仍放在视图组，只用这个标志区分。 */
  materialized?: boolean;
}

export interface KindReport {
  kind: ObjectKind;
  total: number;
  error: string | null;
}

export interface SchemaNode {
  name: string;
  objects: CatalogObject[];
  total: number;
  error: string | null;
  kindReports?: KindReport[];
  pending?: boolean;
}

export interface DatabaseNode {
  name: string;
  schemas: SchemaNode[];
  error: string | null;
}

/** 一节对象详情：条目与该节自己的失败原因，失败不清空其它节。 */
export interface ExtrasList<T> {
  items: T[];
  error: string | null;
}

/** PostgreSQL 扩展；installedVersion 为空表示可安装但未安装。 */
export interface ExtensionInfo {
  name: string;
  defaultVersion: string;
  installedVersion: string | null;
  comment: string;
}

/** 序列参数，数值列可直接用于生成 ALTER SEQUENCE。 */
export interface SequenceInfo {
  schema: string;
  name: string;
  dataType: string;
  startValue: number;
  minValue: number;
  maxValue: number;
  increment: number;
  cacheSize: number;
  cycle: boolean;
  lastValue: number | null;
}

/** MySQL 事件；status 取自 information_schema.EVENTS.STATUS 的小写形式。 */
export interface EventInfo {
  database: string;
  name: string;
  status: string;
  schedule: string;
  onCompletion: string;
  definer: string;
  lastExecuted: string | null;
}

export interface PartitionItem {
  name: string;
  description: string;
  rows: number | null;
}

/** 一张分区表的分区方式与分区清单（只读）。 */
export interface PartitionInfo {
  database: string;
  table: string;
  method: string;
  expression: string;
  partitions: PartitionItem[];
}

/** SQLite 的 PRAGMA 三项，失败的那一项留空并带上原因。 */
export interface SqlitePragmaInfo {
  journalMode: string | null;
  userVersion: number | null;
  foreignKeys: boolean | null;
  error: string | null;
}

export interface AttachedDatabase {
  name: string;
  file: string;
}

/** 连接级对象详情，按需读取；不适用这一节的引擎返回空清单且不报错。 */
export interface CatalogExtras {
  extensions: ExtrasList<ExtensionInfo>;
  sequences: ExtrasList<SequenceInfo>;
  events: ExtrasList<EventInfo>;
  partitions: ExtrasList<PartitionInfo>;
  pragmas: SqlitePragmaInfo;
  attached: ExtrasList<AttachedDatabase>;
}

export interface CatalogConnection {
  id: string;
  name: string;
  engine: Engine;
  env: string;
  envLabel: string;
  envTone: EnvTone;
  status: ConnStatus;
  defaultDatabase: string | null;
  foreignKeys: boolean | null;
  error: string | null;
  databases: DatabaseNode[];
  /** 只有明确请求对象详情时才有值，其余刷新为 null。 */
  extras?: CatalogExtras | null;
}

export interface PaletteTable {
  id: string;
  label: string;
  path: string;
}

export interface PageRequest {
  connectionId: string;
  database: string;
  schema: string;
  offset: number;
  limit: number;
}

export interface PageResult {
  objects: CatalogObject[];
  total: number;
  error: string | null;
  kindReports?: KindReport[];
}

export interface EngineShape {
  databases: boolean;
  schemas: boolean;
  foreignKeys: boolean;
}
