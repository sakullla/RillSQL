import { describe, expect, it } from "vitest";
import { referenceJump, referenceQuery } from "./reference";
import type { StructureForeignKey } from "../structure/types";

function foreignKey(overrides: Partial<StructureForeignKey> = {}): StructureForeignKey {
  return {
    name: "orders_customer_fk",
    columns: ["customer_id"],
    refSchema: "public",
    refTable: "customers",
    refColumns: ["id"],
    onDelete: "NO ACTION",
    onUpdate: "NO ACTION",
    ...overrides,
  };
}

describe("reference jump resolution", () => {
  it("picks the foreign key that contains the clicked column", () => {
    const customer = foreignKey();
    const other = foreignKey({ name: "orders_product_fk", columns: ["product_id"], refTable: "products", refColumns: ["sku"] });
    const jump = referenceJump([other, customer], "customer_id", { customer_id: "7" });
    expect(jump?.foreignKey).toEqual(customer);
    expect(jump?.values).toEqual(["7"]);
  });

  it("returns null when no foreign key uses the column", () => {
    expect(referenceJump([foreignKey()], "note", { note: "x" })).toBeNull();
    expect(referenceJump([], "customer_id", { customer_id: "7" })).toBeNull();
  });

  it("refuses null, missing and preview envelope values", () => {
    const keys = [foreignKey()];
    expect(referenceJump(keys, "customer_id", { customer_id: null })).toBeNull();
    expect(referenceJump(keys, "customer_id", {})).toBeNull();
    // 未展开的页查询预览不是完整值，不能当等值条件。
    expect(referenceJump(keys, "customer_id", { customer_id: `\u0001t:2048:${"长".repeat(600)}` })).toBeNull();
    expect(referenceJump(keys, "customer_id", { customer_id: "\u0001b:128" })).toBeNull();
    expect(referenceJump(keys, "customer_id", { customer_id: "7" })).not.toBeNull();
  });

  it("requires every column of a composite foreign key", () => {
    const composite = foreignKey({ columns: ["tenant_id", "customer_id"], refColumns: ["tenant_id", "id"] });
    expect(referenceJump([composite], "customer_id", { tenant_id: "1", customer_id: "7" })).toEqual({
      foreignKey: composite,
      values: ["1", "7"],
    });
    expect(referenceJump([composite], "customer_id", { tenant_id: null, customer_id: "7" })).toBeNull();
    // 多列里有一列是未展开预览时同样放弃。
    expect(referenceJump([composite], "customer_id", { tenant_id: `\u0001t:2048:${"长".repeat(600)}`, customer_id: "7" })).toBeNull();
  });
});

describe("reference query sql", () => {
  it("quotes identifiers per engine and escapes single quotes in values", () => {
    const fk = foreignKey();
    expect(referenceQuery("postgres", fk, ["7"])).toBe('SELECT *\nFROM "public"."customers"\nWHERE "id" = \'7\';');
    expect(referenceQuery("mysql", fk, ["7"])).toBe("SELECT *\nFROM `public`.`customers`\nWHERE `id` = '7';");
    expect(referenceQuery("postgres", fk, ["a' OR '1'='1"])).toBe('SELECT *\nFROM "public"."customers"\nWHERE "id" = \'a\'\' OR \'\'1\'\'=\'\'1\';');
  });

  it("omits the schema part when it is empty and joins composite columns with AND", () => {
    const bare = foreignKey({ refSchema: "" });
    expect(referenceQuery("sqlite", bare, ["7"])).toBe('SELECT *\nFROM "customers"\nWHERE "id" = \'7\';');
    const composite = foreignKey({ columns: ["tenant_id", "customer_id"], refColumns: ["tenant_id", "id"] });
    expect(referenceQuery("postgres", composite, ["1", "7"])).toBe('SELECT *\nFROM "public"."customers"\nWHERE "tenant_id" = \'1\' AND "id" = \'7\';');
  });
});
