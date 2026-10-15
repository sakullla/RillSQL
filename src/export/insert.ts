import { insertSql } from "../grid/sql";
import type { Cell, Column, Engine, TableSchema } from "../grid/types";

export interface InsertTarget {
  engine: Engine;
  schema: string;
  name: string;
  columns: Pick<Column, "name" | "typeName">[];
}

function schemaOf(target: InsertTarget): TableSchema {
  return {
    engine: target.engine,
    schema: target.schema,
    name: target.name,
    kind: "table",
    key: [],
    columns: target.columns.map((column) => ({
      name: column.name,
      typeName: column.typeName,
      nullable: true,
      primary: false,
      unique: false,
      auto: false,
    })),
  };
}

/** 只生成这些行的 INSERT，不带建表或整库脚本。 */
export function insertStatements(target: InsertTarget, columns: readonly string[], rows: readonly Record<string, Cell>[]): string[] {
  const schema = schemaOf(target);
  return rows.map((row) => {
    const values: Record<string, Cell> = {};
    for (const name of columns) values[name] = row[name] ?? null;
    return `${insertSql(schema, values)};`;
  });
}

export function renderInsert(target: InsertTarget, columns: readonly string[], rows: readonly Record<string, Cell>[]): string {
  return insertStatements(target, columns, rows).join("\r\n");
}
