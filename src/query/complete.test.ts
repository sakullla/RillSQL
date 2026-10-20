import { describe, expect, it } from "vitest";
import { catalogFixture } from "../catalog/fixtures";
import type { CatalogObject } from "../catalog/types";
import { qualifierFor } from "./columns";
import { completionPlan, metadataNote, statementContext } from "./complete";

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
    const item = plan.items.find((entry) => entry.kind === "function");
    expect(item).toMatchObject({ label: "public.add_customer(integer)", detail: "函数" });
    expect(completionPlan({ prefix: "sec", connection: atlas, database: "atlas_dev" }).items.some((entry) => entry.label.includes("secret_fn"))).toBe(false);
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
