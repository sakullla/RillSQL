import { describe, expect, it } from "vitest";
import { formatDocument, formatSql } from "./format";

const sample = "select   status , 'O pen'  -- keep me\nfrom   orders\nwhere status='open' and note = 'it''s'";
const formatted = [
  "SELECT",
  "  status,",
  "  'O pen' -- keep me",
  "FROM",
  "  orders",
  "WHERE",
  "  status='open'",
  "  AND note = 'it''s'",
].join("\n");

describe("sql formatting", () => {
  it("normalizes keywords, indentation and line breaks without rewriting strings or comments", () => {
    const result = formatSql(sample);
    expect(result).toEqual({ ok: true, sql: formatted });
    expect(result.ok && result.sql).toContain("'O pen'");
    expect(result.ok && result.sql).toContain("-- keep me");
    expect(result.ok && result.sql).toContain("'it''s'");
    expect(formatSql(formatted)).toEqual({ ok: true, sql: formatted });
  });

  it("keeps quoted identifiers, numbers and dollar quotes intact", () => {
    const result = formatSql('select "Select", 10.50, $$ do $$ from t -- it\'s');
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.sql).toContain('"Select"');
    expect(result.sql).toContain("10.50");
    expect(result.sql).toContain("$$ do $$");
    expect(result.sql).toContain("-- it's");
    expect(result.sql.startsWith("SELECT\n")).toBe(true);
  });

  it("leaves the original text in place when a quote, comment or dollar quote is open", () => {
    for (const sql of ["select 'abc", "select 1 /* hi", "select $tag$ hi"]) {
      const result = formatSql(sql);
      expect(result.ok).toBe(false);
      expect(result.sql).toBe(sql);
      if (!result.ok) expect(result.reason.length).toBeGreaterThan(0);
    }
    const quote = formatSql("select 'abc");
    const comment = formatSql("select 1 /* hi");
    const dollar = formatSql("select $tag$ hi");
    expect(quote.ok).toBe(false);
    expect(comment.ok).toBe(false);
    expect(dollar.ok).toBe(false);
    if (!quote.ok) expect(quote.reason).toContain("引号");
    if (!comment.ok) expect(comment.reason).toContain("注释");
    if (!dollar.ok) expect(dollar.reason).toContain("dollar-quote");
  });

  it("keeps unicode identifiers, numeric literals and multi-character operators intact", () => {
    const sql = "select 状态, 1e10, 1.5E-3, 1E+10, 0xFFa, X'Ab', id::text, doc->'k', doc->>'v' -- 状态 1e10 ::\nfrom t";
    const result = formatSql(sql);
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.sql).toContain("状态");
    expect(result.sql).not.toMatch(/状\s+态/);
    for (const literal of ["1e10", "1.5E-3", "1E+10", "0xFFa", "X'Ab'", "::", "->>", "'k'", "'v'", "-- 状态 1e10 ::"]) {
      expect(result.sql).toContain(literal);
    }
    expect(result.sql).not.toMatch(/1\s+e/);
    expect(result.sql).not.toMatch(/0\s+x/i);
    expect(result.sql).not.toMatch(/:\s+:/);
    expect(result.sql).not.toMatch(/-\s+>/);
    expect(result.sql.startsWith("SELECT\n")).toBe(true);
    expect(result.sql).toContain("\nFROM\n");
    const open = formatSql("select 状态 'abc");
    expect(open).toMatchObject({ ok: false, sql: "select 状态 'abc" });
  });

  it("keeps parameters, numeric separators and bit strings as single tokens", () => {
    const sql = "select $1, :name, @name, col$1, 1.e+10, 1.e-10, 1_000, 0b101, B'1010', 状态, 1e10, 0xFFa, X'Ab', id::text, doc->'k', doc->>'v' from t";
    const result = formatSql(sql);
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    for (const literal of ["$1", ":name", "@name", "col$1", "1.e+10", "1.e-10", "1_000", "0b101", "B'1010'", "状态", "1e10", "0xFFa", "X'Ab'", "::", "->", "->>"]) {
      expect(result.sql).toContain(literal);
    }
    expect(result.sql).not.toContain("$ 1");
    expect(result.sql).not.toContain(": name");
    expect(result.sql).not.toContain("@ name");
    expect(result.sql).not.toContain("col $");
    expect(result.sql).not.toContain("1 .");
    expect(result.sql).not.toContain(". e");
    expect(result.sql).not.toContain("e +");
    expect(result.sql).not.toContain("e -");
    expect(result.sql).not.toContain("1 _");
    expect(result.sql).not.toContain("0 b");
    expect(result.sql).not.toContain("B '");
    expect(result.sql).not.toMatch(/:\s+:/);
    expect(result.sql).not.toMatch(/-\s+>/);
    const open = formatSql("select $1, :name, B'1010', 'abc");
    expect(open).toMatchObject({ ok: false, sql: "select $1, :name, B'1010', 'abc" });
  });

  it("keeps a hash comment through the end of the line and does not split glued tokens", () => {
    const commented = "delete from t where id = 1 # or 1=1";
    const result = formatSql(commented);
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.sql).toContain("# or 1=1");
    expect(result.sql).not.toMatch(/\n\s*OR\b/);
    expect(result.sql).toContain("\nWHERE\n");
    const glued = formatSql("delete from t where id=1#or 1=1");
    expect(glued.ok).toBe(true);
    if (!glued.ok) return;
    expect(glued.sql).toContain("id=1#or 1=1");
    expect(glued.sql).not.toMatch(/\n\s*OR\b/);
    const quoted = formatSql("select '# or 1=1' from t");
    expect(quoted.ok).toBe(true);
    if (!quoted.ok) return;
    expect(quoted.sql).toContain("'# or 1=1'");
    expect(quoted.sql).toContain("\nFROM\n");
    const apostrophe = formatSql("select 1 # it's or 1=1\nfrom t");
    expect(apostrophe.ok).toBe(true);
    if (!apostrophe.ok) return;
    expect(apostrophe.sql).toContain("# it's or 1=1");
    expect(apostrophe.sql).not.toMatch(/\n\s*OR\b/);
  });

  it("keeps glued literals intact without inserting whitespace inside them", () => {
    const sql = "select $1, E'O pen', a@>b, a&&b, 0o17, :name, @name, col$1, 1.e+10, 1_000, 0b101, B'1010', 0xFFa, X'Ab', id::text, doc->'k', doc->>'v', 1e10, N'ab', a<@b, 状态 from t";
    const result = formatSql(sql);
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    for (const literal of ["$1", "E'O pen'", "a@>b", "a&&b", "0o17", ":name", "@name", "col$1", "1.e+10", "1_000", "0b101", "B'1010'", "0xFFa", "X'Ab'", "::", "->", "->>", "1e10", "N'ab'", "a<@b", "状态"]) {
      expect(result.sql).toContain(literal);
    }
    expect(result.sql).not.toContain("E '");
    expect(result.sql).not.toContain("@ >");
    expect(result.sql).not.toContain("& &");
    expect(result.sql).not.toContain("0 o");
    expect(result.sql).not.toContain("$ 1");
    expect(result.sql).not.toContain("< @");
    expect(result.sql).not.toContain("N '");
    expect(result.sql).not.toMatch(/:\s+:/);
    expect(result.sql).not.toMatch(/-\s+>/);
  });

  it("refuses a rewrite that would separate adjacent non-whitespace and leaves the original text", () => {
    for (const sql of ["select*from t", "select a,b from t", "delete from t where id=1or id=2"]) {
      const result = formatSql(sql);
      expect(result).toMatchObject({ ok: false, sql });
      expect(result.ok).toBe(false);
      if (!result.ok) expect(result.reason).toContain("无法格式化");
    }
    const kept = formatSql("select f(a,b), 0o17 from t");
    expect(kept.ok).toBe(true);
    if (!kept.ok) return;
    expect(kept.sql).toContain("f(a,b)");
    expect(kept.sql).toContain("0o17");
    expect(kept.sql).not.toContain("0 o");
    const document = "select 1; select*from t";
    const from = document.indexOf("select*from");
    const refused = formatDocument(document, { from, to: document.length });
    expect(refused).toMatchObject({ ok: false, sql: document });
    expect(refused.ok).toBe(false);
    if (!refused.ok) expect(refused.reason).toContain("无法格式化");
  });

  it("formats only the selection and keeps the surrounding text", () => {
    const sql = "select  1;\nselect   2;";
    const from = sql.indexOf("select   2;");
    const result = formatDocument(sql, { from, to: sql.length });
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.sql.slice(0, from)).toBe(sql.slice(0, from));
    expect(result.sql.slice(from)).toBe("SELECT\n  2;");
    const broken = "select 'keep';\nselect 'open";
    const refused = formatDocument(broken, { from: broken.indexOf("select 'open"), to: broken.length });
    expect(refused).toMatchObject({ ok: false, sql: broken });
    expect(refused.ok).toBe(false);
    if (!refused.ok) expect(refused.reason).toContain("引号");
  });
});
