import { describe, expect, it } from "vitest";
import { catalogFixture } from "../catalog/fixtures";
import type { CatalogObject } from "../catalog/types";
import { qualifierFor } from "./columns";
import { completionPlan, metadataNote, quotedIdentifier, statementContext } from "./complete";

function routine(name: string, kind: CatalogObject["kind"], signature = "", relation = ""): CatalogObject {
  return { name, kind, estimatedRows: null, stale: false, signature, relation };
}

describe("catalog completion", () => {
  it("offers loaded non-stale objects with their kinds and skips a failed schema", () => {
    const atlas = structuredClone(catalogFixture()[0]!);
    const pub = atlas.databases[0]!.schemas.find((schema) => schema.name === "public");
    pub!.objects.push(
      routine("add_customer", "function", "integer"),
      routine("purge_old", "procedure", "integer"),
      routine("customers_touch", "trigger", "", "customers"),
      { name: "gone_view", kind: "view", estimatedRows: null, stale: true },
    );
    const restricted = atlas.databases[0]!.schemas.find((schema) => schema.name === "restricted");
    restricted!.objects = [routine("secret_fn", "function")];

    const note = metadataNote(atlas, "atlas_dev");
    expect(note.objects).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ name: "add_customer", kind: "function", label: "public.add_customer(integer)" }),
        expect.objectContaining({ name: "purge_old", kind: "procedure" }),
        expect.objectContaining({ name: "customers_touch", kind: "trigger" }),
        expect.objectContaining({ name: "daily_metrics", kind: "table", schema: "analytics" }),
      ]),
    );
    expect(note.objects.some((item) => item.name === "gone_view" || item.name === "secret_fn")).toBe(false);

    const plan = completionPlan({ prefix: "add_cus", connection: atlas, database: "atlas_dev" });
    const item = plan.items.find((entry) => entry.kind === "function" && entry.label.includes("."));
    expect(item).toMatchObject({ label: "public.add_customer(integer)", detail: "函数" });
    expect(completionPlan({ prefix: "sec", connection: atlas, database: "atlas_dev" }).items.some((entry) => entry.label.includes("secret_fn"))).toBe(false);

    pub!.objects.push(routine("orders_id_seq", "sequence"));
    const sequence = completionPlan({ prefix: "orders_id", connection: atlas, database: "atlas_dev" });
    expect(sequence.items.find((entry) => entry.label.includes("orders_id_seq"))).toMatchObject({ kind: "sequence", detail: "序列" });
  });
});

describe("statement completion context", () => {
  const atlas = catalogFixture()[0]!;
  const objects = metadataNote(atlas, "atlas_dev").objects;
  const columns = [
    { label: "customer_id", detail: "public.orders" },
    { label: "name", detail: "public.customers" },
  ];

  it("detects the clause from the tail and ignores keywords inside string literals", () => {
    expect(statementContext("SELECT id, name FROM customers ", objects).clause).toBe("from");
    expect(statementContext("SELECT id FROM customers WHERE name = 'from dual' ", objects).clause).toBe("where");
    expect(statementContext("SELECT 'select from dual' ", objects).clause).toBe("select");
    expect(statementContext("SELECT id FROM customers o JOIN orders ord ON ord.x = o.x GROUP ", objects).clause).toBe("group");
    expect(statementContext("UPDATE orders SET ", objects).clause).toBe("set");
    expect(statementContext("INSERT INTO orders VALUES ", objects).clause).toBe("values");
  });

  it("collects AS and bare aliases of FROM/JOIN table references", () => {
    const parsed = statementContext(
      "SELECT o.customer_id FROM public.orders AS o JOIN customers c ON c.id = o.customer_id WHERE ",
      objects,
    );
    expect(parsed.clause).toBe("where");
    expect(parsed.aliases.get("o")).toMatchObject({ name: "orders", schema: "public" });
    expect(parsed.aliases.get("c")).toMatchObject({ name: "customers", schema: "public" });
  });

  it("offers only objects, never columns, right after FROM", () => {
    const plan = completionPlan({
      prefix: "",
      connection: atlas,
      database: "atlas_dev",
      columns,
      context: { clause: statementContext("SELECT * FROM ", objects).clause },
    });
    expect(plan.items.some((item) => item.kind === "column")).toBe(false);
    expect(plan.items.some((item) => item.kind === "table")).toBe(true);
    expect(plan.items.some((item) => item.kind === "keyword")).toBe(true);
  });

  it("puts cached columns before objects and keywords in the select list", () => {
    const plan = completionPlan({
      prefix: "",
      connection: atlas,
      database: "atlas_dev",
      columns,
      context: { clause: statementContext("SELECT ", objects).clause },
    });
    expect(plan.items[0]?.kind).toBe("column");
    expect(plan.items.findIndex((item) => item.kind === "column"))
      .toBeLessThan(plan.items.findIndex((item) => item.kind === "table"));
    expect(plan.items.findIndex((item) => item.kind === "column"))
      .toBeLessThan(plan.items.findIndex((item) => item.kind === "keyword"));
  });

  it("resolves an alias qualifier to the aliased table", () => {
    const parsed = statementContext("SELECT * FROM public.orders u WHERE u.", objects);
    const qualified = qualifierFor("u.", objects, parsed.aliases);
    expect(qualified).toMatchObject({ kind: "object", columnPrefix: "" });
    if (qualified.kind === "object") expect(qualified.object).toMatchObject({ name: "orders", schema: "public" });
  });

  it("still reports unknown when the qualifier hits no object and no alias", () => {
    const parsed = statementContext("SELECT * FROM public.orders u WHERE z.", objects);
    expect(qualifierFor("z.", objects, parsed.aliases)).toEqual({ kind: "unknown", name: "z" });
  });
});

describe("rule-based completion", () => {
  const atlas = catalogFixture()[0]!;
  const sources = [
    { alias: "o", label: "public.orders", columns: ["id", "customer_id", "status"] },
    { alias: "c", label: "public.customers", columns: ["id", "name"] },
  ];

  it("narrows ON candidates to the joined tables", () => {
    const plan = completionPlan({
      prefix: "",
      connection: atlas,
      database: "atlas_dev",
      engine: "postgres",
      context: { clause: "on", sources, onTables: ["o"] },
    });
    expect(plan.items.filter((item) => item.kind === "column").map((item) => item.label)).toEqual(["id", "customer_id", "status"]);
  });

  it("offers functions with signatures and snippets by prefix", () => {
    const fn = completionPlan({ prefix: "cou", connection: atlas, database: "atlas_dev", engine: "mysql" });
    expect(fn.items).toEqual(expect.arrayContaining([expect.objectContaining({ kind: "function", label: "count", detail: "count(expr) → bigint" })]));
    const snippet = completionPlan({ prefix: "sel", connection: atlas, database: "atlas_dev", engine: "sqlite" });
    expect(snippet.items.some((item) => item.kind === "snippet" && item.label === "select-from")).toBe(true);
  });

  it("uses each dialect's keyword set", () => {
    const pg = completionPlan({ prefix: "ili", connection: atlas, database: "atlas_dev", engine: "postgres" });
    const mysql = completionPlan({ prefix: "ili", connection: atlas, database: "atlas_dev", engine: "mysql" });
    expect(pg.items.some((item) => item.label === "ILIKE")).toBe(true);
    expect(mysql.items.some((item) => item.label === "ILIKE")).toBe(false);
  });

  it("quotes identifiers by engine", () => {
    expect(quotedIdentifier("postgres", "order items")).toBe('"order items"');
    expect(quotedIdentifier("mysql", "order items")).toBe("\u0060order items\u0060");
  });

  it("keeps bare names and qualified names as separate candidates", () => {
    const plan = completionPlan({ prefix: "ord", connection: atlas, database: "atlas_dev", engine: "postgres" });
    const labels = plan.items.map((item) => item.label);
    expect(labels).toContain("public.orders");
    expect(labels).toContain("orders");
  });

  it("returns only columns inside ON", () => {
    const plan = completionPlan({
      prefix: "",
      connection: atlas,
      database: "atlas_dev",
      engine: "postgres",
      context: { clause: "on", sources, onTables: ["o", "c"] },
    });
    expect(plan.items.every((item) => item.kind === "column")).toBe(true);
  });

  it("narrows UPDATE SET and INSERT VALUES to the target table", () => {
    const plan = completionPlan({
      prefix: "",
      connection: atlas,
      database: "atlas_dev",
      engine: "postgres",
      context: { clause: "set", sources, targetTable: "customers" },
    });
    expect(plan.items.map((item) => item.label)).toEqual(["id", "name"]);
    expect(plan.items.every((item) => item.kind === "column")).toBe(true);
  });

  it("reads the clause of a WITH statement", () => {
    const scope = statementContext("WITH recent AS (SELECT 1) SELECT ", metadataNote(atlas, "atlas_dev").objects);
    expect(scope.clause).toBe("select");
  });

  it("ranks SELECT before snippets and offers databases and schemas", () => {
    const plan = completionPlan({ prefix: "SEL", connection: atlas, database: "atlas_dev", engine: "postgres" });
    expect(plan.items[0]).toMatchObject({ kind: "keyword", label: "SELECT" });
    const all = completionPlan({ prefix: "", connection: atlas, database: "atlas_dev", engine: "postgres" });
    expect(all.items.some((item) => item.kind === "database")).toBe(true);
    expect(all.items.some((item) => item.kind === "schema")).toBe(true);
  });

  it("keeps SELECT ahead of an object that shares the prefix", () => {
    const catalog = structuredClone(atlas);
    catalog.databases[0]!.schemas[0]!.objects.push({ name: "sel_archive", kind: "table", estimatedRows: null, stale: false });
    const plan = completionPlan({ prefix: "SEL", connection: catalog, database: "atlas_dev", engine: "postgres" });
    expect(plan.items[0]).toMatchObject({ kind: "keyword", label: "SELECT" });
  });

  it("keeps SELECT ahead of subsequence matches from other groups", () => {
    const plan = completionPlan({
      prefix: "SEL",
      connection: atlas,
      database: "atlas_dev",
      engine: "postgres",
      columns: [{ label: "serial_number", detail: "public.orders" }],
    });
    expect(plan.items[0]).toMatchObject({ kind: "keyword", label: "SELECT" });
  });

  it("offers only databases after USE", () => {
    const plan = completionPlan({
      prefix: "",
      connection: atlas,
      database: "atlas_dev",
      engine: "postgres",
      context: { clause: "use" },
    });
    expect(plan.items.length).toBeGreaterThan(0);
    expect(plan.items.every((item) => item.kind === "database")).toBe(true);
  });

  it("gives no columns inside ON when the joined tables are unknown", () => {
    const plan = completionPlan({
      prefix: "",
      connection: atlas,
      database: "atlas_dev",
      engine: "postgres",
      columns: [{ label: "other_col", detail: "public.other" }],
      context: { clause: "on" },
    });
    expect(plan.items.some((item) => item.kind === "column")).toBe(false);
  });

  it("offers inferred join conditions before columns inside ON", () => {
    const plan = completionPlan({
      prefix: "",
      connection: atlas,
      database: "atlas_dev",
      engine: "postgres",
      context: {
        clause: "on",
        sources: [
          { alias: "o", label: "public.orders", columns: ["id", "customer_id"] },
          { alias: "c", label: "public.customers", columns: ["id", "name"] },
        ],
        onTables: ["o", "c"],
        onConditions: ["o.customer_id = c.id"],
      },
    });
    expect(plan.items[0]).toMatchObject({ kind: "condition", label: "o.customer_id = c.id" });
    expect(plan.items.some((item) => item.kind === "column")).toBe(true);
  });
});
