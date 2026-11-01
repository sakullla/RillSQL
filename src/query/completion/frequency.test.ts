import { beforeEach, describe, expect, it } from "vitest";
import { FREQUENCY_KEY, FREQUENCY_LIMIT, bumpIdentifiers, clearFrequency, identifiers, loadFrequency } from "./frequency";

describe("local completion frequency", () => {
  beforeEach(() => localStorage.clear());

  it("counts identifiers, ignores keywords, strings and comments", () => {
    expect(identifiers("SELECT customer_id FROM orders WHERE status = 'paid' -- note")).toEqual(["customer_id", "orders", "status"]);
    expect(identifiers("/* ignored */ select 'it''s secret' from \"order items\"")).toEqual(["order items"]);
    const map = bumpIdentifiers("SELECT customer_id, orders.id FROM orders");
    expect(map).toEqual({ customer_id: 1, orders: 2, id: 1 });
    expect(Object.keys(map).some((key) => ["select", "from", "where"].includes(key))).toBe(false);
  });

  it("is case insensitive and keeps one entry per identifier", () => {
    bumpIdentifiers("SELECT Orders.id FROM Orders");
    const map = bumpIdentifiers("select ORDERS.id from orders");
    expect(map.orders).toBe(4);
    expect(map.id).toBe(2);
    expect(loadFrequency()).toEqual(map);
  });

  it("persists only identifiers and counts, never SQL text", () => {
    bumpIdentifiers("SELECT secret_token FROM audit_log WHERE memo = 'do not store me'");
    const raw = localStorage.getItem(FREQUENCY_KEY);
    expect(raw).toBeTruthy();
    expect(raw).not.toContain("select");
    expect(raw).not.toContain("do not store me");
    const parsed = JSON.parse(raw!) as Record<string, unknown>;
    expect(Object.keys(parsed).sort()).toEqual(["audit_log", "memo", "secret_token"]);
    expect(Object.values(parsed).every((value) => typeof value === "number")).toBe(true);
  });

  it("caps the table at the documented limit", () => {
    const names = Array.from({ length: FREQUENCY_LIMIT + 30 }, (_, index) => `col_${index}`);
    bumpIdentifiers(`SELECT ${names.join(", ")} FROM t`);
    expect(Object.keys(loadFrequency())).toHaveLength(FREQUENCY_LIMIT);
  });

  it("drops everything on clear and survives a corrupted store", () => {
    bumpIdentifiers("SELECT orders.id FROM orders");
    expect(Object.keys(loadFrequency()).length).toBeGreaterThan(0);
    clearFrequency();
    expect(loadFrequency()).toEqual({});
    expect(localStorage.getItem(FREQUENCY_KEY)).toBeNull();

    localStorage.setItem(FREQUENCY_KEY, "not json");
    expect(loadFrequency()).toEqual({});
  });
});
