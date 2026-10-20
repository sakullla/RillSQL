import { invoke } from "@tauri-apps/api/core";
import type { Cell, Column, CommitReport, Filter, Row, SortSpec, TableKind, TableSchema } from "./types";
import type { TableTarget } from "./target";

export interface LivePage {
  rows: Row[];
  truncated: boolean;
  sql: string;
}

export interface ChangePayload {
  id: string;
  kind: "update" | "insert" | "delete";
  sets: { column: string; value: Cell; resolved?: boolean }[];
  locator: { column: string; value: Cell }[];
  values: { column: string; value: Cell }[];
}

export interface ExportPayload {
  id: string;
  mode: "table-loaded" | "table-full" | "query";
  connectionId: string;
  database: string;
  schema: string;
  table: string;
  engine: string;
  filters: readonly Filter[];
  sort: SortSpec | null;
  limit: number;
  truncated: boolean;
  statement: string;
  acknowledged: boolean;
  path: string;
  delimiter: string;
  nullToken: string;
  bom: boolean;
  header: boolean;
  format?: string;
}

export interface ExportResult {
  status: string;
  success: boolean;
  writtenRows: number;
  path: string | null;
  residualPath: string | null;
  message: string;
  partial: boolean;
}

interface SchemaDto {
  engine: TableSchema["engine"];
  schema: string;
  name: string;
  kind: TableKind;
  columns: Column[];
  key: string[];
}

interface RowDto {
  id: string;
  cells: Record<string, string | null>;
  located: boolean;
  locatorColumns: string[];
  locatorValues: Array<string | null>;
}

interface PageDto {
  rows: RowDto[];
  truncated: boolean;
  sql: string;
}

function targetFields(target: TableTarget) {
  return {
    connectionId: target.connectionId,
    database: target.database,
    schema: target.schema,
    table: target.table,
    engine: target.engine,
  };
}

function filtersPayload(filters: readonly Filter[]) {
  return filters.map((filter) => ({
    column: filter.column,
    op: filter.op,
    value: filter.value ?? null,
    second: filter.second ?? null,
    values: filter.values ?? [],
  }));
}

function sortPayload(sort: SortSpec | null) {
  return sort ? { column: sort.column, direction: sort.direction } : null;
}

function toRow(dto: RowDto): Row {
  return {
    id: dto.id,
    cells: { ...dto.cells },
    located: dto.located,
    locator: dto.locatorColumns.length ? { columns: dto.locatorColumns, values: dto.locatorValues } : null,
  };
}

export async function gridDescribe(target: TableTarget): Promise<TableSchema> {
  const dto = await invoke<SchemaDto>("grid_describe", { request: targetFields(target) });
  return {
    engine: dto.engine,
    schema: dto.schema,
    name: dto.name,
    kind: dto.kind,
    columns: dto.columns,
    key: dto.key,
  };
}

export async function gridPage(target: TableTarget, filters: readonly Filter[], sort: SortSpec | null, offset: number, limit: number): Promise<LivePage> {
  const dto = await invoke<PageDto>("grid_page", {
    request: {
      ...targetFields(target),
      filters: filtersPayload(filters),
      sort: sortPayload(sort),
      offset,
      limit,
    },
  });
  return { rows: dto.rows.map(toRow), truncated: dto.truncated, sql: dto.sql };
}

export async function gridCount(target: TableTarget, filters: readonly Filter[]): Promise<number> {
  const dto = await invoke<{ matched: number }>("grid_count", {
    request: { ...targetFields(target), filters: filtersPayload(filters) },
  });
  return dto.matched;
}

export async function gridCommit(target: TableTarget, changes: readonly ChangePayload[]): Promise<CommitReport> {
  const dto = await invoke<CommitReport>("grid_commit", {
    request: { ...targetFields(target), changes },
  });
  return { ...dto, rolledBack: false };
}

export function gridExport(payload: ExportPayload): Promise<ExportResult> {
  return invoke<ExportResult>("grid_export", {
    request: {
      id: payload.id,
      mode: payload.mode,
      connectionId: payload.connectionId,
      database: payload.database,
      schema: payload.schema,
      table: payload.table,
      engine: payload.engine,
      filters: filtersPayload(payload.filters),
      sort: sortPayload(payload.sort),
      limit: payload.limit,
      truncated: payload.truncated,
      statement: payload.statement,
      acknowledged: payload.acknowledged,
      path: payload.path,
      delimiter: payload.delimiter,
      nullToken: payload.nullToken,
      bom: payload.bom,
      header: payload.header,
      format: payload.format ?? "csv",
    },
  });
}

export function gridExportCancel(id: string): Promise<void> {
  return invoke<void>("grid_export_cancel", { id });
}
