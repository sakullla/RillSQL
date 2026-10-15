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
  "  status = 'open'",
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
