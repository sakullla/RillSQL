import type { Column, Row } from "../grid/types";
import type { StatementDto } from "./api";

export function gridModel(statement: Pick<StatementDto, "columns" | "rows"> & { columnTypes?: string[] }): { columns: Column[]; rows: Row[] } {
  const types = statement.columnTypes ?? [];
  const columns: Column[] = statement.columns.map((name, index) => ({
    name,
    typeName: types[index] ?? "",
    nullable: true,
    primary: false,
    unique: false,
    auto: false,
  }));
  const rows: Row[] = statement.rows.map((cells, index) => ({
    id: `r${index}`,
    cells: Object.fromEntries(columns.map((column, columnIndex) => [column.name, cells[columnIndex] ?? null])),
    locator: null,
    located: false,
  }));
  return { columns, rows };
}
