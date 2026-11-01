import { describe, expect, it } from "vitest";
import { dialectAssets, quoteIdentifier } from "./dialect";

describe("dialect assets", () => {
  it("exposes keywords and builtins per engine", () => {
    for (const engine of ["postgres", "mysql", "sqlite"] as const) {
      const assets = dialectAssets(engine);
      expect(assets.keywords.length).toBeGreaterThan(200);
      expect(assets.types.length).toBeGreaterThan(5);
    }
  });

  it("quotes only identifiers that need it, per engine", () => {
    expect(quoteIdentifier("postgres", "orders")).toBe("orders");
    expect(quoteIdentifier("postgres", "my table")).toBe('"my table"');
    expect(quoteIdentifier("mysql", "my table")).toBe("\u0060my table\u0060");
    expect(quoteIdentifier("sqlite", 'we"ird')).toBe('"we""ird"');
  });
});
