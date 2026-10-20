import { describe, expect, it } from "vitest";
import { findVariables, substituteVariables } from "./variables";

describe("query variables", () => {
  it("collects named placeholders in first-seen order and numbers positional ones", () => {
    expect(findVariables("SELECT * FROM t WHERE a > :min AND b < :max AND c = ? AND d = ? AND a > :min")).toEqual([
      { key: "min", label: ":min" },
      { key: "max", label: ":max" },
      { key: "?1", label: "?1" },
      { key: "?2", label: "?2" },
    ]);
    expect(findVariables("SELECT 1")).toEqual([]);
  });

  it("ignores placeholders inside strings, comments and quoted identifiers", () => {
    expect(findVariables("SELECT ':name'")).toEqual([]);
    expect(findVariables("SELECT 'it''s :name'")).toEqual([]);
    expect(findVariables("SELECT 1 -- :name ?\nSELECT :real")).toEqual([{ key: "real", label: ":real" }]);
    expect(findVariables("SELECT /* :name ? */ :real")).toEqual([{ key: "real", label: ":real" }]);
    expect(findVariables("SELECT `:name` FROM \"t?\"")).toEqual([]);
    expect(findVariables("SELECT $fn$ :name $fn$")).toEqual([]);
    expect(findVariables("SELECT $$ :name ? $$")).toEqual([]);
  });

  it("treats :: casts as casts, not placeholders", () => {
    expect(findVariables("SELECT id::text, total::numeric")).toEqual([]);
    expect(findVariables("SELECT :id::text, id::text FROM t")).toEqual([{ key: "id", label: ":id" }]);
  });

  it("substitutes every occurrence of a name and the i-th question mark", () => {
    expect(substituteVariables("SELECT * FROM t WHERE a > :min AND b < :min", { min: "10" }))
      .toBe("SELECT * FROM t WHERE a > '10' AND b < '10'");
    expect(substituteVariables("SELECT * FROM t WHERE a = ? AND b = ?", { "?1": "x", "?2": "y" }))
      .toBe("SELECT * FROM t WHERE a = 'x' AND b = 'y'");
    expect(substituteVariables("SELECT :a, ?, :b", { a: "1", b: "2", "?1": "3" })).toBe("SELECT '1', '3', '2'");
  });

  it("turns empty input into NULL and leaves no-placeholder sql untouched", () => {
    expect(substituteVariables("SELECT :a, ?, :b", { a: "", "?1": "" })).toBe("SELECT NULL, NULL, NULL");
    expect(substituteVariables("SELECT :a, :b", { a: "" })).toBe("SELECT NULL, NULL");
    expect(substituteVariables("SELECT 'literal' -- :commented", {})).toBe("SELECT 'literal' -- :commented");
  });

  it("escapes single quotes in values", () => {
    expect(substituteVariables("SELECT :name", { name: "O'Brien" })).toBe("SELECT 'O''Brien'");
    expect(substituteVariables("SELECT ?", { "?1": "a'b'c" })).toBe("SELECT 'a''b''c'");
  });

  it("does not substitute inside strings, comments or casts", () => {
    expect(substituteVariables("SELECT ':a' WHERE x = :a", { a: "1" })).toBe("SELECT ':a' WHERE x = '1'");
    expect(substituteVariables("SELECT id::text WHERE x = :id", { id: "1" })).toBe("SELECT id::text WHERE x = '1'");
    expect(substituteVariables("SELECT /* ? */ ?", { "?1": "1" })).toBe("SELECT /* ? */ '1'");
  });
});
