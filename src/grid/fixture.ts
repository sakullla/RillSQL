import { refreshLocated } from "./data";
import type { Cell, Column, Row, TableSchema, TableStore } from "./types";

function col(name: string, typeName: string, flags: Partial<Column> = {}): Column {
  return {
    name,
    typeName,
    nullable: flags.nullable ?? false,
    primary: flags.primary ?? false,
    unique: flags.unique ?? false,
    auto: flags.auto ?? false,
  };
}

function materialize(schema: TableSchema, raw: Array<{ id: string; cells: Record<string, Cell> }>): Row[] {
  const rows: Row[] = raw.map((item) => ({ id: item.id, cells: item.cells, locator: null, located: false }));
  refreshLocated(schema, rows);
  return rows;
}

export function customersFixture(count = 130): TableStore {
  const schema: TableSchema = {
    engine: "postgres",
    schema: "public",
    name: "customers",
    kind: "table",
    key: ["id"],
    columns: [
      col("id", "integer", { primary: true, unique: true, auto: true }),
      col("name", "text"),
      col("email", "text", { unique: true }),
      col("note", "text", { nullable: true }),
    ],
  };
  const raw = Array.from({ length: count }, (_, index) => {
    const id = String(index + 1);
    let note: Cell = `备注${id}`;
    if (index === 0) note = null;
    if (index === 1) note = "";
    if (index === 2) note = "长".repeat(2000);
    if (index === 3) note = '他说"你好"，\n下一行';
    const name = index === count - 1 ? "000-first" : index % 17 === 0 ? "小河" : `客户${id}`;
    return { id, cells: { id, name, email: `u${id}@example.com`, note } };
  });
  return { schema, rows: materialize(schema, raw), readError: null };
}

export function nokeyFixture(): TableStore {
  const schema: TableSchema = {
    engine: "sqlite",
    schema: "",
    name: "notes",
    kind: "table",
    key: [],
    columns: [col("label", "text", { nullable: true }), col("note", "text", { nullable: true })],
  };
  const raw = [
    { id: "a", cells: { label: "重复", note: null } },
    { id: "b", cells: { label: "重复", note: "" } },
    { id: "c", cells: { label: "唯一", note: "中文" } },
  ];
  return { schema, rows: materialize(schema, raw), readError: null };
}

export function viewFixture(): TableStore {
  const schema: TableSchema = {
    engine: "mysql",
    schema: "shop",
    name: "customer_view",
    kind: "view",
    key: ["id"],
    columns: [col("id", "int", { primary: true }), col("name", "varchar(40)", { nullable: true })],
  };
  return { schema, rows: materialize(schema, [{ id: "1", cells: { id: "1", name: "只读" } }]), readError: null };
}

export function fixtureForTab(tabId: string): TableStore {
  const name = tabId.startsWith("table:") ? tabId.slice("table:".length) : tabId;
  if (name === "nokey" || name === "notes") return nokeyFixture();
  if (name === "view") return viewFixture();
  if (name === "broken") {
    const store = customersFixture(4);
    store.readError = "连接已断开，读取失败";
    return store;
  }
  return customersFixture(130);
}
