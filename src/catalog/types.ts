export const DEFAULT_OBJECT_PAGE = 100;

export type Engine = "postgres" | "mysql" | "mariadb" | "sqlite";
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
