import { qualifiedName, quoteIdent, sqlCell } from "../grid/sql";
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

/** 按已读取的结果列和值生成 INSERT，不丢别名，也不改成基表列的交集。 */
export function insertStatements(target: InsertTarget, columns: readonly string[], rows: readonly Record<string, Cell>[]): string[] {
  const schema = schemaOf(target);
  const into = qualifiedName(schema);
  if (!columns.length) {
    if (schema.engine === "mysql" || schema.engine === "mariadb") return rows.map(() => `INSERT INTO ${into} () VALUES ();`);
    return rows.map(() => `INSERT INTO ${into} DEFAULT VALUES;`);
  }
  const list = columns.map((name) => quoteIdent(schema.engine, name)).join(", ");
  return rows.map((row) => {
    const literals = columns.map((name) => sqlCell(schema.columns.find((column) => column.name === name), row[name] ?? null)).join(", ");
    return `INSERT INTO ${into} (${list}) VALUES (${literals});`;
  });
}

export function renderInsert(target: InsertTarget, columns: readonly string[], rows: readonly Record<string, Cell>[]): string {
  return insertStatements(target, columns, rows).join("\r\n");
}
