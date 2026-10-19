import { describe, expect, it } from "vitest";
import { catalogFixture } from "../catalog/fixtures";
import type { CatalogObject } from "../catalog/types";
import { completionPlan, metadataNote } from "./complete";

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
