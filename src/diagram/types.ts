export interface DiagramTable {
  name: string;
  columns: string[];
  outside: string[];
}

export interface DiagramEdge {
  name: string;
  fromTable: string;
  fromColumns: string[];
  toTable: string;
  toColumns: string[];
}

export interface OmittedTable {
  name: string;
  message: string;
}

export interface Diagram {
  database: string;
  schema: string;
  tables: DiagramTable[];
  edges: DiagramEdge[];
  omitted: OmittedTable[];
  notice: string | null;
}

export interface DiagramOpen {
  connectionId: string;
  connectionName: string;
  database: string;
  engine: string;
  schema: string;
  path: string;
  label: string;
}
